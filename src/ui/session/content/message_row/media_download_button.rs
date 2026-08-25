use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use super::document::FileStatus;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/media_download_button.ui")]
    pub(crate) struct MediaDownloadButton {
        #[template_child]
        pub(super) download_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) loading_indicator: TemplateChild<ori::LoadingIndicator>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaDownloadButton {
        const NAME: &'static str = "PaplMediaDownloadButton";
        type Type = super::MediaDownloadButton;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("mediadownloadbutton");
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MediaDownloadButton {
        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for MediaDownloadButton {}
}

glib::wrapper! {
    pub(crate) struct MediaDownloadButton(ObjectSubclass<imp::MediaDownloadButton>)
        @extends gtk::Widget;
}

impl MediaDownloadButton {
    /// Sets the current status of the download button.
    ///
    /// - `FileStatus::CanBeDownloaded` shows the download icon.
    /// - `FileStatus::Downloading(progress)` shows the loading indicator.
    /// - `FileStatus::Downloaded` and `FileStatus::Uploading` hide the button.
    pub(crate) fn set_status(&self, status: FileStatus) {
        let (loading_is_visible, icon_name) = match status {
            FileStatus::Downloading(_) => (true, "media-playback-stop-symbolic"),
            FileStatus::CanBeDownloaded => (false, "document-save-symbolic"),
            FileStatus::Downloaded | FileStatus::Uploading(_) => {
                self.set_visible(false);
                return;
            }
        };

        let imp = self.imp();

        imp.loading_indicator.set_visible(loading_is_visible);
        imp.loading_indicator.set_progress(match status {
            FileStatus::Downloading(progress) => progress,
            _ => 0.0,
        });
        imp.download_image.set_icon_name(Some(icon_name));
    }
}