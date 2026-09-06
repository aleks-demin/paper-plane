use std::cell::{Cell, RefCell};

use glib::clone;
use gtk::CompositeTemplate;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::model::MediaType;
use crate::utils;

use super::MediaPicture;

/// A shared preview widget for media that has a low-resolution minithumbnail
/// and an optional high-resolution thumbnail.
///
/// The minithumbnail is decoded eagerly and shown while the high-resolution
/// thumbnail is not downloaded yet, so that the tile is never empty. Once the
/// high-resolution thumbnail is available, it is decoded on a worker thread
/// and shown instead, falling back to the minithumbnail on error.
///
/// The widget owns a [`MediaPicture`] that renders the preview. The download
/// of the high-resolution thumbnail is driven by the caller, so that the
/// download settings (photo settings for thumbnails, video settings for the
/// media file itself) can be applied separately.
mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/media_thumbnail.ui")]
    pub(crate) struct MediaThumbnail {
        /// The picture rendering the preview.
        #[template_child]
        pub(super) picture: TemplateChild<MediaPicture>,
        /// The low-resolution preview decoded from the minithumbnail, used
        /// while the high-resolution thumbnail is not downloaded.
        pub(super) preview: RefCell<Option<gdk::Texture>>,
        /// The high-resolution thumbnail, if any. The thumbnail is downloaded
        /// on show and used as a higher-quality preview once it is ready.
        pub(super) thumbnail: RefCell<Option<tdlib::types::Thumbnail>>,
        /// The file id of the thumbnail that should be downloaded once the
        /// widget is shown, or 0 if there is none.
        pub(super) pending_thumbnail_file_id: Cell<i32>,
        /// Incremented every time new media is set, used to discard the
        /// results of asynchronous operations that became out-of-date.
        pub(super) generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaThumbnail {
        const NAME: &'static str = "PaplMessageMediaThumbnail";
        type Type = super::MediaThumbnail;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MediaThumbnail {
        fn constructed(&self) {
            self.parent_constructed();
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for MediaThumbnail {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.picture.measure(orientation, for_size)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.picture.allocate(width, height, baseline, None);
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaThumbnail(ObjectSubclass<imp::MediaThumbnail>)
        @extends gtk::Widget;
}

impl Default for MediaThumbnail {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MediaThumbnail {
    /// Sets the low-resolution preview of the media, decoded from the given
    /// minithumbnail. The preview is shown while the high-resolution thumbnail
    /// is not downloaded, so that the tile is never empty.
    pub(crate) fn set_preview(&self, minithumbnail: Option<&tdlib::types::Minithumbnail>) {
        let imp = self.imp();

        let preview = minithumbnail.and_then(utils::texture_from_minithumbnail);
        imp.picture.set_paintable(preview.as_ref());
        *imp.preview.borrow_mut() = preview;
    }

    /// Shows the low-resolution preview, falling back to an empty paintable
    /// if there is none.
    pub(crate) fn show_preview(&self) {
        let imp = self.imp();
        imp.picture.set_paintable(imp.preview.borrow().as_ref());
    }

    /// Returns the low-resolution preview decoded from the minithumbnail, if
    /// any, for use as a placeholder elsewhere (e.g. in the media viewer).
    pub(crate) fn preview_texture(&self) -> Option<gdk::Texture> {
        self.imp().preview.borrow().clone()
    }

    /// Stores the high-resolution thumbnail, if any, so that a higher-quality
    /// preview can be shown once it is downloaded.
    pub(crate) fn set_thumbnail(&self, thumbnail: Option<&tdlib::types::Thumbnail>) {
        let imp = self.imp();

        *imp.thumbnail.borrow_mut() = thumbnail.cloned();
        imp.pending_thumbnail_file_id.set(
            thumbnail
                .filter(|t| !t.file.local.is_downloading_completed)
                .map(|t| t.file.id)
                .unwrap_or(0),
        );
    }

    /// Increments the generation counter, used to discard the results of
    /// asynchronous operations that became out-of-date.
    pub(crate) fn bump_generation(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
    }

    /// Shows the thumbnail as a higher-quality preview if it has been
    /// downloaded and decoded, falling back to the minithumbnail preview.
    pub(crate) fn try_show_thumbnail(&self) {
        let imp = self.imp();

        let thumbnail_ref = imp.thumbnail.borrow();
        let Some(thumbnail) = thumbnail_ref.as_ref() else {
            imp.picture.set_paintable(imp.preview.borrow().as_ref());
            return;
        };

        if !thumbnail.file.local.is_downloading_completed {
            imp.picture.set_paintable(imp.preview.borrow().as_ref());
            return;
        }

        let generation = imp.generation.get();
        let path = thumbnail.file.local.path.clone();
        drop(thumbnail_ref);

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let result = gio::spawn_blocking(move || {
                    utils::decode_image_from_path(&path)
                })
                .await
                .unwrap();

                if obj.imp().generation.get() != generation {
                    return;
                }

                match result {
                    Ok(texture) => {
                        obj.show_thumbnail_texture(Some(&texture));
                    }
                    Err(e) => {
                        log::warn!("Error decoding a thumbnail: {e:?}");
                        obj.show_thumbnail_texture(None);
                    }
                }
            }
        ));
    }

    /// Shows the thumbnail as a higher-quality preview if it has been
    /// downloaded and decoded, falling back to the minithumbnail preview.
    fn show_thumbnail_texture(&self, texture: Option<&gdk::MemoryTexture>) {
        let imp = self.imp();

        if let Some(texture) = texture {
            imp.picture.set_paintable(Some(texture));
        } else {
            imp.picture.set_paintable(imp.preview.borrow().as_ref());
        }
    }

    /// Decodes the image located at `path` on a worker thread and shows it,
    /// falling back to the minithumbnail preview on error or if the media
    /// changed meanwhile.
    pub(crate) fn load_image(&self, path: String) {
        let generation = self.imp().generation.get();

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let result = gio::spawn_blocking(move || {
                    utils::decode_image_from_path(&path)
                })
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
                        log::warn!("Error decoding an image: {e:?}");
                        obj.imp()
                            .picture
                            .set_paintable(obj.imp().preview.borrow().as_ref());
                    }
                }
            }
        ));
    }

    /// Downloads the thumbnail, if the widget is shown, a thumbnail is pending
    /// and the auto-download settings allow it.
    ///
    /// This is called when the widget is mapped, so that thumbnails that are
    /// only scrolled past are not downloaded. The thumbnail is decoded on a
    /// worker thread, in the same way as photos.
    ///
    /// The thumbnail download is governed by the photo download settings, in
    /// contrast to the media file itself, which is governed by its own
    /// settings.
    pub(crate) fn maybe_download_thumbnail(&self, session: &model::ClientStateSession) {
        let imp = self.imp();

        if !self.is_mapped() || imp.pending_thumbnail_file_id.get() == 0 {
            return;
        }

        let Some(thumbnail) = imp.thumbnail.borrow().as_ref().cloned() else {
            return;
        };

        let size = thumbnail.file.size.max(thumbnail.file.expected_size) as u64;
        if !session
            .media_manager()
            .should_auto_download(MediaType::Photo, size)
        {
            return;
        }

        let file_id = imp.pending_thumbnail_file_id.take();
        let generation = imp.generation.get();

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            async move {
                if let Ok(file) = session.download_file(file_id).await {
                    if obj.imp().generation.get() != generation {
                        // New media was set while the thumbnail was
                        // being downloaded; discard the result.
                        return;
                    }

                    if let Some(thumbnail) = obj.imp().thumbnail.borrow_mut().as_mut() {
                        thumbnail.file = file;
                    }

                    let path = obj
                        .imp()
                        .thumbnail
                        .borrow()
                        .as_ref()
                        .map(|t| t.file.local.path.clone());

                    let Some(path) = path else {
                        return;
                    };

                    let result = gio::spawn_blocking(move || {
                        utils::decode_image_from_path(&path)
                    })
                    .await
                    .unwrap();

                    if obj.imp().generation.get() != generation {
                        return;
                    }

                    match result {
                        Ok(texture) => {
                            obj.show_thumbnail_texture(Some(&texture));
                        }
                        Err(e) => {
                            log::warn!("Error decoding a thumbnail: {e:?}");
                            obj.show_thumbnail_texture(None);
                        }
                    }
                }
            }
        ));
    }
}