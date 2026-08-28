use std::cell::Cell;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::VecDeque;

use gio::prelude::*;
use gio::subclass::prelude::*;
use glib::clone;
use gtk::gio;
use gtk::glib;
use thiserror::Error;

use crate::model;
use crate::types::MessageId;

/// The maximum number of newer messages TDLib can return in a single
/// `getChatHistory` request through a negative offset.
const MAX_NEWER_MESSAGES: i32 = 99;

/// The number of newer messages requested when loading a window of messages
/// around the anchor. The rest of the request limit is filled with older
/// messages, so that the window is balanced.
const AROUND_NEWER_MESSAGES: i32 = 50;

/// The number of items the list is trimmed down to.
const TRIM_TO_ITEMS: usize = 300;
/// The number of items at which the list is trimmed.
const TRIM_AT_ITEMS: usize = 500;

#[derive(Error, Debug)]
pub(crate) enum ChatHistoryError {
    #[error("The chat history is already loading messages")]
    AlreadyLoading,
    #[error("TDLib error: {0:?}")]
    Tdlib(tdlib::types::Error),
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct ChatHistoryModel {
        pub(super) chat: glib::WeakRef<model::Chat>,
        pub(super) is_loading_older: Cell<bool>,
        pub(super) is_loading_newer: Cell<bool>,
        /// The message id from which the initial messages are loaded.
        pub(super) anchor: Cell<MessageId>,
        /// Whether this history is anchored at the last read message of a chat
        /// with unread messages.
        pub(super) is_unread_anchor: Cell<bool>,
        /// Whether all the messages newer than the loaded ones are already in
        /// the list.
        pub(super) at_newest: Cell<bool>,
        pub(super) list: RefCell<VecDeque<model::ChatHistoryItem>>,
        /// Messages that arrived while `at_newest` was false. They are added to
        /// the list once the messages newer than the loaded ones are loaded.
        pub(super) pending_new_messages: RefCell<VecDeque<model::Message>>,
        pub(super) handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChatHistoryModel {
        const NAME: &'static str = "ChatHistoryModel";
        type Type = super::ChatHistoryModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for ChatHistoryModel {
        fn dispose(&self) {
            if let Some(chat) = self.chat.upgrade() {
                for handler in self.handlers.borrow_mut().drain(..) {
                    chat.disconnect(handler);
                }
            }
        }
    }

    impl ListModelImpl for ChatHistoryModel {
        fn item_type(&self) -> glib::Type {
            model::ChatHistoryItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.list.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.list
                .borrow()
                .get(position as usize)
                .map(glib::object::Cast::upcast_ref::<glib::Object>)
                .cloned()
        }
    }
}

glib::wrapper! {
    pub(crate) struct ChatHistoryModel(ObjectSubclass<imp::ChatHistoryModel>)
        @implements gio::ListModel;
}

impl ChatHistoryModel {
    /// Creates a `ChatHistoryModel` for the specified chat.
    ///
    /// If the chat has unread messages, the history is anchored at the last
    /// read message, so that the messages are loaded around it instead of the
    /// last message of the chat.
    pub(crate) fn new(chat: &model::Chat) -> Self {
        let anchor = if chat.unread_count() > 0 {
            chat.last_read_inbox_message_id()
        } else {
            0
        };

        Self::with_anchor(chat, anchor, anchor != 0, anchor == 0)
    }

    /// Creates a `ChatHistoryModel` for the specified chat, anchored at the
    /// message with the specified id.
    ///
    /// This can be used to show a specific message of a chat, e.g. when
    /// jumping to a replied message, without having to load the whole history
    /// in between.
    pub(crate) fn new_for_message(chat: &model::Chat, message_id: MessageId) -> Self {
        Self::with_anchor(chat, message_id, false, false)
    }

    fn with_anchor(
        chat: &model::Chat,
        anchor: MessageId,
        is_unread_anchor: bool,
        at_newest: bool,
    ) -> Self {
        let obj: ChatHistoryModel = glib::Object::new();

        let imp = obj.imp();
        imp.chat.set(Some(chat));
        imp.anchor.set(anchor);
        imp.is_unread_anchor.set(is_unread_anchor);
        imp.at_newest.set(at_newest);

        imp.handlers.borrow_mut().push(chat.connect_new_message(
            clone!(
                #[weak]
                obj,
                move |_, message| {
                    obj.handle_new_message(message);
                }
            ),
        ));
        imp.handlers.borrow_mut().push(
            chat.connect_deleted_message(clone!(
                #[weak]
                obj,
                move |_, message| {
                    obj.handle_deleted_message(message);
                }
            )),
        );

        obj
    }

    /// Loads older messages from this chat history.
    ///
    /// Returns `true` when more messages can be loaded.
    pub(crate) async fn load_older_messages(&self, limit: i32) -> Result<bool, ChatHistoryError> {
        let imp = self.imp();

        if imp.is_loading_older.get() {
            return Err(ChatHistoryError::AlreadyLoading);
        }

        let from_message_id = imp
            .list
            .borrow()
            .iter()
            .rev()
            .find_map(|item| item.message())
            .map(|m| m.id())
            .unwrap_or(imp.anchor.get());

        imp.is_loading_older.set(true);

        let result = self
            .chat()
            .get_chat_history(from_message_id, 0, limit)
            .await;

        imp.is_loading_older.set(false);

        let messages = result.map_err(ChatHistoryError::Tdlib)?;

        // Depending on the TDLib version, `getChatHistory` can also return the
        // message with the id `from_message_id`, which is already in the list.
        let messages: Vec<model::Message> = messages
            .into_iter()
            .filter(|m| from_message_id == 0 || m.id() < from_message_id)
            .collect();

        if messages.is_empty() {
            return Ok(false);
        }

        self.append(messages);
        self.trim_front();
        Ok(true)
    }

    /// Loads a window of messages around the anchor of this chat history.
    pub(crate) async fn load_around_anchor(&self) -> Result<(), ChatHistoryError> {
        let imp = self.imp();

        if imp.is_loading_older.get() {
            return Err(ChatHistoryError::AlreadyLoading);
        }

        let anchor = imp.anchor.get();

        imp.is_loading_older.set(true);

        let result = self
            .chat()
            .get_chat_history(
                anchor,
                -AROUND_NEWER_MESSAGES,
                AROUND_NEWER_MESSAGES * 2,
            )
            .await;

        imp.is_loading_older.set(false);

        let messages = result.map_err(ChatHistoryError::Tdlib)?;

        // The chat's last message is the newest one, so if we loaded it, all
        // the messages newer than the anchor are loaded.
        let reached_newest = self.chat().last_message().map(|m| m.id())
            == messages.first().map(|m| m.id());

        if !messages.is_empty() {
            self.append(messages);
        }

        // Depending on the TDLib version, `getChatHistory` might not return the
        // message with the id `anchor` itself.
        if self.find_message_position(anchor).is_none() {
            let message = self
                .chat()
                .fetch_message(anchor)
                .await
                .map_err(ChatHistoryError::Tdlib)?;
            self.insert(message);
        }

        if reached_newest {
            imp.at_newest.set(true);
            self.flush_pending_new_messages();
        }

        Ok(())
    }

    /// Loads messages that are newer than the newest loaded message.
    ///
    /// Returns `true` when more messages can be loaded.
    pub(crate) async fn load_newer_messages(&self, limit: i32) -> Result<bool, ChatHistoryError> {
        let imp = self.imp();

        if imp.at_newest.get() {
            return Ok(false);
        }

        if imp.is_loading_newer.get() {
            return Err(ChatHistoryError::AlreadyLoading);
        }

        let newest_message_id = imp
            .list
            .borrow()
            .front()
            .and_then(|item| item.message())
            .map(|m| m.id())
            .unwrap_or(imp.anchor.get());

        let limit = limit.clamp(1, MAX_NEWER_MESSAGES);

        imp.is_loading_newer.set(true);

        let result = self
            .chat()
            .get_chat_history(newest_message_id, -limit, limit + 1)
            .await;

        imp.is_loading_newer.set(false);

        let messages = result.map_err(ChatHistoryError::Tdlib)?;

        let messages: Vec<model::Message> = messages
            .into_iter()
            .filter(|m| m.id() > newest_message_id)
            .collect();

        if messages.is_empty() {
            imp.at_newest.set(true);
            self.flush_pending_new_messages();
            return Ok(false);
        }

        let added = messages.len();

        {
            let mut list = imp.list.borrow_mut();
            // The messages are ordered from the newest to the oldest. Prepend
            // them from the oldest to the newest, so that the newest message
            // ends up at the front of the list.
            for message in messages.into_iter().rev() {
                list.push_front(model::ChatHistoryItem::for_message(message));
            }
        }

        self.items_changed(0, 0, added as u32);
        self.trim_back();
        Ok(true)
    }

    fn items_changed(&self, position: u32, removed: u32, added: u32) {
        let imp = self.imp();

        // Insert day dividers where needed
        let added = {
            let position = position as usize;
            let added = added as usize;

            let mut list = imp.list.borrow_mut();
            let mut previous_timestamp = if position + added < list.len() {
                list.get(position + added)
                    .and_then(|item| item.message_timestamp())
            } else {
                None
            };
            let mut dividers: Vec<(usize, model::ChatHistoryItem)> = vec![];

            for (index, current) in list.range(position..position + added).enumerate().rev() {
                if let Some(current_timestamp) = current.message_timestamp() {
                    if Some(current_timestamp.ymd()) != previous_timestamp.as_ref().map(|t| t.ymd())
                    {
                        let divider_pos = position + index + 1;
                        dividers.push((
                            divider_pos,
                            model::ChatHistoryItem::for_day_divider(current_timestamp.clone()),
                        ));
                        previous_timestamp = Some(current_timestamp);
                    }
                }
            }

            let dividers_len = dividers.len();
            for (position, item) in dividers {
                list.insert(position, item);
            }

            (added + dividers_len) as u32
        };

        // Check and remove no more needed day divider after removing messages
        let removed = {
            let mut removed = removed as usize;

            if removed > 0 {
                let mut list = imp.list.borrow_mut();
                let position = position as usize;
                let item_before_removed = list.get(position);

                if let Some(model::ChatHistoryItemType::DayDivider(_)) =
                    item_before_removed.map(|i| i.type_())
                {
                    let item_after_removed = if position > 0 {
                        list.get(position - 1)
                    } else {
                        None
                    };

                    match item_after_removed.map(|item| item.type_()) {
                        None | Some(model::ChatHistoryItemType::DayDivider(_)) => {
                            list.remove(position + removed);

                            removed += 1;
                        }
                        _ => {}
                    }
                }
            }

            removed as u32
        };

        // Check and remove no more needed day divider after adding messages
        let (position, removed) = {
            let mut removed = removed;
            let mut position = position as usize;

            if added > 0 && position > 0 {
                let mut list = imp.list.borrow_mut();
                let last_added_timestamp = list.get(position).unwrap().message_timestamp().unwrap();
                let next_item = list.get(position - 1);

                if let Some(model::ChatHistoryItemType::DayDivider(date)) =
                    next_item.map(|item| item.type_())
                {
                    if date.ymd() == last_added_timestamp.ymd() {
                        list.remove(position - 1);

                        removed += 1;
                        position -= 1;
                    }
                }
            }

            (position as u32, removed)
        };

        self.upcast_ref::<gio::ListModel>()
            .items_changed(position, removed, added);
    }

    fn push_front(&self, message: model::Message) {
        self.imp()
            .list
            .borrow_mut()
            .push_front(model::ChatHistoryItem::for_message(message));

        self.items_changed(0, 0, 1);
    }

    fn insert(&self, message: model::Message) {
        let imp = self.imp();

        let index = {
            let mut list = imp.list.borrow_mut();

            let index = message_ordering(&list, &message);
            list.insert(index, model::ChatHistoryItem::for_message(message));
            index
        };

        self.items_changed(index as u32, 0, 1);
    }

    fn append(&self, messages: Vec<model::Message>) {
        let imp = self.imp();
        let added = messages.len();

        imp.list.borrow_mut().reserve(added);

        for message in messages {
            imp.list
                .borrow_mut()
                .push_back(model::ChatHistoryItem::for_message(message));
        }

        let index = imp.list.borrow().len() - added;
        self.items_changed(index as u32, 0, added as u32);
    }

    /// Returns the number of items to trim, if the list grew beyond the trim
    /// threshold.
    fn trimmed_count(&self) -> Option<usize> {
        let len = self.imp().list.borrow().len();

        (len > TRIM_AT_ITEMS).then(|| len - TRIM_TO_ITEMS)
    }

    /// Trims the newest items of the list, if it grew beyond the trim threshold.
    ///
    /// This is called after older messages were loaded, so the viewport is at
    /// the oldest end of the list and the newest items are far away from it.
    /// The trimmed messages are unloaded and are loaded again when the user
    /// scrolls back to them.
    fn trim_front(&self) {
        let imp = self.imp();

        let Some(mut removed) = self.trimmed_count() else {
            return;
        };

        // The trimmed messages are not loaded anymore, so the messages newer
        // than the remaining ones must be loaded again.
        imp.at_newest.set(false);

        {
            let mut list = imp.list.borrow_mut();
            for _ in 0..removed {
                list.pop_front();
            }

            // A day divider at the front of the list always belongs to a day
            // whose messages are before it, so it was trimmed together with
            // them and must be removed as well.
            while matches!(
                list.front().map(|item| item.type_()),
                Some(model::ChatHistoryItemType::DayDivider(_))
            ) {
                list.pop_front();
                removed += 1;
            }
        }

        log::debug!(
            "Trimmed {} items from the front of the chat history",
            removed
        );

        self.upcast_ref::<gio::ListModel>()
            .items_changed(0, removed as u32, 0);
    }

    /// Trims the oldest items of the list, if it grew beyond the trim threshold.
    ///
    /// This is called after newer messages were loaded or when the view is
    /// pinned to the newest message, so the viewport is at the newest end of
    /// the list and the oldest items are far away from it. The trimmed messages
    /// are unloaded and are loaded again when the user scrolls back to them.
    pub(crate) fn trim_back(&self) {
        let imp = self.imp();

        let Some(removed) = self.trimmed_count() else {
            return;
        };

        {
            let mut list = imp.list.borrow_mut();
            for _ in 0..removed {
                list.pop_back();
            }

            // A day divider at the back of the list always belongs to a day
            // whose messages are right before it, so no day divider can be
            // orphaned by trimming the back of the list.
        }

        log::debug!("Trimmed {} items from the back of the chat history", removed);

        let position = imp.list.borrow().len() as u32;
        self.upcast_ref::<gio::ListModel>()
            .items_changed(position, removed as u32, 0);
    }

    fn remove(&self, message: model::Message) {
        let imp = self.imp();

        // Put this in a block, so that we only need to borrow the list once and the runtime
        // borrow checker does not panic in Self::items_changed when it borrows the list again.
        let index = {
            let mut list = imp.list.borrow_mut();

            // The elements in this list are ordered. While the day dividers are ordered
            // only by their date time, the messages are additionally sorted by their id. We
            // can exploit this by applying a binary search.
            let index = list
                .binary_search_by(|m| match m.type_() {
                    model::ChatHistoryItemType::Message(other_message) => {
                        message.id().cmp(&other_message.id())
                    }
                    model::ChatHistoryItemType::DayDivider(date_time) => {
                        let ordering = glib::DateTime::from_unix_utc(message.date() as i64)
                            .unwrap()
                            .cmp(date_time);
                        if let Ordering::Equal = ordering {
                            // We found the day divider of the message. Therefore, the message
                            // must be among the following elements.
                            Ordering::Greater
                        } else {
                            ordering
                        }
                    }
                })
                .ok();

            match index {
                Some(index) => {
                    list.remove(index);
                    index as u32
                }
                // The message is not in this list, e.g. because it was never
                // loaded. There is nothing to remove.
                None => return,
            }
        };

        self.items_changed(index, 1, 0);
    }

    fn handle_new_message(&self, message: model::Message) {
        if !self.imp().at_newest.get() {
            // There might be messages between the newest loaded message and
            // this one that are not loaded yet. Keep the message pending
            // instead of creating a hole in the list.
            self.imp()
                .pending_new_messages
                .borrow_mut()
                .push_back(message);
            return;
        }

        self.push_front(message);
    }

    fn handle_deleted_message(&self, message: model::Message) {
        self.imp()
            .pending_new_messages
            .borrow_mut()
            .retain(|m| m.id() != message.id());

        self.remove(message);
    }

    fn flush_pending_new_messages(&self) {
        let imp = self.imp();

        let pending: Vec<model::Message> = imp.pending_new_messages.borrow_mut().drain(..).collect();

        // The messages are ordered from the oldest to the newest. Prepend them
        // in order, so that the newest message ends up at the front of the list.
        for message in pending {
            self.push_front(message);
        }
    }

    /// Returns the position of the message with the specified id in this chat
    /// history, if it is loaded.
    pub(crate) fn find_message_position(&self, message_id: MessageId) -> Option<u32> {
        // Every message in the list is also alive in the message cache of the
        // chat, so we can use it to compare against the day dividers.
        let message = self.chat().message(message_id)?;

        let list = self.imp().list.borrow();

        list.binary_search_by(|item| match item.type_() {
            model::ChatHistoryItemType::Message(other_message) => {
                message_id.cmp(&other_message.id())
            }
            model::ChatHistoryItemType::DayDivider(date_time) => {
                let ordering = glib::DateTime::from_unix_utc(message.date() as i64)
                    .unwrap()
                    .cmp(date_time);
                if let Ordering::Equal = ordering {
                    // We found the day divider of the message. Therefore, the message
                    // must be among the following elements.
                    Ordering::Greater
                } else {
                    ordering
                }
            }
        })
        .ok()
        .map(|index| index as u32)
    }

    /// The message id from which this chat history loads the initial messages.
    pub(crate) fn anchor(&self) -> MessageId {
        self.imp().anchor.get()
    }

    /// Whether this chat history is anchored at the last read message of a
    /// chat with unread messages.
    pub(crate) fn is_unread_anchor(&self) -> bool {
        self.imp().is_unread_anchor.get()
    }

    /// Whether all the messages newer than the loaded ones are already in the
    /// list.
    pub(crate) fn at_newest(&self) -> bool {
        self.imp().at_newest.get()
    }

    pub(crate) fn chat(&self) -> model::Chat {
        self.imp().chat.upgrade().unwrap()
    }
}

/// Returns the position at which the message must be inserted in the list.
fn message_ordering(list: &VecDeque<model::ChatHistoryItem>, message: &model::Message) -> usize {
    list.partition_point(|item| match item.type_() {
        model::ChatHistoryItemType::Message(other_message) => other_message.id() > message.id(),
        model::ChatHistoryItemType::DayDivider(date_time) => {
            let message_timestamp = glib::DateTime::from_unix_utc(message.date() as i64)
                .and_then(|t| t.to_local())
                .unwrap();
            // The day dividers are followed by the messages of their day, so a
            // divider must only be placed before messages of older days.
            message_timestamp.ymd() < date_time.ymd()
        }
    })
}
