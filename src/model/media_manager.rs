//! The `MediaManager` is responsible for deciding whether media should be
//! downloaded automatically and for performing downloads requested by the
//! user (either manually or as a result of the auto-download settings).

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::config;
use crate::model;

/// The kind of media that can be downloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaType {
    /// A photo.
    Photo,
    /// A video or an animation (GIF).
    Video,
    /// A regular file.
    File,
}

/// The media download manager.
///
/// It holds a reference to the [`ClientStateSession`] so that it can download
/// files on demand. The auto-download decisions are based on the global
/// application settings and are evaluated on every call, so they always
/// reflect the current settings.
#[derive(Debug)]
pub(crate) struct MediaManager {
    session: glib::WeakRef<model::ClientStateSession>,
    settings: gio::Settings,
}

impl MediaManager {
    /// Creates a new `MediaManager` for the given session.
    pub(crate) fn new(session: &model::ClientStateSession) -> Self {
        let session_ref = glib::WeakRef::new();
        session_ref.set(Some(session));

        Self {
            session: session_ref,
            settings: gio::Settings::new(config::APP_ID),
        }
    }

    /// Returns whether media of the given `kind` and `size` should be
    /// downloaded automatically.
    ///
    /// When the master `auto-download-media` switch is disabled, nothing is
    /// downloaded automatically. When the per-content switch is disabled, or
    /// when the file is too large for the configured limit (videos and files
    /// only), the media is not downloaded automatically either.
    pub(crate) fn should_auto_download(&self, kind: MediaType, size: u64) -> bool {
        if !self.settings.boolean("auto-download-media") {
            return false;
        }

        match kind {
            MediaType::Photo => self.settings.boolean("auto-download-photos"),
            MediaType::Video => {
                if !self.settings.boolean("auto-download-videos") {
                    return false;
                }

                let max_size = self.settings.uint("auto-download-videos-max-size");
                size <= max_size as u64
            }
            MediaType::File => {
                if !self.settings.boolean("auto-download-files") {
                    return false;
                }

                let max_size = self.settings.uint("auto-download-files-max-size");
                size <= max_size as u64
            }
        }
    }

    /// Returns whether videos should be played automatically as soon as they
    /// become visible.
    ///
    /// This does not affect animations (GIFs), which are always played
    /// automatically when they are visible.
    pub(crate) fn should_autoplay_videos(&self) -> bool {
        self.settings.boolean("autoplay-videos")
    }

    /// Downloads the file of the given `id`, calling `f` every time there's an
    /// update about the progress or when the download has completed.
    ///
    /// This is used to download files requested by the user (e.g. when they
    /// click on a media preview that should not be downloaded automatically).
    pub(crate) fn download_file_with_updates<F: Fn(tdlib::types::File) + 'static>(
        &self,
        file_id: i32,
        f: F,
    ) {
        if let Some(session) = self.session.upgrade() {
            session.download_file_with_updates(file_id, f);
        }
    }

    /// Cancels the download of the file of the given `id`.
    pub(crate) fn cancel_download_file(&self, file_id: i32) {
        if let Some(session) = self.session.upgrade() {
            session.cancel_download_file(file_id);
        }
    }
}