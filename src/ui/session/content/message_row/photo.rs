use std::cell::Cell;
use std::cell::RefCell;
use std::sync::OnceLock;

use glib::clone;
use glib::closure;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use super::document::FileStatus;
use crate::model;
use crate::model::MediaType;
use crate::types::MessageId;
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
        pub(super) status_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        /// The file id of the photo that should be downloaded automatically
        /// once the widget is shown, or 0 if there is none.
        pub(super) pending_download_file_id: Cell<i32>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) picture: TemplateChild<ui::MediaPicture>,
        #[template_child]
        pub(super) download_button: TemplateChild<ui::MediaDownloadButton>,
        #[template_child]
        pub(super) click: TemplateChild<gtk::GestureClick>,
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

        fn constructed(&self) {
            self.parent_constructed();

            self.obj().connect_scale_factor_notify(|obj| {
                obj.update_photo(&obj.imp().message.upgrade().unwrap());
            });
        }
    }

    impl WidgetImpl for MessagePhoto {
        fn map(&self) {
            self.parent_map();

            log::info!(
                "[photo-debug] map: msg={:?} pending={}",
                self.obj().message_id(),
                self.pending_download_file_id.get()
            );

            self.obj().maybe_start_auto_download();
        }
    }
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

        // Setup caption expression
        let caption_binding = model::Message::this_expression("content")
            .chain_closure::<String>(closure!(
                |_: model::Message, content: model::BoxedMessageContent| {
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
    fn message_id(&self) -> Option<MessageId> {
        self.imp().message.upgrade().map(|message| message.id())
    }

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

    fn update_photo(&self, message: &model::Message) {
        if let tdlib::enums::MessageContent::MessagePhoto(mut data) = message.content().0 {
            let imp = self.imp();
            // Choose the right photo size based on the screen scale factor.
            // See https://core.telegram.org/api/files#image-thumbnail-types for more
            // information about photo sizes.
            let photo_size = if self.scale_factor() > 2 {
                data.photo.sizes.pop().unwrap()
            } else {
                let type_ = if self.scale_factor() > 1 { "y" } else { "x" };

                match data.photo.sizes.iter().position(|s| s.r#type == type_) {
                    Some(pos) => data.photo.sizes.swap_remove(pos),
                    None => data.photo.sizes.pop().unwrap(),
                }
            };

            imp.picture
                .set_aspect_ratio(photo_size.width as f64 / photo_size.height as f64);

            log::info!(
                "[photo-debug] update_photo: msg={} file={} local_completed={} scale={}",
                message.id(),
                photo_size.photo.id,
                photo_size.photo.local.is_downloading_completed,
                self.scale_factor()
            );

            if photo_size.photo.local.is_downloading_completed {
                log::info!("[photo-debug] local completed, loading from disk");

                imp.download_button.set_visible(false);
                self.disconnect_click_handler();

                self.load_photo(photo_size.photo.local.path);
            } else {
                let file_id = photo_size.photo.id;
                let size = photo_size.photo.size.max(photo_size.photo.expected_size) as u64;
                let session = message.chat_().session_();

                // Show a low-resolution preview of the photo while it is not
                // downloaded.
                imp.picture.set_paintable(
                    data.photo
                        .minithumbnail
                        .and_then(|m| {
                            gdk::Texture::from_bytes(&glib::Bytes::from_owned(
                                glib::base64_decode(&m.data),
                            ))
                            .ok()
                        })
                        .as_ref(),
                );

                if session
                    .media_manager()
                    .should_auto_download(MediaType::Photo, size)
                {
                    log::info!("[photo-debug] auto allowed, pending={file_id}");

                    imp.download_button.set_visible(false);
                    self.disconnect_click_handler();

                    // The download is started when the widget is shown, so
                    // that photos that are only scrolled past are not
                    // downloaded.
                    imp.pending_download_file_id.set(file_id);
                    self.maybe_start_auto_download();
                } else {
                    log::info!("[photo-debug] auto NOT allowed, showing download button");

                    imp.download_button.set_status(FileStatus::CanBeDownloaded);
                    imp.download_button.set_visible(true);

                    self.connect_start_download(file_id, &session);
                }
            }
        }
    }

    /// Starts the auto-download of the pending photo, if the widget is shown.
    fn maybe_start_auto_download(&self) {
        let imp = self.imp();

        log::info!(
            "[photo-debug] maybe_start: mapped={} pending={}",
            self.is_mapped(),
            imp.pending_download_file_id.get()
        );

        if !self.is_mapped() || imp.pending_download_file_id.get() == 0 {
            return;
        }

        let file_id = imp.pending_download_file_id.take();

        let Some(message) = imp.message.upgrade() else {
            log::info!("[photo-debug] message gone, download of file {file_id} dropped");
            return;
        };
        let session = message.chat_().session_();

        log::info!("[photo-debug] starting download file={file_id}");

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            async move {
                obj.download_photo(file_id, &session).await;
            }
        ));
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
                obj.download_photo(file_id, &session).await;
            }
        ));
    }

    async fn download_photo(&self, file_id: i32, session: &model::ClientStateSession) {
        match session.download_file(file_id).await {
            Ok(file) => {
                log::info!(
                    "[photo-debug] download done: file={} completed={}",
                    file_id,
                    file.local.is_downloading_completed
                );

                // The download may have been canceled by the user, in which
                // case the returned file is not usable.
                if file.local.is_downloading_completed {
                    self.load_photo(file.local.path);
                } else {
                    log::info!("[photo-debug] Photo download was canceled (file {file_id})");
                }
            }
            Err(e) => {
                log::warn!("[photo-debug] Failed to download a photo (file {file_id}): {e:?}");
            }
        }
    }

    fn load_photo(&self, path: String) {
        if let Some(message_id) = self.message_id() {
            log::info!("[photo-debug] load_photo: msg={message_id}");

            utils::spawn(clone!(
                #[weak(rename_to = obj)]
                self,
                async move {
                    let result = gio::spawn_blocking(move || utils::decode_image_from_path(&path))
                        .await
                        .unwrap();

                    // Check if the current message id is the same as the one at
                    // the time of the request. It may be changed because of the
                    // ListView recycling while decoding the image. It may also
                    // that the message has been already removed from the history
                    // and the WeakRef is None (after successful sent)
                    if obj.message_id().filter(|id| *id == message_id).is_some() {
                        match result {
                            Ok(texture) => {
                                log::info!("[photo-debug] paintable set (msg={message_id})");
                                obj.imp().picture.set_paintable(Some(&texture));
                                obj.imp().download_button.set_visible(false);
                                obj.disconnect_click_handler();
                            }
                            Err(e) => {
                                log::warn!("Error decoding a photo: {e:?}");
                            }
                        }
                    } else {
                        log::info!(
                            "[photo-debug] guard FAILED: captured={message_id} current={:?}",
                            obj.message_id()
                        );
                    }
                }
            ));
        }
    }
}
