use std::cell::Cell;
use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;
use gtk::gio;
use gtk::glib;

use crate::model;
use crate::ui::WebmAnimation;
use crate::utils;

/// A sticker that should be downloaded once the widget is shown.
struct PendingDownload {
    sticker: tdlib::types::Sticker,
    looped: bool,
    session: model::ClientStateSession,
}

impl std::fmt::Debug for PendingDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingDownload")
            .field("sticker", &self.sticker.sticker.id)
            .field("looped", &self.looped)
            .finish_non_exhaustive()
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::Sticker)]
    pub(crate) struct Sticker {
        pub(super) file_id: Cell<i32>,
        pub(super) aspect_ratio: Cell<f64>,
        pub(super) child: RefCell<Option<gtk::Widget>>,
        /// The sticker to download when the widget is shown, so that stickers
        /// that are only scrolled past are not downloaded.
        pub(super) pending_download: RefCell<Option<super::PendingDownload>>,

        #[property(get, set = Self::set_longer_side_size)]
        pub(super) longer_side_size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Sticker {
        const NAME: &'static str = "PaplSticker";
        type Type = super::Sticker;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Sticker {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            Self::derived_set_property(self, id, value, pspec)
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            Self::derived_property(self, id, pspec)
        }

        fn dispose(&self) {
            if let Some(child) = self.child.replace(None) {
                child.unparent()
            }
        }
    }

    impl WidgetImpl for Sticker {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = self.longer_side_size.get();
            let aspect_ratio = self.aspect_ratio.get();

            let min_size = 1;

            let size = if let gtk::Orientation::Horizontal = orientation {
                if aspect_ratio >= 1.0 {
                    size
                } else {
                    (size as f64 * aspect_ratio) as i32
                }
            } else if aspect_ratio >= 1.0 {
                (size as f64 / aspect_ratio) as i32
            } else {
                size
            }
            .max(min_size);

            (size, size, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = &*self.child.borrow() {
                // GTK requires the child to be measured before it is
                // allocated, even if its size is fully determined by the
                // aspect ratio.
                child.measure(gtk::Orientation::Horizontal, -1);
                child.measure(gtk::Orientation::Vertical, -1);

                child.allocate(width, height, baseline, None);
            }
        }

        fn map(&self) {
            self.parent_map();

            self.obj().maybe_start_download();
        }
    }

    impl Sticker {
        fn set_longer_side_size(&self, size: i32) {
            self.longer_side_size.set(size);
            self.obj().queue_resize();
        }
    }
}

glib::wrapper! {
    pub(crate) struct Sticker(ObjectSubclass<imp::Sticker>)
        @extends gtk::Widget;
}

impl Sticker {
    pub(crate) fn update_sticker(
        &self,
        sticker: tdlib::types::Sticker,
        looped: bool,
        session: model::ClientStateSession,
    ) {
        let imp = self.imp();

        let file_id = sticker.sticker.id;
        if self.imp().file_id.replace(file_id) == file_id {
            return;
        }

        // TODO: draw sticker outline with cairo
        self.set_child(None);

        let aspect_ratio = sticker.width as f64 / sticker.height as f64;
        imp.aspect_ratio.set(aspect_ratio);

        if sticker.sticker.local.is_downloading_completed {
            let format = sticker.format;

            utils::spawn(clone!(
                #[weak(rename_to = obj)]
                self,
                async move {
                    obj.load_sticker(
                        sticker.sticker.local.path,
                        file_id,
                        looped,
                        format,
                        session.clone(),
                        sticker.thumbnail.clone(),
                    )
                    .await;
                }
            ));
        } else {
            // The download is started when the widget is shown.
            *imp.pending_download.borrow_mut() = Some(PendingDownload {
                sticker,
                looped,
                session,
            });
            self.maybe_start_download();
        }
    }

    /// Downloads the pending sticker, if the widget is shown.
    fn maybe_start_download(&self) {
        let imp = self.imp();

        if !self.is_mapped() {
            return;
        }

        let Some(pending) = imp.pending_download.borrow_mut().take() else {
            return;
        };

        let file_id = pending.sticker.sticker.id;
        let is_completed = pending.sticker.sticker.local.is_downloading_completed;
        let path = pending.sticker.sticker.local.path.clone();
        let format = pending.sticker.format;
        let looped = pending.looped;
        let session = pending.session;
        let thumbnail = pending.sticker.thumbnail.clone();

        // The file may have been downloaded in the meantime, e.g. by another
        // widget showing the same sticker.
        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                if is_completed {
                    obj.load_sticker(path, file_id, looped, format, session.clone(), thumbnail)
                        .await;
                } else {
                    obj.download_sticker(file_id, &session, looped, format, thumbnail)
                        .await;
                }
            }
        ));
    }

    pub(crate) fn play_animation(&self) {
        if let Some(animation) = &*self.imp().child.borrow() {
            if let Some(animation) = animation.downcast_ref::<rlt::Animation>() {
                if !animation.is_playing() {
                    animation.play();
                }
            } else if let Some(animation) = animation.downcast_ref::<WebmAnimation>() {
                if !animation.is_playing() {
                    animation.replay();
                }
            }
        }
    }

    async fn download_sticker(
        &self,
        file_id: i32,
        session: &model::ClientStateSession,
        looped: bool,
        format: tdlib::enums::StickerFormat,
        thumbnail: Option<tdlib::types::Thumbnail>,
    ) {
        match session.download_file(file_id).await {
            Ok(file) => {
                self.load_sticker(
                    file.local.path,
                    file_id,
                    looped,
                    format,
                    session.clone(),
                    thumbnail,
                )
                .await;
            }
            Err(e) => {
                log::warn!("Failed to download a sticker: {e:?}");
            }
        }
    }

    async fn load_sticker(
        &self,
        path: String,
        file_id: i32,
        looped: bool,
        format: tdlib::enums::StickerFormat,
        session: model::ClientStateSession,
        thumbnail: Option<tdlib::types::Thumbnail>,
    ) {
        let widget: gtk::Widget = match format {
            tdlib::enums::StickerFormat::Tgs => {
                let animation = rlt::Animation::from_filename(&path);
                animation.set_loop(looped);
                animation.use_cache(looped);
                animation.play();
                animation.upcast()
            }
            tdlib::enums::StickerFormat::Webm => {
                let animation = WebmAnimation::new(&path, looped);
                animation.connect_error(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| obj.show_webm_fallback(file_id, thumbnail.clone(), session.clone())
                ));
                animation.upcast()
            }
            tdlib::enums::StickerFormat::Webp => {
                let result = gio::spawn_blocking(move || utils::decode_image_from_path(&path))
                    .await
                    .unwrap();

                match result {
                    Ok(texture) => {
                        let picture = gtk::Picture::new();
                        picture.set_paintable(Some(&texture));
                        picture.upcast()
                    }
                    Err(e) => {
                        log::warn!("Error decoding a sticker: {e:?}");
                        return;
                    }
                }
            }
        };

        // Skip if widget was recycled by ListView
        if self.imp().file_id.get() == file_id {
            self.set_child(Some(widget));
        }
    }

    /// Shows the thumbnail of a sticker whose animation failed to play.
    fn show_webm_fallback(
        &self,
        file_id: i32,
        thumbnail: Option<tdlib::types::Thumbnail>,
        session: model::ClientStateSession,
    ) {
        let Some(thumbnail) = thumbnail else {
            return;
        };

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let path = if thumbnail.file.local.is_downloading_completed {
                    thumbnail.file.local.path
                } else {
                    match session.download_file(thumbnail.file.id).await {
                        Ok(file) => file.local.path,
                        Err(e) => {
                            log::warn!("Failed to download a sticker thumbnail: {e:?}");
                            return;
                        }
                    }
                };

                let result = gio::spawn_blocking(move || utils::decode_image_from_path(&path))
                    .await
                    .unwrap();

                match result {
                    Ok(texture) => {
                        // Skip if widget was recycled by ListView
                        if obj.imp().file_id.get() == file_id {
                            let picture = gtk::Picture::new();
                            picture.set_paintable(Some(&texture));
                            obj.set_child(Some(picture.upcast()));
                        }
                    }
                    Err(e) => {
                        log::warn!("Error decoding a sticker thumbnail: {e:?}");
                    }
                }
            }
        ));
    }

    fn set_child(&self, child: Option<gtk::Widget>) {
        let imp = self.imp();

        if let Some(ref child) = child {
            child.set_parent(self);
        }

        if let Some(old) = imp.child.replace(child) {
            old.unparent()
        }
    }
}
