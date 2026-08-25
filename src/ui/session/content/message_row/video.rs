use std::cell::Cell;
use std::cell::RefCell;
use std::sync::OnceLock;

use glib::clone;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use super::document::FileStatus;
use crate::model;
use crate::model::MediaType;
use crate::ui;
use crate::ui::MessageBaseExt;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/video.ui")]
    pub(crate) struct MessageVideo {
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) status_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) is_animation: Cell<bool>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) picture: TemplateChild<ui::MediaPicture>,
        #[template_child]
        pub(super) indicator: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) download_button: TemplateChild<ui::MediaDownloadButton>,
        #[template_child]
        pub(super) click: TemplateChild<gtk::GestureClick>,
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

        if let Some(old_message) = old_message {
            let handler_id = imp.handler_id.take().unwrap();
            old_message.disconnect(handler_id);
        }

        imp.message_bubble.update_from_message(message, true);

        let handler_id = message.connect_content_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |message| {
                obj.update_content(message.content().0, &message.chat_().session_());
            }
        ));
        imp.handler_id.replace(Some(handler_id));

        self.update_content(message.content().0, &message.chat_().session_());

        imp.message.set(Some(message));
        self.notify("message");
    }
}

impl MessageVideo {
    /// Replaces the click gesture handler of the media preview.
    fn replace_click_handler<F: Fn(&gtk::GestureClick, i32, f64, f64) + 'static>(&self, f: F) {
        let click = &*self.imp().click;

        let handler_id = click.connect_released(f);

        if let Some(handler_id) = self.imp().status_handler_id.replace(Some(handler_id)) {
            click.disconnect(handler_id);
        }
    }

    fn disconnect_click_handler(&self) {
        let imp = self.imp();

        if let Some(handler_id) = imp.status_handler_id.take() {
            imp.click.disconnect(handler_id);
        }
    }

    fn update_content(
        &self,
        content: tdlib::enums::MessageContent,
        session: &model::ClientStateSession,
    ) {
        let imp = self.imp();

        let (caption, file, aspect_ratio, minithumbnail) =
            if let tdlib::enums::MessageContent::MessageAnimation(data) = content {
                imp.indicator.set_label("GIF");
                imp.is_animation.set(true);
                (
                    data.caption,
                    data.animation.animation,
                    data.animation.width as f64 / data.animation.height as f64,
                    data.animation.minithumbnail,
                )
            } else if let tdlib::enums::MessageContent::MessageVideo(data) = content {
                self.update_remaining_time(data.video.duration as i64);
                imp.is_animation.set(false);
                (
                    data.caption,
                    data.video.video,
                    data.video.width as f64 / data.video.height as f64,
                    data.video.minithumbnail,
                )
            } else {
                unreachable!();
            };

        let caption = utils::parse_formatted_text(caption);
        imp.message_bubble.set_label(caption);

        imp.picture.set_aspect_ratio(aspect_ratio);

        if file.local.is_downloading_completed {
            imp.download_button.set_visible(false);
            self.disconnect_click_handler();

            self.load_video(&file.local.path);
        } else {
            let size = file.size.max(file.expected_size) as u64;

            if session
                .media_manager()
                .should_auto_download(MediaType::Video, size)
            {
                imp.download_button.set_visible(false);
                self.disconnect_click_handler();

                let file_id = file.id;
                utils::spawn(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    #[weak]
                    session,
                    async move {
                        obj.download_video(file_id, &session).await;
                    }
                ));
            } else {
                // Show a low-resolution preview of the video while it is not
                // downloaded.
                imp.picture.set_paintable(
                    minithumbnail
                        .and_then(|m| {
                            gdk::Texture::from_bytes(&glib::Bytes::from_owned(glib::base64_decode(
                                &m.data,
                            )))
                            .ok()
                        })
                        .as_ref(),
                );

                imp.download_button.set_status(FileStatus::CanBeDownloaded);
                imp.download_button.set_visible(true);

                let file_id = file.id;
                self.connect_start_download(file_id, session);
            }
        }
    }

    /// Clicking the media preview starts the download.
    fn connect_start_download(&self, file_id: i32, session: &model::ClientStateSession) {
        self.replace_click_handler(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            move |_, _, _, _| {
                obj.start_download(file_id, &session);
            }
        ));
    }

    fn start_download(&self, file_id: i32, session: &model::ClientStateSession) {
        let imp = self.imp();

        imp.download_button
            .set_status(FileStatus::Downloading(0.0));

        // Show the progress of the download on the download button.
        session.media_manager().download_file_with_updates(
            file_id,
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |file| {
                    obj.imp().download_button.set_status(FileStatus::from(&file));
                }
            ),
        );

        // Clicking again cancels the download.
        self.replace_click_handler(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            move |_, _, _, _| {
                session.media_manager().cancel_download_file(file_id);
                obj.imp()
                    .download_button
                    .set_status(FileStatus::CanBeDownloaded);
                obj.connect_start_download(file_id, &session);
            }
        ));

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            async move {
                obj.download_video(file_id, &session).await;
            }
        ));
    }

    async fn download_video(&self, file_id: i32, session: &model::ClientStateSession) {
        match session.download_file(file_id).await {
            Ok(file) => {
                // The download may have been canceled by the user, in which
                // case the returned file is not usable.
                if file.local.is_downloading_completed {
                    self.load_video(&file.local.path);
                } else {
                    log::info!("Video download was canceled");
                }
            }
            Err(e) => {
                log::warn!("Failed to download a video: {e:?}");
            }
        }
    }

    fn load_video(&self, path: &str) {
        let imp = self.imp();

        imp.download_button.set_visible(false);
        self.disconnect_click_handler();

        let media = gtk::MediaFile::for_filename(path);
        media.set_muted(true);
        media.set_loop(true);
        media.play();

        if !imp.is_animation.get() {
            media.connect_timestamp_notify(clone!(
                #[weak(rename_to = obj)]
                self,
                move |media| {
                    let time = (media.duration() - media.timestamp()) / i64::pow(10, 6);
                    obj.update_remaining_time(time);
                }
            ));
        }

        imp.picture.set_paintable(Some(&media));
    }

    fn update_remaining_time(&self, time: i64) {
        let imp = self.imp();
        let seconds = time % 60;
        let minutes = (time % (60 * 60)) / 60;
        let hours = time / (60 * 60);

        if hours > 0 {
            imp.indicator
                .set_label(&format!("{hours}:{minutes:02}:{seconds:02}"));
        } else {
            imp.indicator.set_label(&format!("{minutes}:{seconds:02}"));
        }
    }
}
