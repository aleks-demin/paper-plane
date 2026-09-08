use std::cell::Cell;
use std::cell::RefCell;

use gettextrs::gettext;
use glib::clone;
use glib::ParamSpec;
use glib::Properties;
use glib::Value;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use crate::model;
use crate::strings;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::MessageReply)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/reply.ui")]
    pub(crate) struct MessageReply {
        pub(super) sender_color_class: RefCell<Option<String>>,
        pub(super) bindings: RefCell<Vec<gtk::ExpressionWatch>>,
        pub(super) reply_to_message_id: Cell<i64>,
        pub(super) reply_to_chat_id: Cell<i64>,
        /// Whether the replied message is being fetched.
        pub(super) is_loading: Cell<bool>,
        /// Whether the replied message has been fetched (or is known to not
        /// exist anymore).
        pub(super) is_resolved: Cell<bool>,

        #[property(get, set, construct_only)]
        pub(super) message: glib::WeakRef<model::Message>,

        #[template_child]
        pub(super) separator: TemplateChild<gtk::Separator>,
        #[template_child]
        pub(super) labels_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) sender_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) message_label: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageReply {
        const NAME: &'static str = "PaplMessageReply";
        type Type = super::MessageReply;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_css_name("messagereply");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageReply {
        fn properties() -> &'static [ParamSpec] {
            Self::derived_properties()
        }

        fn set_property(&self, id: usize, value: &Value, pspec: &ParamSpec) {
            self.derived_set_property(id, value, pspec)
        }

        fn property(&self, id: usize, pspec: &ParamSpec) -> Value {
            self.derived_property(id, pspec)
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            self.message_label.set_label(&gettext("Loading…"));

            let gesture = gtk::GestureClick::new();
            gesture.connect_released(clone!(
                #[weak]
                obj,
                move |_, n_press, _, _| {
                    if n_press == 1 {
                        obj.jump_to_replied_message();
                    }
                }
            ));
            obj.add_controller(gesture);
        }

        fn dispose(&self) {
            utils::unparent_children(&*self.obj());
        }
    }

    impl WidgetImpl for MessageReply {
        fn map(&self) {
            self.parent_map();

            // Fetch the replied message only when the widget is actually shown.
            // The result is cached by the chat, so the fetch is done at most
            // once per replied message.
            if !self.is_loading.get() && !self.is_resolved.get() {
                self.is_loading.set(true);

                let obj = self.obj();

                utils::spawn(clone!(
                    #[weak]
                    obj,
                    async move {
                        obj.load_replied_message().await;
                        obj.imp().is_loading.set(false);
                    }
                ));
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct MessageReply(ObjectSubclass<imp::MessageReply>)
        @extends gtk::Widget;
}

impl MessageReply {
    pub(crate) fn new(message: &model::Message) -> Self {
        glib::Object::builder().property("message", message).build()
    }

    async fn load_replied_message(&self) {
        let imp = self.imp();

        let message = self.message().unwrap();
        let is_outgoing = message.is_outgoing();

        match message.reply_to().unwrap().0 {
            tdlib::enums::MessageReplyTo::Message(reply_to) => {
                imp.reply_to_message_id.set(reply_to.message_id);
                imp.reply_to_chat_id.set(reply_to.chat_id);

                match message
                    .chat_()
                    .fetch_reply_target(reply_to.chat_id, reply_to.message_id)
                    .await
                {
                    Ok(Some(replied_message)) => {
                        self.update_from_message(&replied_message, is_outgoing);
                        imp.is_resolved.set(true);
                    }
                    Ok(None) => {
                        imp.message_label.set_label(&gettext("Deleted message"));
                        imp.is_resolved.set(true);
                    }
                    // A transient error (e.g. no network connection). The fetch
                    // is retried when the widget is mapped again.
                    Err(e) => log::warn!("Error fetching replied message: {e:?}"),
                }
            }
            tdlib::enums::MessageReplyTo::Story(_) => {
                // TODO: Implement story replies
                unimplemented!()
            }
        };
    }

    pub(crate) fn set_max_char_width(&self, n_chars: i32) {
        self.imp().message_label.set_max_width_chars(n_chars);
        self.imp().sender_label.set_max_width_chars(n_chars);
    }

    /// Asks the chat history to show the replied message.
    fn jump_to_replied_message(&self) {
        let imp = self.imp();

        let message_id = imp.reply_to_message_id.get();
        if message_id == 0 {
            return;
        }

        let Some(message) = self.message() else {
            return;
        };

        let chat = message.chat_();
        let reply_to_chat_id = imp.reply_to_chat_id.get();

        // Jumping is only supported for replies within the same chat
        if reply_to_chat_id != 0 && reply_to_chat_id != chat.id() {
            return;
        }

        self.activate_action(
            "chat-history.jump-to-message",
            Some(&message_id.to_variant()),
        )
        .unwrap();
    }

    fn update_from_message(&self, replied_message: &model::Message, is_outgoing: bool) {
        let imp = self.imp();
        let mut bindings = imp.bindings.borrow_mut();
        while let Some(binding) = bindings.pop() {
            binding.unwatch();
        }

        // Remove the previous color css class
        let mut sender_color_class = imp.sender_color_class.borrow_mut();
        if let Some(class) = sender_color_class.as_ref() {
            self.remove_css_class(class);
        }
        // Show sender label, if needed
        let show_sender = !matches!(
            replied_message.chat_().chat_type(),
            model::ChatType::Supergroup(data) if data.is_channel()
        );
        if show_sender {
            let sender_name_expression = replied_message.sender_name_expression();
            let sender_binding =
                sender_name_expression.bind(&*imp.sender_label, "label", glib::Object::NONE);

            bindings.push(sender_binding);

            if !is_outgoing {
                // Color sender label
                if let model::MessageSender::User(user) = replied_message.sender() {
                    let classes = &[
                        "sender-text-red",
                        "sender-text-orange",
                        "sender-text-violet",
                        "sender-text-green",
                        "sender-text-cyan",
                        "sender-text-blue",
                        "sender-text-pink",
                    ];

                    let color_class = classes[user.id() as usize % classes.len()];
                    self.add_css_class(color_class);

                    *sender_color_class = Some(color_class.into());
                }
            }
            imp.sender_label.set_visible(true);
        }

        // Set content label expression

        let caption = strings::message_content(replied_message.clone().as_ref());
        imp.message_label.set_label(&caption);
    }
}
