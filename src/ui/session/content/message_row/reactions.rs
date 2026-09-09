use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;

use crate::model;
use crate::utils;

use gtk::gdk;

const SPACING: i32 = 6;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/message_row/reactions.ui")]
    pub(crate) struct MessageReactions {
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) reactions_binding: RefCell<Option<gtk::ExpressionWatch>>,
        pub(super) reactions: RefCell<model::BoxedMessageReactions>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageReactions {
        const NAME: &'static str = "PaplMessageReactions";
        type Type = super::MessageReactions;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_css_name("messagereactions");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageReactions {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecObject::builder::<gtk::Widget>("message")
                        .write_only()
                        .build(),
                    glib::ParamSpecBoxed::builder::<model::BoxedMessageReactions>("reactions")
                        .explicit_notify()
                        .build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();

            match pspec.name() {
                "message" => obj.set_message(value.get().unwrap()),
                "reactions" => obj.set_reactions(value.get().unwrap()),
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();

            match pspec.name() {
                "reactions" => obj.reactions().to_value(),
                _ => unimplemented!(),
            }
        }

        fn dispose(&self) {
            utils::unparent_children(&*self.obj());
        }
    }

    impl WidgetImpl for MessageReactions {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let children: Vec<gtk::Widget> = self
                .obj()
                .observe_children()
                .snapshot()
                .into_iter()
                .filter_map(|child| child.downcast::<gtk::Widget>().ok())
                .filter(|child| child.is_visible())
                .collect();

            let chip_sizes: Vec<(i32, i32)> = children
                .iter()
                .map(|child| {
                    let (min_width, natural_width, _, _) =
                        child.measure(gtk::Orientation::Horizontal, -1);
                    let (_, natural_height, _, _) =
                        child.measure(gtk::Orientation::Vertical, natural_width);
                    (natural_width.max(min_width), natural_height)
                })
                .collect();

            if orientation == gtk::Orientation::Horizontal {
                let min = chip_sizes.iter().map(|size| size.0).max().unwrap_or(0);
                let natural = chip_sizes.iter().map(|size| size.0).sum::<i32>()
                    + SPACING * chip_sizes.len().saturating_sub(1) as i32;

                (min, natural, -1, -1)
            } else {
                let min_width = chip_sizes.iter().map(|size| size.0).max().unwrap_or(0);
                let natural_width = chip_sizes.iter().map(|size| size.0).sum::<i32>()
                    + SPACING * chip_sizes.len().saturating_sub(1) as i32;

                let available_width = if for_size < 0 {
                    natural_width
                } else {
                    for_size.max(min_width)
                };

                let height = Self::packed_height(&chip_sizes, available_width);

                (height, height, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let children: Vec<gtk::Widget> = self
                .obj()
                .observe_children()
                .snapshot()
                .into_iter()
                .filter_map(|child| child.downcast::<gtk::Widget>().ok())
                .filter(|child| child.is_visible())
                .collect();

            let mut x = 0;
            let mut y = 0;
            let mut row_height = 0;

            for child in children {
                let (min_width, natural_width, _, _) =
                    child.measure(gtk::Orientation::Horizontal, -1);
                let chip_width = natural_width.max(min_width).min(width);
                let (_, natural_height, _, _) =
                    child.measure(gtk::Orientation::Vertical, chip_width);

                if x > 0 && x + chip_width > width {
                    x = 0;
                    y += row_height + SPACING;
                    row_height = 0;
                }

                child.size_allocate(
                    &gdk::Rectangle::new(x, y, chip_width, natural_height),
                    -1,
                );

                x += chip_width + SPACING;
                row_height = row_height.max(natural_height);
            }
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
    }

    impl MessageReactions {
        /// Computes the height needed to lay out chips with the given sizes
        /// wrapped to fit into `available_width`.
        fn packed_height(chip_sizes: &[(i32, i32)], available_width: i32) -> i32 {
            let mut height = 0;
            let mut row_width = 0;
            let mut row_height = 0;

            for &(chip_width, chip_height) in chip_sizes {
                if row_width > 0 && row_width + chip_width > available_width {
                    height += row_height + SPACING;
                    row_width = 0;
                    row_height = 0;
                }

                row_width += chip_width + SPACING;
                row_height = row_height.max(chip_height);
            }

            if row_height > 0 {
                height += row_height;
            }

            height
        }
    }
}

glib::wrapper! {
    pub(crate) struct MessageReactions(ObjectSubclass<imp::MessageReactions>)
        @extends gtk::Widget;
}

impl MessageReactions {
    pub(crate) fn set_message(&self, message: glib::Object) {
        let imp = self.imp();

        if let Some(previous) = imp.message.upgrade() {
            if previous == message {
                return;
            }
        }

        if let Some(binding) = imp.reactions_binding.take() {
            binding.unwatch();
        }

        let Some(message) = message.downcast::<model::Message>().ok() else {
            // The row shows a sponsored message, which cannot have reactions
            imp.message.set(None);
            self.set_reactions(model::BoxedMessageReactions::default());
            return;
        };

        imp.message.set(Some(&message));

        let reactions_expression = gtk::ConstantExpression::new(&message)
            .chain_property::<model::Message>("interaction-info")
            .chain_property::<model::MessageInteractionInfo>("reactions");
        let binding = reactions_expression.bind(self, "reactions", glib::Object::NONE);
        imp.reactions_binding.replace(Some(binding));
    }

    fn reactions(&self) -> model::BoxedMessageReactions {
        self.imp().reactions.borrow().clone()
    }

    fn set_reactions(&self, reactions: model::BoxedMessageReactions) {
        if self.reactions() == reactions {
            return;
        }

        for child in self.observe_children().snapshot() {
            let child = child
                .downcast_ref::<gtk::Widget>()
                .expect("Reaction strip child is not a widget");
            child.unparent();
        }

        for reaction in &reactions.0 {
            let emoji = match &reaction.r#type {
                tdlib::enums::ReactionType::Emoji(data) => data.emoji.clone(),
                // TODO: Render custom emoji reactions
                tdlib::enums::ReactionType::CustomEmoji(_) => continue,
                tdlib::enums::ReactionType::Paid => continue,
            };

            let label = gtk::Label::builder()
                .label(format!("{} {}", utils::emoji_string(&emoji), reaction.total_count))
                .build();

            let button = gtk::Button::builder()
                .child(&label)
                .css_classes(if reaction.is_chosen {
                    vec!["reaction-chip", "chosen"]
                } else {
                    vec!["reaction-chip"]
                })
                .build();

            button.connect_clicked(clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| {
                    if let Some(message) = obj.imp().message.upgrade() {
                        message.toggle_reaction(&emoji);
                    }
                }
            ));

            button.set_parent(self);
        }

        self.set_visible(!reactions.0.is_empty());

        self.imp().reactions.replace(reactions);
        self.notify("reactions");
    }
}
