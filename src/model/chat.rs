use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use glib::subclass::Signal;
use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::types::ChatId;
use crate::types::MessageId;

/// A page of the photo and video messages of a chat, as returned by
/// `Chat::search_media_messages`.
pub(crate) struct MediaSearchPage {
    /// The media messages, in reverse chronological order.
    pub(crate) messages: Vec<model::Message>,
    /// The id from which the next page must be requested, or `0` if there are
    /// no more messages.
    pub(crate) next_from_message_id: MessageId,
}

#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "ChatType")]
pub(crate) enum ChatType {
    Private(model::User),
    BasicGroup(model::BasicGroup),
    Supergroup(model::Supergroup),
    Secret(model::SecretChat),
}

impl ChatType {
    pub(crate) fn from_td_object(
        _type: &tdlib::enums::ChatType,
        session: &model::ClientStateSession,
    ) -> Self {
        use tdlib::enums::ChatType::*;

        match _type {
            Private(data) => {
                let user = session.user(data.user_id);
                Self::Private(user)
            }
            BasicGroup(data) => {
                let basic_group = session.basic_group(data.basic_group_id);
                Self::BasicGroup(basic_group)
            }
            Supergroup(data) => {
                let supergroup = session.supergroup(data.supergroup_id);
                Self::Supergroup(supergroup)
            }
            Secret(data) => {
                let secret_chat = session.secret_chat(data.secret_chat_id);
                Self::Secret(secret_chat)
            }
        }
    }

    pub(crate) fn user(&self) -> Option<model::User> {
        Some(match self {
            ChatType::Private(user) => user.to_owned(),
            ChatType::Secret(secret_chat) => secret_chat.user_(),
            _ => return None,
        })
    }

    pub(crate) fn basic_group(&self) -> Option<&model::BasicGroup> {
        Some(match self {
            ChatType::BasicGroup(basic_group) => basic_group,
            _ => return None,
        })
    }

    pub(crate) fn supergroup(&self) -> Option<&model::Supergroup> {
        Some(match self {
            ChatType::Supergroup(supergroup) => supergroup,
            _ => return None,
        })
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Properties, Default)]
    #[properties(wrapper_type = super::Chat)]
    pub(crate) struct Chat {
        pub(super) messages: RefCell<HashMap<MessageId, glib::WeakRef<model::Message>>>,
        /// The chat history model that currently displays this chat, if any.
        pub(super) history: glib::WeakRef<model::ChatHistoryModel>,
        /// Cache of the messages that are replied to by the messages of this
        /// chat, keyed by `(chat_id, message_id)`. A value of `None` means
        /// that the replied message doesn't exist (e.g. it was deleted).
        ///
        /// This exists so that reply previews don't issue a TDLib request
        /// every time they are shown.
        pub(super) replied_messages: RefCell<HashMap<(ChatId, MessageId), Option<model::Message>>>,
        #[property(get, set, construct_only)]
        pub(super) session: glib::WeakRef<model::ClientStateSession>,
        #[property(get, set, construct_only)]
        pub(super) id: Cell<ChatId>,
        #[property(get, set, construct_only)]
        pub(super) chat_type: OnceCell<ChatType>,
        #[property(get)]
        pub(super) block_list: RefCell<Option<model::BoxedBlockList>>,
        #[property(get)]
        pub(super) title: RefCell<String>,
        #[property(get)]
        pub(super) avatar: RefCell<Option<model::Avatar>>,
        #[property(get)]
        pub(super) last_read_inbox_message_id: Cell<MessageId>,
        #[property(get)]
        pub(super) last_read_outbox_message_id: Cell<MessageId>,
        #[property(get)]
        pub(super) is_marked_as_unread: Cell<bool>,
        #[property(get)]
        pub(super) last_message: RefCell<Option<model::Message>>,
        #[property(get)]
        pub(super) unread_mention_count: Cell<i32>,
        #[property(get)]
        pub(super) unread_count: Cell<i32>,
        #[property(get)]
        pub(super) draft_message: RefCell<Option<model::BoxedDraftMessage>>,
        #[property(get)]
        pub(super) notification_settings: RefCell<model::BoxedChatNotificationSettings>,
        #[property(get = Self::actions)]
        pub(super) actions: OnceCell<model::ChatActionList>,
        #[property(get)]
        pub(super) permissions: RefCell<model::BoxedChatPermissions>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Chat {
        const NAME: &'static str = "Chat";
        type Type = super::Chat;
    }

    impl ObjectImpl for Chat {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("new-message")
                        .param_types([model::Message::static_type()])
                        .build(),
                    Signal::builder("deleted-message")
                        .param_types([model::Message::static_type()])
                        .build(),
                ]
            })
        }

        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec)
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }
    }

    impl Chat {
        pub(crate) fn actions(&self) -> model::ChatActionList {
            self.actions
                .get_or_init(|| model::ChatActionList::from(&*self.obj()))
                .to_owned()
        }
    }
}

glib::wrapper! {
    pub(crate) struct Chat(ObjectSubclass<imp::Chat>);
}

impl Chat {
    pub(crate) fn new(session: &model::ClientStateSession, td_chat: tdlib::types::Chat) -> Self {
        let obj: Self = glib::Object::builder()
            .property("session", session)
            .property("id", td_chat.id)
            .property(
                "chat-type",
                ChatType::from_td_object(&td_chat.r#type, session),
            )
            .build();

        let imp = obj.imp();

        imp.block_list
            .replace(td_chat.block_list.map(model::BoxedBlockList));
        imp.title.replace(td_chat.title);
        imp.avatar.replace(td_chat.photo.map(model::Avatar::from));
        imp.last_read_inbox_message_id
            .set(td_chat.last_read_inbox_message_id);
        imp.last_read_outbox_message_id
            .set(td_chat.last_read_outbox_message_id);
        imp.is_marked_as_unread.set(td_chat.is_marked_as_unread);
        imp.last_message
            .replace(td_chat.last_message.map(|message| obj.insert_message(message)));
        imp.unread_mention_count.set(td_chat.unread_mention_count);
        imp.unread_count.set(td_chat.unread_count);
        imp.draft_message
            .replace(td_chat.draft_message.map(model::BoxedDraftMessage));
        imp.notification_settings
            .replace(model::BoxedChatNotificationSettings(
                td_chat.notification_settings,
            ));
        imp.permissions
            .replace(model::BoxedChatPermissions(td_chat.permissions));

        obj
    }

    pub(crate) fn handle_update(&self, update: tdlib::enums::Update) {
        use tdlib::enums::Update::*;

        let imp = self.imp();

        match update {
            ChatAction(update) => {
                self.actions().handle_update(update);
                // TODO: Remove this at some point. Widgets should use the `items-changed` signal
                // for updating their state in the future.
                self.notify_actions();
            }
            ChatDraftMessage(update) => {
                self.set_draft_message(update.draft_message.map(model::BoxedDraftMessage));
            }
            ChatBlockList(update) => {
                self.set_block_list(update.block_list.map(model::BoxedBlockList))
            }
            ChatIsMarkedAsUnread(update) => self.set_marked_as_unread(update.is_marked_as_unread),
            ChatLastMessage(update) => {
                self.set_last_message(update.last_message.map(|m| self.insert_message(m)));
            }
            ChatNotificationSettings(update) => {
                self.set_notification_settings(model::BoxedChatNotificationSettings(
                    update.notification_settings,
                ));
            }
            ChatPermissions(update) => {
                self.set_permissions(model::BoxedChatPermissions(update.permissions))
            }
            ChatPhoto(update) => self.set_avatar(update.photo.map(Into::into)),
            ChatReadInbox(update) => {
                self.set_last_read_inbox_message_id(update.last_read_inbox_message_id);
                self.set_unread_count(update.unread_count);
            }
            ChatReadOutbox(update) => {
                self.set_last_read_outbox_message_id(update.last_read_outbox_message_id);
            }
            ChatTitle(update) => self.set_title(update.title),
            ChatUnreadMentionCount(update) => {
                self.set_unread_mention_count(update.unread_mention_count)
            }
            DeleteMessages(data) => {
                let mut messages = imp.messages.borrow_mut();

                if data.from_cache {
                    // The messages have been removed from TDLib's local cache. They are still
                    // available on the server, so this doesn't affect the UI. We only drop our
                    // references to them to stay in sync with TDLib's cache.
                    for id in &data.message_ids {
                        messages.remove(id);
                    }
                } else {
                    let deleted_messages: Vec<model::Message> = data
                        .message_ids
                        .iter()
                        .filter_map(|id| messages.remove(id).and_then(|m| m.upgrade()))
                        .collect();

                    drop(messages);
                    for message in deleted_messages {
                        self.emit_by_name::<()>("deleted-message", &[&message]);
                    }
                }
            }
            MessageContent(ref data) => {
                if let Some(message) = self.message(data.message_id) {
                    message.handle_update(update);
                }
            }
            MessageEdited(ref data) => {
                if let Some(message) = self.message(data.message_id) {
                    message.handle_update(update);
                }
            }
            MessageInteractionInfo(ref data) => {
                if let Some(message) = self.message(data.message_id) {
                    message.handle_update(update);
                }
            }
            MessageSendSucceeded(data) => {
                let old_message = self.message(data.old_message_id);
                imp.messages.borrow_mut().remove(&data.old_message_id);

                let message = self.insert_message(data.message);

                if let Some(old_message) = old_message {
                    self.emit_by_name::<()>("deleted-message", &[&old_message]);
                }
                self.emit_by_name::<()>("new-message", &[&message]);
            }
            NewMessage(data) => {
                let message = self.insert_message(data.message);

                self.emit_by_name::<()>("new-message", &[&message]);
            }
            MessageMentionRead(update) => {
                self.set_unread_mention_count(update.unread_mention_count)
            }
            _ => {}
        }
    }

    pub(crate) fn session_(&self) -> model::ClientStateSession {
        self.session().unwrap()
    }

    /// The chat history model that currently displays this chat, if any.
    ///
    /// The chat only holds a weak reference to the model, so it stays alive
    /// only while its widget keeps a strong reference to it.
    pub(crate) fn history(&self) -> Option<model::ChatHistoryModel> {
        self.imp().history.upgrade()
    }

    /// Sets the chat history model that displays this chat.
    pub(crate) fn set_history(&self, history: &model::ChatHistoryModel) {
        self.imp().history.set(Some(history));
    }

    pub(crate) fn is_blocked(&self) -> bool {
        matches!(
            self.block_list(),
            Some(model::BoxedBlockList(tdlib::enums::BlockList::Main))
        )
    }

    fn set_block_list(&self, block_list: Option<model::BoxedBlockList>) {
        if self.block_list() == block_list {
            return;
        }
        self.imp().block_list.replace(block_list);
        self.notify_block_list();
    }

    fn set_title(&self, title: String) {
        if self.title() == title {
            return;
        }
        self.imp().title.replace(title);
        self.notify_title();
    }

    fn set_avatar(&self, avatar: Option<model::Avatar>) {
        if self.avatar() == avatar {
            return;
        }
        self.imp().avatar.replace(avatar);
        self.notify_avatar();
    }

    fn set_last_read_inbox_message_id(&self, id: MessageId) {
        if self.last_read_inbox_message_id() == id {
            return;
        }
        self.imp().last_read_inbox_message_id.set(id);
        self.notify_last_read_inbox_message_id();
    }

    fn set_last_read_outbox_message_id(&self, id: MessageId) {
        if self.last_read_outbox_message_id() == id {
            return;
        }
        self.imp().last_read_outbox_message_id.set(id);
        self.notify_last_read_outbox_message_id();
    }

    fn set_marked_as_unread(&self, is_marked_as_unread: bool) {
        if self.is_marked_as_unread() == is_marked_as_unread {
            return;
        }
        self.imp().is_marked_as_unread.set(is_marked_as_unread);
        self.notify_is_marked_as_unread();
    }

    fn set_last_message(&self, last_message: Option<model::Message>) {
        if self.last_message() == last_message {
            return;
        }
        self.imp().last_message.replace(last_message);
        self.notify_last_message();
    }

    fn set_unread_mention_count(&self, unread_mention_count: i32) {
        if self.unread_mention_count() == unread_mention_count {
            return;
        }
        self.imp().unread_mention_count.set(unread_mention_count);
        self.notify_unread_mention_count();
    }

    fn set_unread_count(&self, unread_count: i32) {
        if self.unread_count() == unread_count {
            return;
        }
        self.imp().unread_count.set(unread_count);
        self.notify_unread_count()
    }

    fn set_draft_message(&self, draft_message: Option<model::BoxedDraftMessage>) {
        if self.draft_message() == draft_message {
            return;
        }
        self.imp().draft_message.replace(draft_message);
        self.notify_draft_message();
    }

    fn set_notification_settings(
        &self,
        notification_settings: model::BoxedChatNotificationSettings,
    ) {
        if self.notification_settings() == notification_settings {
            return;
        }
        self.imp()
            .notification_settings
            .replace(notification_settings);
        self.notify_notification_settings();
    }

    pub(crate) fn is_own_chat(&self) -> bool {
        self.chat_type().user() == Some(self.session_().me_())
    }

    fn set_permissions(&self, permissions: model::BoxedChatPermissions) {
        if self.permissions() == permissions {
            return;
        }
        self.imp().permissions.replace(permissions);
        self.notify_permissions();
    }

    pub(crate) fn connect_new_message<F: Fn(&Self, model::Message) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local("new-message", true, move |values| {
            let obj = values[0].get().unwrap();
            let message = values[1].get().unwrap();
            f(obj, message);
            None
        })
    }

    pub(crate) fn connect_deleted_message<F: Fn(&Self, model::Message) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local("deleted-message", true, move |values| {
            let obj = values[0].get().unwrap();
            let message = values[1].get().unwrap();
            f(obj, message);
            None
        })
    }

    /// Returns the `Message` of the specified id, if present in the cache.
    pub(crate) fn message(&self, id: MessageId) -> Option<model::Message> {
        self.imp()
            .messages
            .borrow()
            .get(&id)
            .and_then(|message| message.upgrade())
    }

    /// Inserts a TDLib message into the cache and returns the corresponding `Message`.
    ///
    /// The cache only holds weak references to its messages, so a `Message` stays alive
    /// only while somebody else keeps a strong reference to it (e.g. a `ChatHistoryModel`
    /// or the `last-message` property). If a message with the same id is still alive, it
    /// is returned instead of creating a new one.
    fn insert_message(&self, td_message: tdlib::types::Message) -> model::Message {
        let message_id = td_message.id;
        let mut messages = self.imp().messages.borrow_mut();

        // Drop the weak references to the messages that are not alive anymore, so that
        // the cache doesn't grow unboundedly.
        messages.retain(|_, message| message.upgrade().is_some());

        if let Some(message) = messages.get(&message_id).and_then(|m| m.upgrade()) {
            return message;
        }

        let message = model::Message::new(self, td_message);

        let weak_message = glib::WeakRef::new();
        weak_message.set(Some(&message));
        messages.insert(message_id, weak_message);

        message
    }

    /// Returns the `Message` of the specified id, if present in the cache. Otherwise it
    /// fetches it from the server and then it returns the result.
    pub(crate) async fn fetch_message(
        &self,
        id: MessageId,
    ) -> Result<model::Message, tdlib::types::Error> {
        if let Some(message) = self.message(id) {
            return Ok(message);
        }

        let client_id = self.session_().client_().id();
        let result = tdlib::functions::get_message(self.id(), id, client_id).await;

        result.map(|r| {
            let tdlib::enums::Message::Message(message) = r;

            self.insert_message(message)
        })
    }

    /// Returns the message with the specified id of the chat with the specified id,
    /// or `None` if the message doesn't exist (e.g. it was deleted).
    ///
    /// The result, including negative ones, is cached, so that reply previews
    /// don't issue a TDLib request every time they are shown. Note that the
    /// replied message can belong to another chat, e.g. for replies to messages
    /// in channels' comment sections.
    pub(crate) async fn fetch_reply_target(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
    ) -> Result<Option<model::Message>, tdlib::types::Error> {
        const MAX_CACHED_REPLIED_MESSAGES: usize = 100;

        if let Some(cached) = self
            .imp()
            .replied_messages
            .borrow()
            .get(&(chat_id, message_id))
        {
            return Ok(cached.clone());
        }

        let result = if chat_id == 0 || chat_id == self.id() {
            self.fetch_message(message_id).await.map(Some)
        } else if let Some(chat) = self.session_().try_chat(chat_id) {
            chat.fetch_message(message_id).await.map(Some)
        } else {
            // The chat doesn't exist anymore, so the message doesn't either
            Ok(None)
        };

        let mut replied_messages = self.imp().replied_messages.borrow_mut();

        if replied_messages.len() >= MAX_CACHED_REPLIED_MESSAGES {
            replied_messages.clear();
        }

        match result {
            Ok(message) => {
                replied_messages.insert((chat_id, message_id), message.clone());
                Ok(message)
            }
            // A "MessageIdInvalid" error means that the message doesn't exist
            // anymore (e.g. it was deleted), so the negative result can be cached
            Err(err) if err.code == 400 => {
                replied_messages.insert((chat_id, message_id), None);
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// Returns the messages of the chat, in reverse chronological order, starting from
    /// the message with the id `from_id`, or from the last message if `from_id` is 0.
    ///
    /// If `offset` is negative, `-offset` messages newer than `from_id` are included in
    /// the result. See `getChatHistory` in the TDLib documentation for more details.
    pub(crate) async fn get_chat_history(
        &self,
        from_id: MessageId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<model::Message>, tdlib::types::Error> {
        let client_id = self.session_().client_().id();
        let result =
            tdlib::functions::get_chat_history(self.id(), from_id, offset, limit, false, client_id)
                .await;

        let tdlib::enums::Messages::Messages(data) = result?;

        Ok(data
            .messages
            .into_iter()
            .flatten()
            .map(|m| self.insert_message(m))
            .collect())
    }

    /// Searches for the photo and video messages of this chat, in reverse
    /// chronological order, starting from the message with the id `from_id`
    /// (inclusive), or from the last message if `from_id` is 0.
    ///
    /// See `searchChatMessages` in the TDLib documentation for more details.
    pub(crate) async fn search_media_messages(
        &self,
        from_id: MessageId,
        limit: i32,
    ) -> Result<MediaSearchPage, tdlib::types::Error> {
        let client_id = self.session_().client_().id();
        let result = tdlib::functions::search_chat_messages(
            self.id(),
            None,
            String::new(),
            None,
            from_id,
            0,
            limit,
            Some(tdlib::enums::SearchMessagesFilter::PhotoAndVideo),
            client_id,
        )
        .await;

        let tdlib::enums::FoundChatMessages::FoundChatMessages(data) = result?;

        Ok(MediaSearchPage {
            next_from_message_id: data.next_from_message_id,
            messages: data
                .messages
                .into_iter()
                .map(|m| self.insert_message(m))
                .collect(),
        })
    }

    pub(crate) async fn mark_as_read(&self) -> Result<(), tdlib::types::Error> {
        if let Some(message) = self.last_message() {
            tdlib::functions::view_messages(
                self.id(),
                vec![message.id()],
                None,
                true,
                self.session_().client_().id(),
            )
            .await?;
        }

        tdlib::functions::toggle_chat_is_marked_as_unread(
            self.id(),
            false,
            self.session_().client_().id(),
        )
        .await
    }

    pub(crate) async fn mark_as_unread(&self) -> Result<(), tdlib::types::Error> {
        tdlib::functions::toggle_chat_is_marked_as_unread(
            self.id(),
            true,
            self.session_().client_().id(),
        )
        .await
    }
}
