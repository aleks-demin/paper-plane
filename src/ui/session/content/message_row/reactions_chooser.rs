use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::model;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct ReactionsChooser {
        pub(super) message: glib::WeakRef<model::Message>,
        pub(super) flow_box: gtk::FlowBox,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReactionsChooser {
        const NAME: &'static str = "PaplReactionsChooser";
        type Type = super::ReactionsChooser;
        type ParentType = gtk::Popover;

        fn new() -> Self {
            let flow_box = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .homogeneous(true)
                .max_children_per_line(7)
                .build();

            Self {
                message: glib::WeakRef::new(),
                flow_box,
            }
        }

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("reactionschooser");
        }
    }

    impl ObjectImpl for ReactionsChooser {
        fn constructed(&self) {
            self.parent_constructed();

            let scrolled_window = gtk::ScrolledWindow::builder()
                .propagate_natural_height(true)
                .propagate_natural_width(true)
                .max_content_height(400)
                .child(&self.flow_box)
                .build();

            self.obj().set_child(Some(&scrolled_window));
        }
    }

    impl PopoverImpl for ReactionsChooser {}
    impl WidgetImpl for ReactionsChooser {}
}

glib::wrapper! {
    pub(crate) struct ReactionsChooser(ObjectSubclass<imp::ReactionsChooser>)
        @extends gtk::Popover, gtk::Widget;
}

impl ReactionsChooser {
    pub(crate) fn new(message: &model::Message) -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.imp().message.set(Some(message));
        obj
    }

    pub(crate) fn set_reactions(&self, reactions: Vec<tdlib::types::AvailableReaction>) {
        let imp = self.imp();

        for child in imp.flow_box.observe_children().snapshot() {
            let child = child
                .downcast_ref::<gtk::Widget>()
                .expect("Reaction chooser child is not a widget");
            child.unparent();
        }

        for reaction in reactions {
            let emoji = match &reaction.r#type {
                tdlib::enums::ReactionType::Emoji(data) => data.emoji.clone(),
                // TODO: Support custom emoji reactions
                tdlib::enums::ReactionType::CustomEmoji(_) => continue,
                tdlib::enums::ReactionType::Paid => continue,
            };

            let label = gtk::Label::builder()
                .label(&emoji)
                .css_classes(["emoji"])
                .build();

            let button = gtk::Button::builder()
                .child(&label)
                .css_classes(["emoji"])
                .build();

            if reaction.needs_premium {
                button.add_css_class("premium");
            }

            button.connect_clicked(clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| {
                    obj.popdown();
                    if let Some(message) = obj.imp().message.upgrade() {
                        message.toggle_reaction(&emoji);
                    }
                }
            ));

            imp.flow_box.append(&button);
        }
    }
}
