use std::cell::Cell;
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::album_layout::{self, PhysicalSize};

/// The natural width the album asks for.
///
/// It must be larger than the widest content the message bubble will ever
/// allocate (including its padding and the negative margins of the album),
/// so that the album always fills the whole bubble. The actual layout is
/// computed from the allocated width, so the excess width is never visible.
const NATURAL_WIDTH: i32 = 480;

/// The minimum width the album may be allocated with.
///
/// GTK adds the CSS margins of the album to the measured sizes. The album is
/// styled with negative horizontal margins inside the message bubble, so the
/// minimum requested here must at least compensate them to avoid a negative
/// size request. The minimum height is measured without a width hint, so the
/// height is computed at this width to keep it smaller than the natural
/// height at any larger width.
const MIN_WIDTH: i32 = 16;
/// The minimum height requested by the vertical measurement, compensating
/// the negative vertical margins of the album.
const MIN_HEIGHT: i32 = 10;

/// Tolerance used when deciding whether a tile touches an edge of the grid,
/// to absorb the rounding of the rects.
const EPSILON: f64 = 0.5;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MediaAlbumLayout {
        /// The space between two tiles of the album, in pixels.
        pub(super) spacing: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaAlbumLayout {
        const NAME: &'static str = "PaplMediaAlbumLayout";
        type Type = super::MediaAlbumLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for MediaAlbumLayout {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecDouble::builder("spacing")
                    .minimum(0.0)
                    .default_value(2.0)
                    .construct()
                    .explicit_notify()
                    .build()]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "spacing" => {
                    self.spacing.set(value.get().unwrap());
                    self.obj().layout_changed();
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "spacing" => self.spacing.get().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl LayoutManagerImpl for MediaAlbumLayout {
        fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let children = super::layout_children(widget);
            if children.is_empty() {
                return (MIN_WIDTH, MIN_WIDTH, -1, -1);
            }

            match orientation {
                gtk::Orientation::Horizontal => (MIN_WIDTH, NATURAL_WIDTH, -1, -1),
                // `Orientation` is non-exhaustive, but only has two
                // variants.
                _ => {
                    // The minimum height is measured without a width hint,
                    // so the layout is computed at the minimum width, whose
                    // height is smaller than at any larger width.
                    let width = if for_size > 0 {
                        for_size as f64
                    } else {
                        MIN_WIDTH as f64
                    };
                    let height = super::layout_height(&children, width, self.spacing.get());
                    let height = height.max(MIN_HEIGHT);

                    (height, height, -1, -1)
                }
            }
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, _height: i32, _baseline: i32) {
            let children = super::layout_children(widget);
            if children.is_empty() {
                return;
            }

            let spacing = self.spacing.get();
            let rects = album_layout::calculate_album_grid(
                &children_sizes(&children),
                width as f64,
                spacing,
            );
            let total_height = max_height(&rects);

            let rtl = is_rtl(widget);
            // Mirrors the x coordinate of a rect for right-to-left layouts.
            let mirror_x = |rect: &album_layout::Rect| -> f64 {
                if rtl {
                    width as f64 - rect.x - rect.width
                } else {
                    rect.x
                }
            };

            // The tiles touching the edges of the grid get rounded corners
            // only on their outer corners.
            for ((child, _), rect) in children.iter().zip(rects.iter()) {
                let x = mirror_x(rect);
                let at_left = x <= EPSILON;
                let at_right = x + rect.width >= width as f64 - EPSILON;
                let at_top = rect.y <= EPSILON;
                let at_bottom = rect.y + rect.height >= total_height - EPSILON;

                update_class(child, "edge-left", at_left);
                update_class(child, "edge-right", at_right);
                update_class(child, "edge-top", at_top);
                update_class(child, "edge-bottom", at_bottom);
            }

            for ((child, _), rect) in children.iter().zip(rects.iter()) {
                let allocation = gtk::Allocation::new(
                    mirror_x(rect).round() as i32,
                    rect.y.round() as i32,
                    rect.width.round() as i32,
                    rect.height.round() as i32,
                );

                // GTK requires a child to be measured before it is
                // allocated. The measured sizes are discarded, as the album
                // grid assigns computed cell sizes instead.
                child.measure(gtk::Orientation::Horizontal, -1);
                child.measure(gtk::Orientation::Vertical, allocation.height());

                child.size_allocate(&allocation, -1);
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaAlbumLayout(ObjectSubclass<imp::MediaAlbumLayout>)
        @extends gtk::LayoutManager;
}

impl Default for MediaAlbumLayout {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Collects the children of the widget that take part in the layout,
/// together with their media size.
///
/// The size of a child is its `aspect-ratio` property with a unit height, so
/// that its ratio is preserved. Children without that property, such as the
/// placeholder for unsupported content, are treated as squares.
fn layout_children(widget: &gtk::Widget) -> Vec<(gtk::Widget, PhysicalSize)> {
    let mut children = Vec::new();

    let mut child = widget.first_child();
    while let Some(c) = child {
        child = c.next_sibling();

        if c.should_layout() {
            let ratio = if c.has_property("aspect-ratio", Some(f64::static_type())) {
                c.property_value("aspect-ratio").get::<f64>().unwrap_or(1.0)
            } else {
                1.0
            };

            children.push((
                c,
                PhysicalSize {
                    width: ratio,
                    height: 1.0,
                },
            ));
        }
    }

    children
}

fn children_sizes(children: &[(gtk::Widget, PhysicalSize)]) -> Vec<PhysicalSize> {
    children.iter().map(|(_, size)| *size).collect()
}

/// The height of the album grid laid out at the given width.
fn layout_height(
    children: &[(gtk::Widget, PhysicalSize)],
    width: f64,
    spacing: f64,
) -> i32 {
    let rects = album_layout::calculate_album_grid(&children_sizes(children), width, spacing);
    max_height(&rects).round() as i32
}

fn max_height(rects: &[album_layout::Rect]) -> f64 {
    rects
        .iter()
        .map(|rect| rect.y + rect.height)
        .fold(0.0, f64::max)
}

/// Whether the widget is laid out from right to left.
fn is_rtl(widget: &gtk::Widget) -> bool {
    match widget.direction() {
        gtk::TextDirection::Rtl => true,
        gtk::TextDirection::None => {
            gtk::Widget::default_direction() == gtk::TextDirection::Rtl
        }
        _ => false,
    }
}

/// Adds or removes a CSS class, leaving the widget untouched if it already
/// has the desired state.
fn update_class(widget: &gtk::Widget, class: &str, enabled: bool) {
    if enabled {
        if !widget.has_css_class(class) {
            widget.add_css_class(class);
        }
    } else if widget.has_css_class(class) {
        widget.remove_css_class(class);
    }
}
