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

const PLAY_ICON_NAME: &str = "media-playback-start-symbolic";
const REPLAY_ICON_NAME: &str = "view-refresh-symbolic";

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/video.ui")]
    pub(crate) struct MessageVideo {
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) status_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) is_animation: Cell<bool>,
        /// The loaded media stream, if any.
        pub(super) media: RefCell<Option<gtk::MediaFile>>,
        pub(super) prepared_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        /// The duration of the video in seconds. Always zero for animations.
        pub(super) video_duration_secs: Cell<i64>,
        /// Whether the playback was started at least once.
        pub(super) started: Cell<bool>,
        /// Whether the playback reached the end of the video.
        pub(super) ended: Cell<bool>,
        /// Whether the playback was paused with an explicit user action.
        ///
        /// A playback paused this way is never resumed automatically,
        /// in contrast to a playback paused because the content went out
        /// of sight.
        pub(super) user_paused: Cell<bool>,
        /// Whether enough of the preview intersects with the visible area
        /// of the chat history.
        pub(super) visible_on_screen: Cell<bool>,
        /// The scroll adjustments of the enclosing scrolled window together
        /// with their connected signal handler ids, used for tracking the
        /// visibility on screen. It is only set while this widget is mapped.
        /// The vertical adjustment comes first.
        pub(super) adjustment_handlers:
            RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
        /// The list view showing this widget, used as a coordinate space
        /// for computing the visibility on screen.
        pub(super) list_view: glib::WeakRef<gtk::ListView>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) picture: TemplateChild<ui::MediaPicture>,
        #[template_child]
        pub(super) indicator: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) download_button: TemplateChild<ui::MediaDownloadButton>,
        #[template_child]
        pub(super) play_icon: TemplateChild<gtk::Image>,
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

    impl WidgetImpl for MessageVideo {
        fn map(&self) {
            self.parent_map();
            self.obj().attach_visibility_tracker();
        }

        fn unmap(&self) {
            self.obj().detach_visibility_tracker();
            self.parent_unmap();
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);

            if self.obj().is_mapped() {
                self.obj().update_visible_on_screen();
            }
        }
    }

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

        // This widget may be recycled for another message, in which case
        // the playback of the previous media needs to be stopped.
        self.reset_media_state();

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
                imp.video_duration_secs.set(data.video.duration as i64);
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

            self.prepare_video(&file.local.path);
        } else {
            let size = file.size.max(file.expected_size) as u64;

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
                    self.prepare_video(&file.local.path);
                } else {
                    log::info!("Video download was canceled");
                }
            }
            Err(e) => {
                log::warn!("Failed to download a video: {e:?}");
            }
        }
    }

    /// Loads the video or animation located at `path` into a media stream
    /// without starting the playback.
    ///
    /// The playback is started as soon as the content becomes sufficiently
    /// visible on screen (see [`Self::set_visible_on_screen`]), provided the
    /// current settings allow it. Animations are always played automatically,
    /// whereas videos are only played automatically when the corresponding
    /// setting is enabled. Clicking the preview toggles the playback in any
    /// case.
    fn prepare_video(&self, path: &str) {
        let imp = self.imp();

        self.reset_media_state();

        // The media may have been downloaded right before this call.
        imp.download_button.set_visible(false);

        let media = gtk::MediaFile::for_filename(path);
        media.set_muted(true);
        media.set_loop(imp.is_animation.get());

        // Start the playback as soon as the stream is ready, if it should
        // already be playing at that point.
        let handler_id = media.connect_prepared_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                obj.imp().prepared_handler_id.take();
                obj.maybe_start_playback();
                obj.update_play_icon();
            }
        ));
        imp.prepared_handler_id.replace(Some(handler_id));

        if !imp.is_animation.get() {
            media.connect_timestamp_notify(clone!(
                #[weak(rename_to = obj)]
                self,
                move |media| {
                    obj.update_playback_timestamp(media);
                }
            ));
            media.connect_ended_notify(clone!(
                #[weak(rename_to = obj)]
                self,
                move |media| {
                    if media.is_ended() {
                        obj.handle_end_of_playback();
                    }
                }
            ));
        }

        imp.media.replace(Some(media.clone()));
        imp.picture.set_paintable(Some(&media));

        self.connect_playback_toggle_handler();
        self.update_play_icon();
    }

    /// Stops the playback and drops the current media, resetting all
    /// associated state.
    ///
    /// This is used when the media is replaced or when this widget is
    /// recycled for another message.
    fn reset_media_state(&self) {
        let imp = self.imp();

        if let (Some(handler_id), Some(media)) = (
            imp.prepared_handler_id.take(),
            imp.media.borrow().as_ref(),
        ) {
            media.disconnect(handler_id);
        }

        if let Some(media) = imp.media.borrow_mut().take() {
            media.pause();
        }

        imp.started.set(false);
        imp.ended.set(false);
        imp.user_paused.set(false);
        imp.video_duration_secs.set(0);

        self.update_play_icon();
    }

    /// Returns whether videos should be played automatically according to
    /// the current settings.
    fn autoplay_videos_enabled(&self) -> bool {
        self.imp()
            .message
            .upgrade()
            .map(|m| m.chat_().session_().media_manager().should_autoplay_videos())
            .unwrap_or_default()
    }

    /// Starts the playback if the current state allows it: the media must
    /// be prepared and visible on screen, not ended and not paused by the
    /// user.
    ///
    /// Animations are always played automatically, whereas videos are only
    /// played automatically when the corresponding setting is enabled.
    fn maybe_start_playback(&self) {
        let imp = self.imp();

        if !imp.visible_on_screen.get() || imp.ended.get() || imp.user_paused.get() {
            return;
        }

        let media = match imp.media.borrow().as_ref() {
            Some(media) => media.clone(),
            None => return,
        };

        if !media.is_prepared() || media.is_playing() {
            return;
        }

        if !imp.started.get() && !imp.is_animation.get() && !self.autoplay_videos_enabled() {
            return;
        }

        media.play();
        imp.started.set(true);
        self.update_play_icon();
    }

    /// Pauses the playback because the content went out of sight.
    ///
    /// The playback is not marked as paused by the user, so that it is
    /// resumed automatically when the content becomes visible again.
    fn pause_for_invisibility(&self) {
        if let Some(media) = &*self.imp().media.borrow() {
            if media.is_playing() {
                media.pause();
            }
        }

        self.update_play_icon();
    }

    /// Clicking the media preview starts, pauses or restarts the playback.
    fn toggle_playback(&self) {
        let imp = self.imp();

        let media = match imp.media.borrow().as_ref() {
            Some(media) => media.clone(),
            None => return,
        };

        if imp.ended.get() {
            self.replay();
        } else if media.is_playing() {
            imp.user_paused.set(true);
            media.pause();
        } else {
            imp.user_paused.set(false);
            // If the stream is not ready yet, the playback starts as soon
            // as it is.
            if media.is_prepared() {
                media.play();
                imp.started.set(true);
            }
        }

        self.update_play_icon();
    }

    /// Restarts the playback of the video from the beginning after it
    /// reached its end.
    fn replay(&self) {
        let imp = self.imp();

        let Some(media) = &*imp.media.borrow() else {
            return;
        };

        imp.user_paused.set(false);
        imp.ended.set(false);
        media.seek(0);
        media.play();
        imp.started.set(true);

        self.update_play_icon();
    }

    fn connect_playback_toggle_handler(&self) {
        self.replace_click_handler(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, _, _| {
                obj.toggle_playback();
            }
        ));
    }

    fn update_playback_timestamp(&self, media: &gtk::MediaFile) {
        let duration = media.duration();
        let timestamp = media.timestamp();

        // Detect the end of the playback here in addition to the `ended`
        // notification, as not all backends report the latter reliably.
        if duration > 0 && timestamp >= duration {
            self.handle_end_of_playback();
            return;
        }

        let time = (duration - timestamp) / i64::pow(10, 6);
        self.update_remaining_time(time);
    }

    fn handle_end_of_playback(&self) {
        let imp = self.imp();

        if imp.is_animation.get() || imp.ended.get() {
            return;
        }

        imp.ended.set(true);
        self.update_remaining_time(imp.video_duration_secs.get());
        self.update_play_icon();
    }

    /// Updates the visibility of the play/replay icon according to the
    /// current playback state.
    fn update_play_icon(&self) {
        let imp = self.imp();

        let media = imp.media.borrow();
        let media_ready = media
            .as_ref()
            .is_some_and(|media| media.is_prepared() && !media.is_playing());

        imp.play_icon.set_visible(!imp.is_animation.get() && media_ready);

        if media_ready {
            imp.play_icon.set_icon_name(Some(if imp.ended.get() {
                REPLAY_ICON_NAME
            } else {
                PLAY_ICON_NAME
            }));
        }
    }

    /// Starts tracking whether this widget is visible on screen.
    ///
    /// This watches the adjustments of the enclosing scrolled window and
    /// computes the intersection between this widget and the visible area
    /// of the chat history on every change.
    fn attach_visibility_tracker(&self) {
        let imp = self.imp();

        if !imp.adjustment_handlers.borrow().is_empty() {
            return;
        }

        let scrolled_window = self
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>();
        let list_view = self
            .ancestor(gtk::ListView::static_type())
            .and_downcast::<gtk::ListView>();

        let (Some(scrolled_window), Some(list_view)) = (scrolled_window, list_view) else {
            log::warn!("Couldn't track the visibility of a video: no chat history found");
            return;
        };

        imp.list_view.set(Some(&list_view));

        let handler_pairs = [scrolled_window.vadjustment(), scrolled_window.hadjustment()]
            .into_iter()
            .map(|adjustment| {
                let handler_id = adjustment.connect_value_changed(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| {
                        obj.update_visible_on_screen();
                    }
                ));
                (adjustment, handler_id)
            })
            .collect();
        imp.adjustment_handlers.replace(handler_pairs);

        self.update_visible_on_screen();
    }

    /// Stops tracking the visibility on screen and pauses the playback.
    fn detach_visibility_tracker(&self) {
        let imp = self.imp();

        for (adjustment, handler_id) in imp.adjustment_handlers.borrow_mut().drain(..) {
            adjustment.disconnect(handler_id);
        }

        imp.list_view.set(None);

        self.set_visible_on_screen(false);
    }

    /// Computes whether this widget is sufficiently visible on screen and
    /// starts or pauses the playback accordingly.
    ///
    /// The content counts as visible when at least one third of its preview
    /// intersects with the visible area of the chat history. Requiring more
    /// than a single pixel avoids starting the playback during fast
    /// scrolling.
    fn update_visible_on_screen(&self) {
        let imp = self.imp();

        let visible_on_screen = (|| {
            let list_view = imp.list_view.upgrade()?;

            let width = imp.picture.width() as f32;
            let height = imp.picture.height() as f32;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }

            let bounds = imp.picture.compute_bounds(&list_view)?;

            let handlers = imp.adjustment_handlers.borrow();
            let (vadjustment, _) = handlers.first()?;
            let (hadjustment, _) = handlers.get(1)?;

            // The area of the chat history that is currently visible.
            let viewport_rect = gtk::graphene::Rect::new(
                hadjustment.value() as f32,
                vadjustment.value() as f32,
                hadjustment.page_size() as f32,
                vadjustment.page_size() as f32,
            );

            let intersection = bounds.intersection(&viewport_rect)?;

            Some(intersection.area() * 3.0 >= bounds.area())
        })()
        .unwrap_or(false);

        self.set_visible_on_screen(visible_on_screen);
    }

    /// Sets whether this widget is sufficiently visible on screen and
    /// reacts to the change: the playback is started when the content
    /// becomes visible and paused when it goes out of sight.
    fn set_visible_on_screen(&self, visible_on_screen: bool) {
        let imp = self.imp();

        if imp.visible_on_screen.get() == visible_on_screen {
            return;
        }
        imp.visible_on_screen.set(visible_on_screen);

        if visible_on_screen {
            self.maybe_start_playback();
        } else {
            self.pause_for_invisibility();
        }
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
