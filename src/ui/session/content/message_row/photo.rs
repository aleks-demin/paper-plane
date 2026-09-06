use std::cell::RefCell;
use std::sync::OnceLock;

use glib::clone;
use glib::closure;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use crate::model;
use crate::ui;
use crate::ui::MessageBaseExt;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/photo.ui")]
    pub(crate) struct MessagePhoto {
        pub(super) binding: RefCell<Option<gtk::ExpressionWatch>>,
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) photo_tile: TemplateChild<ui::MediaPhotoTile>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessagePhoto {
        const NAME: &'static str = "PaplMessagePhoto";
        type Type = super::MessagePhoto;
        type ParentType = ui::MessageBase;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessagePhoto {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecObject::builder::<model::Message>("message")
                    .explicit_notify()
                    .build()]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();

            match pspec.name() {
                "message" => obj.set_message(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "message" => self.message.upgrade().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for MessagePhoto {}
    impl ui::MessageBaseImpl for MessagePhoto {}
}

glib::wrapper! {
    pub(crate) struct MessagePhoto(ObjectSubclass<imp::MessagePhoto>)
        @extends gtk::Widget, ui::MessageBase;
}

impl ui::MessageBaseExt for MessagePhoto {
    type Message = model::Message;

    fn set_message(&self, message: &Self::Message) {
        let imp = self.imp();

        let old_message = imp.message.upgrade();
        if old_message.as_ref() == Some(message) {
            return;
        }

        if let Some(binding) = imp.binding.take() {
            binding.unwatch();
        }

        if let Some(old_message) = old_message {
            let handler_id = imp.handler_id.take().unwrap();
            old_message.disconnect(handler_id);
        }

        imp.message.set(Some(message));

        imp.message_bubble.update_from_message(message, true);

        log::debug!(
            "Photo::set_message: message id={:?} content={:?}",
            message.id(),
            message.content().0,
        );

        // Setup caption expression
        let caption_binding = model::Message::this_expression("content")
            .chain_closure::<String>(closure!(
                |_: model::Message, content: model::BoxedMessageContent| {
                    log::debug!("Photo caption closure evaluating content={:?}", content.0);
                    if let tdlib::enums::MessageContent::MessagePhoto(data) = content.0 {
                        utils::parse_formatted_text(data.caption)
                    } else {
                        unreachable!();
                    }
                }
            ))
            .bind(&*imp.message_bubble, "label", Some(message));
        imp.binding.replace(Some(caption_binding));

        // Load photo
        let handler_id = message.connect_content_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |message| {
                obj.update_photo(message);
            }
        ));
        imp.handler_id.replace(Some(handler_id));
        self.update_photo(message);

        self.notify("message");
    }
}

impl MessagePhoto {
    fn update_photo(&self, message: &model::Message) {
        if let tdlib::enums::MessageContent::MessagePhoto(data) = message.content().0 {
            let session = message.chat_().session_();
            self.imp().photo_tile.set_message(message);
            self.imp().photo_tile.set_photo(&session, &data.photo);
        }
    }
}
