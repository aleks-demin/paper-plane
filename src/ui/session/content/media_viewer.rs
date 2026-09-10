use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::iter;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;
use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::CompositeTemplate;

use super::super::playback_manager;
use super::media_viewer_page::MediaViewerPage;
use crate::model;
use crate::types::MessageId;
use crate::ui::ScaleRevealer;
use crate::ui::Session;
use crate::utils;

/// The number of media messages requested per search when older media is
/// loaded.
const MEDIA_PAGE_SIZE: i32 = 50;

/// How many pages from the beginning of the loaded media the next batch of
/// older media is fetched at the latest.
///
/// The beginning of the carousel holds the oldest media, which the user
/// reaches when navigating back in history.
const PRELOAD_AHEAD: usize = 3;

/// How many pages are kept alive on each side of the displayed one.
///
/// Pages further away are recycled, so that the viewer doesn't keep a
/// widget and a decoded image for every item of the media it browses. The
/// window is symmetric, so that the neighbors of the displayed item are
/// always ready while it is swiped.
const PAGE_WINDOW: usize = 2;

/// The duration of the animation that fades the background, in ms.
const ANIMATION_DURATION: u32 = 250;

/// An item shown by the media viewer.
#[derive(Debug, Clone)]
pub(crate) enum ViewerItem {
    /// A photo, identified by the file of its full-resolution version.
    Photo {
        file: tdlib::types::File,
        /// A low-resolution preview shown while the photo is decoded or
        /// downloaded.
        placeholder: Option<gdk::Texture>,
    },
    /// A video or an animation (GIF), identified by the file of its media.
    Video {
        file: tdlib::types::File,
        /// A low-resolution preview shown while the video is decoded or
        /// downloaded.
        placeholder: Option<gdk::Texture>,
        /// Whether the item is an animation, which loops and stays muted.
        is_animation: bool,
    },
}

/// An entry of the media viewer: an item together with the id of the message
/// it belongs to.
///
/// The message ids are used to keep the pages of a media album together and
/// to avoid showing the same media twice.
#[derive(Debug, Clone)]
pub(crate) struct ViewerEntry {
    pub(crate) item: ViewerItem,
    pub(crate) message_id: MessageId,
}

impl ViewerItem {
    /// Builds the viewer item of the given message.
    ///
    /// The file is taken from the message content, which is a snapshot of
    /// the file state at the time the message was received. The tiles of a
    /// message track the file updates themselves, so they can provide a
    /// more recent state (see `MediaVideoTile::viewer_item`).
    ///
    /// Returns `None` for messages without viewable media content.
    pub(crate) fn from_message(message: &model::Message) -> Option<Self> {
        let content = message.content();

        match &content.0 {
            tdlib::enums::MessageContent::MessagePhoto(data) => {
                let photo_size = data.photo.sizes.last()?;
                Some(Self::Photo {
                    file: photo_size.photo.clone(),
                    placeholder: data
                        .photo
                        .minithumbnail
                        .as_ref()
                        .and_then(utils::texture_from_minithumbnail),
                })
            }
            tdlib::enums::MessageContent::MessageVideo(data) => Some(Self::Video {
                file: data.video.video.clone(),
                placeholder: data
                    .video
                    .minithumbnail
                    .as_ref()
                    .and_then(utils::texture_from_minithumbnail),
                is_animation: false,
            }),
            tdlib::enums::MessageContent::MessageAnimation(data) => Some(Self::Video {
                file: data.animation.animation.clone(),
                placeholder: data
                    .animation
                    .minithumbnail
                    .as_ref()
                    .and_then(utils::texture_from_minithumbnail),
                is_animation: true,
            }),
            _ => None,
        }
    }

    /// The file of this item.
    pub(crate) fn file(&self) -> &tdlib::types::File {
        match self {
            Self::Photo { file, .. } | Self::Video { file, .. } => file,
        }
    }

    /// The low-resolution preview of this item, if any.
    pub(crate) fn placeholder(&self) -> Option<&gdk::Texture> {
        match self {
            Self::Photo { placeholder, .. } | Self::Video { placeholder, .. } => {
                placeholder.as_ref()
            }
        }
    }
}

/// Builds the viewer item of the given message for the media stream.
///
/// The stream only contains the photos and videos of the chat, matching
/// `SearchMessagesFilter::PhotoAndVideo`, so animations are excluded from
/// it even though the viewer can display them.
fn stream_item(message: &model::Message) -> Option<ViewerItem> {
    match ViewerItem::from_message(message) {
        Some(ViewerItem::Video {
            is_animation: true, ..
        }) => None,
        item => item,
    }
}

/// Collects the entries of the media messages of the loaded history of the
/// given chat that are newer than the message with the given id, in
/// chronological order.
///
/// The history contains the messages that are currently loaded in the chat,
/// which usually includes all the media around the message the viewer was
/// opened for. Media that is newer but not loaded is not part of the result.
fn collect_newer_entries(chat: &model::Chat, anchor_newest_id: MessageId) -> Vec<ViewerEntry> {
    let Some(history) = chat.history() else {
        return Vec::new();
    };

    let mut entries = Vec::new();

    for i in (0..history.n_items()).rev() {
        let Some(item) = history.item(i).and_downcast::<model::ChatHistoryItem>() else {
            continue;
        };

        let Some(message) = item.message() else {
            continue;
        };

        if message.media_album_id() != 0 {
            let Some(album) = message.media_album() else {
                continue;
            };

            entries.extend(album.messages().into_iter().filter_map(|message| {
                if message.id() <= anchor_newest_id {
                    return None;
                }

                stream_item(&message).map(|item| ViewerEntry {
                    item,
                    message_id: message.id(),
                })
            }));
        } else {
            if message.id() <= anchor_newest_id {
                continue;
            }

            entries.extend(stream_item(message).map(|item| ViewerEntry {
                item,
                message_id: message.id(),
            }));
        }
    }

    entries
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/app/drey/paper-plane/ui/session/content/media_viewer.ui")]
    pub(crate) struct MediaViewer {
        #[template_child]
        pub(super) toolbar_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub(super) header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub(super) revealer: TemplateChild<ScaleRevealer>,
        #[template_child]
        pub(super) carousel: TemplateChild<adw::Carousel>,
        #[template_child]
        pub(super) previous_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) next_button: TemplateChild<gtk::Button>,
        /// The session this viewer belongs to.
        pub(super) session: glib::WeakRef<model::ClientStateSession>,
        /// The chat whose media is browsed by this viewer.
        pub(super) chat: glib::WeakRef<model::Chat>,
        /// The entries shown by the viewer, in chronological order, from
        /// the oldest to the newest message.
        ///
        /// This is the single source of truth of the content of the viewer:
        /// the carousel only holds the pages of the entries around the
        /// displayed one, which are recycled as the user navigates.
        pub(super) entries: RefCell<Vec<ViewerEntry>>,
        /// The live page of each entry, if any.
        ///
        /// This is always aligned with `entries`: the page of an entry is
        /// `Some` only while the entry is within the window of pages kept
        /// alive around the displayed one.
        pub(super) pages: RefCell<Vec<Option<MediaViewerPage>>>,
        /// The pages that were recycled and can be reused for other
        /// entries.
        pub(super) pool: RefCell<Vec<MediaViewerPage>>,
        /// Whether the live pages are currently being reconciled with the
        /// entries, in which case the page changes caused by the inserts
        /// and removals of the carousel are ignored.
        pub(super) is_reconciling: Cell<bool>,
        /// The id of the oldest media message that is shown. Fetched
        /// messages with a greater or equal id are known already and are
        /// filtered out.
        pub(super) oldest_message_id: Cell<MessageId>,
        /// The id from which the next batch of older media is searched, or
        /// `0` if there is none.
        pub(super) next_older_id: Cell<MessageId>,
        /// Whether there may be older media to load.
        pub(super) has_more: Cell<bool>,
        /// Whether a batch of older media is currently being searched.
        pub(super) is_fetching: Cell<bool>,
        /// The index of the currently displayed item.
        pub(super) active_index: Cell<usize>,
        /// The page of the currently displayed item, if any.
        pub(super) active_page: glib::WeakRef<MediaViewerPage>,
        /// Whether the viewer was closed, in which case no item may be
        /// displayed anymore.
        ///
        /// The completion of a download is asynchronous, so it may still
        /// arrive after the viewer was closed.
        pub(super) closed: Cell<bool>,
        /// Incremented every time the viewer is opened, used to discard the
        /// results of asynchronous operations that became out-of-date.
        pub(super) open_generation: Cell<u64>,
        /// The API to keep track of the animation that fades the background.
        pub(super) fade_animation: OnceCell<adw::TimedAnimation>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaViewer {
        const NAME: &'static str = "PaplMediaViewer";
        type Type = super::MediaViewer;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.install_action("media-viewer.close", None, |viewer, _, _| {
                viewer.close();
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MediaViewer {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            let imp = obj.imp();

            let shortcut_controller = gtk::ShortcutController::new();
            shortcut_controller.set_scope(gtk::ShortcutScope::Local);
            shortcut_controller.add_shortcut(gtk::Shortcut::new(
                Some(gtk::ShortcutTrigger::parse_string("Escape").unwrap()),
                Some(gtk::CallbackAction::new(|widget, _| {
                    let Some(viewer) = widget.downcast_ref::<super::MediaViewer>() else {
                        return glib::Propagation::Proceed;
                    };

                    viewer.close();

                    glib::Propagation::Stop
                })),
            ));

            for (trigger, delta) in [("Left", -1), ("Right", 1)] {
                shortcut_controller.add_shortcut(gtk::Shortcut::new(
                    Some(gtk::ShortcutTrigger::parse_string(trigger).unwrap()),
                    Some(gtk::CallbackAction::new(move |widget, _| {
                        if let Some(viewer) = widget.downcast_ref::<super::MediaViewer>() {
                            viewer.navigate(delta);
                        }

                        glib::Propagation::Stop
                    })),
                ));
            }
            obj.add_controller(shortcut_controller);

            imp.previous_button.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.navigate(-1);
                }
            ));
            imp.next_button.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.navigate(1);
                }
            ));

            imp.carousel.connect_page_changed(clone!(
                #[weak]
                obj,
                move |_, index| {
                    obj.handle_page_changed(index);
                }
            ));

            imp.revealer.connect_transition_done(clone!(
                #[weak]
                obj,
                move |revealer| {
                    if !revealer.reveal_child() {
                        obj.set_visible(false);
                    }
                }
            ));
        }

        fn dispose(&self) {
            if let Some(manager) = self.obj().playback_manager() {
                manager.stop();
            }

            self.dispose_template();
        }
    }

    impl WidgetImpl for MediaViewer {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.toolbar_view.measure(gtk::Orientation::Vertical, width);

            let allocation = gtk::Allocation::new(0, 0, width, height);
            self.toolbar_view.size_allocate(&allocation, baseline);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();

            let progress = self.fade_animation().value();

            if progress > 0.0 {
                let background_color = gdk::RGBA::new(0.0, 0.0, 0.0, progress as f32);
                let bounds = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
                snapshot.append_color(&background_color, &bounds);
            }

            obj.snapshot_child(&*self.toolbar_view, snapshot);
        }
    }

    impl MediaViewer {
        /// The API to keep track of the animation that fades the background.
        pub(super) fn fade_animation(&self) -> &adw::TimedAnimation {
            self.fade_animation.get_or_init(|| {
                let obj = self.obj();
                let target = adw::CallbackAnimationTarget::new(clone!(
                    #[weak]
                    obj,
                    move |value| {
                        obj.imp().header_bar.set_opacity(value);

                        obj.queue_draw();
                    }
                ));

                adw::TimedAnimation::new(&*obj, 0.0, 1.0, ANIMATION_DURATION, target)
            })
        }
    }
}

glib::wrapper! {
    pub(crate) struct MediaViewer(ObjectSubclass<imp::MediaViewer>)
        @extends gtk::Widget;
}

impl MediaViewer {
    /// Opens the media viewer on the given entries, starting at `index`.
    ///
    /// The viewer is an overlay of the session that the given widget
    /// belongs to and is revealed with an animation that starts at that
    /// widget.
    ///
    /// The viewer browses the media of the chat of the entries like a
    /// timeline: navigating to the left scrolls back in history, towards
    /// older media, which is searched with TDLib and prepended as the user
    /// reaches it, while the media that is newer than the initially
    /// displayed group is taken from the loaded history of the chat.
    pub(crate) fn present(
        parent: &impl IsA<gtk::Widget>,
        chat: &model::Chat,
        entries: Vec<ViewerEntry>,
        index: usize,
    ) {
        if entries.is_empty() {
            return;
        }

        let Some(session) = parent
            .ancestor(Session::static_type())
            .and_downcast::<Session>()
        else {
            return;
        };

        session.show_media_viewer(parent, chat, entries, index);
    }

    /// Prepares the viewer to show the given entries, starting at `index`,
    /// and reveals it with a transition from `source_widget`.
    ///
    /// The viewer browses the media of the chat of the entries like a
    /// timeline: navigating to the left scrolls back in history, towards
    /// older media, which is searched with TDLib and prepended as the user
    /// reaches it, while the media that is newer than the initially
    /// displayed group is taken from the loaded history of the chat.
    pub(crate) fn open(
        &self,
        source_widget: &impl IsA<gtk::Widget>,
        chat: &model::Chat,
        entries: Vec<ViewerEntry>,
        index: usize,
    ) {
        if entries.is_empty() {
            return;
        }

        let imp = self.imp();
        let session = chat.session_();

        imp.open_generation.set(imp.open_generation.get() + 1);
        imp.closed.set(false);
        imp.is_fetching.set(false);

        if let Some(manager) = self.playback_manager() {
            manager.stop();
        }

        imp.session.set(Some(&session));
        imp.chat.set(Some(chat));

        let anchor_newest_id = entries.iter().map(|entry| entry.message_id).max().unwrap();
        let prefix = collect_newer_entries(chat, anchor_newest_id);
        let index = index.min(entries.len() - 1);

        let anchor_oldest_id = entries.iter().map(|entry| entry.message_id).min().unwrap();
        imp.oldest_message_id.set(anchor_oldest_id);
        imp.next_older_id.set(anchor_oldest_id);
        imp.has_more.set(true);

        let recycled: Vec<MediaViewerPage> = imp.pages.borrow_mut().drain(..).flatten().collect();
        for page in recycled {
            page.deactivate();
            imp.carousel.remove(&page);
            imp.pool.borrow_mut().push(page);
        }

        *imp.entries.borrow_mut() = entries.into_iter().chain(prefix).collect();
        *imp.pages.borrow_mut() = vec![None; imp.entries.borrow().len()];
        imp.active_page.set(None);
        imp.active_index.set(index);

        self.reconcile_pages(&session);

        let page = imp
            .pages
            .borrow()
            .get(imp.active_index.get())
            .cloned()
            .flatten()
            .unwrap();
        imp.carousel.scroll_to(&page, false);

        self.display_item(imp.active_index.get());

        self.maybe_fetch_older();

        self.reveal(source_widget);
    }

    /// Reveals the viewer with a transition that starts at `source_widget`.
    fn reveal(&self, source_widget: &impl IsA<gtk::Widget>) {
        let imp = self.imp();

        self.set_visible(true);
        self.grab_focus();

        imp.revealer
            .set_source_widget(Some(source_widget.upcast_ref()));
        imp.revealer.set_reveal_child(true);

        let animation = imp.fade_animation();
        animation.set_value_from(animation.value());
        animation.set_value_to(1.0);
        animation.play();
    }

    /// Closes the viewer, transitioning back to its source widget.
    pub(crate) fn close(&self) {
        let imp = self.imp();

        if imp.closed.get() {
            return;
        }

        imp.closed.set(true);

        if let Some(page) = imp.active_page.upgrade() {
            page.deactivate();
        }

        imp.revealer.set_reveal_child(false);

        let animation = imp.fade_animation();
        animation.set_value_from(animation.value());
        animation.set_value_to(0.0);
        animation.play();
    }

    /// Makes sure that a page exists for every entry within the window
    /// around the displayed one, and recycles the pages outside of it.
    ///
    /// The carousel only ever holds the pages of the window, in the order
    /// of the entries. Inserts and removals shift the indices reported by
    /// the carousel, so the changes it reports while this method runs are
    /// ignored and the index of the displayed page is re-derived from the
    /// page itself afterwards.
    fn reconcile_pages(&self, session: &model::ClientStateSession) {
        let imp = self.imp();

        if imp.closed.get() {
            return;
        }

        let count = imp.entries.borrow().len();
        if count == 0 {
            return;
        }

        imp.is_reconciling.set(true);

        let active = imp.active_index.get().min(count - 1);
        imp.active_index.set(active);

        let start = active.saturating_sub(PAGE_WINDOW);
        let end = (active + PAGE_WINDOW).min(count - 1);

        let recycled: Vec<MediaViewerPage> = {
            let mut pages = imp.pages.borrow_mut();
            let mut recycled = Vec::new();

            for (i, slot) in pages.iter_mut().enumerate() {
                if (i < start || i > end) && slot.is_some() {
                    recycled.push(slot.take().unwrap());
                }
            }

            recycled
        };

        for page in recycled {
            page.deactivate();
            imp.carousel.remove(&page);
            imp.pool.borrow_mut().push(page);
        }

        // Create or reuse a page for every entry of the window that has
        // none yet.
        for i in start..=end {
            if imp.pages.borrow()[i].is_some() {
                continue;
            }

            let page = self.make_page(session, i);

            let position = imp.pages.borrow()[..i]
                .iter()
                .filter(|slot| slot.is_some())
                .count() as u32;
            imp.carousel.insert(&page, position as i32);

            imp.pages.borrow_mut()[i] = Some(page);
        }

        if let Some(selected) = imp.active_page.upgrade() {
            if let Some(index) = imp
                .pages
                .borrow()
                .iter()
                .position(|slot| slot.as_ref() == Some(&selected))
            {
                imp.active_index.set(index);
            }
        }

        imp.is_reconciling.set(false);

        self.update_navigation(imp.active_index.get());
    }

    /// Creates a page for the entry at the given index, reusing a recycled
    /// page if there is one.
    fn make_page(&self, session: &model::ClientStateSession, index: usize) -> MediaViewerPage {
        let imp = self.imp();
        let entry = imp.entries.borrow()[index].clone();
        let page = imp.pool.borrow_mut().pop().unwrap_or_default();

        page.set_item(session, entry.item, self.playback_manager().as_ref());

        page
    }

    /// The shared playback engine of the session this viewer belongs to.
    fn playback_manager(&self) -> Option<playback_manager::PlaybackManager> {
        self.ancestor(Session::static_type())
            .and_downcast::<Session>()
            .map(|session| session.playback_manager().clone())
    }

    /// Updates the navigation UI for the given index.
    fn update_navigation(&self, index: usize) {
        let imp = self.imp();
        let count = imp.entries.borrow().len();

        imp.previous_button
            .set_visible(index > 0 || imp.has_more.get());
        imp.next_button.set_visible(index + 1 < count);
    }

    /// Navigates to the next or previous item, if there is one.
    ///
    /// Navigating towards the beginning of the loaded media, which holds
    /// the oldest media, fetches more of it, so that the next navigation
    /// can continue.
    fn navigate(&self, delta: i32) {
        let imp = self.imp();

        let count = imp.entries.borrow().len() as i64;
        let index = imp.active_index.get() as i64 + delta as i64;

        if index < 0 {
            if delta < 0 {
                self.maybe_fetch_older();
            }

            return;
        }

        if index >= count {
            return;
        }

        let Some(page) = imp.pages.borrow().get(index as usize).cloned().flatten() else {
            return;
        };

        imp.carousel.scroll_to(&page, true);
    }

    /// Updates the navigation UI and displays the page the carousel settled
    /// on.
    ///
    /// `page-changed` is emitted only once the settle animation is done, so
    /// the items the carousel flies through during a swipe are never loaded.
    fn handle_page_changed(&self, index: u32) {
        let imp = self.imp();

        if imp.closed.get() || imp.is_reconciling.get() {
            return;
        }

        let widget = imp.carousel.nth_page(index);
        let Some(page) = widget.downcast_ref::<MediaViewerPage>() else {
            return;
        };

        let Some(entry_index) = imp
            .pages
            .borrow()
            .iter()
            .position(|slot| slot.as_ref() == Some(page))
        else {
            return;
        };

        if let Some(selected) = imp.active_page.upgrade() {
            if selected == *page {
                imp.active_index.set(entry_index);
                self.update_navigation(entry_index);
                self.maybe_preload(entry_index);

                return;
            }
        }

        self.update_navigation(entry_index);

        imp.active_index.set(entry_index);

        if let Some(session) = imp.session.upgrade() {
            self.reconcile_pages(&session);
        }

        self.display_item(entry_index);

        self.maybe_preload(entry_index);
    }

    /// Fetches the next batch of older media if the user is close to the
    /// beginning of the loaded media.
    fn maybe_preload(&self, index: usize) {
        let imp = self.imp();

        if imp.has_more.get() && index <= PRELOAD_AHEAD {
            self.maybe_fetch_older();
        }
    }

    /// Fetches the next batch of older media, if there may be one and none
    /// is being fetched already.
    fn maybe_fetch_older(&self) {
        let imp = self.imp();

        if imp.closed.get() || !imp.has_more.get() || imp.is_fetching.get() {
            return;
        }

        let Some(session) = imp.session.upgrade() else {
            return;
        };

        let Some(chat) = imp.chat.upgrade() else {
            return;
        };

        imp.is_fetching.set(true);

        let from_id = imp.next_older_id.get();
        let generation = imp.open_generation.get();

        utils::spawn(clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let result = chat.search_media_messages(from_id, MEDIA_PAGE_SIZE).await;

                let imp = obj.imp();

                if imp.closed.get() || imp.open_generation.get() != generation {
                    return;
                }

                imp.is_fetching.set(false);

                match result {
                    Ok(page) => obj.handle_media_page(&session, page),
                    Err(error) => {
                        log::warn!("Failed to search media messages: {error:?}");
                        imp.has_more.set(false);
                        obj.update_navigation(imp.active_index.get());
                    }
                }
            }
        ));
    }

    /// Adds the fetched page of media messages to the viewer.
    ///
    /// The messages are in reverse chronological order and are prepended
    /// one by one, so that the pages end up in chronological order at the
    /// beginning of the carousel. The pages of a media album stay together,
    /// since its messages are consecutive and every batch is a contiguous
    /// range of messages.
    fn handle_media_page(&self, session: &model::ClientStateSession, page: model::MediaSearchPage) {
        let imp = self.imp();

        imp.has_more.set(page.next_from_message_id != 0);
        imp.next_older_id.set(page.next_from_message_id);

        let oldest_known_id = imp.oldest_message_id.get();
        let fetched: Vec<model::Message> = page
            .messages
            .into_iter()
            .filter(|message| message.id() < oldest_known_id)
            .collect();

        if fetched.is_empty() {
            self.update_navigation(imp.active_index.get());
            return;
        }

        if let Some(oldest) = fetched.last().map(|message| message.id()) {
            imp.oldest_message_id.set(oldest);
        }

        let new_entries: Vec<ViewerEntry> = fetched
            .iter()
            .filter_map(|message| {
                stream_item(message).map(|item| ViewerEntry {
                    item,
                    message_id: message.id(),
                })
            })
            .collect();

        if new_entries.is_empty() {
            self.update_navigation(imp.active_index.get());
            return;
        }

        let n_new = new_entries.len();
        imp.active_index.set(imp.active_index.get() + n_new);
        imp.entries
            .borrow_mut()
            .splice(0..0, new_entries.into_iter().rev());
        imp.pages
            .borrow_mut()
            .splice(0..0, iter::repeat_n(None, n_new));

        self.reconcile_pages(session);

        self.update_navigation(imp.active_index.get());

        self.maybe_preload(imp.active_index.get());
    }

    /// Displays the item at the given index.
    ///
    /// The previously displayed page is deactivated, so that at most one
    /// item is fully decoded and one video is played at any time. The
    /// display itself is up to the page: a photo is decoded right away,
    /// while the playback of a video is started as soon as its file is
    /// downloaded.
    fn display_item(&self, index: usize) {
        let imp = self.imp();

        if imp.closed.get() {
            return;
        }

        let Some(page) = imp.pages.borrow().get(index).cloned().flatten() else {
            return;
        };

        imp.active_index.set(index);

        if let Some(previous_page) = imp.active_page.upgrade() {
            if previous_page != page {
                previous_page.deactivate();
            }
        }
        imp.active_page.set(Some(&page));

        page.activate();
    }
}
