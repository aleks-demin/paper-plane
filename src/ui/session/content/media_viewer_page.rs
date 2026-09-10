use std::cell::Cell;
use std::cell::RefCell;

use glib::clone;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::super::playback_manager;
use super::media_viewer::ViewerItem;
use super::message_row::FileStatus;
use super::message_row::MediaDownloadButton;
use super::message_row::MediaLoader;
use crate::model;
use crate::model::MediaType;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MediaViewerPage {
        pub(super) overlay: gtk::Overlay,
        pub(super) picture: gtk::Picture,
        /// The video widget rendering the media stream of a video item, with
        /// graphics offload enabled and its own built-in media controls.
        /// Only shown while a stream is bound to it.
        pub(super) video: gtk::Video,
        pub(super) download_button: MediaDownloadButton,
        pub(super) click: gtk::GestureClick,
        pub(super) loader: MediaLoader,
        /// The item shown by this page.
        pub(super) item: RefCell<Option<ViewerItem>>,
        /// The shared playback engine used to play the video of this page.
        pub(super) playback: RefCell<Option<playback_manager::PlaybackManager>>,
        /// The low-resolution preview shown until the media is displayed.
        pub(super) placeholder: RefCell<Option<gdk::Texture>>,
        /// Whether this page is the one displayed by the viewer.
        pub(super) displayed: Cell<bool>,
        /// Whether the photo of this page has been decoded already.
        pub(super) photo_decoded: Cell<bool>,
        /// Whether the picture shows the media stream of the playback
        /// engine, in which case it must be restored to the preview when
        /// the playback is taken over or stopped.
        pub(super) is_stream_bound: Cell<bool>,
        /// Incremented every time new media is set, used to discard the
        /// results of asynchronous operations that became out-of-date.
        pub(super) generation: Cell<u64>,
        pub(super) loader_handler_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaViewerPage {
        const NAME: &'static str = "PaplMediaViewerPage";
        type Type = super::MediaViewerPage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MediaViewerPage {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            obj.set_hexpand(true);
            obj.set_vexpand(true);

            obj.set_layout_manager(Some(gtk::BinLayout::new()));

            self.picture.set_hexpand(true);
            self.picture.set_vexpand(true);
            self.overlay.set_child(Some(&self.picture));

            self.video.set_hexpand(true);
            self.video.set_vexpand(true);
            self.video
                .set_graphics_offload(gtk::GraphicsOffloadEnabled::Enabled);
            self.video.set_visible(false);
            self.overlay.add_overlay(&self.video);

            self.download_button.set_halign(gtk::Align::Center);
            self.download_button.set_valign(gtk::Align::Center);
            self.download_button.set_visible(false);
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

            self.click.connect_released(clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.handle_click();
                }
            ));
        }

        fn dispose(&self) {
            if let Some(handler_id) = self.loader_handler_id.borrow_mut().take() {
                self.loader.disconnect(handler_id);
            }

            self.overlay.unparent();
        }
    }

    impl WidgetImpl for MediaViewerPage {}
}

glib::wrapper! {
    pub(crate) struct MediaViewerPage(ObjectSubclass<imp::MediaViewerPage>)
        @extends gtk::Widget;
}

impl Default for MediaViewerPage {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// A page of the media viewer, showing a single photo or video.
///
/// The page is self-contained: it shows the low-resolution preview of its
/// item until it is displayed, decodes its photo or plays its video on the
/// shared playback stream of the session, which its `gtk::Video` provides
/// the media controls for. Videos are never played on a pipeline of their
/// own: the playback engine of the session owns the single media stream,
/// which the page binds to its graphics-offloaded `gtk::Video` while it is
/// displayed.
impl MediaViewerPage {
    /// Shows the given item.
    pub(crate) fn set_item(
        &self,
        session: &model::ClientStateSession,
        item: ViewerItem,
        playback: Option<&playback_manager::PlaybackManager>,
    ) {
        let imp = self.imp();

        imp.generation.set(imp.generation.get() + 1);
        imp.displayed.set(false);
        imp.photo_decoded.set(false);
        imp.is_stream_bound.set(false);

        *imp.playback.borrow_mut() = playback.cloned();

        let placeholder = item.placeholder().cloned();
        imp.picture.set_paintable(placeholder.as_ref());
        *imp.placeholder.borrow_mut() = placeholder;

        let media_type = match &item {
            ViewerItem::Photo { .. } => MediaType::Photo,
            ViewerItem::Video { .. } => MediaType::Video,
        };
        imp.loader.bind(session, media_type, item.file());

        *imp.item.borrow_mut() = Some(item);

        imp.video.set_media_stream(gtk::MediaStream::NONE);
        imp.video.set_visible(false);

        self.update_status();
    }

    /// The path of the media file of this item, if it has been downloaded.
    fn media_path(&self) -> Option<glib::GString> {
        if self.imp().loader.status() != FileStatus::Downloaded {
            return None;
        }

        self.imp().loader.path()
    }

    /// Displays the media of this page.
    ///
    /// This is called by the viewer when this page becomes the active one.
    /// The download of the media is started here as well, so that the media
    /// of the pages that are only swiped past is not downloaded.
    pub(crate) fn activate(&self) {
        let imp = self.imp();

        imp.displayed.set(true);

        imp.loader.maybe_start_auto_download();

        match &*imp.item.borrow() {
            Some(ViewerItem::Photo { .. }) => {
                imp.picture.set_paintable(imp.placeholder.borrow().as_ref());

                self.maybe_decode_photo();
            }
            Some(ViewerItem::Video { .. }) => {
                if imp.loader.status() == FileStatus::CanBeDownloaded {
                    imp.loader.request_download();
                }

                if self.media_path().is_some() {
                    self.start_playback()
                }
            }
            None => {}
        }
    }

    /// Stops displaying the media of this page.
    ///
    /// The decoded photo or the video frame bound by the playback is
    /// dropped, so that at most one item is fully decoded at any time, and
    /// the playback of the video is stopped if this page owns the shared
    /// media stream.
    pub(crate) fn deactivate(&self) {
        let imp = self.imp();

        imp.displayed.set(false);

        if let (Some(manager), Some(path)) = (imp.playback.borrow().clone(), self.media_path()) {
            if manager.is_current(&path) {
                manager.stop();
            }
        }

        self.reset_playback_ui();

        imp.picture.set_paintable(imp.placeholder.borrow().as_ref());
    }

    /// Unbinds the media stream and hides the video widget of this page.
    fn reset_playback_ui(&self) {
        let imp = self.imp();

        if imp.is_stream_bound.get() {
            imp.is_stream_bound.set(false);
            imp.video.set_media_stream(gtk::MediaStream::NONE);
            imp.video.set_visible(false);
            imp.picture.set_paintable(imp.placeholder.borrow().as_ref());
            imp.picture.set_visible(true);
        }
    }

    /// Plays the video of this page on the shared media stream of the
    /// session.
    ///
    /// Animations loop and stay muted, while videos are played with sound.
    fn start_playback(&self) {
        let imp = self.imp();

        let is_animation = match &*imp.item.borrow() {
            Some(ViewerItem::Video { is_animation, .. }) => *is_animation,
            _ => return,
        };
        let Some(manager) = imp.playback.borrow().clone() else {
            return;
        };
        let Some(path) = self.media_path() else {
            return;
        };

        let media = manager.media_file();

        media.set_muted(is_animation);
        media.set_loop(is_animation);

        imp.is_stream_bound.set(false);
        imp.picture.set_paintable(imp.placeholder.borrow().as_ref());

        let generation = imp.generation.get();

        manager.play_file(
            &path,
            clone!(
                #[weak(rename_to = page)]
                self,
                move || {
                    page.reset_playback_ui();
                }
            ),
            clone!(
                #[weak(rename_to = page)]
                self,
                move |media: &gtk::MediaFile| {
                    let imp = page.imp();

                    if !imp.displayed.get() || imp.generation.get() != generation {
                        return;
                    }

                    imp.is_stream_bound.set(true);
                    imp.picture.set_paintable(gdk::Paintable::NONE);
                    imp.picture.set_visible(false);
                    imp.video.set_media_stream(Some(media));
                    imp.video.set_visible(true);

                    media.play();
                }
            ),
            clone!(
                #[weak(rename_to = page)]
                self,
                move || {
                    page.reset_playback_ui();
                }
            ),
        );
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status(&self) {
        let imp = self.imp();

        match imp.loader.status() {
            FileStatus::Downloaded => {
                imp.download_button.set_visible(false);

                match &*imp.item.borrow() {
                    Some(ViewerItem::Photo { .. }) => self.maybe_decode_photo(),
                    Some(ViewerItem::Video { .. }) if imp.displayed.get() => self.start_playback(),
                    Some(ViewerItem::Video { .. }) => {}
                    None => {}
                }
            }
            FileStatus::Downloading(progress) => {
                imp.download_button.set_visible(true);
                imp.download_button
                    .set_status(FileStatus::Downloading(progress));
            }
            FileStatus::CanBeDownloaded => {
                imp.download_button.set_visible(true);
                imp.download_button.set_status(FileStatus::CanBeDownloaded);
            }
            FileStatus::Uploading(_) => {}
        }
    }

    /// Decodes the photo of this page on a worker thread, if it has been
    /// downloaded and not decoded yet.
    ///
    /// Pages that are not displayed are decoded too, so that the whole
    /// carousel shows the full images of the media that is already
    /// downloaded instead of the low-resolution previews. The result of a
    /// page that stopped being displayed in the meantime is stored as its
    /// preview rather than bound to its picture.
    fn maybe_decode_photo(&self) {
        let imp = self.imp();

        if imp.photo_decoded.get() {
            return;
        }

        let Some(path) = imp.loader.path() else {
            return;
        };

        imp.photo_decoded.set(true);

        let generation = imp.generation.get();
        let path = path.to_string();

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let result = gio::spawn_blocking(move || utils::decode_image_from_path(&path))
                    .await
                    .unwrap();

                if obj.imp().generation.get() != generation {
                    return;
                }

                match result {
                    Ok(texture) => {
                        let imp = obj.imp();

                        *imp.placeholder.borrow_mut() = Some(texture.clone().into());

                        if imp.displayed.get() {
                            imp.picture.set_paintable(Some(&texture));
                        }
                    }
                    Err(e) => {
                        log::warn!("Error decoding a photo: {e:?}");
                    }
                }
            }
        ));
    }

    /// Clicking the page downloads its media, or cancels an ongoing manual
    /// download.
    fn handle_click(&self) {
        let imp = self.imp();

        match imp.loader.status() {
            FileStatus::CanBeDownloaded => imp.loader.request_download(),
            FileStatus::Downloading(_) if !imp.loader.is_auto() => imp.loader.cancel_download(),
            _ => {}
        }
    }
}
