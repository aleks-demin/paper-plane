use std::{
    cell::{Cell, RefCell},
    os::raw::{c_int, c_uint},
    path::PathBuf,
    ptr,
    sync::mpsc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use matroska_demuxer::{Frame, MatroskaFile, TrackType};

/// A single decoded video frame, ready to be turned into a
/// [`gdk::MemoryTexture`].
struct DecodedFrame {
    width: i32,
    height: i32,
    rgba: Vec<u8>,
}

/// The VP8 or VP9 decoder that backs a WebM animation.
///
/// Thin wrapper over `libvpx` that converts the decoded I420 frames
/// into RGBA buffers right away, since the buffers of `libvpx` are
/// reused between frames.
struct Decoder {
    ctx: Box<vpx_sys::vpx_codec_ctx>,
}

// The decoder context is only ever used from the thread that owns it,
// but a worker thread may be the owner, so it must be movable.
unsafe impl Send for Decoder {}

/// The codec of a WebM animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Vp8,
    Vp9,
}

impl Codec {
    fn from_codec_id(codec_id: &str) -> Option<Self> {
        match codec_id {
            "V_VP8" => Some(Self::Vp8),
            "V_VP9" => Some(Self::Vp9),
            _ => None,
        }
    }
}

impl Decoder {
    fn new(codec: Codec) -> Result<Self, String> {
        let iface = match codec {
            Codec::Vp8 => unsafe { vpx_sys::vpx_codec_vp8_dx() },
            Codec::Vp9 => unsafe { vpx_sys::vpx_codec_vp9_dx() },
        };

        let mut ctx = Box::new(vpx_sys::vpx_codec_ctx::default());
        let result = unsafe {
            vpx_sys::vpx_codec_dec_init_ver(
                &mut *ctx,
                iface,
                ptr::null(),
                0,
                VPX_DECODER_ABI_VERSION,
            )
        };
        if result != vpx_sys::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(error_message(result));
        }

        Ok(Self { ctx })
    }

    /// Decodes a compressed packet and returns the frames that became
    /// available because of it.
    fn decode(&mut self, data: &[u8]) -> Result<Vec<DecodedFrame>, String> {
        let result = unsafe {
            vpx_sys::vpx_codec_decode(
                &mut *self.ctx,
                data.as_ptr(),
                data.len() as c_uint,
                ptr::null_mut(),
                0,
            )
        };
        if result != vpx_sys::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(error_message(result));
        }

        Ok(self.take_frames())
    }

    /// Signals the end of the stream and returns the frames that were
    /// still buffered.
    fn flush(&mut self) -> Vec<DecodedFrame> {
        let result = unsafe {
            vpx_sys::vpx_codec_decode(&mut *self.ctx, ptr::null(), 0, ptr::null_mut(), 0)
        };

        if result != vpx_sys::vpx_codec_err_t::VPX_CODEC_OK {
            log::warn!("Failed to flush a WebM decoder: {}", error_message(result));
            return Vec::new();
        }

        self.take_frames()
    }

    fn take_frames(&mut self) -> Vec<DecodedFrame> {
        let mut iter = ptr::null();
        let mut frames = Vec::new();

        loop {
            let image = unsafe { vpx_sys::vpx_codec_get_frame(&mut *self.ctx, &mut iter) };
            if image.is_null() {
                break;
            }

            match unsafe { convert_image(&*image) } {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => {}
                Err(e) => log::warn!("Skipping an unsupported WebM frame: {e}"),
            }
        }

        frames
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let result = unsafe { vpx_sys::vpx_codec_destroy(&mut *self.ctx) };
        if result != vpx_sys::vpx_codec_err_t::VPX_CODEC_OK {
            log::warn!(
                "Failed to destroy a WebM decoder: {}",
                error_message(result)
            );
        }
    }
}

/// Converts a decoded I420 image to an RGBA buffer.
unsafe fn convert_image(image: &vpx_sys::vpx_image) -> Result<Option<DecodedFrame>, String> {
    if image.fmt != vpx_sys::vpx_img_fmt::VPX_IMG_FMT_I420 {
        // High bit depth stickers are not known to exist in the wild.
        return Ok(None);
    }

    let width = image.d_w as usize;
    let height = image.d_h as usize;
    let y_plane = image.planes[0] as *const u8;
    let u_plane = image.planes[1] as *const u8;
    let v_plane = image.planes[2] as *const u8;
    let y_stride = image.stride[0] as usize;
    let u_stride = image.stride[1] as usize;
    let v_stride = image.stride[2] as usize;

    let mut rgba = vec![0_u8; width * height * 4];

    for row in 0..height {
        let y_row = std::slice::from_raw_parts(y_plane.add(row * y_stride), width);
        let u_row = std::slice::from_raw_parts(u_plane.add(row / 2 * u_stride), width / 2 + 1);
        let v_row = std::slice::from_raw_parts(v_plane.add(row / 2 * v_stride), width / 2 + 1);

        for (pixel, col) in rgba[row * width * 4..(row + 1) * width * 4]
            .chunks_exact_mut(4)
            .zip(0..width)
        {
            let y = y_row[col] as i32;
            let u = u_row[col / 2] as i32 - 128;
            let v = v_row[col / 2] as i32 - 128;

            let (r, g, b) = (
                y + ((11_882 * v) >> 16),
                y - ((6_081 * u) >> 16) - ((12_396 * v) >> 16),
                y + ((30_807 * u) >> 16),
            );

            pixel[0] = r.clamp(0, 255) as u8;
            pixel[1] = g.clamp(0, 255) as u8;
            pixel[2] = b.clamp(0, 255) as u8;
            pixel[3] = 255;
        }
    }

    Ok(Some(DecodedFrame {
        width: width as i32,
        height: height as i32,
        rgba,
    }))
}

fn error_message(result: vpx_sys::vpx_codec_err_t) -> String {
    unsafe {
        std::ffi::CStr::from_ptr(vpx_sys::vpx_codec_err_to_string(result))
            .to_string_lossy()
            .into_owned()
    }
}

// 3 + VPX_CODEC_ABI_VERSION (4) + VPX_IMAGE_ABI_VERSION (5)
const VPX_DECODER_ABI_VERSION: c_int = 12;

/// A message from a worker thread to its widget.
enum WorkerMessage {
    Frame {
        width: i32,
        height: i32,
        rgba: Vec<u8>,
    },
    Done,
    Failed(String),
}

/// The commands that a widget sends to its worker thread.
enum WorkerCommand {
    Pause,
    Resume,
}

fn run_worker(
    path: PathBuf,
    looped: bool,
    commands: mpsc::Receiver<WorkerCommand>,
    sender: async_channel::Sender<WorkerMessage>,
) {
    let send_failed = |error: String| -> bool {
        // Sending fails when the widget is gone, in which case the
        // worker should exit anyway.
        sender.send_blocking(WorkerMessage::Failed(error)).is_ok()
    };

    loop {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) => {
                send_failed(format!("Failed to open a WebM animation: {e:?}"));
                return;
            }
        };

        let mut mkv = match MatroskaFile::open(file) {
            Ok(mkv) => mkv,
            Err(e) => {
                send_failed(format!("Failed to demux a WebM animation: {e:?}"));
                return;
            }
        };

        let Some((video_track, codec)) = mkv
            .tracks()
            .iter()
            .find(|t| t.track_type() == TrackType::Video)
            .and_then(|t| {
                Codec::from_codec_id(t.codec_id()).map(|codec| (t.track_number().get(), codec))
            })
        else {
            send_failed("WebM animation has no supported video track".to_owned());
            return;
        };

        let mut decoder = match Decoder::new(codec) {
            Ok(decoder) => decoder,
            Err(e) => {
                send_failed(format!("Failed to create a WebM decoder: {e}"));
                return;
            }
        };

        let mut frame = Frame::default();
        let mut previous_timestamp: Option<u64> = None;
        let mut previous_presented: Option<Instant> = None;

        loop {
            // Handle the commands of the widget, if there are any. The
            // thread is left through the `return`s when the widget is
            // gone and the sender was dropped.
            match commands.try_recv() {
                Ok(WorkerCommand::Pause) => {
                    while let Ok(command) = commands.recv() {
                        if matches!(command, WorkerCommand::Resume) {
                            // The timing of the animation is relative,
                            // so it just continues where it left off.
                            previous_timestamp = None;
                            previous_presented = None;
                            break;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                _ => {}
            }

            match mkv.next_frame(&mut frame) {
                Ok(true) => {}
                Ok(false) => {
                    // The last frames may still be buffered in the
                    // decoder, so it must be flushed. They are presented
                    // right away, since their timing information is
                    // already spent.
                    for decoded in decoder.flush() {
                        if sender
                            .send_blocking(WorkerMessage::Frame {
                                width: decoded.width,
                                height: decoded.height,
                                rgba: decoded.rgba,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    break;
                }
                Err(e) => {
                    send_failed(format!("Failed to demux a WebM frame: {e:?}"));
                    return;
                }
            }

            if frame.track != video_track || frame.is_invisible {
                continue;
            }

            let decoded = match decoder.decode(&frame.data) {
                Ok(decoded) => decoded,
                Err(e) => {
                    send_failed(format!("Failed to decode a WebM frame: {e}"));
                    return;
                }
            };

            if decoded.is_empty() {
                continue;
            }

            // Wait until the frame is due to be presented. Decoding and
            // conversion time is taken into account, so that the
            // animation does not run slower than intended.
            if let (Some(timestamp), Some(presented)) = (previous_timestamp, previous_presented) {
                let due =
                    presented + Duration::from_millis(frame.timestamp.saturating_sub(timestamp));
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
            }
            previous_timestamp = Some(frame.timestamp);
            previous_presented = Some(Instant::now());

            for decoded in decoded {
                if sender
                    .send_blocking(WorkerMessage::Frame {
                        width: decoded.width,
                        height: decoded.height,
                        rgba: decoded.rgba,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }

        if !looped {
            break;
        }
    }

    sender.send_blocking(WorkerMessage::Done).ok();
}

mod imp {
    use super::*;
    use std::sync::OnceLock;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::WebmAnimation)]
    pub(crate) struct WebmAnimation {
        #[property(get, set, construct_only)]
        path: RefCell<String>,
        #[property(get, construct_only)]
        looped: Cell<bool>,
        #[property(get)]
        pub(super) is_playing: Cell<bool>,
        pub(super) picture: OnceLock<gtk::Picture>,
        pub(super) commands: RefCell<Option<mpsc::Sender<WorkerCommand>>>,
        pub(super) failed: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WebmAnimation {
        const NAME: &'static str = "PaplWebmAnimation";
        type Type = super::WebmAnimation;
        type ParentType = adw::Bin;
    }

    #[glib::derived_properties]
    impl ObjectImpl for WebmAnimation {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("error").build()])
        }

        fn constructed(&self) {
            self.parent_constructed();

            let picture = gtk::Picture::new();
            self.obj().set_child(Some(&picture));
            self.picture.set(picture).unwrap();
        }

        fn dispose(&self) {
            // Terminates the worker thread, if there is one.
            self.commands.replace(None);
        }
    }

    impl WidgetImpl for WebmAnimation {
        fn map(&self) {
            self.parent_map();

            let obj = self.obj();

            if self.commands.borrow().is_none() {
                if !self.failed.get() {
                    obj.start();
                }
            } else {
                obj.set_is_playing(true);
                if let Some(commands) = self.commands.borrow().as_ref() {
                    commands.send(WorkerCommand::Resume).ok();
                }
            }
        }

        fn unmap(&self) {
            self.parent_unmap();

            let obj = self.obj();
            obj.set_is_playing(false);
            if let Some(commands) = self.commands.borrow().as_ref() {
                commands.send(WorkerCommand::Pause).ok();
            }
        }
    }

    impl BinImpl for WebmAnimation {}
}

glib::wrapper! {
    pub(crate) struct WebmAnimation(ObjectSubclass<imp::WebmAnimation>)
        @extends gtk::Widget, adw::Bin;
}

impl WebmAnimation {
    pub(crate) fn new(path: &str, looped: bool) -> Self {
        glib::Object::builder()
            .property("path", path)
            .property("looped", looped)
            .build()
    }

    /// Restarts the animation from the beginning.
    pub(crate) fn replay(&self) {
        if self.imp().failed.get() {
            return;
        }

        self.imp().commands.replace(None);
        self.start();
    }

    /// Connects to the `error` signal, which is emitted when the
    /// animation failed to play.
    pub(crate) fn connect_error<F: Fn(&Self) + 'static>(&self, f: F) {
        self.connect_closure(
            "error",
            false,
            glib::RustClosure::new_local(move |values| {
                let obj = values[0].get::<Self>().unwrap();
                f(&obj);
                None
            }),
        );
    }

    fn start(&self) {
        let imp = self.imp();

        let (commands, command_receiver) = mpsc::channel();
        imp.commands.replace(Some(commands));

        let (sender, receiver) = async_channel::unbounded::<WorkerMessage>();
        let path = PathBuf::from(self.path());
        let looped = self.looped();

        std::thread::Builder::new()
            .name("webm-animation".to_owned())
            .spawn(move || run_worker(path, looped, command_receiver, sender))
            .expect("failed to spawn a WebM animation thread");

        let obj = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let Some(obj) = obj.upgrade() else {
                    break;
                };

                match message {
                    WorkerMessage::Frame {
                        width,
                        height,
                        rgba,
                    } => {
                        let texture = gdk::MemoryTexture::new(
                            width,
                            height,
                            gdk::MemoryFormat::R8g8b8a8Premultiplied,
                            &glib::Bytes::from_owned(rgba),
                            width as usize * 4,
                        );
                        obj.imp()
                            .picture
                            .get()
                            .unwrap()
                            .set_paintable(Some(&texture));
                    }
                    WorkerMessage::Done => {
                        // Keep the last frame on screen.
                        obj.set_is_playing(false);
                        break;
                    }
                    WorkerMessage::Failed(error) => {
                        log::warn!("WebM animation failed: {error}");
                        obj.imp().failed.set(true);
                        obj.imp().commands.replace(None);
                        obj.set_is_playing(false);
                        obj.emit_by_name::<()>("error", &[]);
                        break;
                    }
                }
            }
        });
    }

    fn set_is_playing(&self, is_playing: bool) {
        self.imp().is_playing.set(is_playing);
        self.notify("is-playing");
    }
}
