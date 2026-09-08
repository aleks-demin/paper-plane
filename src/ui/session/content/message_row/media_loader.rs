use std::cell::Cell;
use std::cell::RefCell;

use glib::clone;
use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;
use crate::model::MediaType;

use super::FileStatus;

mod imp {
    use super::*;

    #[derive(Debug, Properties, Default)]
    #[properties(wrapper_type = super::MediaLoader)]
    pub(crate) struct MediaLoader {
        /// The session used to download the bound file.
        pub(super) session: glib::WeakRef<model::ClientStateSession>,
        /// The TDLib id of the bound file.
        pub(super) file_id: Cell<i32>,
        /// The size of the bound file in bytes.
        #[property(get)]
        pub(super) size: Cell<u64>,
        /// The current status of the bound file.
        #[property(get, explicit_notify)]
        pub(super) status: Cell<FileStatus>,
        /// The local path of the bound file, if it has been downloaded.
        #[property(get)]
        pub(super) path: RefCell<Option<glib::GString>>,
        /// The most recent state of the bound file, as reported by TDLib.
        ///
        /// This is more recent than the file snapshot of the message
        /// content, which is not updated when the file is downloaded.
        pub(super) file: RefCell<Option<tdlib::types::File>>,
        /// Whether the ongoing download was started automatically according
        /// to the auto-download settings instead of a user action.
        #[property(get)]
        pub(super) is_auto: Cell<bool>,
        /// Whether the bound file should be downloaded automatically
        /// according to the auto-download settings, but the download has not
        /// been started yet.
        ///
        /// Downloads are not started by [`MediaLoader::bind`] itself, but
        /// when the widget showing the file is mapped, so that media that is
        /// only scrolled past is not downloaded. See
        /// [`MediaLoader::maybe_start_auto_download`].
        pub(super) auto_download_pending: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaLoader {
        const NAME: &'static str = "MediaLoader";
        type Type = super::MediaLoader;
    }

    impl ObjectImpl for MediaLoader {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn set_property(&self, id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            self.derived_set_property(id, value, pspec)
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaLoader(ObjectSubclass<imp::MediaLoader>);
}

impl Default for MediaLoader {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// The download state machine of a single media file.
///
/// A `MediaLoader` is bound to a TDLib file and exposes its download status
/// as a property, so that widgets can watch it. The actual download is
/// performed through the `MediaManager` of the session, which multiplexes
/// downloads that are requested by several widgets at once.
impl MediaLoader {
    /// Binds this loader to the given file, resetting all previous state.
    ///
    /// If the file has already been downloaded, its path becomes available
    /// and the status is set to [`FileStatus::Downloaded`]. Otherwise, the
    /// auto-download settings are checked: if they allow it, the download is
    /// marked as pending and is started by
    /// [`Self::maybe_start_auto_download`]; if not, the status stays at
    /// [`FileStatus::CanBeDownloaded`] until [`Self::request_download`] is
    /// called by the user.
    pub(crate) fn bind(
        &self,
        session: &model::ClientStateSession,
        media_type: MediaType,
        file: &tdlib::types::File,
    ) {
        let imp = self.imp();

        imp.session.set(Some(session));
        imp.file_id.set(file.id);
        imp.size.set(file.size.max(file.expected_size) as u64);
        imp.is_auto.set(false);
        imp.auto_download_pending.set(false);
        *imp.path.borrow_mut() = None;
        *imp.file.borrow_mut() = Some(file.clone());

        match FileStatus::from(file) {
            FileStatus::Downloaded => {
                *imp.path.borrow_mut() = Some(file.local.path.clone().into());
                self.set_status(FileStatus::Downloaded);
            }
            // Uploads are driven by the sender side, so there is nothing to
            // do here but reflect the progress.
            FileStatus::Uploading(_) => self.set_status(FileStatus::from(file)),
            FileStatus::Downloading(_) | FileStatus::CanBeDownloaded => {
                let size = imp.size.get();

                if session
                    .media_manager()
                    .should_auto_download(media_type, size)
                {
                    imp.auto_download_pending.set(true);
                }

                self.set_status(FileStatus::CanBeDownloaded);
            }
        }
    }

    /// Starts the download of the bound file, if it should be downloaded
    /// automatically and the download has not been started yet.
    ///
    /// This is called when the widget showing the file is mapped, so that
    /// media that is only scrolled past is not downloaded.
    pub(crate) fn maybe_start_auto_download(&self) {
        if !self.imp().auto_download_pending.take() {
            return;
        }

        if self.imp().status.get() != FileStatus::CanBeDownloaded {
            return;
        }

        self.start_download(true);
    }

    /// Starts downloading the bound file as a result of a user action.
    ///
    /// The progress is reported through the `status` property until the file
    /// is fully downloaded or the download is canceled with
    /// [`Self::cancel_download`].
    pub(crate) fn request_download(&self) {
        self.start_download(false);
    }

    /// Cancels the download of the bound file.
    pub(crate) fn cancel_download(&self) {
        let imp = self.imp();

        if let Some(session) = imp.session.upgrade() {
            session
                .media_manager()
                .cancel_download_file(imp.file_id.get());
        }

        self.set_status(FileStatus::CanBeDownloaded);
    }

    /// Returns the session this loader is bound to, if it is still alive.
    pub(crate) fn session(&self) -> Option<model::ClientStateSession> {
        self.imp().session.upgrade()
    }

    /// Returns the most recent state of the bound file, as reported by
    /// TDLib.
    pub(crate) fn file(&self) -> Option<tdlib::types::File> {
        self.imp().file.borrow().clone()
    }

    fn start_download(&self, auto: bool) {
        let imp = self.imp();
        let Some(session) = imp.session.upgrade() else {
            return;
        };

        imp.is_auto.set(auto);
        self.set_status(FileStatus::Downloading(0.0));

        session.media_manager().download_file_with_updates(
            imp.file_id.get(),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |file| {
                    obj.handle_file_update(file);
                }
            ),
        );
    }

    fn handle_file_update(&self, file: tdlib::types::File) {
        let imp = self.imp();

        // The loader may have been rebound to another file while an old
        // subscription was still active.
        if imp.file_id.get() != file.id {
            return;
        }

        if file.local.is_downloading_completed {
            *imp.path.borrow_mut() = Some(file.local.path.clone().into());
        }

        *imp.file.borrow_mut() = Some(file.clone());

        self.set_status(FileStatus::from(&file));
    }

    fn set_status(&self, status: FileStatus) {
        let imp = self.imp();

        if imp.status.get() == status {
            return;
        }
        imp.status.set(status);
        self.notify("status");
    }
}
