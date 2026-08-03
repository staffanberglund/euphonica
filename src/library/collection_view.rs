use adw::subclass::prelude::*;
use glib::{Properties, SignalHandlerId, WeakRef, clone, subclass::Signal};
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, TreeListRow, gio, glib,
    prelude::*,
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::OnceLock,
};

use super::Library;
use crate::{
    cache::Cache,
    common::{Album, ArtistInfo, ContentStack, ImageStack, RowAddButtons, Song},
    utils::{LazyInit, SearchableView, format_secs_as_duration},
    window::EuphonicaWindow,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectionNodeKind {
    #[default]
    Section,
    Genre,
    AlbumArtist,
    Album,
    Song,
    Loading,
    Status,
}

mod node_imp {
    use super::*;

    #[derive(Debug)]
    pub struct CollectionNode {
        pub kind: Cell<CollectionNodeKind>,
        pub title: RefCell<String>,
        pub count: Cell<u32>,
        pub album: OnceCell<Album>,
        pub song: OnceCell<Song>,
        pub children: gio::ListStore,
        pub children_loading: Cell<bool>,
        pub children_loaded: Cell<bool>,
    }

    impl Default for CollectionNode {
        fn default() -> Self {
            Self {
                kind: Cell::new(CollectionNodeKind::default()),
                title: RefCell::new(String::new()),
                count: Cell::new(0),
                album: OnceCell::new(),
                song: OnceCell::new(),
                children: gio::ListStore::new::<super::CollectionNode>(),
                children_loading: Cell::new(false),
                children_loaded: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionNode {
        const NAME: &'static str = "EuphonicaCollectionNode";
        type Type = super::CollectionNode;
    }

    impl ObjectImpl for CollectionNode {}
}

glib::wrapper! {
    pub struct CollectionNode(ObjectSubclass<node_imp::CollectionNode>);
}

impl CollectionNode {
    fn group(
        kind: CollectionNodeKind,
        title: impl Into<String>,
        children: &[CollectionNode],
    ) -> Self {
        let node: Self = glib::Object::new();
        node.imp().kind.set(kind);
        *node.imp().title.borrow_mut() = title.into();
        node.imp().count.set(children.len() as u32);
        node.imp().children.extend_from_slice(children);
        node
    }

    fn album(album: &Album) -> Self {
        let node: Self = glib::Object::new();
        node.imp().kind.set(CollectionNodeKind::Album);
        *node.imp().title.borrow_mut() = album.get_title().to_owned();
        let _ = node.imp().album.set(album.clone());
        node.imp()
            .children
            .append(&Self::status(CollectionNodeKind::Loading, "Loading songs…"));
        node
    }

    fn song(song: &Song) -> Self {
        let node: Self = glib::Object::new();
        node.imp().kind.set(CollectionNodeKind::Song);
        *node.imp().title.borrow_mut() = song.get_name().to_owned();
        let _ = node.imp().song.set(song.clone());
        node
    }

    fn status(kind: CollectionNodeKind, title: impl Into<String>) -> Self {
        let node: Self = glib::Object::new();
        node.imp().kind.set(kind);
        *node.imp().title.borrow_mut() = title.into();
        node
    }

    fn kind(&self) -> CollectionNodeKind {
        self.imp().kind.get()
    }

    fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }

    fn count(&self) -> u32 {
        self.imp().count.get()
    }

    fn album_item(&self) -> Option<Album> {
        self.imp().album.get().cloned()
    }

    fn song_item(&self) -> Option<Song> {
        self.imp().song.get().cloned()
    }

    fn children(&self) -> gio::ListStore {
        self.imp().children.clone()
    }

    fn load_songs(&self, library: &Library, tree_row: &TreeListRow, collection: &CollectionView) {
        let imp = self.imp();
        if self.kind() != CollectionNodeKind::Album
            || imp.children_loading.get()
            || imp.children_loaded.get()
        {
            return;
        }
        let Some(album) = self.album_item() else {
            return;
        };

        imp.children_loading.set(true);
        imp.children.remove_all();
        imp.children
            .append(&Self::status(CollectionNodeKind::Loading, "Loading songs…"));

        glib::spawn_future_local(clone!(
            #[strong(rename_to = this)]
            self,
            #[strong]
            library,
            #[weak]
            tree_row,
            #[weak]
            collection,
            async move {
                let mut songs = Vec::new();
                let result = library
                    .get_album_songs(&album, &mut |mut batch| songs.append(&mut batch))
                    .await;
                let imp = this.imp();
                imp.children.remove_all();
                imp.children_loading.set(false);

                match result {
                    Ok(()) => {
                        songs.sort_by(|left, right| {
                            sortable_song_number(left.get_disc())
                                .cmp(&sortable_song_number(right.get_disc()))
                                .then_with(|| {
                                    sortable_song_number(left.get_track())
                                        .cmp(&sortable_song_number(right.get_track()))
                                })
                                .then_with(|| left.get_uri().cmp(right.get_uri()))
                        });
                        if songs.is_empty() {
                            imp.children.append(&Self::status(
                                CollectionNodeKind::Status,
                                "No songs found",
                            ));
                        } else {
                            let nodes = songs.iter().map(Self::song).collect::<Vec<_>>();
                            imp.children.extend_from_slice(&nodes);
                        }
                        imp.children_loaded.set(true);
                    }
                    Err(error) => {
                        eprintln!(
                            "Failed to load songs for Collection album `{}`: {error:?}",
                            album.get_title()
                        );
                        imp.children.append(&Self::status(
                            CollectionNodeKind::Status,
                            "Couldn’t load songs",
                        ));
                    }
                }

                if tree_row.is_expanded() {
                    collection.request_follow_expansion(&tree_row);
                }
            }
        ));
    }
}

fn sortable_song_number(number: i64) -> i64 {
    if number < 0 { i64::MAX } else { number }
}

mod row_imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/collection-row.ui")]
    pub struct CollectionRow {
        #[template_child]
        pub expander: TemplateChild<gtk::TreeExpander>,
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover: TemplateChild<ImageStack>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub row_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub song_index: TemplateChild<gtk::Label>,
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub song_duration: TemplateChild<gtk::Label>,
        #[template_child]
        pub song_actions: TemplateChild<gtk::Box>,
        pub bound_album_id: RefCell<Option<String>>,
        pub bound_artist_name: RefCell<Option<String>>,
        pub expanded_signal: RefCell<Option<(TreeListRow, SignalHandlerId)>>,
        pub album: WeakRef<Album>,
        pub library: WeakRef<Library>,
        pub collection: WeakRef<CollectionView>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionRow {
        const NAME: &'static str = "EuphonicaCollectionRow";
        type Type = super::CollectionRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CollectionRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.cover.set_is_thumbnail(true);

            self.play_button.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |button| {
                    let Some(album) = this.album.upgrade() else {
                        return;
                    };
                    let Some(library) = this.library.upgrade() else {
                        return;
                    };
                    button.set_sensitive(false);
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let Err(error) =
                                library.queue_album(album.clone(), true, true, None).await
                            {
                                eprintln!(
                                    "Failed to play album `{}`: {error:?}",
                                    album.get_title()
                                );
                                if let Some(window) = this.window.upgrade() {
                                    window.send_simple_toast("Failed to play album", 3);
                                }
                            }
                            this.play_button.set_sensitive(true);
                        }
                    ));
                }
            ));
        }

        fn dispose(&self) {
            if let Some((tree_row, signal_id)) = self.expanded_signal.take() {
                tree_row.disconnect(signal_id);
            }
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CollectionRow {}
}

glib::wrapper! {
    pub struct CollectionRow(ObjectSubclass<row_imp::CollectionRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionRow {
    fn new() -> Self {
        glib::Object::new()
    }

    fn setup(&self, library: &Library, collection: &CollectionView, window: &EuphonicaWindow) {
        self.imp().library.set(Some(library));
        self.imp().collection.set(Some(collection));
        self.imp().window.set(Some(window));
    }

    fn bind(&self, tree_row: &TreeListRow, node: &CollectionNode, cache: Rc<Cache>) {
        let imp = self.imp();
        if let Some((old_tree_row, signal_id)) = imp.expanded_signal.take() {
            old_tree_row.disconnect(signal_id);
        }
        imp.expander.set_list_row(Some(tree_row));
        imp.content.remove_css_class("collection-section");
        imp.content.remove_css_class("collection-group");
        imp.content.remove_css_class("collection-album");
        imp.content.remove_css_class("collection-song");
        imp.content.set_margin_top(4);
        imp.content.set_margin_bottom(4);
        imp.title.remove_css_class("heading");
        imp.title.remove_css_class("caption-heading");
        imp.subtitle.set_visible(false);
        imp.cover.set_visible(false);
        imp.icon.set_visible(false);
        imp.avatar.set_visible(false);
        imp.avatar
            .set_custom_image(Option::<&gtk::gdk::Texture>::None);
        imp.avatar.set_text(None);
        imp.song_index.set_visible(false);
        imp.quality_grade.set_visible(false);
        imp.song_duration.set_visible(false);
        while let Some(child) = imp.song_actions.first_child() {
            imp.song_actions.remove(&child);
        }
        imp.song_actions.set_visible(false);
        imp.count.set_visible(false);
        imp.play_button.set_visible(false);
        imp.play_button.set_sensitive(true);
        imp.row_spinner.set_visible(false);
        imp.expander.set_hide_expander(false);
        imp.bound_album_id.take();
        imp.bound_artist_name.take();
        imp.album.set(None);

        if matches!(
            node.kind(),
            CollectionNodeKind::Genre | CollectionNodeKind::AlbumArtist | CollectionNodeKind::Album
        ) {
            let expansion_node = node.clone();
            let signal_id = tree_row.connect_expanded_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |tree_row| {
                    if !tree_row.is_expanded() {
                        return;
                    }
                    let imp = this.imp();
                    let Some(collection) = imp.collection.upgrade() else {
                        return;
                    };
                    collection.request_follow_expansion(tree_row);
                    if expansion_node.kind() == CollectionNodeKind::Album
                        && let Some(library) = imp.library.upgrade()
                    {
                        expansion_node.load_songs(&library, tree_row, &collection);
                    }
                }
            ));
            imp.expanded_signal
                .replace(Some((tree_row.clone(), signal_id)));
        }

        match node.kind() {
            CollectionNodeKind::Section => {
                imp.expander.set_hide_expander(true);
                imp.content.add_css_class("collection-section");
                imp.title.add_css_class("heading");
                imp.title.set_label(&node.title());
            }
            CollectionNodeKind::Genre => {
                imp.content.add_css_class("collection-group");
                imp.title.add_css_class("heading");
                imp.title.set_label(&node.title());
                imp.icon.set_icon_name(Some("music-note-single-symbolic"));
                imp.icon.set_visible(true);
                imp.count.set_label(&node.count().to_string());
                imp.count.set_visible(true);
            }
            CollectionNodeKind::AlbumArtist => {
                let artist_name = node.title();
                imp.content.add_css_class("collection-group");
                imp.title.add_css_class("caption-heading");
                imp.title.set_label(&artist_name);
                imp.icon.set_icon_name(Some("music-artist-symbolic"));
                imp.icon.set_visible(true);
                imp.avatar.set_text(Some(&artist_name));
                imp.bound_artist_name.replace(Some(artist_name.clone()));
                imp.count.set_label(&node.count().to_string());
                imp.count.set_visible(true);

                glib::spawn_future_local(clone!(
                    #[weak(rename_to = this)]
                    self,
                    async move {
                        let artist = ArtistInfo::new(&artist_name, None, false);
                        let result = cache.clone().get_artist_avatar(&artist, true, false).await;
                        if this
                            .imp()
                            .bound_artist_name
                            .borrow()
                            .as_deref()
                            .is_some_and(|current| current == artist_name)
                        {
                            match result {
                                Ok(Some(texture)) => {
                                    this.imp().avatar.set_custom_image(Some(&texture));
                                    this.imp().icon.set_visible(false);
                                    this.imp().avatar.set_visible(true);
                                }
                                Ok(None) => {}
                                Err(error) => eprintln!(
                                    "Failed to load Collection avatar for `{artist_name}`: {error:?}"
                                ),
                            }
                        }
                    }
                ));
            }
            CollectionNodeKind::Album => {
                let album = node.album_item().expect("Album nodes contain an album");
                let album_id = album.get_info().get_comp_id().to_owned();
                let title = album
                    .get_release_date()
                    .map(|date| format!("{} – {}", date.year(), album.get_title()))
                    .unwrap_or_else(|| album.get_title().to_owned());
                imp.content.add_css_class("collection-album");
                imp.title.set_label(&title);
                imp.cover.set_visible(true);
                imp.play_button.set_visible(true);
                imp.album.set(Some(&album));
                imp.cover.show_spinner();
                imp.bound_album_id.replace(Some(album_id.clone()));

                if tree_row.is_expanded()
                    && let Some(library) = imp.library.upgrade()
                    && let Some(collection) = imp.collection.upgrade()
                {
                    node.load_songs(&library, tree_row, &collection);
                }

                glib::spawn_future_local(clone!(
                    #[weak(rename_to = this)]
                    self,
                    async move {
                        let result = cache.clone().get_album_cover(album.get_info(), true).await;
                        if this
                            .imp()
                            .bound_album_id
                            .borrow()
                            .as_deref()
                            .is_some_and(|current| current == album_id)
                        {
                            match result {
                                Ok(Some(texture)) => this.imp().cover.show(&texture),
                                Ok(None) => this.imp().cover.clear(),
                                Err(error) => {
                                    this.imp().cover.clear();
                                    eprintln!(
                                        "Failed to load Collection cover for `{}`: {error:?}",
                                        album.get_title()
                                    );
                                }
                            }
                        }
                    }
                ));
            }
            CollectionNodeKind::Song => {
                let song = node.song_item().expect("Song nodes contain a song");
                let track = song.get_track();
                let disc = song.get_disc();
                let index = match (disc, track) {
                    (disc, track) if disc > 1 && track >= 0 => format!("{disc}.{track}"),
                    (_, track) if track >= 0 => track.to_string(),
                    _ => String::new(),
                };
                imp.expander.set_hide_expander(true);
                imp.content.add_css_class("collection-song");
                imp.content.set_margin_top(2);
                imp.content.set_margin_bottom(2);
                imp.song_index.set_label(&index);
                imp.song_index.set_visible(!index.is_empty());
                imp.title.set_label(song.get_name());
                let quality_icon = song.get_quality_grade().to_icon_name();
                imp.quality_grade.set_icon_name(quality_icon);
                imp.quality_grade.set_visible(quality_icon.is_some());
                imp.song_duration
                    .set_label(&format_secs_as_duration(song.get_duration() as f64));
                imp.song_duration.set_visible(true);
                if let Some(library) = imp.library.upgrade() {
                    let end_widget = RowAddButtons::new(&library);
                    end_widget.set_song(Some(&song));
                    imp.song_actions.append(&end_widget);
                    imp.song_actions.set_visible(true);
                }
            }
            CollectionNodeKind::Loading => {
                imp.expander.set_hide_expander(true);
                imp.content.add_css_class("collection-song");
                imp.title.set_label(&node.title());
                imp.title.add_css_class("caption-heading");
                imp.row_spinner.set_visible(true);
            }
            CollectionNodeKind::Status => {
                imp.expander.set_hide_expander(true);
                imp.content.add_css_class("collection-song");
                imp.title.set_label(&node.title());
                imp.title.add_css_class("caption-heading");
                imp.icon.set_icon_name(Some("dialog-information-symbolic"));
                imp.icon.set_visible(true);
            }
        }
    }

    fn unbind(&self) {
        let imp = self.imp();
        if let Some((tree_row, signal_id)) = imp.expanded_signal.take() {
            tree_row.disconnect(signal_id);
        }
        imp.bound_album_id.take();
        imp.bound_artist_name.take();
        imp.album.set(None);
        while let Some(child) = imp.song_actions.first_child() {
            imp.song_actions.remove(&child);
        }
        imp.expander.set_list_row(None);
        imp.cover.clear();
        imp.avatar
            .set_custom_image(Option::<&gtk::gdk::Texture>::None);
    }
}

#[derive(Default)]
struct AlbumArtistBucket {
    title: String,
    sort_title: String,
    albums: Vec<Album>,
}

#[derive(Default)]
struct GenreBucket {
    title: String,
    // Like collect_albumartists(), use the MBID when known and the readable
    // name otherwise. The album loader has already reconciled missing MBIDs
    // where a name maps unambiguously to one known identity.
    album_artists: BTreeMap<String, AlbumArtistBucket>,
}

mod imp {
    use super::*;

    #[derive(Default, Debug, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::CollectionView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/collection-view.ui")]
    pub struct CollectionView {
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub stack: TemplateChild<ContentStack>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,
        pub roots: OnceCell<gio::ListStore>,
        pub tree: OnceCell<gtk::TreeListModel>,
        pub library: WeakRef<Library>,
        pub initializing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionView {
        const NAME: &'static str = "EuphonicaCollectionView";
        type Type = super::CollectionView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for CollectionView {
        fn constructed(&self) {
            self.parent_constructed();
            self.stack.show_spinner();

            self.obj()
                .bind_property("collapsed", &self.show_sidebar.get(), "visible")
                .sync_create()
                .build();
            self.show_sidebar.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.obj().emit_by_name::<()>("show-sidebar-clicked", &[])
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CollectionView {}
}

glib::wrapper! {
    pub struct CollectionView(ObjectSubclass<imp::CollectionView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for CollectionView {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectionView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn setup(&self, library: &Library, cache: Rc<Cache>, window: &EuphonicaWindow) {
        self.imp().library.set(Some(library));

        let roots = gio::ListStore::new::<CollectionNode>();
        let tree = gtk::TreeListModel::new(roots.clone(), false, false, |item| {
            let node = item.downcast_ref::<CollectionNode>()?;
            (node.children().n_items() > 0).then(|| node.children().upcast::<gio::ListModel>())
        });
        let selection = SingleSelection::new(Some(tree));
        self.imp().list_view.set_model(Some(&selection));
        self.imp()
            .roots
            .set(roots)
            .expect("Collection model already set");
        self.imp()
            .tree
            .set(
                selection
                    .model()
                    .and_downcast::<gtk::TreeListModel>()
                    .expect("Collection selection wraps a TreeListModel"),
            )
            .expect("Collection tree already set");

        let factory = SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[strong]
            library,
            #[weak(rename_to = collection)]
            self,
            #[weak]
            window,
            move |_, object| {
                let item = object.downcast_ref::<ListItem>().expect("Needs a ListItem");
                let row = CollectionRow::new();
                row.setup(&library, &collection, &window);
                item.set_child(Some(&row));
            }
        ));
        factory.connect_bind(clone!(
            #[strong]
            cache,
            move |_, object| {
                let item = object.downcast_ref::<ListItem>().expect("Needs a ListItem");
                let tree_row = item
                    .item()
                    .and_downcast::<TreeListRow>()
                    .expect("Collection model contains TreeListRows");
                let node = tree_row
                    .item()
                    .and_downcast::<CollectionNode>()
                    .expect("Tree row contains a CollectionNode");
                let row = item
                    .child()
                    .and_downcast::<CollectionRow>()
                    .expect("Collection factory creates CollectionRows");
                row.bind(&tree_row, &node, cache.clone());
            }
        ));
        factory.connect_unbind(|_, object| {
            let item = object.downcast_ref::<ListItem>().expect("Needs a ListItem");
            if let Some(row) = item.child().and_downcast::<CollectionRow>() {
                row.unbind();
            }
        });
        self.imp().list_view.set_factory(Some(&factory));

        self.imp().list_view.connect_activate(clone!(
            #[weak]
            window,
            move |view, position| {
                let Some(tree_row) = view
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<TreeListRow>()
                else {
                    return;
                };
                let Some(node) = tree_row.item().and_downcast::<CollectionNode>() else {
                    return;
                };
                if let Some(album) = node.album_item() {
                    window.goto_album_from_collection(&album);
                } else if matches!(
                    node.kind(),
                    CollectionNodeKind::Genre | CollectionNodeKind::AlbumArtist
                ) {
                    tree_row.set_expanded(!tree_row.is_expanded());
                }
            }
        ));

        let search_bar = self.imp().search_bar.get();
        let search_entry = self.imp().search_entry.get();
        search_bar.connect_entry(&search_entry);
        self.imp()
            .search_btn
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();
        search_entry.connect_search_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.rebuild()
        ));

        library.albums().connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| {
                let loading = this.imp().initializing.get()
                    || this
                        .imp()
                        .library
                        .upgrade()
                        .is_some_and(|library| library.albums_and_albumartists_initializing());
                if loading {
                    this.imp().stack.show_spinner();
                } else {
                    this.rebuild();
                }
            }
        ));
    }

    pub fn search_bar(&self) -> gtk::SearchBar {
        self.imp().search_bar.get()
    }

    fn request_follow_expansion(&self, tree_row: &TreeListRow) {
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            tree_row,
            move || this.scroll_to_last_descendant(&tree_row)
        ));
    }

    fn scroll_to_last_descendant(&self, expanded_row: &TreeListRow) {
        if !expanded_row.is_expanded() {
            return;
        }
        let Some(tree) = self.imp().tree.get() else {
            return;
        };
        let Some(parent_position) = self.tree_row_position(expanded_row) else {
            return;
        };

        let parent_depth = expanded_row.depth();
        let mut last_descendant = parent_position;
        for position in (parent_position + 1)..tree.n_items() {
            let Some(row) = tree.item(position).and_downcast::<TreeListRow>() else {
                break;
            };
            if row.depth() <= parent_depth {
                break;
            }
            last_descendant = position;
        }

        if last_descendant > parent_position {
            let list_view = self.imp().list_view.get();
            list_view.scroll_to(last_descendant, gtk::ListScrollFlags::NONE, None);

            // Let GTK reveal the final child first. On the following frame,
            // ensure that did not scroll the expanded parent out of view. If
            // the whole range fits this is a no-op; otherwise it caps the
            // movement with the parent row at the top of the viewport.
            list_view.add_tick_callback(clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                expanded_row,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move |_, _| {
                    glib::idle_add_local_once(clone!(
                        #[weak]
                        this,
                        #[weak]
                        expanded_row,
                        move || {
                            if expanded_row.is_expanded()
                                && let Some(position) = this.tree_row_position(&expanded_row)
                            {
                                this.imp().list_view.scroll_to(
                                    position,
                                    gtk::ListScrollFlags::NONE,
                                    None,
                                );
                            }
                        }
                    ));
                    glib::ControlFlow::Break
                }
            ));
        }
    }

    fn tree_row_position(&self, target: &TreeListRow) -> Option<u32> {
        let tree = self.imp().tree.get()?;
        (0..tree.n_items()).find(|position| {
            tree.item(*position)
                .and_downcast::<TreeListRow>()
                .is_some_and(|row| row == *target)
        })
    }

    fn rebuild(&self) {
        let Some(roots) = self.imp().roots.get() else {
            return;
        };
        let Some(library) = self.imp().library.upgrade() else {
            return;
        };
        let query = self.imp().search_entry.text().to_lowercase();
        let mut genres: BTreeMap<String, GenreBucket> = BTreeMap::new();

        for album in library.albums().iter::<Album>().flatten() {
            let album_artists = collection_album_artists(&album);
            let artist_matches_query = album_artists
                .iter()
                .any(|artist| artist.name.to_lowercase().contains(&query));
            let mut album_genres: Vec<String> = album.get_genres().iter().cloned().collect();
            if album_genres.is_empty() {
                album_genres.push("Unknown Genre".to_owned());
            }
            album_genres.sort_by_key(|value| value.to_lowercase());
            let year = album
                .get_release_date()
                .map(|date| date.year().to_string())
                .unwrap_or_default();

            for genre in album_genres {
                if !query.is_empty()
                    && !genre.to_lowercase().contains(&query)
                    && !artist_matches_query
                    && !album.get_title().to_lowercase().contains(&query)
                    && !year.contains(&query)
                {
                    continue;
                }

                let genre_key = genre.to_lowercase();
                let genre_bucket = genres.entry(genre_key).or_insert_with(|| GenreBucket {
                    title: genre,
                    ..Default::default()
                });
                for artist in &album_artists {
                    let album_artist_bucket = genre_bucket
                        .album_artists
                        .entry(artist.get_comp_id().to_owned())
                        .or_insert_with(|| AlbumArtistBucket {
                            title: artist.name.clone(),
                            sort_title: artist
                                .sort_tag
                                .as_deref()
                                .unwrap_or(&artist.name)
                                .to_owned(),
                            ..Default::default()
                        });
                    album_artist_bucket.albums.push(album.clone());
                }
            }
        }

        roots.remove_all();
        if let Some(tree) = self.imp().tree.get() {
            tree.set_autoexpand(false);
        }
        let mut previous_section: Option<String> = None;
        for (_, genre) in genres {
            let section = section_for(&genre.title);
            if previous_section.as_deref() != Some(section.as_str()) {
                roots.append(&CollectionNode::group(
                    CollectionNodeKind::Section,
                    section.clone(),
                    &[],
                ));
                previous_section = Some(section);
            }

            let mut album_artists = genre.album_artists.into_values().collect::<Vec<_>>();
            album_artists.sort_by(|left, right| {
                left.sort_title
                    .to_lowercase()
                    .cmp(&right.sort_title.to_lowercase())
                    .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
            });
            let mut album_artist_nodes = Vec::with_capacity(album_artists.len());
            for mut album_artist in album_artists {
                album_artist.albums.sort_by(|left, right| {
                    left.get_release_date()
                        .map(|date| date.year())
                        .unwrap_or(i32::MAX)
                        .cmp(
                            &right
                                .get_release_date()
                                .map(|date| date.year())
                                .unwrap_or(i32::MAX),
                        )
                        .then_with(|| {
                            left.get_sortable_title()
                                .to_lowercase()
                                .cmp(&right.get_sortable_title().to_lowercase())
                        })
                        .then_with(|| left.get_folder_uri().cmp(right.get_folder_uri()))
                });
                let albums = album_artist
                    .albums
                    .iter()
                    .map(CollectionNode::album)
                    .collect::<Vec<_>>();
                album_artist_nodes.push(CollectionNode::group(
                    CollectionNodeKind::AlbumArtist,
                    album_artist.title,
                    &albums,
                ));
            }
            roots.append(&CollectionNode::group(
                CollectionNodeKind::Genre,
                genre.title,
                &album_artist_nodes,
            ));
        }

        if !query.is_empty()
            && let Some(tree) = self.imp().tree.get()
        {
            // Reveal matching albums, but stop before the Album level so a
            // search does not fetch every matching album's tracks.
            let mut position = 0;
            while position < tree.n_items() {
                if let Some(tree_row) = tree.item(position).and_downcast::<TreeListRow>()
                    && let Some(node) = tree_row.item().and_downcast::<CollectionNode>()
                    && matches!(
                        node.kind(),
                        CollectionNodeKind::Genre | CollectionNodeKind::AlbumArtist
                    )
                {
                    tree_row.set_expanded(true);
                }
                position += 1;
            }
        }

        if roots.n_items() > 0 {
            self.imp().stack.show_content();
        } else {
            self.imp().stack.show_placeholder();
        }
    }
}

fn collection_album_artists(album: &Album) -> Vec<ArtistInfo> {
    let mut artists = BTreeMap::new();
    for artist in album
        .get_artists()
        .iter()
        .filter(|artist| !artist.name.trim().is_empty())
    {
        artists
            .entry(artist.get_comp_id().to_owned())
            .or_insert_with(|| artist.clone());
    }

    if artists.is_empty() {
        let unknown = ArtistInfo::new("Unknown Album Artist", None, false);
        artists.insert(unknown.get_comp_id().to_owned(), unknown);
    }

    artists.into_values().collect()
}

fn section_for(value: &str) -> String {
    value
        .trim_start()
        .chars()
        .next()
        .filter(|character| character.is_alphabetic())
        .map(|character| character.to_uppercase().collect())
        .unwrap_or_else(|| "#".to_owned())
}

impl LazyInit for CollectionView {
    fn populate(&self) {
        let Some(library) = self.imp().library.upgrade() else {
            return;
        };
        if self.imp().initializing.get() {
            return;
        }
        if self
            .imp()
            .roots
            .get()
            .is_some_and(|roots| roots.n_items() > 0)
        {
            self.imp().stack.show_content();
            return;
        }
        if library.albums().n_items() > 0 {
            self.rebuild();
            return;
        }

        self.imp().initializing.set(true);
        self.imp().stack.show_spinner();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Err(error) = library.init_albums_and_albumartists().await {
                    eprintln!("Failed to populate Collection: {error:?}");
                }
                this.rebuild();
                this.imp().initializing.set(false);
            }
        ));
    }
}

impl SearchableView for CollectionView {
    fn trigger_search(&self) {
        self.imp().search_bar.set_search_mode(true);
    }
}

#[cfg(test)]
mod tests {
    use super::section_for;

    #[test]
    fn section_labels_are_case_insensitive_and_unicode_aware() {
        assert_eq!(section_for("yacht rock"), "Y");
        assert_eq!(section_for("  ångest"), "Å");
        assert_eq!(section_for("1980s"), "#");
    }
}
