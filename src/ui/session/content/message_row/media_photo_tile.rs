use std::cell::RefCell;

use glib::clone;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::model::MediaType;
use crate::ui::MediaViewer;
use crate::ui::ViewerEntry;
use crate::ui::ViewerItem;
use crate::utils;

use super::FileStatus;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MediaPhotoTile {
        pub(super) overlay: gtk::Overlay,
        pub(super) picture: super::super::MediaPicture,
        pub(super) download_button: super::super::MediaDownloadButton,
        pub(super) click: gtk::GestureClick,
        pub(super) loader: super::super::MediaLoader,
        /// The shared preview component rendering the minithumbnail or the
        /// high-resolution photo.
        pub(super) thumbnail: super::super::MediaThumbnail,
        /// The photo that is currently displayed.
        pub(super) photo: RefCell<Option<tdlib::types::Photo>>,
        /// The message this tile displays.
        ///
        /// The media viewer needs it for browsing the media of the chat of
        /// the message.
        pub(super) message: glib::WeakRef<model::Message>,
        /// The entries of the media album this tile belongs to, together
        /// with the index of this tile's entry, used to open the media
        /// viewer on the whole album.
        ///
        /// This is only set for the tiles of a media album; the tiles of
        /// single media messages open the viewer on their own media only.
        pub(super) viewer_context: RefCell<Option<(Vec<ViewerEntry>, usize)>>,
        pub(super) click_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) loader_handler_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaPhotoTile {
        const NAME: &'static str = "PaplMediaPhotoTile";
        type Type = super::MediaPhotoTile;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MediaPhotoTile {
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

            obj.connect_scale_factor_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.update_photo_size();
                }
            ));
        }

        fn dispose(&self) {
            if let Some(handler_id) = self.loader_handler_id.borrow_mut().take() {
                self.loader.disconnect(handler_id);
            }

            self.thumbnail.unparent();
            self.overlay.unparent();
        }
    }

    impl WidgetImpl for MediaPhotoTile {
        fn map(&self) {
            self.parent_map();

            // The download is started when the widget is shown, so that
            // photos that are only scrolled past are not downloaded.
            self.obj().imp().loader.maybe_start_auto_download();
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaPhotoTile(ObjectSubclass<imp::MediaPhotoTile>)
        @extends gtk::Widget;
}

impl Default for MediaPhotoTile {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// A tile of a media album (or a single media message) showing a photo.
impl MediaPhotoTile {
    /// Shows the given photo.
    ///
    /// The photo size is chosen based on the scale factor and re-evaluated
    /// when it changes.
    pub(crate) fn set_photo(
        &self,
        session: &model::ClientStateSession,
        photo: &tdlib::types::Photo,
    ) {
        let imp = self.imp();

        self.disconnect_click_handler();
        *imp.photo.borrow_mut() = Some(photo.clone());

        imp.thumbnail.bump_generation();

        imp.picture.set_paintable(gdk::Paintable::NONE);
        self.update_photo_size_with_session(session);
    }

    fn update_photo_size(&self) {
        let Some(session) = self.imp().loader.session() else {
            return;
        };

        self.update_photo_size_with_session(&session);
    }

    fn update_photo_size_with_session(&self, session: &model::ClientStateSession) {
        let imp = self.imp();
        let Some(photo) = &*imp.photo.borrow() else {
            return;
        };

        // Choose the right photo size based on the screen scale factor.
        let Some(photo_size) = utils::photo_size_for_scale_factor(&photo.sizes, self.scale_factor())
        else {
            return;
        };

        imp.picture.set_aspect_ratio(photo_size.width as f64 / photo_size.height as f64);

        imp.thumbnail.set_preview(photo.minithumbnail.as_ref());
        imp.thumbnail.set_thumbnail(None);

        imp.loader.bind(session, MediaType::Photo, &photo_size.photo);

        // Show a low-resolution preview of the photo while it is not
        // downloaded.
        if imp.loader.status() != FileStatus::Downloaded {
            imp.thumbnail.show_preview();
        }

        self.update_status();

        // If the widget is already shown, the pending auto-download must be
        // started right away instead of waiting for the next mapping.
        if self.is_mapped() {
            imp.loader.maybe_start_auto_download();
        }
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status(&self) {
        let imp = self.imp();
        let status = imp.loader.status();

        match status {
            FileStatus::Downloaded => {
                imp.download_button.set_visible(false);
                self.disconnect_click_handler();

                if let Some(path) = imp.loader.path() {
                    imp.thumbnail.load_image(path.to_string());
                }

                // Downloaded photos are opened in the media viewer.
                self.connect_open_viewer_handler();
            }
            FileStatus::Downloading(progress) => {
                let manual = !imp.loader.is_auto();

                imp.download_button.set_visible(manual);
                imp.download_button
                    .set_status(FileStatus::Downloading(progress));

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
                imp.thumbnail.show_preview();

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
            // Photos are never uploaded by this client.
            FileStatus::Uploading(_) => {}
        }
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

    /// Builds the viewer item of the photo of this tile.
    ///
    /// The viewer shows the full-resolution version of the photo, which may
    /// still have to be downloaded.
    pub(crate) fn viewer_item(&self) -> Option<ViewerItem> {
        let imp = self.imp();

        let photo = imp.photo.borrow().clone()?;
        let photo_size = photo.sizes.last()?.clone();

        Some(ViewerItem::Photo {
            file: photo_size.photo,
            placeholder: imp.thumbnail.preview_texture(),
        })
    }

    /// Opens the media viewer with the photo of this tile.
    ///
    /// The tile of a media album opens the viewer on the whole album, at
    /// its own entry. The low-resolution preview is passed along, so that
    /// the viewer has something to show while the photo is decoded.
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

    /// Replaces the click gesture handler of the media preview.
    fn replace_click_handler<F: Fn(&gtk::GestureClick, i32, f64, f64) + 'static>(&self, f: F) {
        let click = self.imp().click.clone();

        let handler_id = click.connect_released(f);

        if let Some(handler_id) = self
            .imp()
            .click_handler_id
            .borrow_mut()
            .replace(handler_id)
        {
            click.disconnect(handler_id);
        }
    }

    fn disconnect_click_handler(&self) {
        if let Some(handler_id) = self.imp().click_handler_id.borrow_mut().take() {
            self.imp().click.disconnect(handler_id);
        }
    }
}