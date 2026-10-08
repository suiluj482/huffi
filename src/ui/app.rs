use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ui::config::UiConfig;
use gtk4::cairo;
use gtk4::gdk;
use gtk4::glib;
use gtk4::pango;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GBox, Button, DrawingArea, Entry, EventControllerKey, EventControllerScroll,
    EventControllerScrollFlags, GestureClick, GestureDrag, Image, Label, Orientation, Overflow,
    Overlay, PropagationPhase, Separator, Window,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use huffi::engine::Engine;
use huffi::engine::provider::{EntryMeta, Icon, QuerySuggestion, is_detail_key};
use huffi::engine::provider::{PreprocessedQuery, ProviderMeta, matches_prefix, matches_query};
use huffi::engine::scoring::Scored;

use crate::ui::control::{self, ControlRequest};
use crate::ui::{tasks, theme};

const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(300);

/// Every widget id [`Launcher::build_row`] binds a field onto.
///
/// A template may declare any of these plus whatever structural containers it
/// likes, but a shipped template that misspells one of these silently loses the
/// binding — a `subtile` label renders empty and nothing complains. Keeping the
/// list here, next to the bindings that consume it, lets the theme tests check
/// the shipped templates against it.
#[cfg(test)]
pub(crate) const KNOWN_WIDGET_IDS: &[&str] = &[
    "row",
    "clickable",
    "icon",
    "title-area",
    "title",
    "subtitle",
    "comment",
    "scores",
    "score-base",
    "score-history",
    "boost",
    "delete",
];

#[derive(Debug, Clone, Copy)]
enum Step {
    Next,
    Prev,
}

#[derive(Debug, Clone, Copy)]
enum ModifyKind {
    Boost,
    Delete,
}

/// Control messages forwarded from the socket threads to the main GTK loop.
enum ControlMsg {
    Show(String),
    Hide,
    Toggle(String),
    Quit,
}

/// A render-ready row, cut from the engine's full ranked result set as the
/// `page_size` window around the current selection.
#[derive(Debug, Clone)]
struct Row {
    entry_id: String,
    provider_id: Option<String>,
    history_key: Option<String>,
    base_score: f64,
    history_score: Option<f64>,
    title: String,
    subtitle: Option<String>,
    comment: Option<String>,
    details: BTreeMap<String, String>,
    variant: Option<String>,
    icon: Option<Icon>,
    set_query: Option<QuerySuggestion>,
    /// The suggestion the row's [`Action::SetQuery`] applies on Enter —
    /// resolved with the active prefix when the row is submitted, keeping
    /// the launcher open. Independent of `set_query`, which Tab applies.
    ///
    /// [`Action::SetQuery`]: crate::engine::provider::Action::SetQuery
    action_set_query: Option<QuerySuggestion>,
}

impl From<Scored<EntryMeta>> for Row {
    fn from(scored: Scored<EntryMeta>) -> Self {
        let entry = scored.entry;
        let action_set_query = entry.action.query_suggestion().cloned();
        Self {
            entry_id: entry.id,
            provider_id: entry.provider_id,
            history_key: scored.history_key,
            base_score: scored.base_score,
            history_score: scored.history_score,
            title: entry.title,
            subtitle: entry.subtitle,
            comment: entry.comment,
            details: entry.details,
            variant: entry.variant,
            icon: entry.icon,
            set_query: entry.set_query,
            action_set_query,
        }
    }
}

/// Widgets of one rendered row that change appearance with selection, kept so
/// selection moves can toggle CSS classes in place instead of rebuilding every
/// row on each step.
struct BuiltRow {
    row: GBox,
    title: Label,
    sub: Option<Label>,
    comment: Option<Label>,
    scores: Vec<Label>,
}

/// A footer entry: the provider id its chip scopes to, and its display text.
type FooterChip = (String, String);

struct State {
    query: String,
    /// The query's resolved prefixing, from the engine: which prefix scopes
    /// the results (a declared trigger like `=`, or a `\<id> ` target carrying
    /// its separator) and whether that scope is exclusive. Feeds both the
    /// badge and the footer's active/inactive split. `None` until the first
    /// fetch returns.
    active_pre: Option<PreprocessedQuery>,
    providers: Vec<ProviderMeta>,
    /// Delimiter introducing a `\<id> ` provider target, from the engine.
    target_prefix: String,
    entries: Vec<Row>,
    rows: Vec<BuiltRow>,
    total: usize,
    selected: usize,
    last_click: Option<(usize, Instant)>,
    fetch_id: u64,
}

pub struct Launcher {
    window: Window,
    entry: Entry,
    list: GBox,
    rail: DrawingArea,
    badge_box: GBox,
    badge_label: Label,
    footer: GBox,
    footer_active: GBox,
    footer_idle: GBox,
    backdrop: GBox,
    engine: Arc<Mutex<Engine>>,
    visible: Arc<AtomicBool>,
    state: RefCell<State>,
    page_size: usize,
    icon_size: i32,
    theme: theme::Theme,
}

impl Launcher {
    pub fn new(
        listener: UnixListener,
        engine: Arc<Mutex<Engine>>,
        main_loop: glib::MainLoop,
        ui: UiConfig,
    ) -> Rc<Self> {
        let theme = theme::Theme::new(ui.theme.as_str());
        if let Some(display) = gdk::Display::default() {
            theme::load_css(&display, &theme);
        }

        let window = Window::new();
        window.set_decorated(false);

        let entry = Entry::new();
        entry.set_placeholder_text(Some("Type to search..."));
        entry.add_css_class("huffi-entry");
        entry.set_hexpand(true);
        entry.set_valign(Align::Center);

        let badge_label = Label::new(None);
        badge_label.add_css_class("badge");
        let badge_box = GBox::new(Orientation::Horizontal, 0);
        badge_box.append(&badge_label);
        badge_box.set_valign(Align::Center);
        badge_box.set_visible(false);

        let top = GBox::new(Orientation::Horizontal, 8);
        top.append(&entry);
        top.append(&badge_box);

        let sep1 = Separator::new(Orientation::Horizontal);

        let list = GBox::new(Orientation::Vertical, 0);
        list.set_hexpand(true);
        list.set_vexpand(true);
        list.set_margin_top(2);
        list.set_margin_bottom(2);
        list.set_margin_start(2);
        list.set_margin_end(2);

        let rail = DrawingArea::new();
        rail.set_width_request(6);
        rail.set_vexpand(true);

        let middle = GBox::new(Orientation::Horizontal, 0);
        middle.append(&list);
        middle.append(&rail);

        let sep2 = Separator::new(Orientation::Horizontal);

        // The footer splits providers by whether they answer the current
        // query: active ones hug the left in the accent colour, the rest sit
        // on the right in the muted footer colour as a reminder of the
        // prefixes available. Each provider is its own clickable chip that
        // scopes the query to it; the groups clip instead of overflowing.
        let footer_active = GBox::new(Orientation::Horizontal, 8);
        footer_active.add_css_class("footer");
        footer_active.set_halign(Align::Start);
        footer_active.set_hexpand(true);
        footer_active.set_overflow(Overflow::Hidden);

        let footer_idle = GBox::new(Orientation::Horizontal, 8);
        footer_idle.add_css_class("footer");
        footer_idle.set_halign(Align::End);
        footer_idle.set_hexpand(false);
        footer_idle.set_overflow(Overflow::Hidden);

        let footer = GBox::new(Orientation::Horizontal, 12);
        footer.append(&footer_active);
        footer.append(&footer_idle);

        let panel = GBox::new(Orientation::Vertical, 0);
        panel.add_css_class("panel");
        panel.append(&top);
        panel.append(&sep1);
        panel.append(&middle);
        panel.append(&sep2);
        panel.append(&footer);
        panel.set_size_request(ui.width, ui.height);
        panel.set_hexpand(false);
        panel.set_vexpand(false);
        panel.set_halign(Align::Center);
        panel.set_valign(Align::Center);

        let backdrop = GBox::new(Orientation::Vertical, 0);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);

        let overlay = Overlay::new();
        overlay.set_child(Some(&backdrop));
        overlay.add_overlay(&panel);
        window.set_child(Some(&overlay));

        if gtk4_layer_shell::is_supported() {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_namespace(Some("huffi"));
            window.set_keyboard_mode(KeyboardMode::Exclusive);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
        } else {
            window.set_default_size(ui.width, ui.height);
        }

        let this = Rc::new(Self {
            window,
            entry,
            list,
            rail,
            badge_box,
            badge_label,
            footer,
            footer_active,
            footer_idle,
            backdrop,
            engine,
            visible: Arc::new(AtomicBool::new(false)),
            state: RefCell::new(State {
                query: String::new(),
                active_pre: None,
                providers: Vec::new(),
                target_prefix: String::new(),
                entries: Vec::new(),
                rows: Vec::new(),
                total: 0,
                selected: 0,
                last_click: None,
                fetch_id: 0,
            }),
            page_size: ui.page_size,
            icon_size: ui.icon_size,
            theme,
        });

        this.attach_handlers(listener, main_loop);

        tasks::run_blocking(
            {
                let engine = Arc::clone(&this.engine);
                move || {
                    let engine = engine.lock().unwrap();
                    (engine.providers(), engine.target_prefix().to_string())
                }
            },
            {
                let weak = Rc::downgrade(&this);
                move |(providers, target_prefix)| {
                    if let Some(this) = weak.upgrade() {
                        {
                            let mut st = this.state.borrow_mut();
                            st.providers = providers;
                            st.target_prefix = target_prefix;
                        }
                        this.render_list();
                    }
                }
            },
        );

        this
    }

    pub fn show_with_query(self: &Rc<Self>, query: String) {
        {
            let mut st = self.state.borrow_mut();
            st.query = query.clone();
            st.active_pre = None;
            st.entries.clear();
            st.total = 0;
            st.selected = 0;
            st.last_click = None;
        }
        if self.entry.text() != query {
            self.entry.set_text(&query);
            self.entry.set_position(-1);
        }
        self.render_list();
        self.visible.store(true, Ordering::Relaxed);
        self.window.present();
        self.entry.grab_focus();
        self.fetch_page();
    }

    fn attach_handlers(self: &Rc<Self>, listener: UnixListener, main_loop: glib::MainLoop) {
        {
            let weak = Rc::downgrade(self);
            self.entry.connect_changed(move |_| {
                let Some(this) = weak.upgrade() else { return };
                let text = this.entry.text().to_string();
                if this.state.borrow().query != text {
                    this.set_query(text);
                }
            });
        }

        {
            let weak = Rc::downgrade(self);
            let click = GestureClick::new();
            click.set_button(1);
            click.connect_pressed(move |_, _, _, _| {
                if let Some(this) = weak.upgrade() {
                    this.dismiss();
                }
            });
            self.backdrop.add_controller(click);
        }

        let keys = EventControllerKey::new();
        keys.set_propagation_phase(PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _code, _mods| {
                let Some(this) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match key {
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        this.submit();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Escape => {
                        this.dismiss();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Down => {
                        this.select_step(Step::Next);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Up => {
                        this.select_step(Step::Prev);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                        this.apply_suggestion();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        self.window.add_controller(keys);

        let scroll = EventControllerScroll::new(
            EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::DISCRETE,
        );
        {
            let weak = Rc::downgrade(self);
            scroll.connect_scroll(move |_, _dx, dy| {
                if let Some(this) = weak.upgrade() {
                    this.scroll_step(dy);
                }
                glib::Propagation::Proceed
            });
        }
        self.window.add_controller(scroll);

        {
            let weak = Rc::downgrade(self);
            self.window.connect_is_active_notify(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if !this.window.is_active() && this.window.is_visible() {
                    this.dismiss();
                }
            });
        }

        {
            let weak = Rc::downgrade(self);
            self.window.connect_close_request(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.dismiss();
                }
                glib::Propagation::Stop
            });
        }

        {
            let weak = Rc::downgrade(self);
            self.rail.set_draw_func(move |_da, cr, width, height| {
                if let Some(this) = weak.upgrade() {
                    this.draw_rail(cr, width, height);
                }
            });
        }

        let rail_click = GestureClick::new();
        rail_click.set_button(1);
        {
            let weak = Rc::downgrade(self);
            rail_click.connect_pressed(move |_, _n_press, _x, y| {
                if let Some(this) = weak.upgrade() {
                    this.rail_clicked(y);
                }
            });
        }
        self.rail.add_controller(rail_click);

        let rail_drag = GestureDrag::new();
        rail_drag.set_button(1);
        {
            let weak = Rc::downgrade(self);
            rail_drag.connect_drag_update(move |drag, _offset_x, offset_y| {
                if let Some(this) = weak.upgrade()
                    && let Some((_start_x, start_y)) = drag.start_point()
                {
                    this.rail_dragged(start_y + offset_y);
                }
            });
        }
        self.rail.add_controller(rail_drag);

        let (tx, rx) = async_channel::unbounded::<ControlMsg>();
        {
            let weak = Rc::downgrade(self);
            let quit_loop = main_loop.clone();
            glib::spawn_future_local(async move {
                while let Ok(msg) = rx.recv().await {
                    let Some(this) = weak.upgrade() else {
                        continue;
                    };
                    match msg {
                        ControlMsg::Show(query) => this.show_with_query(query),
                        ControlMsg::Hide => {
                            if this.window.is_visible() {
                                this.dismiss();
                            }
                        }
                        ControlMsg::Toggle(query) => {
                            if this.window.is_visible() {
                                this.dismiss();
                            } else {
                                this.show_with_query(query);
                            }
                        }
                        ControlMsg::Quit => quit_loop.quit(),
                    }
                }
            });
        }
        let visible = Arc::clone(&self.visible);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(mut stream) => {
                        let tx = tx.clone();
                        let visible = Arc::clone(&visible);
                        std::thread::spawn(move || {
                            let Ok(Some(req)) = control::read_request(&stream) else {
                                return;
                            };
                            match req {
                                ControlRequest::Show { query } => {
                                    let _ = tx.send_blocking(ControlMsg::Show(query));
                                }
                                ControlRequest::Hide => {
                                    let _ = tx.send_blocking(ControlMsg::Hide);
                                }
                                ControlRequest::Toggle { query } => {
                                    let _ = tx.send_blocking(ControlMsg::Toggle(
                                        query.unwrap_or_default(),
                                    ));
                                }
                                ControlRequest::Quit => {
                                    let _ = tx.send_blocking(ControlMsg::Quit);
                                }
                                ControlRequest::Status => {
                                    let resp = control::ControlResponse {
                                        visible: visible.load(Ordering::Relaxed),
                                    };
                                    let _ = control::write_response(&mut stream, &resp);
                                }
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
        });
    }

    fn page(&self) -> usize {
        self.state.borrow().selected / self.page_size
    }

    fn set_query(self: &Rc<Self>, text: String) {
        {
            let mut st = self.state.borrow_mut();
            if st.query == text {
                return;
            }
            st.query = text.clone();
            st.selected = 0;
            st.entries.clear();
        }
        if self.entry.text() != text {
            self.entry.set_text(&text);
            self.entry.set_position(-1);
        }
        self.render_list();
        self.fetch_page();
    }

    /// Move the selection and refresh only what needs to change: the window is
    /// refetched when the selection lands on a different page, otherwise just
    /// the highlight classes are updated.
    fn move_selection(self: &Rc<Self>, new_selected: usize) {
        let old_selected = {
            let st = self.state.borrow();
            st.selected
        };
        if old_selected == new_selected {
            return;
        }
        let old_page = old_selected / self.page_size;
        self.state.borrow_mut().selected = new_selected;
        if self.page() != old_page {
            // Clear the current window so a stale page is never shown (and
            // never submitted) while the new one is being fetched.
            self.state.borrow_mut().entries.clear();
            self.render_list();
            self.fetch_page();
        } else {
            self.apply_selection();
        }
    }

    fn select_step(self: &Rc<Self>, dir: Step) {
        let (total, len, selected) = {
            let st = self.state.borrow();
            (st.total, st.entries.len(), st.selected)
        };
        if len == 0 {
            return;
        }
        let new_selected = match dir {
            Step::Next => {
                if selected + 1 < total {
                    selected + 1
                } else {
                    0
                }
            }
            Step::Prev => {
                if selected == 0 {
                    return;
                }
                selected - 1
            }
        };
        self.move_selection(new_selected);
    }

    fn scroll_step(self: &Rc<Self>, dy: f64) {
        let (total, selected) = {
            let st = self.state.borrow();
            (st.total, st.selected)
        };
        if total == 0 {
            return;
        }
        let new_selected = if dy > 0.0 {
            if selected + 1 < total {
                selected + 1
            } else {
                return;
            }
        } else if dy < 0.0 && selected > 0 {
            selected - 1
        } else {
            return;
        };
        self.move_selection(new_selected);
    }

    fn apply_suggestion(self: &Rc<Self>) {
        let (len, selected) = {
            let st = self.state.borrow();
            (st.entries.len(), st.selected)
        };
        if len == 0 {
            return;
        }
        let local = selected % self.page_size;
        let suggestion = {
            let st = self.state.borrow();
            // The active prefix carries a target's separator (`"\calc "`), so a
            // keeping-prefix suggestion lands under it (`\calc 42`) rather than
            // in the malformed `\calc42`. Resolved here rather than per row per
            // keystroke, because only this one row is ever applied.
            let prefix = st.active_pre.as_ref().and_then(|pre| pre.prefix.as_deref());
            st.entries
                .get(local)
                .and_then(|row| row.set_query.as_ref().map(|s| s.resolve(prefix)))
        };
        if let Some(suggestion) = suggestion {
            self.set_query(suggestion);
        }
    }

    fn row_pressed(self: &Rc<Self>, index: usize) {
        let total = self.state.borrow().total;
        if index >= total {
            return;
        }
        let now = Instant::now();
        let is_double = matches!(
            self.state.borrow().last_click,
            Some((prev_index, prev_time))
                if prev_index == index && now.duration_since(prev_time) < DOUBLE_CLICK_INTERVAL
        );
        self.state.borrow_mut().last_click = Some((index, now));
        self.move_selection(index);

        if is_double {
            self.submit();
        }
    }

    fn submit(self: &Rc<Self>) {
        let hit = {
            let st = self.state.borrow();
            let local = st.selected % self.page_size;
            st.entries.get(local).cloned()
        };
        if let Some(hit) = hit {
            let engine = Arc::clone(&self.engine);
            let query = self.state.borrow().query.clone();
            let entry_id = hit.entry_id.clone();
            let work = move || {
                engine.lock().unwrap().select(&query, &entry_id);
            };
            match hit.action_set_query.clone() {
                // Enter applied a query suggestion: like Tab, the launcher
                // stays open and the suggestion goes in after the selection
                // has been recorded.
                Some(suggestion) => {
                    let weak = Rc::downgrade(self);
                    tasks::run_blocking(work, move |_| {
                        let Some(this) = weak.upgrade() else { return };
                        let prefix = this
                            .state
                            .borrow()
                            .active_pre
                            .as_ref()
                            .and_then(|pre| pre.prefix.clone());
                        this.set_query(suggestion.resolve(prefix.as_deref()));
                    });
                }
                None => {
                    tasks::run_blocking(work, |_| {});
                    self.dismiss();
                }
            }
        }
    }

    fn dismiss(&self) {
        self.visible.store(false, Ordering::Relaxed);
        self.window.set_visible(false);
    }

    fn modify_history(self: &Rc<Self>, index: usize, kind: ModifyKind) {
        let history_key = {
            let st = self.state.borrow();
            st.entries.get(index).and_then(|h| h.history_key.clone())
        };
        let Some(history_key) = history_key else {
            return;
        };

        let query = self.state.borrow().query.clone();
        let offset = self.page() * self.page_size;
        let page_size = self.page_size;
        let id = {
            let mut st = self.state.borrow_mut();
            st.fetch_id += 1;
            st.fetch_id
        };
        let weak = Rc::downgrade(self);
        tasks::run_blocking(
            {
                let engine = Arc::clone(&self.engine);
                move || {
                    let mut engine = engine.lock().unwrap();
                    match kind {
                        ModifyKind::Boost => {
                            engine.boost(&query, &history_key);
                        }
                        ModifyKind::Delete => {
                            engine.delete(&query, &history_key);
                        }
                    }
                    Self::fetch_window(&mut engine, &query, offset, page_size)
                }
            },
            move |(prefix, entries, total)| {
                if let Some(this) = weak.upgrade()
                    && id == this.state.borrow().fetch_id
                {
                    this.apply_entries(prefix, entries, total);
                }
            },
        );
    }

    /// Run the query and cut the `page_size` window at `offset` out of the
    /// full ranked result set. Repeated queries for the same input are served
    /// from the engine's cache, so only the visible rows are cloned.
    fn fetch_window(
        engine: &mut Engine,
        query: &str,
        offset: usize,
        page_size: usize,
    ) -> (PreprocessedQuery, Vec<Row>, usize) {
        let reply = engine.query(query);
        let total = reply.scored.len();
        let entries = reply
            .scored
            .iter()
            .skip(offset)
            .take(page_size)
            .cloned()
            .map(Row::from)
            .collect();
        (reply.pre.clone(), entries, total)
    }

    fn fetch_page(self: &Rc<Self>) {
        let (query, offset, id) = {
            let mut st = self.state.borrow_mut();
            (
                st.query.clone(),
                (st.selected / self.page_size) * self.page_size,
                {
                    st.fetch_id += 1;
                    st.fetch_id
                },
            )
        };
        let page_size = self.page_size;
        let weak = Rc::downgrade(self);
        tasks::run_blocking(
            {
                let engine = Arc::clone(&self.engine);
                move || {
                    let mut engine = engine.lock().unwrap();
                    Self::fetch_window(&mut engine, &query, offset, page_size)
                }
            },
            move |(pre, entries, total)| {
                if let Some(this) = weak.upgrade()
                    && id == this.state.borrow().fetch_id
                {
                    this.apply_entries(pre, entries, total);
                }
            },
        );
    }

    fn apply_entries(self: &Rc<Self>, pre: PreprocessedQuery, entries: Vec<Row>, total: usize) {
        {
            let mut st = self.state.borrow_mut();
            st.active_pre = Some(pre);
            st.entries = entries;
            st.total = total;
        }
        self.render_list();
    }

    /// The footer text for one provider: its name, or `name: prefixes` when
    /// it declares triggers.
    fn footer_label(meta: &ProviderMeta) -> String {
        if meta.prefixes.is_empty() {
            meta.name.clone()
        } else {
            format!("{}: {}", meta.name, meta.prefixes.join(", "))
        }
    }

    /// Split providers into the footer's two groups — the ones answering the
    /// current query and the rest — as `(id, label)` pairs the renderer turns
    /// into clickable chips. Disabled providers are omitted entirely. A provider
    /// is active exactly when [`matches_query`] holds, the same gate dispatch
    /// uses; with no resolved query yet, none are active.
    fn provider_footer_entries(
        providers: &[ProviderMeta],
        pre: Option<&PreprocessedQuery>,
        target_prefix: &str,
    ) -> (Vec<FooterChip>, Vec<FooterChip>) {
        let mut active = Vec::new();
        let mut idle = Vec::new();
        for p in providers {
            if !p.enabled {
                continue;
            }
            let entry = (p.id.clone(), Self::footer_label(p));
            if pre.is_some_and(|pre| matches_query(p, target_prefix, pre)) {
                active.push(entry);
            } else {
                idle.push(entry);
            }
        }
        (active, idle)
    }

    /// The query that scopes `typed` to provider `id`: swap the active prefix
    /// or `\<id> ` target for this provider's target, keeping whatever text
    /// followed. A fresh query keeps all of its text, so clicking a provider
    /// after typing `fire` yields `\desktop fire`; an empty query yields just
    /// the target token (`\desktop `).
    fn scoped_query(
        target_prefix: &str,
        id: &str,
        typed: &str,
        active: Option<&PreprocessedQuery>,
    ) -> String {
        let rest = match active.and_then(|pre| pre.prefix.as_deref()) {
            Some(prefix) => typed.strip_prefix(prefix).unwrap_or(typed),
            None => typed,
        };
        format!("{target_prefix}{id} {}", rest.trim_start())
    }

    /// Scope the query to the provider with this id, moving the caret to the
    /// end and keeping keyboard focus in the entry.
    fn scope_to_provider(self: &Rc<Self>, id: &str) {
        let (target_prefix, active) = {
            let st = self.state.borrow();
            (st.target_prefix.clone(), st.active_pre.clone())
        };
        let typed = self.entry.text();
        let text = Self::scoped_query(&target_prefix, id, &typed, active.as_ref());
        self.set_query(text);
        self.entry.grab_focus();
    }

    /// A borderless footer chip that scopes the query to `id` when clicked.
    fn footer_chip(self: &Rc<Self>, id: &str, text: &str, active: bool) -> Button {
        let button = Button::with_label(text);
        button.add_css_class("footer-provider");
        if active {
            button.add_css_class("footer-active");
        }
        button.set_can_focus(false);
        button.set_overflow(Overflow::Hidden);
        button.set_cursor_from_name(Some("pointer"));
        button.set_tooltip_text(Some(&format!("Scope to {text}")));
        if let Some(label) = button.child().and_then(|c| c.downcast::<Label>().ok()) {
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_single_line_mode(true);
        }
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.scope_to_provider(&id);
            }
        });
        button
    }

    fn render_list(self: &Rc<Self>) {
        let (page_base, entries, selected) = {
            let st = self.state.borrow();
            (
                (st.selected / self.page_size) * self.page_size,
                st.entries.clone(),
                st.selected,
            )
        };

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let local_sel = selected
            .checked_sub(page_base)
            .filter(|local| *local < entries.len());
        let mut rows = Vec::with_capacity(entries.len());
        for (i, hit) in entries.iter().enumerate() {
            let row = self.build_row(hit, local_sel == Some(i), page_base + i, i);
            self.list.append(&row.row);
            rows.push(row);
        }
        self.state.borrow_mut().rows = rows;

        let (pre, providers, target_prefix) = {
            let st = self.state.borrow();
            (
                st.active_pre.clone(),
                st.providers.clone(),
                st.target_prefix.clone(),
            )
        };
        match pre.as_ref().and_then(|pre| pre.prefix.as_deref()) {
            // Label with the provider owning the prefix: the target of a
            // `\<id> ` query, or the provider that declared it.
            Some(pfx) => {
                let label = match providers
                    .iter()
                    .find(|p| matches_prefix(p, &target_prefix, pfx))
                {
                    Some(p) => format!("{}  {}", pfx.trim_end(), p.name),
                    None => pfx.to_string(),
                };
                self.badge_label.set_text(&label);
                self.badge_box.set_visible(true);
            }
            None => self.badge_box.set_visible(false),
        }

        // Split by whether the provider answers this query. Active ones stay in
        // the accent colour on the left; the rest are the muted reminder of
        // what prefixes are available on the right. Every chip scopes the
        // query to its provider when clicked. Disabled providers are omitted
        // entirely, so a footer with nothing to show is hidden.
        let (active, idle) =
            Self::provider_footer_entries(&providers, pre.as_ref(), &target_prefix);
        if active.is_empty() && idle.is_empty() {
            self.footer.set_visible(false);
        } else {
            for group in [&self.footer_active, &self.footer_idle] {
                while let Some(child) = group.first_child() {
                    group.remove(&child);
                }
            }
            for (id, text) in &active {
                self.footer_active.append(&self.footer_chip(id, text, true));
            }
            for (id, text) in &idle {
                self.footer_idle.append(&self.footer_chip(id, text, false));
            }
            // The active group stays visible even when empty: it expands to
            // push the idle chips to the right edge.
            self.footer_active.set_visible(true);
            self.footer_idle.set_visible(!idle.is_empty());
            self.footer.set_visible(true);
        }

        self.rail.queue_draw();
    }

    /// Toggle the selection CSS classes on the already-built rows. Called on
    /// any in-page selection move, avoiding a full widget rebuild per step.
    fn apply_selection(self: &Rc<Self>) {
        let (page_base, selected) = {
            let st = self.state.borrow();
            ((st.selected / self.page_size) * self.page_size, st.selected)
        };
        let rows = self.state.borrow();
        for (i, row) in rows.rows.iter().enumerate() {
            let selected = page_base + i == selected;
            toggle_selected(&row.row, selected, "row-selected");
            toggle_selected(&row.title, selected, "title-selected");
            if let Some(sub) = &row.sub {
                toggle_selected(sub, selected, "subtitle-selected");
            }
            if let Some(comment) = &row.comment {
                toggle_selected(comment, selected, "comment-selected");
            }
            for score in &row.scores {
                toggle_selected(score, selected, "score-selected");
            }
        }
        drop(rows);
        self.rail.queue_draw();
    }

    fn build_row(
        self: &Rc<Self>,
        hit: &Row,
        is_selected: bool,
        global_index: usize,
        local_index: usize,
    ) -> BuiltRow {
        // Instantiate the theme's row template (variant- and
        // provider-specific when the theme ships one) and bind the entry's
        // fields onto the widgets it declares. Unknown ids are fine; optional
        // widgets are skipped.
        let xml = self
            .theme
            .entry_template(hit.provider_id.as_deref(), hit.variant.as_deref());
        let builder = gtk4::Builder::from_string(&xml);

        let row = builder
            .object::<GBox>("row")
            .unwrap_or_else(|| GBox::new(Orientation::Horizontal, 0));
        row.set_valign(Align::Center);
        state_class(&row, is_selected, "row", "row-selected");
        if let Some(id) = &hit.provider_id {
            row.add_css_class(&format!("provider-{id}"));
            if let Some(variant) = &hit.variant {
                row.add_css_class(&format!("provider-{id}-{variant}"));
            }
        }
        // `variant-<name>` is provider-independent, so a shared
        // `variants/<name>/entry.ui` layout can be styled for every provider
        // that reports that variant.
        if let Some(variant) = &hit.variant {
            row.add_css_class(&format!("variant-{variant}"));
        }

        // A template that omits `title` shows no title. The detached label is
        // still returned so `apply_selection` has something to toggle, but it
        // is deliberately not appended anywhere: it would otherwise land in
        // `row` instead of `title-area`, outside the `clickable` target.
        let title = builder
            .object::<Label>("title")
            .unwrap_or_else(|| Label::new(Some("")));
        // The template supplies the label, not its text: every shipped template
        // declares `title` with no `label` property, so the text has to come
        // from here or the row renders blank.
        title.set_text(&hit.title);
        state_class(&title, is_selected, "title", "title-selected");
        title.set_ellipsize(pango::EllipsizeMode::End);
        title.set_halign(Align::Start);
        title.set_xalign(0.0);

        if let Some(icon) = builder.object::<Image>("icon") {
            self.bind_icon(&icon, hit);
        }

        if let Some(area) = builder.object::<GBox>("title-area") {
            area.set_hexpand(true);
        }

        // The subtitle is optional: only rendered when the entry has one AND
        // the template declares a `subtitle` widget.
        let sub = if let Some(sub) = &hit.subtitle {
            title.set_hexpand(false);
            builder.object::<Label>("subtitle").inspect(|sub_label| {
                sub_label.set_text(sub);
                state_class(sub_label, is_selected, "subtitle", "subtitle-selected");
                sub_label.set_halign(Align::Start);
                sub_label.set_visible(true);
            })
        } else {
            title.set_hexpand(true);
            None
        };

        // The comment is optional in the same way the subtitle is: shown only
        // when the entry carries one *and* the template declares a `comment`
        // widget. It holds prose rather than a value, so it belongs below the
        // title and reads as muted context.
        let comment = hit.comment.as_ref().and_then(|text| {
            builder.object::<Label>("comment").inspect(|label| {
                label.set_text(text);
                state_class(label, is_selected, "comment", "comment-selected");
                label.set_halign(Align::Start);
                label.set_visible(true);
            })
        });

        let mut scores = Vec::new();
        if let Some(base) = builder.object::<Label>("score-base") {
            base.set_text(&format!("{:.2}", hit.base_score));
            state_class(&base, is_selected, "score", "score-selected");
            scores.push(base);
        }
        if let Some(h) = hit.history_score
            && let Some(history) = builder.object::<Label>("score-history")
        {
            history.set_text(&format!("{h:.2}"));
            state_class(&history, is_selected, "score", "score-selected");
            history.set_visible(true);
            scores.push(history);
        }

        // Named display fields bind onto `detail-<key>` widgets. The entry
        // drives this loop, so a template that doesn't declare a widget for
        // some key simply doesn't show that field, and a widget whose key the
        // entry doesn't carry stays hidden. Selection state is handled purely
        // in CSS (`.row-selected .detail`), so there's nothing to track here.
        //
        // A key that can't be a GTK object id is skipped rather than looked up
        // as-is: `EntryBuilder::detail` debug-asserts on it, so this only
        // happens in a release build whose provider is already broken, and
        // dropping the field is better than looking up an id we can't trust.
        for (key, value) in &hit.details {
            if !is_detail_key(key) {
                continue;
            }
            let id = format!("detail-{key}");
            if let Some(label) = builder.object::<Label>(&id) {
                label.set_text(value);
                label.add_css_class("detail");
                label.set_visible(true);
            }
        }

        if let Some(clickable) = builder.object::<GBox>("clickable") {
            clickable.set_cursor_from_name(Some("pointer"));
            let weak = Rc::downgrade(self);
            let click = GestureClick::new();
            click.connect_pressed(move |_, _n_press, _x, _y| {
                if let Some(this) = weak.upgrade() {
                    this.row_pressed(global_index);
                }
            });
            clickable.add_controller(click);
        }

        let boost = builder.object::<Button>("boost");
        let delete = builder.object::<Button>("delete");
        if hit.history_key.is_some() {
            if let Some(boost) = &boost {
                boost.add_css_class("flat-btn");
                boost.set_visible(true);
                let weak = Rc::downgrade(self);
                boost.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.modify_history(local_index, ModifyKind::Boost);
                    }
                });
            }
            if let Some(delete) = &delete {
                delete.add_css_class("flat-btn");
                delete.set_visible(true);
                let weak = Rc::downgrade(self);
                delete.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.modify_history(local_index, ModifyKind::Delete);
                    }
                });
            }
        } else {
            if let Some(boost) = &boost {
                boost.set_visible(false);
            }
            if let Some(delete) = &delete {
                delete.set_visible(false);
            }
        }

        BuiltRow {
            row,
            title,
            sub,
            comment,
            scores,
        }
    }

    /// Point a template `icon` widget at the entry's icon, or leave it blank
    /// (reserving `icon_size` pixels) when there is none or it fails to load.
    fn bind_icon(&self, image: &Image, hit: &Row) {
        image.set_pixel_size(self.icon_size);
        match &hit.icon {
            Some(Icon::Name(name)) => image.set_icon_name(Some(name)),
            Some(Icon::Path(path)) if path.exists() => {
                let file = gtk4::gio::File::for_path(path);
                if let Ok(texture) = gdk::Texture::from_file(&file) {
                    image.set_paintable(Some(&texture));
                }
            }
            _ => {}
        }
    }

    fn draw_rail(&self, cr: &cairo::Context, width: i32, height: i32) {
        let (total, selected) = {
            let st = self.state.borrow();
            (st.total, st.selected)
        };
        if total == 0 || height <= 0 {
            return;
        }
        let height = height as f64;
        let bar_height = height / total as f64;
        let max_pos = (height - bar_height).max(0.0);
        let bar_y = if total <= 1 {
            0.0
        } else {
            selected as f64 / (total - 1) as f64 * max_pos
        };

        rounded_rect(cr, 0.0, bar_y, width as f64, bar_height, 3.0);
        let (r, g, b) = theme::accent(&self.rail.style_context());
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
    }

    fn rail_dragged(self: &Rc<Self>, y: f64) {
        self.rail_select_at(y);
    }

    fn rail_clicked(self: &Rc<Self>, y: f64) {
        self.rail_select_at(y);
    }

    fn rail_select_at(self: &Rc<Self>, y: f64) {
        let total = self.state.borrow().total;
        if total == 0 {
            return;
        }
        let height = self.rail.allocation().height() as f64;
        if height <= 0.0 {
            return;
        }
        let bar_height = height / total as f64;
        let max_pos = (height - bar_height).max(0.0);
        if max_pos <= 0.0 {
            return;
        }
        let pos = (y - bar_height * 0.5).clamp(0.0, max_pos);
        let new_selected = ((pos / max_pos * (total - 1) as f64).round() as usize).min(total - 1);
        self.move_selection(new_selected);
    }
}

/// Give a widget the classes for its role and, when the row is selected, for
/// its state.
///
/// The two **coexist** rather than replace one another: `title` styles every
/// title and `title-selected` layers the selected state on top. That keeps a
/// rule from having to be restated per state — `.title { font-size: 17px }`
/// covers both — and it means a state rule can say only what actually differs,
/// instead of repeating the role's declarations to beat it.
fn state_class(
    widget: &impl IsA<gtk4::Widget>,
    is_selected: bool,
    role_class: &str,
    selected_class: &str,
) {
    widget.add_css_class(role_class);
    if is_selected {
        widget.add_css_class(selected_class);
    }
}

/// Move a widget in and out of the selected state, leaving its role class on.
///
/// The counterpart to [`state_class`] for rows that already exist: selection
/// moves far more often than rows are rebuilt, so this touches one class and
/// leaves the row's markup and styling otherwise intact.
fn toggle_selected(widget: &impl IsA<gtk4::Widget>, is_selected: bool, selected_class: &str) {
    if is_selected {
        widget.add_css_class(selected_class);
    } else {
        widget.remove_css_class(selected_class);
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2 * 3.0,
    );
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, name: &str, prefixes: &[&str], prefix_only: bool) -> ProviderMeta {
        ProviderMeta {
            id: id.into(),
            name: name.into(),
            prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
            enabled: true,
            prefix_only,
        }
    }

    fn pre(prefix: Option<&str>, exclusive: bool) -> PreprocessedQuery {
        PreprocessedQuery {
            original_query: String::new(),
            prefix: prefix.map(String::from),
            exclusive,
            query: String::new(),
        }
    }

    fn chip(id: &str, label: &str) -> (String, String) {
        (id.to_string(), label.to_string())
    }

    #[test]
    fn footer_splits_active_from_idle() {
        let providers = vec![
            meta("desktop", "Desktop", &[], false),
            meta("calc", "Calc", &["="], true),
        ];
        let split = |pre: Option<&PreprocessedQuery>| {
            Launcher::provider_footer_entries(&providers, pre, "\\")
        };

        // No resolved query yet: nothing is marked active.
        assert_eq!(
            split(None),
            (
                Vec::<(String, String)>::new(),
                vec![chip("desktop", "Desktop"), chip("calc", "Calc: =")]
            )
        );

        // An unscoped query leaves the unprefixed provider active, the
        // prefix-only one idle.
        let unscoped = pre(None, false);
        assert_eq!(
            split(Some(&unscoped)),
            (
                vec![chip("desktop", "Desktop")],
                vec![chip("calc", "Calc: =")]
            )
        );

        // A target scopes to one provider alone.
        let target = pre(Some("\\calc "), true);
        assert_eq!(
            split(Some(&target)),
            (
                vec![chip("calc", "Calc: =")],
                vec![chip("desktop", "Desktop")]
            )
        );

        // An exclusive declared prefix does the same.
        let exclusive = pre(Some("="), true);
        assert_eq!(
            split(Some(&exclusive)),
            (
                vec![chip("calc", "Calc: =")],
                vec![chip("desktop", "Desktop")]
            )
        );
    }

    #[test]
    fn footer_omits_disabled_providers() {
        let mut disabled = meta("calc", "Calc", &["="], true);
        disabled.enabled = false;
        let providers = vec![meta("desktop", "Desktop", &[], false), disabled];
        let (active, idle) = Launcher::provider_footer_entries(&providers, None, "\\");
        assert!(active.is_empty());
        assert_eq!(idle, vec![chip("desktop", "Desktop")]);
    }

    #[test]
    fn scoped_query_swaps_prefix_and_keeps_text() {
        // Unscoped text is kept behind the new target.
        assert_eq!(
            Launcher::scoped_query("\\", "desktop", "fire", None),
            "\\desktop fire"
        );
        // An empty query is just the target token, trailing space included.
        assert_eq!(
            Launcher::scoped_query("\\", "desktop", "", None),
            "\\desktop "
        );
        // A `\<id> ` target is replaced, its body kept.
        let target = pre(Some("\\calc "), true);
        assert_eq!(
            Launcher::scoped_query("\\", "desktop", "\\calc 2", Some(&target)),
            "\\desktop 2"
        );
        // A declared prefix is replaced, its text kept (leading space trimmed).
        let declared = pre(Some("="), true);
        assert_eq!(
            Launcher::scoped_query("\\", "desktop", "= 2 + 2", Some(&declared)),
            "\\desktop 2 + 2"
        );
        // The configured target delimiter is honoured.
        assert_eq!(
            Launcher::scoped_query("@", "desktop", "fire", None),
            "@desktop fire"
        );
    }
}
