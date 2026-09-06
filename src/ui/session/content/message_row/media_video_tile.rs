use std::cell::Cell;
use std::cell::RefCell;

use glib::clone;
use glib::Properties;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::model::MediaType;
use crate::ui::MediaViewer;
use crate::ui::ViewerEntry;
use crate::ui::ViewerItem;

use super::FileStatus;

const ANIMATION_INDICATOR_LABEL: &str = "GIF";

mod imp {
    use super::*;

    #[derive(Debug, Default, Properties)]
    #[properties(wrapper_type = super::MediaVideoTile)]
    pub(crate) struct MediaVideoTile {
        pub(super) overlay: gtk::Overlay,
        pub(super) picture: super::super::MediaPicture,
        pub(super) indicator: gtk::Label,
        pub(super) download_button: super::super::MediaDownloadButton,
        pub(super) click: gtk::GestureClick,
        pub(super) loader: super::super::MediaLoader,
        /// Whether the current media is an animation (GIF).
        pub(super) is_animation: Cell<bool>,
        /// The duration of the video in seconds. Always zero for animations.
        pub(super) video_duration_secs: Cell<i64>,
        /// The most recent state of the media file of this tile, as
        /// reported by the loader.
        pub(super) file: RefCell<Option<tdlib::types::File>>,
        /// The items of the media album this tile belongs to, together with
        /// the index of this tile's item, used to open the media viewer on
        /// the whole album.
        ///
        /// This is only set for the tiles of a media album; the tiles of
        /// single media messages open the viewer on their own media only.
        pub(super) viewer_context: RefCell<Option<(Vec<ViewerEntry>, usize)>>,
        /// The message this tile displays.
        ///
        /// The media viewer needs it for browsing the media of the chat of
        /// the message.
        pub(super) message: glib::WeakRef<model::Message>,
        /// The loaded media stream, if any. Only animations own a stream in
        /// the chat; videos are played in the media viewer.
        pub(super) media: RefCell<Option<gtk::MediaFile>>,
        pub(super) prepared_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        /// Whether the playback should start as soon as the media stream is
        /// ready, as a result of an explicit user action. Only animations
        /// use this: videos are opened in the media viewer.
        pub(super) pending_play: Cell<bool>,
        /// Whether the video file has been loaded into a media stream.
        /// This is set to true in [`Self::prepare_video`] and reset when
        /// the stream is closed, so that the stream is only created once
        /// per media lifetime.
        pub(super) media_prepared: Cell<bool>,
        /// The shared preview component rendering the minithumbnail or the
        /// high-resolution thumbnail.
        pub(super) thumbnail: super::super::MediaThumbnail,
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
        pub(super) click_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) loader_handler_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaVideoTile {
        const NAME: &'static str = "PaplMediaVideoTile";
        type Type = super::MediaVideoTile;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MediaVideoTile {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec)
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            obj.set_layout_manager(Some(gtk::BinLayout::new()));

            self.picture.set_hexpand(true);
            self.picture.set_vexpand(true);
            self.overlay.set_child(Some(&self.picture));

            self.thumbnail.set_hexpand(true);
            self.thumbnail.set_vexpand(true);
            self.overlay.add_overlay(&self.thumbnail);

            self.indicator.set_halign(gtk::Align::Start);
            self.indicator.set_valign(gtk::Align::Start);
            self.indicator.add_css_class("osd-indicator");
            self.overlay.add_overlay(&self.indicator);

            self.download_button.set_halign(gtk::Align::Center);
            self.download_button.set_valign(gtk::Align::Center);
            self.overlay.add_overlay(&self.download_button);

            self.click.set_button(1);
            self.overlay.add_controller(self.click.clone());

            self.overlay.set_parent(&*obj);

            let handler_id = self.loader.connect_status_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.update_status();
                }
            ));
            *self.loader_handler_id.borrow_mut() = Some(handler_id);
        }

        fn dispose(&self) {
            if let Some(handler_id) = self.loader_handler_id.borrow_mut().take() {
                self.loader.disconnect(handler_id);
            }

            self.thumbnail.unparent();
            self.overlay.unparent();
        }
    }

    impl WidgetImpl for MediaVideoTile {
        fn map(&self) {
            self.parent_map();

            // The download is started when the widget is shown, so that
            // videos that are only scrolled past are not downloaded.
            let obj = self.obj();
            let imp = obj.imp();
            imp.loader.maybe_start_auto_download();

            if let Some(session) = imp.loader.session() {
                imp.thumbnail.maybe_download_thumbnail(&session);
            }
            obj.try_show_thumbnail();

            obj.attach_visibility_tracker();
        }

        fn unmap(&self) {
            // The media pipeline is stopped as soon as the widget goes out
            // of sight. Leaving a merely paused pipeline behind would keep
            // its resources allocated, and its synchronous teardown on the
            // main thread at widget destruction could then freeze the whole
            // UI if the pipeline had gotten stuck in the meantime.
            let obj = self.obj();
            let imp = obj.imp();

            // Only animations own a media stream in the chat.
            if imp.is_animation.get() {
                obj.close_media();
            }

            obj.detach_visibility_tracker();
            self.parent_unmap();
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);

            if self.obj().is_mapped() {
                self.obj().update_visible_on_screen();
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaVideoTile(ObjectSubclass<imp::MediaVideoTile>)
        @extends gtk::Widget;
}

impl Default for MediaVideoTile {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// A tile of a media album (or a single media message) showing a video or an
/// animation (GIF).
impl MediaVideoTile {
    /// Shows the given video.
    pub(crate) fn set_video(
        &self,
        session: &model::ClientStateSession,
        video: &tdlib::types::Video,
    ) {
        self.update_media(
            session,
            &video.video,
            video.width as f64 / video.height as f64,
            video.duration as i64,
            false,
            video.minithumbnail.as_ref(),
            video.thumbnail.as_ref(),
        );
    }

    /// Shows the given animation (GIF).
    pub(crate) fn set_animation(
        &self,
        session: &model::ClientStateSession,
        animation: &tdlib::types::Animation,
    ) {
        self.update_media(
            session,
            &animation.animation,
            animation.width as f64 / animation.height as f64,
            0,
            true,
            animation.minithumbnail.as_ref(),
            animation.thumbnail.as_ref(),
        );
    }

    fn update_media(
        &self,
        session: &model::ClientStateSession,
        file: &tdlib::types::File,
        aspect_ratio: f64,
        duration_secs: i64,
        is_animation: bool,
        minithumbnail: Option<&tdlib::types::Minithumbnail>,
        thumbnail: Option<&tdlib::types::Thumbnail>,
    ) {
        let imp = self.imp();

        // This widget may be reused for other media, in which case the
        // playback of the previous media needs to be stopped.
        self.reset_media_state();

        imp.is_animation.set(is_animation);
        imp.video_duration_secs.set(duration_secs);
        *imp.file.borrow_mut() = Some(file.clone());
        // The loader still refers to the previous media here, so the
        // indicator is finalized by `update_status` after binding below.
        self.update_indicator();

        imp.picture.set_paintable(gdk::Paintable::NONE);
        imp.picture.set_aspect_ratio(aspect_ratio);

        imp.loader.bind(session, MediaType::Video, file);
        self.update_status();

        // Store the low-resolution preview and the high-resolution thumbnail,
        // so that the preview is never empty. The component's generation is
        // bumped before binding the media, so that in-flight asynchronous
        // operations for the previous media are discarded.
        imp.thumbnail.set_preview(minithumbnail);
        imp.thumbnail.set_thumbnail(thumbnail);
        imp.thumbnail.bump_generation();

        // If the widget is already shown, the pending downloads must be
        // started right away instead of waiting for the next mapping.
        if self.is_mapped() {
            imp.loader.maybe_start_auto_download();
            imp.thumbnail.maybe_download_thumbnail(session);
        }
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status(&self) {
        let imp = self.imp();
        let status = imp.loader.status();

        match status {
            FileStatus::Downloaded => {
                imp.download_button.set_visible(false);

                // Show the preview until the media is playing, so that the
                // tile is not empty.
                self.try_show_thumbnail();

                if imp.is_animation.get() {
                    // Animations are played manually by clicking.
                    self.connect_playback_toggle_handler();
                } else {
                    // Videos are opened in the media viewer on click.
                    self.connect_open_viewer_handler();
                }
            }
            FileStatus::Downloading(progress) => {
                let manual = !imp.loader.is_auto();

                imp.download_button.set_visible(manual);
                imp.download_button.set_status(FileStatus::Downloading(progress));

                if manual {
                    // Clicking again cancels the download.
                    self.replace_click_handler(clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _, _| {
                            obj.imp().loader.cancel_download();
                        }
                    ));
                } else {
                    self.disconnect_click_handler();
                }
            }
            FileStatus::CanBeDownloaded => {
                imp.download_button.set_visible(true);
                imp.download_button.set_status(FileStatus::CanBeDownloaded);

                self.replace_click_handler(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _, _, _| {
                        obj.imp().loader.request_download();
                    }
                ));
            }
            // Videos are never uploaded by this client.
            FileStatus::Uploading(_) => {}
        }

        // The indicator depends on the loader status: it shows the file
        // size while the download is pending.
        self.update_indicator();
    }

    /// Loads the video or animation located at `path` into a media stream
    /// without starting the playback.
    ///
    /// The playback is started as soon as the stream is ready, if a playback
    /// was requested meanwhile. Clicking the preview toggles the playback.
    ///
    /// Only animations reach this: videos are opened in the media viewer
    /// instead.
    fn prepare_video(&self, path: &glib::GString) {
        let imp = self.imp();

        self.reset_media_state();

        // The media may have been downloaded right before this call.
        imp.download_button.set_visible(false);

        let media = gtk::MediaFile::for_filename(path.as_str());
        media.set_muted(true);
        media.set_loop(imp.is_animation.get());

        // Start the playback as soon as the stream is ready, if a playback
        // was requested meanwhile.
        let handler_id = media.connect_prepared_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                obj.imp().prepared_handler_id.take();
                obj.maybe_start_playback();
            }
        ));
        *imp.prepared_handler_id.borrow_mut() = Some(handler_id);

        *imp.media.borrow_mut() = Some(media.clone());
        imp.picture.set_paintable(Some(&media));

        imp.media_prepared.set(true);
    }

    /// Stops the playback and drops the current media, resetting all
    /// associated state.
    ///
    /// This is used when the media is replaced or when this widget is
    /// recycled for other media. The media metadata (animation type and
    /// duration) is kept, as the callers set it anew right after this.
    fn reset_media_state(&self) {
        self.close_media();

        self.imp().user_paused.set(false);
    }

    /// Stops the playback and closes the current media stream, freeing the
    /// resources of the underlying pipeline.
    ///
    /// Closing the stream is essential: merely pausing it would leave a
    /// full media pipeline behind, whose teardown happens synchronously on
    /// the main thread when the stream is eventually dropped. If such a
    /// leftover pipeline got stuck, the whole UI would freeze. The loader
    /// binding and the media metadata are kept, so that the stream can be
    /// prepared again later with [`Self::maybe_prepare_media`].
    fn close_media(&self) {
        let imp = self.imp();

        if let Some(handler_id) = imp.prepared_handler_id.take() {
            if let Some(media) = imp.media.borrow().as_ref() {
                media.disconnect(handler_id);
            }
        }

        if let Some(media) = imp.media.borrow_mut().take() {
            media.pause();
            // Clearing the file closes the stream, stopping the pipeline
            // of the media backend. Merely dropping the stream would leave
            // the teardown to the object finalization.
            media.clear();
        }

        imp.pending_play.set(false);
        imp.media_prepared.set(false);

        self.disconnect_click_handler();

        // Show the preview, so that the tile is not empty once the
        // media stream is closed.
        self.try_show_thumbnail();
    }

    /// Prepares the media stream from the bound file if it has been downloaded
    /// and the stream has not been prepared yet.
    ///
    /// This is only called when the user explicitly clicks to play, so that
    /// videos that are merely shown on screen do not keep a pipeline alive.
    fn maybe_prepare_media(&self) {
        let imp = self.imp();

        if imp.media_prepared.get() || imp.media.borrow().is_some() {
            return;
        }

        if imp.loader.status() == FileStatus::Downloaded {
            if let Some(path) = imp.loader.path() {
                self.prepare_video(&path);
            }
        }

        // The thumbnail may have been downloaded already.
        self.try_show_thumbnail();
    }

    /// Shows the thumbnail as a preview if it has been downloaded already.
    ///
    /// The video stream is not replaced while it is playing, so that the
    /// playback is not interrupted.
    fn try_show_thumbnail(&self) {
        let imp = self.imp();

        if imp.media.borrow().is_some() {
            // The video stream is playing, so the thumbnail is not needed.
            return;
        }

        imp.thumbnail.try_show_thumbnail();
    }

    /// Starts the playback if it has been explicitly requested and the
    /// current state allows it: the media must be prepared and visible on
    /// screen, not paused by the user.
    fn maybe_start_playback(&self) {
        let imp = self.imp();

        if !imp.pending_play.get() {
            return;
        }

        if !imp.visible_on_screen.get() || imp.user_paused.get() {
            return;
        }

        let media = match imp.media.borrow().as_ref() {
            Some(media) => media.clone(),
            None => return,
        };

        if !media.is_prepared() || media.is_playing() {
            return;
        }

        media.play();
        imp.pending_play.set(false);
    }

    /// Pauses the playback because the content went out of sight.
    ///
    /// The playback is only resumed when the user clicks to play again.
    fn pause_for_invisibility(&self) {
        if let Some(media) = &*self.imp().media.borrow() {
            if media.is_playing() {
                media.pause();
            }
        }
    }

    /// Clicking the media preview starts or pauses the playback.
    ///
    /// If the video has not been loaded into a media stream yet, the stream
    /// is created on demand and playback starts as soon as it is ready.
    fn toggle_playback(&self) {
        let imp = self.imp();

        if imp.media.borrow().is_none() {
            // The video has not been decoded yet. Create the stream and
            // start playback as soon as it is ready.
            imp.pending_play.set(true);
            self.maybe_prepare_media();
            return;
        }

        let media = match imp.media.borrow().as_ref() {
            Some(media) => media.clone(),
            None => return,
        };

        if media.is_playing() {
            imp.user_paused.set(true);
            imp.pending_play.set(false);
            media.pause();
        } else {
            imp.user_paused.set(false);
            imp.pending_play.set(false);
            // If the stream is not ready yet, the playback starts as soon
            // as it is.
            if media.is_prepared() {
                media.play();
            } else {
                imp.pending_play.set(true);
            }
        }
    }

    /// Updates the label of the OSD indicator according to the current media
    /// and the status of the loader.
    ///
    /// Animations show a static label. Videos show their duration, plus the
    /// file size as long as the file has to be downloaded manually.
    fn update_indicator(&self) {
        let imp = self.imp();

        if imp.is_animation.get() {
            imp.indicator.set_label(ANIMATION_INDICATOR_LABEL);
            return;
        }

        let mut label = format_duration(imp.video_duration_secs.get());

        if imp.loader.status() == FileStatus::CanBeDownloaded {
            label.push(' ');
            label.push_str(&glib::format_size(imp.loader.size()));
        }

        imp.indicator.set_label(&label);
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
        *imp.adjustment_handlers.borrow_mut() = handler_pairs;

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

    /// Computes whether this widget is sufficiently visible on screen.
    ///
    /// The content counts as visible when at least one third of its preview
    /// intersects with the visible area of the chat history. Requiring more
    /// than a single pixel avoids starting the playback during fast
    /// scrolling.
    fn update_visible_on_screen(&self) {
        let imp = self.imp();

        let computed = (|| {
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
        })();

        let visible_on_screen = computed.unwrap_or(false);

        self.set_visible_on_screen(visible_on_screen);
    }

    /// Sets whether this widget is sufficiently visible on screen and
    /// pauses the playback when it goes out of sight. A playback that was
    /// requested by the user while the content was off screen is resumed
    /// once the content becomes visible again.
    fn set_visible_on_screen(&self, visible_on_screen: bool) {
        let imp = self.imp();

        if imp.visible_on_screen.get() == visible_on_screen {
            return;
        }
        imp.visible_on_screen.set(visible_on_screen);

        if visible_on_screen {
            if imp.pending_play.get() {
                self.maybe_start_playback();
            }
        } else {
            self.pause_for_invisibility();
        }
    }

    /// Replaces the click gesture handler of the media preview.
    fn replace_click_handler<F: Fn(&gtk::GestureClick, i32, f64, f64) + 'static>(&self, f: F) {
        let click = self.imp().click.clone();

        let handler_id = click.connect_released(f);

        if let Some(handler_id) = self.imp().click_handler_id.borrow_mut().replace(handler_id) {
            click.disconnect(handler_id);
        }
    }

    fn disconnect_click_handler(&self) {
        if let Some(handler_id) = self.imp().click_handler_id.borrow_mut().take() {
            self.imp().click.disconnect(handler_id);
        }
    }

    /// Connects the click gesture to start or pause the playback.
    fn connect_playback_toggle_handler(&self) {
        self.replace_click_handler(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, _, _| {
                obj.toggle_playback();
            }
        ));
    }

    /// Connects the click gesture to open the media viewer.
    fn connect_open_viewer_handler(&self) {
        self.replace_click_handler(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, _, _| {
                obj.open_viewer();
            }
        ));
    }

    /// Sets the message this tile displays.
    pub(crate) fn set_message(&self, message: &model::Message) {
        self.imp().message.set(Some(message));
    }

    /// Sets the entries of the media album this tile belongs to, together
    /// with the index of this tile's entry.
    ///
    /// Clicking the tile opens the media viewer on the whole album, at the
    /// entry of this tile.
    pub(crate) fn set_viewer_context(&self, entries: Vec<ViewerEntry>, index: usize) {
        *self.imp().viewer_context.borrow_mut() = Some((entries, index));
    }

    /// Builds the viewer item of the media of this tile.
    ///
    /// The file is taken from the loader, which tracks the file updates, so
    /// that it is more recent than the snapshot of the message content.
    pub(crate) fn viewer_item(&self) -> Option<ViewerItem> {
        let imp = self.imp();

        Some(ViewerItem::Video {
            file: imp.loader.file()?,
            placeholder: imp.thumbnail.preview_texture(),
            is_animation: imp.is_animation.get(),
        })
    }

    /// Opens the media viewer with the media of this tile.
    ///
    /// The tile of a media album opens the viewer on the whole album, at
    /// its own entry. The low-resolution preview is passed along, so that
    /// the viewer has something to show while the media is decoded.
    fn open_viewer(&self) {
        let imp = self.imp();

        let Some(message) = imp.message.upgrade() else {
            return;
        };

        let chat = message.chat_();

        if let Some((entries, index)) = &*imp.viewer_context.borrow() {
            MediaViewer::present(self, &chat, entries.clone(), *index);
            return;
        }

        let Some(item) = self.viewer_item() else {
            return;
        };

        MediaViewer::present(
            self,
            &chat,
            vec![ViewerEntry {
                item,
                message_id: message.id(),
            }],
            0,
        );
    }
}

/// Formats a duration in seconds as `h:mm:ss`, or as `m:ss` if it is shorter
/// than an hour.
fn format_duration(time: i64) -> String {
    let seconds = time % 60;
    let minutes = (time % (60 * 60)) / 60;
    let hours = time / (60 * 60);

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
