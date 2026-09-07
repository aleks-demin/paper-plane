use std::cell::{Cell, RefCell};

use glib::clone;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::model::MediaType;
use crate::utils;

use super::media_viewer::ViewerItem;
use super::message_row::FileStatus;
use super::message_row::MediaDownloadButton;
use super::message_row::MediaLoader;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MediaViewerPage {
        pub(super) overlay: gtk::Overlay,
        pub(super) picture: gtk::Picture,
        pub(super) download_button: MediaDownloadButton,
        pub(super) click: gtk::GestureClick,
        pub(super) loader: MediaLoader,
        /// The item shown by this page.
        pub(super) item: RefCell<Option<ViewerItem>>,
        /// The low-resolution preview shown until the media is displayed.
        pub(super) placeholder: RefCell<Option<gdk::Texture>>,
        /// Whether this page is the one displayed by the viewer.
        pub(super) displayed: Cell<bool>,
        /// Whether the photo of this page has been decoded already.
        pub(super) photo_decoded: Cell<bool>,
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

            // The carousel sizes a page to its natural width unless the page
            // expands, which would let neighboring pages peek in from the
            // sides while they show their low-resolution preview.
            obj.set_hexpand(true);
            obj.set_vexpand(true);

            obj.set_layout_manager(Some(gtk::BinLayout::new()));

            self.picture.set_hexpand(true);
            self.picture.set_vexpand(true);
            self.overlay.set_child(Some(&self.picture));

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
/// The page shows the low-resolution preview of its item until its media is
/// displayed: photos are decoded once they are downloaded, while the video
/// is played on the media stream of the viewer, which binds its first frame
/// to the picture of the page.
impl MediaViewerPage {
    /// Shows the given item.
    pub(crate) fn set_item(&self, session: &model::ClientStateSession, item: ViewerItem) {
        let imp = self.imp();

        imp.generation.set(imp.generation.get() + 1);
        imp.displayed.set(false);
        imp.photo_decoded.set(false);

        let placeholder = item.placeholder().cloned();
        imp.picture.set_paintable(placeholder.as_ref());
        *imp.placeholder.borrow_mut() = placeholder;

        let media_type = match &item {
            ViewerItem::Photo { .. } => MediaType::Photo,
            ViewerItem::Video { .. } => MediaType::Video,
        };
        imp.loader.bind(session, media_type, item.file());

        *imp.item.borrow_mut() = Some(item);

        self.update_status();
    }

    /// The loader of the media file of this item.
    pub(crate) fn loader(&self) -> MediaLoader {
        self.imp().loader.clone()
    }

    /// The path of the media file of this item, if it has been downloaded.
    pub(crate) fn media_path(&self) -> Option<glib::GString> {
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
    pub(crate) fn display(&self) {
        let imp = self.imp();

        imp.displayed.set(true);

        imp.loader.maybe_start_auto_download();

        if let Some(ViewerItem::Photo { .. }) = &*imp.item.borrow() {
            self.maybe_decode_photo();
        }
    }

    /// Restores this page to its preview after it stopped being displayed.
    ///
    /// The decoded photo or the video frame bound by the viewer is dropped,
    /// so that at most one item is fully decoded at any time.
    pub(crate) fn restore(&self) {
        let imp = self.imp();

        imp.displayed.set(false);
        imp.photo_decoded.set(false);
        imp.picture.set_paintable(imp.placeholder.borrow().as_ref());
    }

    /// Binds the given paintable to the picture of this page, or restores
    /// the preview if there is none.
    ///
    /// This is used by the viewer to show the video of its media stream on
    /// the active page.
    pub(crate) fn set_media_paintable(&self, paintable: Option<&gdk::Paintable>) {
        let imp = self.imp();

        match paintable {
            Some(paintable) => imp.picture.set_paintable(Some(paintable)),
            None => imp.picture.set_paintable(imp.placeholder.borrow().as_ref()),
        }
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status(&self) {
        let imp = self.imp();

        match imp.loader.status() {
            FileStatus::Downloaded => {
                imp.download_button.set_visible(false);

                if let Some(ViewerItem::Photo { .. }) = &*imp.item.borrow() {
                    self.maybe_decode_photo();
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
            // Viewer media is never uploaded by this client.
            FileStatus::Uploading(_) => {}
        }
    }

    /// Decodes the photo of this page on a worker thread, if it has been
    /// downloaded and not decoded yet.
    fn maybe_decode_photo(&self) {
        let imp = self.imp();

        if imp.photo_decoded.get() || !imp.displayed.get() {
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
                let result =
                    gio::spawn_blocking(move || utils::decode_image_from_path(&path))
                        .await
                        .unwrap();

                if obj.imp().generation.get() != generation {
                    return;
                }

                match result {
                    Ok(texture) => {
                        obj.imp().picture.set_paintable(Some(&texture));
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
