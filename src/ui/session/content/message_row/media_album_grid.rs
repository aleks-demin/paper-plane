use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::utils;

use super::media_album_layout::MediaAlbumLayout;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MediaAlbumGrid {}

    #[glib::object_subclass]
    impl ObjectSubclass for MediaAlbumGrid {
        const NAME: &'static str = "PaplMediaAlbumGrid";
        type Type = super::MediaAlbumGrid;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("mediaalbumgrid");
        }
    }

    impl ObjectImpl for MediaAlbumGrid {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .set_layout_manager(Some(MediaAlbumLayout::default()));
        }
        fn dispose(&self) {
            utils::unparent_children(&*self.obj());
        }
    }

    impl WidgetImpl for MediaAlbumGrid {}
}

glib::wrapper! {
    pub(crate) struct MediaAlbumGrid(ObjectSubclass<imp::MediaAlbumGrid>)
        @extends gtk::Widget;
}

impl Default for MediaAlbumGrid {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// A container that arranges its children as a grid of media tiles.
///
/// Every child covers the cell that the [`MediaAlbumLayout`] computes from
/// the aspect ratio of its media, so that the tiles form a compact grid
/// whose size only depends on the available width. The children are managed
/// with the common widget API (`append`, `first_child`).
impl MediaAlbumGrid {
    /// Adds a tile to the end of the grid.
    pub(crate) fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.insert_before(self, gtk::Widget::NONE);
    }
}
