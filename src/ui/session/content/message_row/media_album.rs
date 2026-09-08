use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use glib::clone;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use crate::model;
use crate::ui;
use crate::ui::MessageBaseExt;
use crate::ui::ViewerEntry;
use crate::ui::ViewerItem;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/media_album.ui")]
    pub(crate) struct MessageMediaAlbum {
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) album: RefCell<Option<model::MediaAlbum>>,
        pub(super) store_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        /// Maps a message id to the tile widget that renders it.
        ///
        /// Reusing existing tiles instead of recreating them on every album
        /// update preserves in-progress downloads, image decoding and video
        /// playback state.
        pub(super) tiles: RefCell<HashMap<i64, glib::WeakRef<gtk::Widget>>>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) grid: TemplateChild<ui::MediaAlbumGrid>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageMediaAlbum {
        const NAME: &'static str = "PaplMessageMediaAlbum";
        type Type = super::MessageMediaAlbum;
        type ParentType = ui::MessageBase;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageMediaAlbum {
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

        fn dispose(&self) {
            if let Some(handler_id) = self.store_handler_id.take() {
                if let Some(album) = &*self.album.borrow() {
                    album.store().disconnect(handler_id);
                }
            }
        }
    }

    impl WidgetImpl for MessageMediaAlbum {}
    impl ui::MessageBaseImpl for MessageMediaAlbum {}
}

glib::wrapper! {
    pub(crate) struct MessageMediaAlbum(ObjectSubclass<imp::MessageMediaAlbum>)
        @extends gtk::Widget, ui::MessageBase;
}

/// Displays a media album as a grid of tiles inside a message bubble.
///
/// The album is shown on behalf of its representative message (see
/// `ChatHistoryModel`). The bubble shows the sender and the indicators of the
/// representative as well as the last caption of the album, and the grid shows
/// one tile per message, sized by its media aspect ratio.
impl MessageMediaAlbum {
    /// Lays out the whole album from scratch.
    ///
    /// The tiles are appended in message order; the grid positions and sizes
    /// are computed by its layout manager from the aspect ratio of the media
    /// of every tile.
    fn render_album(&self, album: &model::MediaAlbum) {
        let imp = self.imp();
        let grid = &*imp.grid;

        while let Some(child) = grid.first_child() {
            child.unparent();
        }
        imp.tiles.borrow_mut().clear();

        let messages = album.messages();

        if messages.is_empty() {
            return;
        }

        let session = messages[0].chat_().session_();

        for message in &messages {
            let tile = self.create_tile(&session, message);
            grid.append(&tile);
            imp.tiles
                .borrow_mut()
                .insert(message.id(), tile.downgrade());
        }

        self.update_viewer_contexts(album);
    }

    /// Attaches tiles for the messages of the album that are not rendered
    /// yet, appending them at the end of the grid.
    ///
    /// This must only be used when the new messages were appended at the end
    /// of the album, so that the existing tiles keep their order. The layout
    /// adapts to the new tile count automatically.
    fn attach_new_tiles(&self, album: &model::MediaAlbum) {
        let imp = self.imp();
        let grid = &*imp.grid;

        let Some(first_message) = album.first_message() else {
            return;
        };
        let session = first_message.chat_().session_();

        for message in album.messages() {
            let mut tiles = imp.tiles.borrow_mut();

            if tiles.contains_key(&message.id()) {
                continue;
            }

            let tile = self.create_tile(&session, &message);
            grid.append(&tile);
            tiles.insert(message.id(), tile.downgrade());
        }

        self.update_viewer_contexts(album);
    }

    fn create_tile(
        &self,
        session: &model::ClientStateSession,
        message: &model::Message,
    ) -> gtk::Widget {
        let content = message.content();

        match &content.0 {
            tdlib::enums::MessageContent::MessagePhoto(data) => {
                let tile = ui::MediaPhotoTile::default();
                tile.set_message(message);
                tile.set_photo(session, &data.photo);
                tile.upcast()
            }
            tdlib::enums::MessageContent::MessageVideo(data) => {
                let tile = ui::MediaVideoTile::default();
                tile.set_message(message);
                tile.set_video(session, &data.video);
                tile.upcast()
            }
            tdlib::enums::MessageContent::MessageAnimation(data) => {
                let tile = ui::MediaVideoTile::default();
                tile.set_message(message);
                tile.set_animation(session, &data.animation);
                tile.upcast()
            }
            // Albums only contain photos, videos and animations, but a
            // content update could change the type of a message.
            _ => gtk::Label::new(Some("Unsupported")).upcast(),
        }
    }

    /// Assigns the viewer context to the tiles of the album.
    ///
    /// Each tile opens the media viewer on the whole album, at the entry of
    /// its own message. The items are built from the tiles, which track the
    /// file updates, and fall back to the message content for the tiles
    /// that are not alive anymore. The indices are assigned in the order of
    /// the viewable messages, so that they stay consistent even when some
    /// messages have no viewable media.
    fn update_viewer_contexts(&self, album: &model::MediaAlbum) {
        let imp = self.imp();

        let mut entries: Vec<ViewerEntry> = Vec::new();

        for message in album.messages() {
            let tile = imp
                .tiles
                .borrow()
                .get(&message.id())
                .and_then(|tile| tile.upgrade());

            let item = tile
                .as_ref()
                .and_then(|tile| {
                    tile.downcast_ref::<ui::MediaPhotoTile>()
                        .and_then(ui::MediaPhotoTile::viewer_item)
                        .or_else(|| {
                            tile.downcast_ref::<ui::MediaVideoTile>()
                                .and_then(ui::MediaVideoTile::viewer_item)
                        })
                })
                .or_else(|| ViewerItem::from_message(&message));

            let Some(item) = item else {
                continue;
            };

            entries.push(ViewerEntry {
                item,
                message_id: message.id(),
            });
        }

        // The tiles are assigned the whole album, so that the viewer can
        // show the media around the clicked item. The index of a message
        // within the viewable messages is looked up by its id.
        for message in album.messages() {
            let Some(tile) = imp
                .tiles
                .borrow()
                .get(&message.id())
                .and_then(|tile| tile.upgrade())
            else {
                continue;
            };

            let Some(index) = entries
                .iter()
                .position(|entry| entry.message_id == message.id())
            else {
                continue;
            };

            if let Some(photo_tile) = tile.downcast_ref::<ui::MediaPhotoTile>() {
                photo_tile.set_viewer_context(entries.clone(), index);
            } else if let Some(video_tile) = tile.downcast_ref::<ui::MediaVideoTile>() {
                video_tile.set_viewer_context(entries.clone(), index);
            }
        }
    }

    fn set_album(&self, album: &model::MediaAlbum) {
        let imp = self.imp();

        if imp.album.borrow().as_ref() == Some(album) {
            return;
        }

        if let Some(handler_id) = imp.store_handler_id.take() {
            if let Some(old_album) = &*imp.album.borrow() {
                old_album.store().disconnect(handler_id);
            }
        }

        imp.album.replace(Some(album.clone()));

        imp.message_bubble.set_label(album.caption());

        self.render_album(album);

        let store = album.store();
        let handler_id = store.connect_items_changed(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            album,
            move |store, position, removed, added| {
                // The caption may be on any message of the album, so it must
                // be re-read whenever the album changes.
                obj.imp().message_bubble.set_label(album.caption());

                let is_append = removed == 0 && position + added == store.n_items();

                if is_append {
                    obj.attach_new_tiles(&album);
                } else {
                    // Tiles were removed or reordered, so the whole grid must
                    // be laid out again.
                    obj.render_album(&album);
                }
            }
        ));
        imp.store_handler_id.replace(Some(handler_id));
    }
}

impl ui::MessageBaseExt for MessageMediaAlbum {
    type Message = model::Message;

    fn set_message(&self, message: &Self::Message) {
        let imp = self.imp();

        let old_message = imp.message.upgrade();
        let album = message.media_album();

        if old_message.as_ref() == Some(message) {
            // The message is unchanged, but it may represent another album
            // now, e.g. after the history was reloaded around it.
            if album.as_ref() == imp.album.borrow().as_ref() {
                return;
            }
        }

        imp.message.set(Some(message));

        debug_assert!(
            album.is_some(),
            "MessageMediaAlbum can only display the representative of a media album"
        );

        // The sender, the indicators and the outgoing state of the album are
        // those of its representative.
        imp.message_bubble.update_from_message(message, true);

        if let Some(album) = album {
            self.set_album(&album);
        }

        self.notify("message");
    }
}
