use std::cell::Cell;
use std::cell::RefCell;

use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, glib::Boxed)]
#[boxed_type(name = "BoxedMessageReactions")]
pub(crate) struct BoxedMessageReactions(pub(crate) Vec<tdlib::types::MessageReaction>);

mod imp {
    use super::*;

    #[derive(Debug, Properties, Default)]
    #[properties(wrapper_type = super::MessageInteractionInfo)]
    pub(crate) struct MessageInteractionInfo {
        #[property(get)]
        pub(super) reply_count: Cell<u32>,
        #[property(get)]
        pub(super) reactions: RefCell<BoxedMessageReactions>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageInteractionInfo {
        const NAME: &'static str = "MessageInteractionInfo";
        type Type = super::MessageInteractionInfo;
    }

    impl ObjectImpl for MessageInteractionInfo {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }
        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }
    }
}

glib::wrapper! {
    pub(crate) struct MessageInteractionInfo(ObjectSubclass<imp::MessageInteractionInfo>);
}

impl From<Option<tdlib::types::MessageInteractionInfo>> for MessageInteractionInfo {
    fn from(interaction_info: Option<tdlib::types::MessageInteractionInfo>) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp()
            .reply_count
            .set(extract_reply_count(&interaction_info));
        obj.set_reactions(extract_reactions(&interaction_info));
        obj
    }
}

impl MessageInteractionInfo {
    pub(crate) fn update(&self, interaction_info: Option<tdlib::types::MessageInteractionInfo>) {
        self.set_reply_count(extract_reply_count(&interaction_info));
        self.set_reactions(extract_reactions(&interaction_info));
    }

    fn set_reply_count(&self, reply_count: u32) {
        if self.reply_count() == reply_count {
            return;
        }
        self.imp().reply_count.set(reply_count);
        self.notify_reply_count()
    }

    fn set_reactions(&self, reactions: BoxedMessageReactions) {
        if *self.imp().reactions.borrow() == reactions {
            return;
        }
        self.imp().reactions.replace(reactions);
        self.notify_reactions();
    }
}

fn extract_reply_count(interaction_info: &Option<tdlib::types::MessageInteractionInfo>) -> u32 {
    interaction_info
        .as_ref()
        .and_then(|interaction_info| interaction_info.reply_info.as_ref())
        .map(|reply_info| reply_info.reply_count)
        .unwrap_or(0) as u32
}

fn extract_reactions(
    interaction_info: &Option<tdlib::types::MessageInteractionInfo>,
) -> BoxedMessageReactions {
    BoxedMessageReactions(
        interaction_info
            .as_ref()
            .and_then(|interaction_info| interaction_info.reactions.as_ref())
            .map(|reactions| reactions.reactions.clone())
            .unwrap_or_default(),
    )
}
