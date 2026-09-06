use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;
use gtk::gdk;
use gtk::glib;
use gtk::CompositeTemplate;
use gtk::graphene;

use crate::model;
use crate::types::MessageId;
use crate::ui::{ScaleRevealer, Session};
use crate::utils;

use super::media_viewer_page::MediaViewerPage;

/// The number of media messages requested per search when older media is
/// loaded.
const MEDIA_PAGE_SIZE: i32 = 50;

/// How many pages from the beginning of the loaded media the next batch of
/// older media is fetched at the latest.
///
/// The beginning of the carousel holds the oldest media, which the user
/// reaches when navigating back in history.
const PRELOAD_AHEAD: usize = 3;

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
                // The viewer shows the full-resolution version of the photo.
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

    // The history is ordered from the newest to the oldest message, so it
    // is walked backwards to end up with the chronological order.
    for i in (0..history.n_items()).rev() {
        let Some(item) = history.item(i).and_downcast::<model::ChatHistoryItem>() else {
            continue;
        };

        let Some(message) = item.message() else {
            continue;
        };

        if message.media_album_id() != 0 {
            // An album is displayed by its representative, which holds all
            // of its messages.
            let Some(album) = message.media_album() else {
                continue;
            };

            let Some(last_message_id) = album.last_message_id() else {
                continue;
            };

            if last_message_id <= anchor_newest_id {
                // The album is the initially displayed one or an older one.
                continue;
            }

            entries.extend(album.messages().into_iter().filter_map(|message| {
                stream_item(&message).map(|item| ViewerEntry {
                    item,
                    message_id: message.id(),
                })
            }));
        } else {
            if message.id() <= anchor_newest_id {
                // The initially displayed message or an older one.
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
        #[template_child]
        pub(super) controls: TemplateChild<gtk::MediaControls>,
        /// The session this viewer belongs to.
        pub(super) session: glib::WeakRef<model::ClientStateSession>,
        /// The chat whose media is browsed by this viewer.
        pub(super) chat: glib::WeakRef<model::Chat>,
        /// The entries shown by the viewer, in chronological order, from
        /// the oldest to the newest message.
        pub(super) entries: RefCell<Vec<ViewerEntry>>,
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
        /// The media stream the videos are played on, created lazily on the
        /// first playback.
        pub(super) media: RefCell<Option<gtk::MediaFile>>,
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

            // The shortcuts are handled while the focus is within the
            // viewer, which grabs the focus when it is revealed.
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

            // Hide the viewer once the hiding transition is done.
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
            // The stream is closed explicitly, so that the teardown of a
            // possibly stuck pipeline happens here rather than lazily at
            // the finalization of the media stream.
            self.obj().stop_video();

            self.dispose_template();
        }
    }

    impl WidgetImpl for MediaViewer {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            // The toolbar view fills the whole viewer.
            let allocation = gtk::Allocation::new(0, 0, width, height);
            self.toolbar_view.size_allocate(&allocation, baseline);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();

            // Change the background opacity depending on the progress of
            // the animation that fades the background.
            let progress = self.fade_animation().value();

            if progress > 0.0 {
                let background_color = gdk::RGBA::new(0.0, 0.0, 0.0, progress as f32);
                let bounds =
                    graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
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
                        // Fade the header bar content too.
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

        // Results of asynchronous operations that were started for a
        // previous opening of the viewer become irrelevant.
        imp.open_generation.set(imp.open_generation.get() + 1);
        imp.closed.set(false);
        imp.is_fetching.set(false);

        // The playback of a previous opening doesn't survive the new one.
        self.stop_video();

        imp.session.set(Some(&session));
        imp.chat.set(Some(chat));

        // The media that is newer than the initially displayed group is
        // taken from the loaded history of the chat, so that the user can
        // navigate to it without having to load anything.
        let anchor_newest_id = entries
            .iter()
            .map(|entry| entry.message_id)
            .max()
            .unwrap();
        let prefix = collect_newer_entries(chat, anchor_newest_id);
        let index = index.min(entries.len() - 1);

        // The first batch of older media is searched from the oldest
        // message of the initially displayed group. The search includes
        // that message, so the duplicates are filtered out when a batch
        // arrives.
        let anchor_oldest_id = entries
            .iter()
            .map(|entry| entry.message_id)
            .min()
            .unwrap();
        imp.oldest_message_id.set(anchor_oldest_id);
        imp.next_older_id.set(anchor_oldest_id);
        imp.has_more.set(true);

        // The carousel may still hold the pages of a previous opening.
        while imp.carousel.n_pages() > 0 {
            imp.carousel.remove(&imp.carousel.nth_page(0));
        }

        *imp.entries.borrow_mut() = entries.into_iter().chain(prefix).collect();
        imp.active_page.set(None);

        self.fill_pages(&session);

        // The carousel starts at the item the viewer was opened for.
        imp.active_index.set(index);
        let page = imp.carousel.nth_page(index as u32);
        imp.carousel.scroll_to(&page, false);
        self.update_navigation(index);

        self.display_item(index);

        // The older media is loaded right away, so that navigating
        // towards it doesn't have to wait for the first search.
        self.maybe_fetch_older();

        self.reveal(source_widget);
    }

    /// Reveals the viewer with a transition that starts at `source_widget`.
    fn reveal(&self, source_widget: &impl IsA<gtk::Widget>) {
        let imp = self.imp();

        self.set_visible(true);
        self.grab_focus();

        // Trigger the revealer.
        imp.revealer
            .set_source_widget(Some(source_widget.upcast_ref()));
        imp.revealer.set_reveal_child(true);

        // Fade in the background.
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

        // The playback is stopped right away; it is not visible behind the
        // fade-out.
        self.stop_video();

        // Trigger the revealer.
        imp.revealer.set_reveal_child(false);

        // Fade out the background.
        let animation = imp.fade_animation();
        animation.set_value_from(animation.value());
        animation.set_value_to(0.0);
        animation.play();
    }

    /// Creates a page for every entry and adds it to the carousel.
    fn fill_pages(&self, session: &model::ClientStateSession) {
        for entry in self.imp().entries.borrow().iter() {
            self.append_entry(session, entry);
        }
    }

    /// Creates a page for the given entry and adds it to the end of the
    /// carousel.
    fn append_entry(&self, session: &model::ClientStateSession, entry: &ViewerEntry) {
        let page = self.make_page(session, entry);

        self.imp().carousel.append(&page);
    }

    /// Creates a page for each of the given entries and inserts them at the
    /// beginning of the carousel, keeping the carousel on the page it is
    /// currently showing.
    ///
    /// The entries must be in reverse chronological order, so that inserting
    /// them one by one results in a chronologically ordered carousel.
    fn prepend_entries(&self, session: &model::ClientStateSession, new_entries: &[ViewerEntry]) {
        let imp = self.imp();

        if new_entries.is_empty() {
            return;
        }

        // The index of the displayed page shifts by the number of inserted
        // pages. It is adjusted upfront, so that the position notifications
        // of the inserts see the index of the page that is displayed in the
        // end.
        imp.active_index.set(imp.active_index.get() + new_entries.len());

        for entry in new_entries {
            self.prepend_entry(session, entry);
        }

        imp.entries
            .borrow_mut()
            .splice(0..0, new_entries.iter().cloned());
    }

    /// Creates a page for the given entry and inserts it at the beginning of
    /// the carousel.
    fn prepend_entry(&self, session: &model::ClientStateSession, entry: &ViewerEntry) {
        let page = self.make_page(session, entry);

        self.imp().carousel.insert(&page, 0);
    }

    /// Creates a page for the given entry, tracking the status of its media
    /// file.
    fn make_page(&self, session: &model::ClientStateSession, entry: &ViewerEntry) -> MediaViewerPage {
        let page = MediaViewerPage::default();
        page.set_item(session, entry.item.clone());

        // The viewer plays the video of a page as soon as its download
        // completes.
        page.loader().connect_status_notify(clone!(
            #[weak(rename_to = obj)]
            self,
            #[weak]
            page,
            move |_| {
                obj.handle_media_status(&page);
            }
        ));

        page
    }

    /// The number of pages of the carousel.
    fn page_count(&self) -> u32 {
        self.imp().carousel.n_pages()
    }

    /// Updates the navigation UI for the given index.
    fn update_navigation(&self, index: usize) {
        let imp = self.imp();
        let count = self.page_count();

        imp.previous_button
            .set_visible(index > 0 || imp.has_more.get());
        imp.next_button.set_visible(index + 1 < count as usize);
    }

    /// Navigates to the next or previous item, if there is one.
    ///
    /// Navigating towards the beginning of the loaded media, which holds
    /// the oldest media, fetches more of it, so that the next navigation
    /// can continue.
    fn navigate(&self, delta: i32) {
        let imp = self.imp();

        let count = self.page_count() as i64;
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

        let page = imp.carousel.nth_page(index as u32);
        imp.carousel.scroll_to(&page, true);
    }

    /// Updates the navigation UI and displays the page the carousel settled
    /// on.
    ///
    /// `page-changed` is emitted only once the settle animation is done, so
    /// the items the carousel flies through during a swipe are never loaded.
    fn handle_page_changed(&self, index: u32) {
        let imp = self.imp();

        let index = index as usize;

        // An empty carousel emits `(int)index == -1`, which arrives here as
        // `u32::MAX`. The viewer is never empty, but the guard is cheap.
        if index >= self.page_count() as usize {
            return;
        }

        self.update_navigation(index);

        if index != imp.active_index.get() {
            self.display_item(index);
        }

        self.maybe_preload(index);
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

                // The result is irrelevant if the viewer was closed or
                // reopened for another group of media in the meantime.
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

        // The search includes the message it started from and may overlap
        // with the previous batches, so the messages that are known already
        // are filtered out.
        let oldest_known_id = imp.oldest_message_id.get();
        let fetched: Vec<model::Message> = page
            .messages
            .into_iter()
            .filter(|message| message.id() < oldest_known_id)
            .collect();

        if fetched.is_empty() {
            // No new media was found.
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

        self.prepend_entries(session, &new_entries);

        // The total count may have changed even if nothing was appended.
        self.update_navigation(imp.active_index.get());

        self.maybe_preload(imp.active_index.get());
    }

    /// Displays the item at the given index.
    ///
    /// The previously displayed page is restored to its preview, so that at
    /// most one item is fully decoded at any time. A photo is decoded right
    /// away, while the playback of a video is started as soon as its file
    /// is downloaded.
    fn display_item(&self, index: usize) {
        let imp = self.imp();

        if imp.closed.get() {
            return;
        }

        let Some(item) = imp
            .entries
            .borrow()
            .get(index)
            .map(|entry| entry.item.clone())
        else {
            return;
        };

        let Some(page) = imp
            .carousel
            .nth_page(index as u32)
            .downcast_ref::<MediaViewerPage>()
            .cloned()
        else {
            return;
        };

        imp.active_index.set(index);

        if let Some(previous_page) = imp.active_page.upgrade() {
            previous_page.restore();
        }
        imp.active_page.set(Some(&page));

        match &item {
            ViewerItem::Photo { .. } => {
                self.stop_video();
                page.display();
            }
            ViewerItem::Video { is_animation, .. } => {
                if let Some(path) = page.media_path() {
                    self.play_video(&page, &path, *is_animation);
                } else {
                    // The video still has to be downloaded. The playback is
                    // started as soon as the download completes (see
                    // `handle_media_status`).
                    self.stop_video();
                    page.display();
                }
            }
        }
    }

    /// Reacts to a status change of the media file of one of the pages.
    ///
    /// When the download of the video of the active page completes, its
    /// playback is started.
    fn handle_media_status(&self, page: &MediaViewerPage) {
        let imp = self.imp();

        if imp.closed.get() || imp.active_page.upgrade().as_ref() != Some(page) {
            return;
        }

        let Some(item) = imp
            .entries
            .borrow()
            .get(imp.active_index.get())
            .map(|entry| entry.item.clone())
        else {
            return;
        };

        if let ViewerItem::Video { is_animation, .. } = &item {
            if let Some(path) = page.media_path() {
                self.play_video(page, &path, *is_animation);
            } else {
                // The download may have been canceled.
                self.stop_video();
            }
        }
    }

    /// Returns the media stream, creating it on the first call.
    ///
    /// The stream is created empty; its source is set with `set_filename`
    /// when a playback starts, so that no pipeline exists while nothing
    /// plays.
    fn media(&self) -> Option<gtk::MediaFile> {
        let imp = self.imp();

        if let Some(media) = &*imp.media.borrow() {
            return Some(media.clone());
        }

        let media = gtk::MediaFile::new();

        let obj = self.clone();
        media.connect_prepared_notify(clone!(
            #[weak]
            obj,
            move |media| {
                let imp = obj.imp();

                // The viewer was closed while the stream was being
                // prepared, so the playback must not start.
                if imp.closed.get() || !media.is_prepared() {
                    return;
                }

                // The first frame replaces the low-resolution preview of
                // the page, and the playback starts as soon as the stream
                // is ready.
                if let Some(page) = obj.imp().active_page.upgrade() {
                    page.set_media_paintable(Some(media.upcast_ref()));
                }

                media.play();
            }
        ));
        media.connect_error_notify(clone!(
            #[weak]
            obj,
            move |media| {
                let Some(error) = media.error() else {
                    return;
                };

                log::warn!("Video playback failed: {error:?}");

                // The playback can't continue, so the viewer is closed.
                obj.stop_video();
                obj.close();
            }
        ));

        *imp.media.borrow_mut() = Some(media.clone());

        Some(media)
    }

    /// Plays the video at `path` on the given page, stopping the previous
    /// playback.
    ///
    /// Animations loop and stay muted, while videos are played with sound.
    fn play_video(&self, page: &MediaViewerPage, path: &glib::GString, is_animation: bool) {
        let imp = self.imp();

        let Some(media) = self.media() else {
            return;
        };

        // The pipeline of the previous source is freed by clearing the
        // stream before the new source is loaded.
        media.pause();
        media.clear();

        // The page keeps showing its preview until the stream is prepared,
        // so that it doesn't flash black.
        imp.controls.set_media_stream(Some(&media));
        imp.controls.set_visible(true);

        // Swapping the source reconfigures the existing pipeline instead of
        // creating a new one, so that at most one decoder exists in the
        // viewer at any time.
        media.set_muted(is_animation);
        // The looping state is always set anew, so that a looping playback
        // doesn't leak into the next, non-looping one.
        media.set_loop(is_animation);
        media.set_filename(Some(path.as_str()));

        if media.is_prepared() {
            // The stream was already prepared (e.g. the same source is
            // reattached after a clear didn't change the file), so the
            // `prepared` signal won't fire again. The page is notified
            // directly instead.
            page.set_media_paintable(Some(media.upcast_ref()));
            media.play();
        }
    }

    /// Stops the playback and closes the media stream, freeing the
    /// resources of the underlying pipeline.
    ///
    /// Closing the stream is essential: merely pausing it would leave a
    /// full media pipeline behind, whose teardown happens synchronously on
    /// the main thread when the stream is eventually dropped. If such a
    /// leftover pipeline got stuck, the whole UI would freeze.
    fn stop_video(&self) {
        let imp = self.imp();

        if let Some(media) = &*imp.media.borrow() {
            media.pause();
            media.clear();
        }

        imp.controls.set_media_stream(gtk::MediaStream::NONE);
        imp.controls.set_visible(false);
    }
}
