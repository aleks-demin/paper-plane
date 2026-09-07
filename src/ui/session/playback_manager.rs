//! A shared playback engine that owns a single `gtk::MediaFile` per session.
//!
//! Widgets that play media (the media viewer, and in the future audio
//! message rows) borrow the stream of this manager instead of creating
//! their own, so that at most one media pipeline exists per session.
//!
//! The `MediaFile` is exposed to the clients for read-only inspection
//! (`is_playing()`, `position()`, `duration()`) and for binding it to
//! render targets like `gtk::Video` or `gtk::MediaControls`, as well as
//! for pre-load configuration (`set_muted()`, `set_loop()`). Its source
//! and lifecycle, however, are owned by the manager: clients must not
//! call `set_file()`, `set_filename()`, `clear()`, `play()`, `pause()`
//! or connect their own `prepared`/`error` handlers. Playback control
//! goes through the methods of the manager, and the per-playback hooks
//! passed to `play_file` replace signal handlers.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// The hook that is called when the stream of the current playback is
/// prepared.
type OnPrepared = Box<dyn Fn(&gtk::MediaFile)>;

struct Inner {
    /// The shared media stream. Created empty; its source is set by
    /// `play_file`, so no pipeline exists while nothing plays.
    media_file: gtk::MediaFile,
    /// The path of the currently loaded source, if any.
    current_path: RefCell<Option<glib::GString>>,
    /// Resets the UI of the widget that owns the current playback. Run
    /// when another widget takes over or the playback is stopped.
    on_reset_previous: RefCell<Option<Box<dyn Fn()>>>,
    /// Called when the stream of the current playback is prepared, if it
    /// is still current. Set by the widget that started the playback.
    on_prepared: RefCell<Option<OnPrepared>>,
    /// Called when the playback of the current source fails. Set by the
    /// widget that started the playback.
    on_error: RefCell<Option<Box<dyn Fn()>>>,
    /// The `notify::playing` handlers of the widget that owns the current
    /// playback. Disconnected when the playback is taken over or stopped.
    playing_handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for id in self.playing_handlers.borrow_mut().drain(..) {
            self.media_file.disconnect(id);
        }
    }
}

/// A cloneable handle to the shared playback engine of a session.
#[derive(Clone)]
pub(crate) struct PlaybackManager(Rc<Inner>);

impl fmt::Debug for PlaybackManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PlaybackManager").finish()
    }
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackManager {
    pub(crate) fn new() -> Self {
        let media_file = gtk::MediaFile::new();

        let inner = Rc::new(Inner {
            media_file: media_file.clone(),
            current_path: RefCell::new(None),
            on_reset_previous: RefCell::new(None),
            on_prepared: RefCell::new(None),
            on_error: RefCell::new(None),
            playing_handlers: RefCell::new(Vec::new()),
        });

        // The handlers are connected once and dispatch to the hooks of
        // whichever widget owns the current playback.
        media_file.connect_notify_local(
            Some("prepared"),
            glib::clone!(
                #[strong]
                inner,
                move |media: &gtk::MediaFile, _| {
                    let on_prepared = inner.on_prepared.borrow();
                    if media.is_prepared() {
                        if let Some(on_prepared) = on_prepared.as_ref() {
                            on_prepared(media);
                        }
                    }
                }
            ),
        );
        media_file.connect_notify_local(
            Some("error"),
            glib::clone!(
                #[strong]
                inner,
                move |media: &gtk::MediaFile, _| {
                    if let Some(error) = media.error() {
                        log::warn!("Media playback failed: {error:?}");

                        if let Some(on_error) = inner.on_error.borrow().as_ref() {
                            on_error();
                        }
                    }
                }
            ),
        );

        Self(inner)
    }

    /// The shared media stream, for inspection, binding it to a render
    /// target and pre-load configuration. See the module docs for what
    /// clients must not do with it.
    pub(crate) fn media_file(&self) -> gtk::MediaFile {
        self.0.media_file.clone()
    }

    /// Whether the stream is currently loaded from `path`.
    //
    // Part of the API for the future audio message widgets; only the
    // media viewer plays media for now.
    #[allow(dead_code)]
    pub(crate) fn is_current(&self, path: &str) -> bool {
        self.0
            .current_path
            .borrow()
            .as_ref()
            .is_some_and(|current| current == path)
    }

    /// Plays the file at `path`, taking the stream over from the previous
    /// playback.
    ///
    /// Before calling this method, the stream may be configured via
    /// `media_file()` (e.g. `set_muted` / `set_loop`), which applies to
    /// the upcoming source.
    ///
    /// The previous playback is stopped: its reset callback is run and
    /// its handlers are disconnected, so that the widget that owned it
    /// can restore its UI.
    ///
    /// `on_prepared` is called when the stream is prepared and its first
    /// frame is available; the playback is *not* started automatically,
    /// the hook decides what to do with the prepared stream. If the
    /// stream is already prepared (e.g. the same source is reattached),
    /// it is called directly, since `notify::prepared` won't fire again.
    ///
    /// `on_error` is called if the playback fails. Note that it can also
    /// be called while another widget owns the stream, so it must not
    /// assume the error belongs to this playback.
    pub(crate) fn play_file<R, P, E>(&self, path: &str, on_reset: R, on_prepared: P, on_error: E)
    where
        R: Fn() + 'static,
        P: Fn(&gtk::MediaFile) + 'static,
        E: Fn() + 'static,
    {
        // 1. Stop the previous playback and reset its owner's UI.
        if let Some(on_reset_previous) = self.0.on_reset_previous.take() {
            on_reset_previous();
        }
        let old_handlers = std::mem::take(&mut *self.0.playing_handlers.borrow_mut());
        for id in old_handlers {
            self.0.media_file.disconnect(id);
        }

        // 2. Register the new playback.
        *self.0.on_reset_previous.borrow_mut() = Some(Box::new(on_reset));
        *self.0.on_prepared.borrow_mut() = Some(Box::new(on_prepared));
        *self.0.on_error.borrow_mut() = Some(Box::new(on_error));
        *self.0.current_path.borrow_mut() = Some(glib::GString::from(path));

        // 3. Load the new source. The pipeline of the previous source is
        // freed by clearing the stream before the new one is loaded.
        let media = &self.0.media_file;
        media.pause();
        media.clear();
        media.set_filename(Some(path));

        if media.is_prepared() {
            // The stream was already prepared (e.g. the same source is
            // reattached after a clear didn't change the file), so the
            // `prepared` notification won't fire again.
            let on_prepared = self.0.on_prepared.borrow();
            if let Some(on_prepared) = on_prepared.as_ref() {
                on_prepared(media);
            }
        }
    }

    /// Connects `on_playing` to the `playing` state of the stream on
    /// behalf of the current playback, replacing the connection of the
    /// previous one.
    ///
    /// Must be called by the widget that started the current playback,
    /// ideally right before or right after `play_file`, so that its
    /// connection doesn't get disconnected by a subsequent call.
    //
    // Part of the API for the future audio message widgets; only the
    // media viewer plays media for now.
    #[allow(dead_code)]
    pub(crate) fn connect_playing<F: Fn(&gtk::MediaFile) + 'static>(&self, on_playing: F) {
        let id = self.0.media_file.connect_notify_local(
            Some("playing"),
            move |media: &gtk::MediaFile, _| on_playing(media),
        );
        self.0.playing_handlers.borrow_mut().push(id);
    }

    /// Resumes or pauses the current playback.
    //
    // Part of the API for the future audio message widgets; only the
    // media viewer plays media for now.
    #[allow(dead_code)]
    pub(crate) fn toggle_pause(&self) {
        let media = &self.0.media_file;
        media.set_playing(!media.is_playing());
    }

    /// Stops the current playback and closes the stream, freeing the
    /// resources of the underlying pipeline, and resets the UI of the
    /// widget that owned it.
    ///
    /// Closing the stream is essential: merely pausing it would leave a
    /// full media pipeline behind, whose teardown happens synchronously
    /// on the main thread when the stream is eventually dropped. If such
    /// a leftover pipeline got stuck, the whole UI would freeze.
    pub(crate) fn stop(&self) {
        if let Some(on_reset_previous) = self.0.on_reset_previous.take() {
            on_reset_previous();
        }

        let old_handlers = std::mem::take(&mut *self.0.playing_handlers.borrow_mut());
        for id in old_handlers {
            self.0.media_file.disconnect(id);
        }

        *self.0.on_prepared.borrow_mut() = None;
        *self.0.on_error.borrow_mut() = None;
        *self.0.current_path.borrow_mut() = None;

        self.0.media_file.pause();
        self.0.media_file.clear();
    }
}
