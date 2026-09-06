use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use glib::clone;

use crate::model;
use crate::ui;
use crate::ui::MessageBaseExt;
use crate::ui::ViewerEntry;
use crate::ui::ViewerItem;

/// Computes the grid position of the tile at the given linear index.
///
/// An even-count layout is a uniform 2-column grid. An odd-count layout shows
/// the first tile (the oldest message of the album) as a full-width tile on
/// top of a 2-column grid with the remaining ones.
fn grid_position(index: usize, layout_even: bool) -> (i32, i32, i32) {
    if layout_even {
        ((index % 2) as i32, (index / 2) as i32, 1)
    } else if index == 0 {
        (0, 0, 2)
    } else {
        let index = index - 1;
        ((index % 2) as i32, (index / 2 + 1) as i32, 1)
    }
}

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
        /// Whether the current layout is the even-count layout (uniform
        /// 2-column grid), in contrast to the odd-count layout with a
        /// full-width tile on top.
        pub(super) layout_even: Cell<bool>,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) grid: TemplateChild<gtk::Grid>,
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
/// one tile per message.
impl MessageMediaAlbum {
    /// Lays out the whole album from scratch.
    ///
    /// The layout depends on the number of messages: an even number is laid
    /// out as a uniform 2-column grid, while an odd number shows the oldest
    /// message as a full-width tile on top of a 2-column grid with the
    /// remaining ones.
    fn render_album(&self, album: &model::MediaAlbum) {
        let imp = self.imp();
        let grid = &*imp.grid;

        while let Some(child) = grid.first_child() {
            child.unparent();
        }
        imp.tiles.borrow_mut().clear();

        let messages = album.messages();
        let count = messages.len();
        imp.layout_even.set(count.is_multiple_of(2));

        if count == 0 {
            return;
        }

        let session = messages[0].chat_().session_();
        let layout_even = imp.layout_even.get();

        for (index, message) in messages.iter().enumerate() {
            let (column, row, width) = grid_position(index, layout_even);

            let tile = self.create_tile(&session, message);
            grid.attach(&tile, column, row, width, 1);
            imp.tiles.borrow_mut().insert(message.id(), tile.downgrade());
        }

        self.update_viewer_contexts(album);
    }

    /// Attaches tiles for the messages of the album that are not rendered
    /// yet, appending them at the end of the grid.
    ///
    /// This must only be used when the layout didn't change and the new
    /// messages were appended at the end of the album, so that the existing
    /// tiles keep their positions.
    fn attach_new_tiles(&self, album: &model::MediaAlbum) {
        let imp = self.imp();
        let grid = &*imp.grid;

        let Some(first_message) = album.first_message() else {
            return;
        };
        let session = first_message.chat_().session_();

        let layout_even = imp.layout_even.get();

        for message in album.messages() {
            let mut tiles = imp.tiles.borrow_mut();

            if tiles.contains_key(&message.id()) {
                continue;
            }

            let index = tiles.len();
            let (column, row, width) = grid_position(index, layout_even);

            let tile = self.create_tile(&session, &message);
            grid.attach(&tile, column, row, width, 1);
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

            let index = entries.len();
            entries.push(ViewerEntry {
                item,
                message_id: message.id(),
            });

            let Some(tile) = tile else {
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

                if is_append && obj.imp().layout_even.get() == album.len().is_multiple_of(2) {
                    obj.attach_new_tiles(&album);
                } else {
                    // The layout changed or tiles were removed, so the whole
                    // grid must be laid out again.
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
