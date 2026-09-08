use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use futures::Future;
use gettextrs::gettext;
use glib::clone;
use gtk::gio;
use gtk::glib;
use gtk::CompositeTemplate;

use crate::expressions;
use crate::model;
use crate::ui;
use crate::utils;

/// The number of messages requested when the viewport is first filled with
/// messages. If it is not enough, smaller chunks are requested until it is.
const INITIAL_LOAD_LIMIT: i32 = 30;

/// The delay, in milliseconds, before the viewed messages are sent to TDLib.
/// Messages are accumulated in the meantime, so that scrolling doesn't issue
/// a TDLib request on every change of the visible messages.
const VIEW_MESSAGES_DELAY_MS: u64 = 150;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/chat_history.ui")]
    pub(crate) struct ChatHistory {
        pub(super) chat: glib::WeakRef<model::Chat>,
        pub(super) chat_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) model: RefCell<Option<model::ChatHistoryModel>>,
        pub(super) message_menu: OnceCell<gtk::PopoverMenu>,
        pub(super) is_auto_scrolling: Cell<bool>,
        pub(super) is_loading_messages: Cell<bool>,
        pub(super) sticky: Cell<bool>,
        pub(super) viewed_message_ids: RefCell<HashSet<i64>>,
        pub(super) viewed_message_ids_changed: Cell<bool>,
        pub(super) view_messages_scheduled: Cell<bool>,
        #[template_child]
        pub(super) window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub(super) background: TemplateChild<ui::Background>,
        #[template_child]
        pub(super) scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub(super) list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(super) chat_action_bar: TemplateChild<ui::ChatActionBar>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChatHistory {
        const NAME: &'static str = "PaplChatHistory";
        type Type = super::ChatHistory;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_action("chat-history.view-info", None, move |widget, _, _| {
                widget.open_info_dialog();
            });
            klass.install_action("chat-history.scroll-down", None, move |widget, _, _| {
                widget.handle_scroll_down();
            });
            klass.install_action(
                "chat-history.reply",
                Some(glib::VariantTy::INT64),
                move |widget, _, variant| {
                    let message_id = variant.and_then(|v| v.get()).unwrap();
                    widget.imp().chat_action_bar.reply_to_message_id(message_id);
                },
            );
            klass.install_action(
                "chat-history.edit",
                Some(glib::VariantTy::INT64),
                move |widget, _, variant| {
                    let message_id = variant.and_then(|v| v.get()).unwrap();
                    widget.imp().chat_action_bar.edit_message_id(message_id);
                },
            );
            klass.install_action(
                "chat-history.jump-to-message",
                Some(glib::VariantTy::INT64),
                move |widget, _, variant| {
                    let message_id = variant.and_then(|v| v.get()).unwrap();
                    widget.jump_to_message(message_id);
                },
            );
            klass.install_action_async(
                "chat-history.leave-chat",
                None,
                |widget, _, _| async move {
                    widget.show_leave_chat_dialog().await;
                },
            );
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ChatHistory {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecObject::builder::<model::Chat>("chat")
                        .explicit_notify()
                        .build(),
                    glib::ParamSpecBoolean::builder("sticky")
                        .read_only()
                        .build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();

            match pspec.name() {
                "chat" => {
                    let chat = value.get().unwrap();
                    obj.set_chat(chat);
                }
                "sticky" => obj.set_sticky(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();

            match pspec.name() {
                "chat" => obj.chat().to_value(),
                "sticky" => obj.sticky().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            obj.setup_expressions();

            let adj = self.list_view.vadjustment().unwrap();
            adj.connect_value_changed(clone!(
                #[weak]
                obj,
                move |adj| {
                    obj.schedule_view_messages();

                    let imp = obj.imp();

                    if imp.is_loading_messages.get() {
                        return;
                    }

                    if imp.is_auto_scrolling.get() {
                        if adj.value() + adj.page_size() >= adj.upper() {
                            imp.is_auto_scrolling.set(false);
                            obj.set_sticky(true);
                        }
                } else {
                    obj.set_sticky(adj.value() + adj.page_size() >= adj.upper());

                    // The list is reversed: a small value means that the oldest
                    // loaded message is visible, while a value close to the upper
                    // limit means that the newest loaded message is visible.
                    let near_oldest = adj.value() < adj.page_size() * 2.0
                        || adj.upper() <= adj.page_size() * 2.0;
                    let near_newest = adj.upper() > adj.page_size() * 2.0
                        && adj.value() + adj.page_size() * 2.0 >= adj.upper();

                    log::debug!(
                        "Scrolled (near_oldest = {near_oldest}, near_newest = {near_newest}, adj.value = {}, adj.upper = {}, adj.page_size = {})",
                        adj.value(),
                        adj.upper(),
                        adj.page_size(),
                    );

                    if !near_oldest && !near_newest {
                        return;
                    }

                    if let Some(model) = imp.model.borrow().as_ref() {
                        imp.is_loading_messages.set(true);

                        utils::spawn(clone!(
                            #[weak]
                            obj,
                            #[weak]
                            model,
                            async move {
                                let result = if near_oldest {
                                    log::debug!("Loading older messages on scroll (limit = 30)");
                                    model.load_older_messages(30).await
                                } else {
                                    log::debug!("Loading newer messages on scroll (limit = 50)");
                                    model.load_newer_messages(50).await
                                };

                                obj.imp().is_loading_messages.set(false);

                                if let Err(model::ChatHistoryError::AlreadyLoading) = result {
                                    log::debug!("Scroll-triggered load already in flight");
                                }
                                if let Err(model::ChatHistoryError::Tdlib(e)) = result {
                                    log::warn!("Couldn't load more chat messages: {:?}", e);
                                }
                            }
                        ));
                    }
                }
                }
            ));

            adj.connect_upper_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    if obj.sticky() || obj.imp().is_auto_scrolling.get() {
                        obj.scroll_down();
                    }
                }
            ));
        }

        fn dispose(&self) {
            if let Some(chat) = self.obj().chat() {
                perform_chat_action(&chat, tdlib::functions::close_chat);
            }
        }
    }

    impl WidgetImpl for ChatHistory {
        fn direction_changed(&self, previous_direction: gtk::TextDirection) {
            let obj = self.obj();

            if obj.direction() == previous_direction {
                return;
            }

            if let Some(menu) = self.message_menu.get() {
                menu.set_halign(if obj.direction() == gtk::TextDirection::Rtl {
                    gtk::Align::End
                } else {
                    gtk::Align::Start
                });
            }
        }
    }

    impl BinImpl for ChatHistory {}
}

glib::wrapper! {
    pub(crate) struct ChatHistory(ObjectSubclass<imp::ChatHistory>)
        @extends gtk::Widget, adw::Bin;
}

impl Default for ChatHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatHistory {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    fn setup_expressions(&self) {
        let chat_expression = Self::this_expression("chat");

        // Chat title
        expressions::chat_display_name(&chat_expression).bind(
            &*self.imp().window_title,
            "title",
            Some(self),
        );
    }

    fn open_info_dialog(&self) {
        if let Some(chat) = self.chat() {
            ui::ChatInfoWindow::new(&self.parent_window(), &chat).present();
        }
    }

    async fn show_leave_chat_dialog(&self) {
        if let Some(chat) = self.chat() {
            let dialog = adw::AlertDialog::new(
                Some(&gettext("Leave chat?")),
                Some(&gettext("Do you want to leave this chat?")),
            );
            dialog.add_responses(&[("no", &gettext("_No")), ("yes", &gettext("_Yes"))]);
            dialog.set_default_response(Some("no"));
            dialog.set_close_response("no");
            dialog.set_response_appearance("yes", adw::ResponseAppearance::Destructive);

            if dialog.choose_future(&self.parent_window().unwrap()).await == "yes" {
                match tdlib::functions::leave_chat(chat.id(), chat.session_().client_().id()).await
                {
                    Ok(_) => {
                        // Unselect recently left chat
                        utils::ancestor::<_, ui::Sidebar>(self)
                            .set_selected_chat(Option::<model::Chat>::None);
                    }
                    Err(e) => log::warn!("Failed to leave chat: {:?}", e),
                }
            }
        }
    }

    fn parent_window(&self) -> Option<gtk::Window> {
        self.root()?.downcast().ok()
    }

    fn request_sponsored_message(&self, chat: &model::Chat, list: &gio::ListStore) {
        utils::spawn(clone!(
            #[weak]
            chat,
            #[weak]
            list,
            async move {
                match model::SponsoredMessage::request(&chat).await {
                    Ok(sponsored_message) => {
                        if let Some(sponsored_message) = sponsored_message {
                            list.append(&sponsored_message);
                        }
                    }
                    Err(e) => {
                        if e.code != 404 {
                            log::warn!("Failed to request a SponsoredMessage: {:?}", e);
                        }
                    }
                }
            }
        ));
    }

    pub(crate) fn add_to_viewed_message_ids(&self, message_id: i64) {
        let imp = self.imp();
        if imp.viewed_message_ids.borrow_mut().insert(message_id) {
            imp.viewed_message_ids_changed.set(true);
        }
    }

    pub(crate) fn remove_from_viewed_message_ids(&self, message_id: i64) {
        let imp = self.imp();
        if imp.viewed_message_ids.borrow_mut().remove(&message_id) {
            imp.viewed_message_ids_changed.set(true);
        }
    }

    pub(crate) fn message_menu(&self) -> &gtk::PopoverMenu {
        self.imp().message_menu.get_or_init(|| {
            let menu = gtk::Builder::from_resource(
                "/app/drey/paper-plane/ui/session/content/message_menu.ui",
            )
            .object::<gtk::PopoverMenu>("menu")
            .unwrap();

            menu.set_halign(if self.direction() == gtk::TextDirection::Rtl {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });

            menu
        })
    }

    pub(crate) fn handle_paste_action(&self) {
        self.imp().chat_action_bar.handle_paste_action();
    }

    pub(crate) fn chat(&self) -> Option<model::Chat> {
        self.imp().chat.upgrade()
    }

    /// Opens or closes the chat in TDLib. While a chat is opened in TDLib,
    /// no notifications are created for it.
    pub(crate) fn set_chat_open(&self, open: bool) {
        if let Some(chat) = self.chat() {
            if open {
                perform_chat_action(&chat, tdlib::functions::open_chat);
            } else {
                perform_chat_action(&chat, tdlib::functions::close_chat);
            }
        }
    }

    pub(crate) fn set_chat(&self, chat: Option<&model::Chat>) {
        let old_chat = self.chat();
        if chat == old_chat.as_ref() {
            return;
        }

        let imp = self.imp();

        if let Some(chat) = old_chat {
            chat.disconnect(imp.chat_handler.replace(None).unwrap());
            perform_chat_action(chat.as_ref(), tdlib::functions::close_chat);
        }

        if let Some(chat) = chat {
            log::debug!(
                "Opening chat {} (unread_count = {}, anchor = {})",
                chat.id(),
                chat.unread_count(),
                if chat.unread_count() > 0 {
                    chat.last_read_inbox_message_id()
                } else {
                    0
                },
            );
            self.action_set_enabled(
                "chat-history.leave-chat",
                match chat.chat_type() {
                    model::ChatType::BasicGroup(data) => {
                        data.status().0 != tdlib::enums::ChatMemberStatus::Left
                    }
                    model::ChatType::Supergroup(data) => {
                        data.status().0 != tdlib::enums::ChatMemberStatus::Left
                    }
                    _ => false,
                },
            );

            let model = model::ChatHistoryModel::new(chat);

            let handler = chat.connect_new_message(clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, msg| {
                    if msg.is_outgoing() {
                        obj.imp().background.animate();
                    }
                }
            ));
            imp.chat_handler.replace(Some(handler));

            self.set_history_model(chat, &model);

            perform_chat_action(chat, tdlib::functions::open_chat);
        }

        imp.chat.set(chat);
        self.notify("chat");
    }

    /// Creates the model shown by the list view, which wraps the chat history
    /// together with the sponsored messages, if needed.
    fn create_list_model(
        &self,
        chat: &model::Chat,
        model: &model::ChatHistoryModel,
    ) -> gio::ListModel {
        if matches!(chat.chat_type(), model::ChatType::Supergroup(supergroup) if supergroup.is_channel())
        {
            let list = gio::ListStore::new::<gio::ListModel>();

            // We need to create a list here so that we can append the sponsored message
            // to the chat history in the GtkListView using a GtkFlattenListModel
            let sponsored_message_list = gio::ListStore::new::<model::SponsoredMessage>();
            list.append(&sponsored_message_list);
            self.request_sponsored_message(chat, &sponsored_message_list);

            list.append(model);

            gtk::FlattenListModel::new(Some(list)).upcast()
        } else {
            model.clone().upcast()
        }
    }

    /// The number of sponsored messages shown before the chat history in the
    /// list view.
    fn sponsored_message_count(&self) -> u32 {
        let imp = self.imp();

        let Some(selection) = imp.list_view.model() else {
            return 0;
        };
        let Some(no_selection) = selection.downcast_ref::<gtk::NoSelection>() else {
            return 0;
        };
        let Some(flatten_model) = no_selection.model().and_downcast::<gtk::FlattenListModel>()
        else {
            return 0;
        };

        let history_count = imp.model.borrow().as_ref().map_or(0, |m| m.n_items());

        flatten_model.n_items().saturating_sub(history_count)
    }

    /// Loads a window of messages around the anchor of the chat history and
    /// scrolls the list view to it.
    async fn load_around_anchor(&self, model: &model::ChatHistoryModel) {
        let imp = self.imp();

        let anchor = model.anchor();

        if let Err(e) = model.load_around_anchor().await {
            log::warn!("Couldn't load the messages around the anchor: {}", e);
            return;
        }

        let Some(mut position) = model.find_message_position(anchor) else {
            return;
        };

        // Show the unread messages below the anchor instead of the anchor itself
        if model.is_unread_anchor() {
            let unread_count = self
                .chat()
                .map(|chat| chat.unread_count())
                .unwrap_or_default();
            position = position.saturating_sub(unread_count as u32);
        }

        position += self.sponsored_message_count();

        imp.list_view
            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
    }

    /// Installs the chat history model in the list view and fills it
    /// asynchronously.
    ///
    /// If the model is anchored at the newest message, the history is filled
    /// from the newest end until the viewport can scroll, e.g. when opening
    /// a chat without unread messages or jumping to its newest message.
    /// Otherwise, a window of messages is loaded around the anchor and the
    /// viewport is positioned at it, e.g. when opening a chat with unread
    /// messages or jumping to a specific message.
    fn set_history_model(&self, chat: &model::Chat, model: &model::ChatHistoryModel) {
        let imp = self.imp();

        // Claim the loading flag synchronously, so that the scroll handler
        // can't start a concurrent load before the initial fill below had
        // a chance to run.
        imp.is_loading_messages.set(true);

        let list_view_model = self.create_list_model(chat, model);

        let selection = gtk::NoSelection::new(Some(list_view_model));
        imp.list_view.set_model(Some(&selection));

        imp.model.replace(Some(model.clone()));

        let widget = self.clone();
        let model_weak = model.downgrade();

        utils::spawn(clone!(
            #[strong(rename_to = obj)]
            widget,
            #[strong]
            model_weak,
            async move {
                let imp = obj.imp();

                log::debug!(
                    "Filling chat history (at_newest = {}, is_unread_anchor = {})",
                    model_weak.upgrade().map(|m| m.at_newest()).unwrap_or(false),
                    model_weak
                        .upgrade()
                        .map(|m| m.is_unread_anchor())
                        .unwrap_or(false),
                );

                let scrollbar = imp.scrolled_window.vscrollbar();
                scrollbar.set_visible(false);

                let adj = imp.list_view.vadjustment().unwrap();
                adj.set_value(0.0);

                if obj.chat().is_some() {
                    if let Some(model) = model_weak.upgrade() {
                        if model.at_newest() {
                            // Fill the viewport with messages, using
                            // progressively smaller chunks, so that the first
                            // paint happens after a single TDLib request
                            let mut limit = INITIAL_LOAD_LIMIT;

                            while adj.value() == 0.0 {
                                let adj_value = adj.value();
                                let adj_upper = adj.upper();
                                let adj_page_size = adj.page_size();
                                match model.load_older_messages(limit).await {
                                    Ok(true) => {
                                        log::debug!(
                                            "Loaded older messages (limit = {limit}, remaining = true, adj.value = {adj_value}, adj.upper = {adj_upper}, adj.page_size = {adj_page_size})"
                                        );
                                        limit = (limit / 2).max(2);
                                    }
                                    Ok(false) => {
                                        log::debug!(
                                            "No more older messages (limit = {limit}, adj.value = {adj_value}, adj.upper = {adj_upper}, adj.page_size = {adj_page_size})"
                                        );
                                        break;
                                    }
                                    Err(model::ChatHistoryError::AlreadyLoading) => {
                                        // Another load is in flight. Try again
                                        // after giving it a chance to finish.
                                        log::debug!(
                                            "load_older_messages already in flight (limit = {limit}), retrying"
                                        );
                                        glib::timeout_future(std::time::Duration::from_millis(20))
                                            .await;
                                    }
                                    Err(model::ChatHistoryError::Tdlib(e)) => {
                                        log::warn!("Couldn't load initial history messages: {e:?}");
                                        break;
                                    }
                                }
                            }

                            log::debug!(
                                "Setting sticky (adj.value = {}, adj.upper = {}, adj.page_size = {}, n_items = {})",
                                adj.value(),
                                adj.upper(),
                                adj.page_size(),
                                model.n_items(),
                            );
                            obj.set_sticky(true);
                        } else {
                            log::debug!("Loading messages around the anchor");
                            obj.load_around_anchor(&model).await;

                            log::debug!("Anchor window loaded (n_items = {})", model.n_items(),);

                            obj.set_sticky(false);
                        }
                    }
                }

                log::debug!(
                    "Chat history fill done (n_items = {}), showing scrollbar",
                    model_weak.upgrade().map(|m| m.n_items()).unwrap_or(0),
                );
                scrollbar.set_visible(true);

                imp.is_loading_messages.set(false);

                if obj.chat().is_some() {
                    obj.view_messages();
                }
            }
        ));
    }

    /// Shows the message with the specified id in the chat history, loading
    /// the messages around it instead of the whole history in between.
    fn jump_to_message(&self, message_id: i64) {
        let Some(chat) = self.chat() else {
            return;
        };

        let model = model::ChatHistoryModel::new_for_message(&chat, message_id);

        self.set_history_model(&chat, &model);
    }

    pub(crate) fn sticky(&self) -> bool {
        self.imp().sticky.get()
    }

    fn set_sticky(&self, sticky: bool) {
        if self.sticky() == sticky {
            return;
        }

        self.imp().sticky.set(sticky);
        self.notify("sticky");
    }

    fn scroll_down(&self) {
        let imp = self.imp();

        imp.is_auto_scrolling.set(true);

        imp.scrolled_window
            .emit_by_name::<bool>("scroll-child", &[&gtk::ScrollType::End, &false]);
    }

    /// Handles the scroll-down button press.
    ///
    /// If the chat has unread messages that are not in the loaded history
    /// window, jumps to the first unread message. If the newest messages are
    /// not loaded anymore, e.g. after jumping to an old message, re-opens
    /// the history at the newest message. Otherwise, plain scrolling to the
    /// bottom is used.
    fn handle_scroll_down(&self) {
        let Some(chat) = self.chat() else {
            self.scroll_down();
            return;
        };

        let Some(model) = self.imp().model.borrow().as_ref().cloned() else {
            self.scroll_down();
            return;
        };

        // The history is anchored at the last read message when there are
        // unread messages. A chat that was never read has no such anchor and
        // jumping to the newest message is used instead.
        if chat.unread_count() > 0 && chat.last_read_inbox_message_id() != 0 && !model.at_newest() {
            self.jump_to_first_unread();
        } else if !model.at_newest() {
            self.go_to_newest();
        } else {
            self.scroll_down();
        }
    }

    /// Re-anchors the history at the first unread message and positions the
    /// viewport at it. This is similar to opening the chat with unread
    /// messages, so that the unread block is shown instead of reloading
    /// newer messages one chunk at a time.
    fn jump_to_first_unread(&self) {
        let Some(chat) = self.chat() else {
            return;
        };

        let model = model::ChatHistoryModel::new(&chat);

        self.set_history_model(&chat, &model);
    }

    /// Re-opens the history at the newest message of the chat, discarding
    /// the currently loaded window. Used when the newest messages are not
    /// loaded anymore, e.g. after jumping to an old message.
    fn go_to_newest(&self) {
        let Some(chat) = self.chat() else {
            return;
        };

        let model = model::ChatHistoryModel::new(&chat);

        self.set_history_model(&chat, &model);
    }

    /// Schedules a flush of the viewed messages with a short delay, so that
    /// at most one `viewMessages` request is sent per delay interval.
    pub(crate) fn schedule_view_messages(&self) {
        let imp = self.imp();

        if imp.view_messages_scheduled.get() {
            return;
        }
        imp.view_messages_scheduled.set(true);

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                glib::timeout_future(std::time::Duration::from_millis(VIEW_MESSAGES_DELAY_MS))
                    .await;

                obj.imp().view_messages_scheduled.set(false);
                obj.view_messages();
            }
        ));
    }

    pub(crate) fn view_messages(&self) {
        let imp = self.imp();

        if imp.viewed_message_ids_changed.get() {
            imp.viewed_message_ids_changed.set(false);

            let chat = self.chat().unwrap();
            let chat_id = chat.id();
            let client_id = chat.session_().client_().id();
            let viewed_message_ids =
                Vec::from_iter(imp.viewed_message_ids.borrow().iter().copied());

            log::debug!(
                "Reporting {count} viewed messages to TDLib (chat_id = {chat_id})",
                count = viewed_message_ids.len(),
            );

            utils::spawn(async move {
                tdlib::functions::view_messages(chat_id, viewed_message_ids, None, true, client_id)
                    .await
                    .unwrap();
            });
        }
    }
}

fn perform_chat_action<F, Fut>(chat: &model::Chat, op: F)
where
    F: Fn(i64, i32) -> Fut + 'static,
    Fut: Future<Output = Result<(), tdlib::types::Error>>,
{
    utils::spawn(clone!(
        #[weak]
        chat,
        async move {
            op(chat.id(), chat.session_().client_().id()).await.unwrap();
        }
    ));
}
