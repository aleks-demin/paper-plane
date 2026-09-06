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
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/video.ui")]
    pub(crate) struct MessageVideo {
        pub(super) binding: RefCell<Option<gtk::ExpressionWatch>>,
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) video_tile: TemplateChild<ui::MediaVideoTile>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageVideo {
        const NAME: &'static str = "PaplMessageVideo";
        type Type = super::MessageVideo;
        type ParentType = ui::MessageBase;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageVideo {
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

    impl WidgetImpl for MessageVideo {}
    impl ui::MessageBaseImpl for MessageVideo {}
}

glib::wrapper! {
    pub(crate) struct MessageVideo(ObjectSubclass<imp::MessageVideo>)
        @extends gtk::Widget, ui::MessageBase;
}

impl ui::MessageBaseExt for MessageVideo {
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
            "Video::set_message: message id={:?} content={:?}",
            message.id(),
            message.content().0,
        );

        // Setup caption expression
        let caption_binding = model::Message::this_expression("content")
            .chain_closure::<String>(closure!(
                |_: model::Message, content: model::BoxedMessageContent| {
                    log::debug!("Video caption closure evaluating content={:?}", content.0);
                    match content.0 {
                        tdlib::enums::MessageContent::MessageVideo(data) => {
                            utils::parse_formatted_text(data.caption)
                        }
                        tdlib::enums::MessageContent::MessageAnimation(data) => {
                            utils::parse_formatted_text(data.caption)
                        }
                        _ => unreachable!(),
                    }
                }
            ))
            .bind(&*imp.message_bubble, "label", Some(message));
        imp.binding.replace(Some(caption_binding));

        let handler_id = message.connect_content_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |message| {
                obj.update_content(message, &message.content().0, &message.chat_().session_());
            }
        ));
        imp.handler_id.replace(Some(handler_id));

        self.update_content(message, &message.content().0, &message.chat_().session_());

        self.notify("message");
    }
}

impl MessageVideo {
    fn update_content(
        &self,
        message: &model::Message,
        content: &tdlib::enums::MessageContent,
        session: &model::ClientStateSession,
    ) {
        let video_tile = &*self.imp().video_tile;

        video_tile.set_message(message);

        match content {
            tdlib::enums::MessageContent::MessageAnimation(data) => {
                video_tile.set_animation(session, &data.animation);
            }
            tdlib::enums::MessageContent::MessageVideo(data) => {
                video_tile.set_video(session, &data.video);
            }
            _ => unreachable!(),
        }
    }
}
