use std::cell::Cell;
use std::cell::RefCell;
use std::sync::OnceLock;

use glib::clone;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use ui::MessageBaseExt;

use crate::model;
use crate::model::MediaType;
use crate::ui;
use crate::ui::session::playback_manager;
use crate::utils;

use super::FileStatus;
use super::MediaDownloadButton;
use super::MediaLoader;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/voice.ui")]
    pub(crate) struct MessageVoiceNote {
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) loader_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        /// The handlers of the shared media stream that drive the waveform
        /// progress. Disconnected when the playback is taken over or stopped.
        pub(super) progress_handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
        /// Whether the playback should start once the download of the voice
        /// note finishes.
        pub(super) play_when_downloaded: Cell<bool>,
        /// The shared playback engine used to play the voice note.
        pub(super) playback: RefCell<Option<playback_manager::PlaybackManager>>,
        pub(super) loader: MediaLoader,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) waveform: TemplateChild<ui::Waveform>,
        #[template_child]
        pub(super) duration_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) play_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) download_button: TemplateChild<MediaDownloadButton>,
        #[template_child]
        pub(super) click: TemplateChild<gtk::GestureClick>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageVoiceNote {
        const NAME: &'static str = "PaplMessageVoiceNote";
        type Type = super::MessageVoiceNote;
        type ParentType = ui::MessageBase;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_css_name("messagevoice");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageVoiceNote {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecObject::builder::<model::Message>("message")
                    .explicit_notify()
                    .build()]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "message" => self.obj().set_message(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "message" => self.message.upgrade().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            let handler_id = self.loader.connect_status_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.update_status_ui();
                }
            ));
            *self.loader_handler_id.borrow_mut() = Some(handler_id);

            self.click.connect_released(clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.handle_click();
                }
            ));
        }

        fn dispose(&self) {
            let obj = self.obj();

            if let Some(handler_id) = self.loader_handler_id.borrow_mut().take() {
                self.loader.disconnect(handler_id);
            }

            if let (Some(manager), Some(path)) = (obj.playback_manager(), obj.media_path()) {
                if manager.is_current(&path) {
                    manager.stop();
                }
            }
        }
    }

    impl WidgetImpl for MessageVoiceNote {}
    impl ui::MessageBaseImpl for MessageVoiceNote {}
}

glib::wrapper! {
    pub(crate) struct MessageVoiceNote(ObjectSubclass<imp::MessageVoiceNote>)
        @extends gtk::Widget, ui::MessageBase;
}

impl MessageBaseExt for MessageVoiceNote {
    type Message = model::Message;

    fn set_message(&self, message: &Self::Message) {
        let imp = self.imp();

        let old_message = imp.message.upgrade();
        if old_message.as_ref() == Some(message) {
            return;
        }

        if let Some(old_message) = old_message {
            let handler_id = imp.handler_id.take().unwrap();
            old_message.disconnect(handler_id);
        }

        imp.message_bubble.update_from_message(message, true);
        imp.message_bubble.add_message_label_class("caption");
        imp.message_bubble.add_message_label_class("dim-label");

        // Update the message.
        let handler_id = message.connect_content_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |message| {
                obj.update_row(message);
            }
        ));
        imp.handler_id.replace(Some(handler_id));
        self.update_row(message);

        imp.message.set(Some(message));
        self.notify("message");
    }
}

impl MessageVoiceNote {
    /// The shared playback engine of the session, if it is still alive.
    fn playback_manager(&self) -> Option<playback_manager::PlaybackManager> {
        self.ancestor(ui::Session::static_type())
            .and_downcast::<ui::Session>()
            .map(|session| session.playback_manager().clone())
    }

    /// The path of the voice note, if it has been downloaded.
    fn media_path(&self) -> Option<glib::GString> {
        if self.imp().loader.status() != FileStatus::Downloaded {
            return None;
        }

        self.imp().loader.path()
    }

    fn update_row(&self, message: &model::Message) {
        match message.content().0 {
            tdlib::enums::MessageContent::MessageVoiceNote(td_message) => {
                let imp = self.imp();

                let voice_note = td_message.voice_note;

                imp.waveform.set_waveform(&voice_note.waveform);
                imp.duration_label
                    .set_text(&format_duration(voice_note.duration));
                imp.message_bubble
                    .set_label(utils::parse_formatted_text(td_message.caption));

                imp.play_when_downloaded.set(false);
                imp.loader.bind(
                    &message.chat_().session_(),
                    MediaType::File,
                    &voice_note.voice,
                );
                imp.loader.maybe_start_auto_download();

                self.update_status_ui();
            }
            _ => unreachable!(),
        }
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status_ui(&self) {
        let imp = self.imp();
        let status = imp.loader.status();

        match status {
            FileStatus::Downloaded => {
                imp.download_button.set_visible(false);

                if imp.play_when_downloaded.take() {
                    self.toggle_playback();
                }
            }
            // Uploads are driven by the sender side, so there is nothing to
            // react to.
            FileStatus::Uploading(_) => imp.download_button.set_visible(false),
            _ => {
                imp.download_button.set_visible(true);
                imp.download_button.set_status(status);
            }
        }
    }

    fn handle_click(&self) {
        let imp = self.imp();

        match imp.loader.status() {
            // Clicking again cancels the download.
            FileStatus::Downloading(_) => imp.loader.cancel_download(),
            // Download the voice note and start playing it as soon as it is
            // done.
            FileStatus::CanBeDownloaded => {
                imp.play_when_downloaded.set(true);
                imp.loader.request_download();
            }
            // Uploads are driven by the sender side, so there is nothing to
            // react to.
            FileStatus::Uploading(_) => {}
            FileStatus::Downloaded => self.toggle_playback(),
        }
    }

    /// Plays or pauses the voice note on the shared media stream of the
    /// session.
    fn toggle_playback(&self) {
        let imp = self.imp();

        let Some(manager) = self.playback_manager() else {
            return;
        };
        let Some(path) = self.media_path() else {
            return;
        };

        *imp.playback.borrow_mut() = Some(manager.clone());

        if manager.is_current(&path) {
            manager.toggle_pause();
            return;
        }

        let media = manager.media_file();
        media.set_muted(false);
        media.set_loop(false);

        manager.play_file(
            &path,
            clone!(
                #[weak(rename_to = obj)]
                self,
                move || {
                    obj.reset_playback_ui();
                }
            ),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |media: &gtk::MediaFile| {
                    obj.update_waveform_progress(media);
                    media.play();
                }
            ),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move || {
                    obj.reset_playback_ui();
                }
            ),
        );

        manager.connect_playing(clone!(
            #[weak(rename_to = obj)]
            self,
            move |media: &gtk::MediaFile| {
                obj.update_play_icon(media.is_playing());
            }
        ));
        self.connect_progress_handlers(&manager);

        // The stream is paused until it is prepared, so the icon reflects
        // the upcoming playback rather than the current state.
        self.update_play_icon(true);
    }

    /// Connects the handlers that keep the waveform progress in sync with
    /// the position of the shared media stream.
    ///
    /// The handlers stay connected as long as this row owns the playback;
    /// they are disconnected when it is taken over or stopped.
    fn connect_progress_handlers(&self, manager: &playback_manager::PlaybackManager) {
        let imp = self.imp();
        let media = manager.media_file();

        let timestamp_id = media.connect_notify_local(
            Some("timestamp"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |media: &gtk::MediaFile, _| obj.update_waveform_progress(media),
            ),
        );
        let duration_id = media.connect_notify_local(
            Some("duration"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |media: &gtk::MediaFile, _| obj.update_waveform_progress(media),
            ),
        );

        imp.progress_handler_ids
            .borrow_mut()
            .extend([timestamp_id, duration_id]);
    }

    /// Updates the waveform progress according to the position of the
    /// shared media stream.
    fn update_waveform_progress(&self, media: &gtk::MediaFile) {
        let duration = media.duration();
        if duration > 0 {
            self.imp()
                .waveform
                .set_progress(media.timestamp() as f64 / duration as f64);
        }
    }

    /// Disconnects the progress handlers of the shared media stream.
    fn disconnect_progress_handlers(&self) {
        let imp = self.imp();

        let ids: Vec<_> = imp.progress_handler_ids.borrow_mut().drain(..).collect();
        if let Some(manager) = imp.playback.borrow().clone() {
            let media = manager.media_file();
            for id in ids {
                media.disconnect(id);
            }
        }
    }

    /// Restores the play icon and the waveform progress.
    ///
    /// This is called when another playback takes over the shared stream,
    /// or when the playback of this voice note is stopped or fails.
    fn reset_playback_ui(&self) {
        let imp = self.imp();

        self.disconnect_progress_handlers();

        imp.play_image
            .set_icon_name(Some("media-playback-start-symbolic"));
        imp.waveform.set_progress(0.0);
    }

    fn update_play_icon(&self, is_playing: bool) {
        let icon_name = if is_playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };

        self.imp().play_image.set_icon_name(Some(icon_name));
    }
}

fn format_duration(seconds: i32) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;

    format!("{minutes:02}:{seconds:02}")
}
