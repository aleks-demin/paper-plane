use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

const MIN_BAR_HEIGHT: f64 = 3.0;
const BAR_WIDTH: f64 = 3.0;
const BAR_SPACING: f64 = 2.0;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct Waveform {
        pub(super) waveform: RefCell<String>,
        pub(super) peaks: RefCell<Vec<f32>>,
        pub(super) progress: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Waveform {
        const NAME: &'static str = "PaplWaveform";
        type Type = super::Waveform;
        type ParentType = gtk::DrawingArea;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("waveform");
        }
    }

    impl ObjectImpl for Waveform {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("waveform")
                        .explicit_notify()
                        .build(),
                    glib::ParamSpecDouble::builder("progress")
                        .maximum(1.0)
                        .explicit_notify()
                        .build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "waveform" => self.obj().set_waveform(&value.get::<String>().unwrap()),
                "progress" => self.obj().set_progress(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "waveform" => self.obj().waveform().to_value(),
                "progress" => self.obj().progress().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = &*self.obj();

            obj.set_height_request(28);
            obj.set_hexpand(true);
            obj.set_valign(gtk::Align::Center);
            obj.set_draw_func(draw);

            let adw_style_manager = adw::StyleManager::default();
            adw_style_manager.connect_dark_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.queue_draw()
            ));
            adw_style_manager.connect_high_contrast_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.queue_draw()
            ));
        }
    }
    impl WidgetImpl for Waveform {}
    impl gtk::subclass::drawing_area::DrawingAreaImpl for Waveform {}
}

fn draw(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let area = area.downcast_ref::<Waveform>().unwrap();

    let foreground = area.color();
    let accent = adw::StyleManager::default().accent_color_rgba();

    let peaks = area.imp().peaks.borrow();
    let progress = area.imp().progress.get();

    let width = width as f64;
    let height = height as f64;

    let bar_total_width = BAR_WIDTH + BAR_SPACING;
    let max_bars = ((width / bar_total_width).floor() as usize).max(1);

    cr.set_line_width(BAR_WIDTH);
    cr.set_line_cap(gtk::cairo::LineCap::Round);

    for i in 0..max_bars {
        // Resample the peaks to fit the available width, using the maximum of
        // each bucket so the shape is preserved.
        let amplitude = if peaks.len() <= max_bars {
            peaks.get(i).copied().unwrap_or(0.0)
        } else {
            let bucket_size = peaks.len() as f64 / max_bars as f64;
            let start = (i as f64 * bucket_size).floor() as usize;
            let end = (((i + 1) as f64 * bucket_size).ceil() as usize).min(peaks.len());

            peaks[start..end].iter().copied().fold(0.0_f32, f32::max)
        };

        let bar_height = (height * amplitude as f64).max(MIN_BAR_HEIGHT);
        let y1 = (height - bar_height) / 2.0;
        let y2 = y1 + bar_height;

        let x = i as f64 * bar_total_width + BAR_WIDTH / 2.0;

        let color = if (i + 1) as f64 / max_bars as f64 <= progress {
            accent
        } else {
            foreground
        };
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );

        cr.move_to(x, y1);
        cr.line_to(x, y2);

        let _ = cr.stroke();
    }
}

glib::wrapper! {
    pub(crate) struct Waveform(ObjectSubclass<imp::Waveform>)
        @extends gtk::DrawingArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Waveform {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Waveform {
    pub(crate) fn waveform(&self) -> String {
        self.imp().waveform.borrow().clone()
    }

    pub(crate) fn set_waveform(&self, waveform: &str) {
        if self.waveform() == waveform {
            return;
        }

        {
            let mut waveform_ = self.imp().waveform.borrow_mut();
            waveform_.clear();
            waveform_.push_str(waveform);
        }

        let peaks = decode_waveform(waveform);

        self.imp().peaks.replace(peaks);
        self.queue_draw();
        self.notify("waveform");
    }

    pub(crate) fn progress(&self) -> f64 {
        self.imp().progress.get()
    }

    pub(crate) fn set_progress(&self, value: f64) {
        let value = value.clamp(0.0, 1.0);

        if self.progress() != value {
            self.imp().progress.set(value);
            self.queue_draw();
            self.notify("progress");
        }
    }
}

/// Decodes the waveform of a voice note. TDLib provides it as a string where
/// each sample is 5 bits wide, packed in little-endian bit order.
fn decode_waveform(waveform: &str) -> Vec<f32> {
    let bytes = waveform.as_bytes();

    let sample_count = bytes.len() * 8 / 5;
    let mut peaks = Vec::with_capacity(sample_count);

    for i in 0..sample_count {
        let start_bit = i * 5;

        let byte_index = start_bit / 8;
        let bit_offset = start_bit % 8;

        // The sample can span up to two bytes.
        let buffer = (bytes[byte_index] as u16)
            | (bytes.get(byte_index + 1).copied().unwrap_or(0) as u16) << 8;
        let value = (buffer >> bit_offset) & 0x1f;

        peaks.push(value as f32 / 31.0);
    }

    peaks
}
