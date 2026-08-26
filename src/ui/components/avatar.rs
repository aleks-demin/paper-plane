use std::cell::OnceCell;
use std::cell::RefCell;
use std::sync::OnceLock;

use adw::subclass::prelude::*;
use glib::clone;
use gtk::cairo;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::CompositeTemplate;

use crate::model;
use crate::strings;
use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/components/avatar.ui")]
    pub(crate) struct Avatar {
        /// A `Chat` or `User`
        pub(super) item: RefCell<Option<glib::Object>>,
        pub(super) user_signal_group: OnceCell<glib::SignalGroup>,
        pub(super) chat_signal_group: OnceCell<glib::SignalGroup>,
        #[template_child]
        pub(super) avatar: TemplateChild<adw::Avatar>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Avatar {
        const NAME: &'static str = "PaplAvatar";
        type Type = super::Avatar;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_css_name("avatar");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Avatar {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecObject::builder::<glib::Object>("item")
                        .explicit_notify()
                        .build(),
                    glib::ParamSpecString::builder("custom-text")
                        .explicit_notify()
                        .build(),
                    glib::ParamSpecInt::builder("size")
                        .explicit_notify()
                        .build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();

            match pspec.name() {
                "item" => obj.set_item(value.get().unwrap()),
                "custom-text" => obj.set_custom_text(value.get().unwrap()),
                "size" => obj.set_size(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();

            match pspec.name() {
                "item" => obj.item().to_value(),
                "custom-text" => obj.custom_text().to_value(),
                "size" => obj.size().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().create_signal_groups();
        }
    }

    impl WidgetImpl for Avatar {}
    impl BinImpl for Avatar {}
}

glib::wrapper! {
    pub(crate) struct Avatar(ObjectSubclass<imp::Avatar>)
        @extends gtk::Widget, adw::Bin;
}

impl Default for Avatar {
    fn default() -> Self {
        Self::new()
    }
}

/// How an avatar looks when there is no photo to display.
struct AvatarAppearance {
    icon_name: Option<&'static str>,
    text: Option<String>,
    show_initials: bool,
}

fn appearance_for_user(user: &model::User) -> AvatarAppearance {
    if user.user_type().0 == tdlib::enums::UserType::Deleted {
        AvatarAppearance {
            icon_name: Some("ghost-symbolic"),
            text: Some(strings::user_display_name(user, true)),
            show_initials: false,
        }
    } else {
        AvatarAppearance {
            icon_name: None,
            text: Some(strings::user_display_name(user, true)),
            show_initials: true,
        }
    }
}

fn appearance_for_chat(chat: &model::Chat) -> AvatarAppearance {
    if chat.is_own_chat() {
        AvatarAppearance {
            icon_name: Some("user-bookmarks-symbolic"),
            text: Some("-".into()),
            show_initials: false,
        }
    } else {
        AvatarAppearance {
            icon_name: None,
            text: Some(chat.title().to_string()),
            show_initials: true,
        }
    }
}

impl Avatar {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    fn create_signal_groups(&self) {
        let imp = self.imp();

        let user_signal_group = glib::SignalGroup::new::<model::User>();
        user_signal_group.connect_notify_local(
            Some("type"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| {
                    obj.update_display_name();
                    obj.update_avatar();
                }
            ),
        );
        user_signal_group.connect_notify_local(
            Some("first-name"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| {
                    obj.update_display_name();
                }
            ),
        );
        user_signal_group.connect_notify_local(
            Some("last-name"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| {
                    obj.update_display_name();
                }
            ),
        );
        user_signal_group.connect_notify_local(
            Some("avatar"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| {
                    obj.update_avatar();
                }
            ),
        );
        imp.user_signal_group.set(user_signal_group).unwrap();

        let chat_signal_group = glib::SignalGroup::new::<model::Chat>();
        chat_signal_group.connect_notify_local(
            Some("title"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| obj.update_display_name()
            ),
        );
        chat_signal_group.connect_notify_local(
            Some("avatar"),
            clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| {
                    obj.update_avatar();
                }
            ),
        );

        imp.chat_signal_group.set(chat_signal_group).unwrap();
    }

    fn load_image(&self, avatar: Option<model::Avatar>, session: model::ClientStateSession) {
        if let Some(avatar) = avatar {
            let file = avatar.0;
            if file.local.is_downloading_completed {
                let texture = gdk::Texture::from_filename(&file.local.path).unwrap();
                self.imp().avatar.set_custom_image(Some(&texture));
            } else {
                let file_id = file.id;

                utils::spawn(clone!(
                    #[weak(rename_to = obj)]
                    self,
                    #[weak]
                    session,
                    async move {
                        obj.download_avatar(file_id, &session).await;
                    }
                ));
            }
        } else {
            self.imp().avatar.set_custom_image(gdk::Paintable::NONE);
        }
    }

    fn update_avatar(&self) {
        let imp = self.imp();

        imp.avatar.set_custom_image(gdk::Paintable::NONE);
        imp.avatar.set_icon_name(None);
        imp.avatar.set_show_initials(true);

        if let Some(item) = self.item() {
            if let Some(user) = item.downcast_ref::<model::User>() {
                let appearance = appearance_for_user(user);
                imp.avatar.set_icon_name(appearance.icon_name);
                imp.avatar.set_show_initials(appearance.show_initials);

                // A photo replaces the default appearance.
                if appearance.icon_name.is_none() {
                    self.load_image(user.avatar(), user.session_());
                }
            } else if let Some(chat) = item.downcast_ref::<model::Chat>() {
                let appearance = appearance_for_chat(chat);
                imp.avatar.set_icon_name(appearance.icon_name);
                imp.avatar.set_show_initials(appearance.show_initials);

                if appearance.icon_name.is_none() {
                    self.load_image(chat.avatar(), chat.session_());
                }
            }
        }
    }

    fn update_display_name(&self) {
        let imp = self.imp();

        if let Some(item) = self.item() {
            if let Some(user) = item.downcast_ref::<model::User>() {
                imp.avatar
                    .set_text(appearance_for_user(user).text.as_deref());
            } else if let Some(chat) = item.downcast_ref::<model::Chat>() {
                imp.avatar
                    .set_text(appearance_for_chat(chat).text.as_deref());
            }
        }
    }

    async fn download_avatar(&self, file_id: i32, session: &model::ClientStateSession) {
        match session.download_file(file_id).await {
            Ok(file) => {
                let texture = gdk::Texture::from_filename(file.local.path).unwrap();
                self.imp().avatar.set_custom_image(Some(&texture));
            }
            Err(e) => {
                log::warn!("Failed to download an avatar: {e:?}");
            }
        }
    }

    pub(crate) fn item(&self) -> Option<glib::Object> {
        self.imp().item.borrow().clone()
    }

    pub(crate) fn set_item(&self, item: Option<glib::Object>) {
        let imp = self.imp();

        imp.chat_signal_group.get().unwrap().set_target(
            item.as_ref()
                .and_then(|item| item.downcast_ref::<model::Chat>()),
        );
        imp.user_signal_group.get().unwrap().set_target(
            item.as_ref()
                .and_then(|item| item.downcast_ref::<model::User>()),
        );

        imp.item.replace(item);

        self.update_display_name();
        self.update_avatar();

        self.notify("item");
    }

    pub(crate) fn custom_text(&self) -> Option<String> {
        self.imp().avatar.text().map(Into::into)
    }

    pub(crate) fn set_custom_text(&self, text: Option<&str>) {
        self.imp().avatar.set_text(text);
        self.notify("custom-text");
    }

    pub(crate) fn size(&self) -> i32 {
        self.imp().avatar.size()
    }

    pub(crate) fn set_size(&self, size: i32) {
        self.imp().avatar.set_size(size);
        self.notify("size");
    }

    /// Renders the default photo-less avatar for a chat offscreen, e.g. for
    /// use in notifications.
    pub(crate) fn default_icon_for_chat(chat: &model::Chat) -> gio::Icon {
        let appearance = appearance_for_chat(chat);

        match appearance.icon_name {
            Some(icon_name) => gio::ThemedIcon::new(icon_name).upcast(),
            None => render_initials_texture(appearance.text.as_deref().unwrap_or("?")).upcast(),
        }
    }
}

/// The avatar palette from the Adwaita stylesheet:
/// (foreground, gradient top, gradient bottom).
const AVATAR_COLORS: [(u32, u32, u32); 14] = [
    (0xcfe1f5, 0x83b6ec, 0x337fdc), // blue
    (0xcaeaf2, 0x7ad9f1, 0x0f9ac8), // cyan
    (0xcef8d8, 0x8de6b1, 0x29ae74), // green
    (0xe6f9d7, 0xb5e98a, 0x6ab85b), // lime
    (0xf9f4e1, 0xf8e359, 0xd29d09), // yellow
    (0xffead1, 0xffcb62, 0xd68400), // gold
    (0xffe5c5, 0xffa95a, 0xed5b00), // orange
    (0xf8d2ce, 0xf78773, 0xe62d42), // raspberry
    (0xfac7de, 0xe973ab, 0xe33b6a), // magenta
    (0xe7c2e8, 0xcb78d4, 0x9945b5), // purple
    (0xd5d2f5, 0x9e91e8, 0x7a59ca), // violet
    (0xf2eade, 0xe3cf9c, 0xb08952), // beige
    (0xe5d6ca, 0xbe916d, 0x785336), // brown
    (0xd8d7d3, 0xc0bfbc, 0x6e6d71), // gray
];

/// The same as GLib's `g_str_hash`, which libadwaita uses to pick the avatar
/// color.
fn g_str_hash(s: &str) -> u32 {
    s.bytes()
        .fold(5381, |h, b| h.wrapping_mul(33).wrapping_add(u32::from(b)))
}

/// The same as libadwaita's `extract_initials_from_text`: the first character
/// and the character after the last space, uppercased.
fn initials_from_text(text: &str) -> String {
    let normalized = text.trim().to_uppercase();

    let mut initials: String = normalized.chars().take(1).collect();

    if let Some(space_index) = normalized.rfind(' ') {
        let after_space = &normalized[space_index + ' '.len_utf8()..];
        if let Some(c) = after_space.chars().next() {
            initials.push(c);
        }
    }

    initials
}

fn hex_color(c: u32) -> (f64, f64, f64) {
    (
        f64::from((c >> 16) & 0xff) / 255.,
        f64::from((c >> 8) & 0xff) / 255.,
        f64::from(c & 0xff) / 255.,
    )
}

fn render_initials_texture(text: &str) -> gdk::MemoryTexture {
    const SIZE: i32 = 128;
    const FONT_SIZE: f64 = 46.;
    let size = f64::from(SIZE);

    let color = AVATAR_COLORS[(g_str_hash(text) % AVATAR_COLORS.len() as u32) as usize];
    let (fg, top, bottom) = color;

    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, SIZE, SIZE)
        .expect("Failed to create an image surface");
    let cr = cairo::Context::new(&surface).expect("Failed to create a cairo context");

    let gradient = cairo::LinearGradient::new(0., 0., 0., size);
    let (r, g, b) = hex_color(top);
    gradient.add_color_stop_rgb(0., r, g, b);
    let (r, g, b) = hex_color(bottom);
    gradient.add_color_stop_rgb(1., r, g, b);

    cr.arc(size / 2., size / 2., size / 2., 0., std::f64::consts::PI * 2.);
    cr.set_source(&gradient)
        .expect("Failed to set the gradient source");
    cr.fill();

    let initials = initials_from_text(text);
    cr.select_font_face("Adwaita Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(FONT_SIZE);
    let (r, g, b) = hex_color(fg);
    cr.set_source_rgb(r, g, b);

    let extents = cr
        .text_extents(&initials)
        .expect("Failed to get the text extents");
    cr.move_to(
        (size - extents.width()) / 2. - extents.x_bearing(),
        (size - extents.height()) / 2. - extents.y_bearing(),
    );
    cr.show_text(&initials);

    drop(cr);

    let stride = surface.stride() as usize;
    let data = surface.data().expect("Failed to access the image data");
    gdk::MemoryTexture::new(
        SIZE,
        SIZE,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &glib::Bytes::from(&data[..]),
        stride,
    )
}
