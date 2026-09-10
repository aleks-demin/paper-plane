mod status_indicator;

use std::cell::Cell;
use std::cell::RefCell;
use std::sync::OnceLock;

use glib::clone;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

pub(crate) use self::status_indicator::StatusIndicator;
pub(crate) use super::file_status::FileStatus;
use crate::model;
use crate::model::MediaType;
use crate::ui;
use crate::ui::MessageBaseExt;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/document/mod.ui")]
    pub(crate) struct MessageDocument {
        pub(super) bindings: RefCell<Vec<gtk::ExpressionWatch>>,
        pub(super) handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) click_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) loader_handler_id: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) message: glib::WeakRef<model::Message>,
        /// The file id of the thumbnail that should be downloaded once the
        /// widget is shown, or 0 if there is none.
        pub(super) pending_thumbnail_file_id: Cell<i32>,
        pub(super) loader: super::super::MediaLoader,
        #[template_child]
        pub(super) message_bubble: TemplateChild<ui::MessageBubble>,
        #[template_child]
        pub(super) file_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) click: TemplateChild<gtk::GestureClick>,
        #[template_child]
        pub(super) file_thumbnail_picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub(super) status_indicator: TemplateChild<ui::MessageDocumentStatusIndicator>,
        #[template_child]
        pub(super) file_name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) file_size_label: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageDocument {
        const NAME: &'static str = "PaplMessageDocument";
        type Type = super::MessageDocument;
        type ParentType = ui::MessageBase;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageDocument {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecObject::builder::<glib::Object>("message")
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

        fn constructed(&self) {
            self.parent_constructed();

            let handler_id = self.loader.connect_status_notify(clone!(
                #[weak(rename_to = obj)]
                self.obj(),
                move |_| {
                    obj.update_status_ui();
                }
            ));
            *self.loader_handler_id.borrow_mut() = Some(handler_id);
        }

        fn dispose(&self) {
            if let Some(handler_id) = self.loader_handler_id.borrow_mut().take() {
                self.loader.disconnect(handler_id);
            }
        }
    }

    impl WidgetImpl for MessageDocument {
        fn map(&self) {
            self.parent_map();

            self.obj().maybe_download_thumbnail();
        }
    }
    impl ui::MessageBaseImpl for MessageDocument {}
}

glib::wrapper! {
    pub(crate) struct MessageDocument(ObjectSubclass<imp::MessageDocument>)
        @extends gtk::Widget, ui::MessageBase;
}

impl ui::MessageBaseExt for MessageDocument {
    type Message = model::Message;

    fn set_message(&self, message: &Self::Message) {
        let imp = self.imp();

        if imp.message.upgrade().as_ref() == Some(message) {
            return;
        }

        let mut bindings = imp.bindings.borrow_mut();

        while let Some(binding) = bindings.pop() {
            binding.unwatch();
        }

        imp.message_bubble.update_from_message(message, false);

        let handler_id = message.connect_content_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            move |message| {
                obj.update_document(message);
            }
        ));
        imp.handler_id.replace(Some(handler_id));

        imp.message.set(Some(message));
        self.update_document(message);

        self.notify("message");
    }
}

impl MessageDocument {
    /// Downloads the pending thumbnail, if the widget is shown.
    fn maybe_download_thumbnail(&self) {
        let imp = self.imp();

        if !self.is_mapped() || imp.pending_thumbnail_file_id.get() == 0 {
            return;
        }

        let file_id = imp.pending_thumbnail_file_id.take();

        let Some(message) = imp.message.upgrade() else {
            return;
        };
        let session = message.chat_().session_();

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            session,
            async move {
                if let Ok(file) = session.download_file(file_id).await {
                    obj.imp()
                        .file_thumbnail_picture
                        .set_filename(Some(&file.local.path));
                }
            }
        ));
    }

    fn update_document(&self, message: &model::Message) {
        if let tdlib::enums::MessageContent::MessageDocument(data) = message.content().0 {
            let imp = self.imp();

            let message_text = utils::parse_formatted_text(data.caption);
            imp.message_bubble.set_label(message_text);

            imp.file_name_label.set_label(&data.document.file_name);

            self.try_load_thumbnail(message);

            imp.loader.bind(
                &message.chat_().session_(),
                MediaType::File,
                &data.document.document,
            );

            // In contrast to photos and videos, documents are downloaded as
            // soon as they are shown in the history, even if the widget is
            // not mapped yet.
            imp.loader.maybe_start_auto_download();

            self.update_status_ui();
        }
    }

    /// Replaces the click gesture handler of the file box.
    fn replace_click_handler<F: Fn(&gtk::GestureClick, i32, f64, f64) + 'static>(&self, f: F) {
        let imp = self.imp();
        let click = &*imp.click;

        let handler_id = click.connect_released(f);

        if let Some(handler_id) = imp.click_handler_id.borrow_mut().replace(handler_id) {
            click.disconnect(handler_id);
        }
    }

    fn disconnect_click_handler(&self) {
        let imp = self.imp();

        if let Some(handler_id) = imp.click_handler_id.borrow_mut().take() {
            imp.click.disconnect(handler_id);
        }
    }

    /// Updates the widgets according to the status of the loader.
    fn update_status_ui(&self) {
        let imp = self.imp();
        let status = imp.loader.status();

        imp.status_indicator.set_status(status);
        self.update_size_label(status, imp.loader.size());

        match status {
            FileStatus::Downloading(_) => {
                if imp.loader.is_auto() {
                    // Ignore clicks while the file is downloading
                    // automatically.
                    self.disconnect_click_handler();
                } else {
                    // Clicking again cancels the download.
                    self.replace_click_handler(clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _, _| {
                            obj.imp().loader.cancel_download();
                        }
                    ));
                }
            }
            // Uploads are driven by the sender side, so there is nothing to
            // react to.
            FileStatus::Uploading(_) => self.disconnect_click_handler(),
            FileStatus::CanBeDownloaded => {
                self.replace_click_handler(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _, _, _| {
                        obj.imp().loader.request_download();
                    }
                ));
            }
            FileStatus::Downloaded => {
                // Open file
                if imp.file_thumbnail_picture.file().is_some() {
                    imp.status_indicator.set_visible(false);
                }

                let Some(path) = imp.loader.path() else {
                    self.disconnect_click_handler();
                    return;
                };

                let gio_file = gio::File::for_path(path.as_str());
                self.replace_click_handler(move |_, _, _, _| {
                    if let Err(err) = gio::AppInfo::launch_default_for_uri(
                        &gio_file.uri(),
                        gio::AppLaunchContext::NONE,
                    ) {
                        log::error!("Error: {}", err);
                    }
                });
            }
        }
    }

    fn update_size_label(&self, status: FileStatus, size: u64) {
        let size_label = &self.imp().file_size_label;

        match status {
            FileStatus::Downloading(progress) | FileStatus::Uploading(progress) => {
                let downloaded = glib::format_size((size as f64 * progress) as u64);
                let full_size = glib::format_size(size);

                size_label.set_label(&format!("{downloaded} / {full_size}"));
            }
            FileStatus::CanBeDownloaded | FileStatus::Downloaded => {
                size_label.set_label(&glib::format_size(size));
            }
        }
    }

    fn try_load_thumbnail(&self, message: &model::Message) {
        if let tdlib::enums::MessageContent::MessageDocument(data) = message.content().0 {
            let imp = self.imp();
            if let Some(thumbnail) = data.document.thumbnail {
                imp.status_indicator.set_masked(false);
                imp.file_thumbnail_picture.set_visible(true);
                imp.file_box.add_css_class("with-thumbnail");
                if thumbnail.file.local.is_downloading_completed {
                    imp.file_thumbnail_picture
                        .set_filename(Some(&thumbnail.file.local.path));
                } else {
                    if let Some(minithumbnail) = data.document.minithumbnail {
                        let minithumbnail = gdk::Texture::from_bytes(&glib::Bytes::from_owned(
                            glib::base64_decode(&minithumbnail.data),
                        ))
                        .unwrap();
                        imp.file_thumbnail_picture
                            .set_paintable(Some(&minithumbnail));
                    }

                    // The thumbnail is downloaded when the widget is shown, so
                    // that thumbnails that are only scrolled past are not
                    // downloaded.
                    imp.pending_thumbnail_file_id.set(thumbnail.file.id);
                    self.maybe_download_thumbnail();
                }
            } else {
                imp.status_indicator.set_masked(true);
                imp.file_thumbnail_picture.set_visible(false);
                imp.file_thumbnail_picture
                    .set_paintable(gdk::Paintable::NONE);
            }
        }
    }
}
