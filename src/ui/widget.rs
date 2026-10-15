//! The toktop card: totals, rate limits, top models and recent sessions,
//! activity heatmap and trend.
//!
//! Every list has a fixed number of row slots built once; updates only
//! change label text and visibility, so a live update never rebuilds
//! widgets and stays cheap. The sessions list grows its slots the first time
//! it is expanded.
//!
//! Rows and chart days open a [`Detail`] card when clicked.

use super::chart::{self, Heatmap, Trend};
use super::detail::{Detail, Selection};
use chrono::{Days, Local};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use toktop::aggregate::{Period, Row, SessionRow, Summary, Usage};
use toktop::config::{self, Sections};
use toktop::format;
use toktop::model::{LimitWindow, Tool};

/// Periods offered by the switcher. Others can still be chosen from the
/// tray menu or the config file.
const SWITCHER: [(Period, &str); 4] = [
    (Period::Today, "DAY"),
    (Period::ThisWeek, "WEEK"),
    (Period::ThisMonth, "MONTH"),
    (Period::AllTime, "ALL"),
];
const TREND_DAYS: u64 = 30;
/// Most sessions listed when the sessions section is expanded.
const MAX_SESSIONS: usize = 100;

#[derive(Clone, Copy, PartialEq)]
enum Breakdown {
    Models,
    Projects,
    Tools,
}

pub(super) fn label(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A label that shrinks with an ellipsis instead of widening the card.
pub(super) fn ellipsized(classes: &[&str]) -> gtk::Label {
    let label = label(classes);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_hexpand(true);
    // Keeps the natural width small so long text cannot widen the card.
    label.set_max_width_chars(1);
    label
}

pub(super) fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub(super) fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// Small colored mark for a model vendor or tool.
pub(super) fn dot() -> gtk::Box {
    gtk::Box::builder()
        .css_classes(["dot"])
        .valign(gtk::Align::Center)
        .build()
}

pub(super) fn set_family(dot: &gtk::Box, family: &str) {
    dot.set_css_classes(&["dot", family]);
}

/// Style class for a model id, by vendor.
pub(super) fn model_family(model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    if name.contains("claude") {
        "claude"
    } else if name.contains("gemini") || name.starts_with("gemma") {
        "gemini"
    } else if name.contains("deepseek") {
        "deepseek"
    } else if model.contains("kimi") {
        "kimi"
    } else if name.starts_with("gpt")
        || name.starts_with("codex")
        || name.starts_with("chatgpt")
        || (name.starts_with('o') && name[1..].starts_with(|c: char| c.is_ascii_digit()))
    {
        "openai"
    } else {
        "other"
    }
}

pub(super) fn tool_family(tool: Tool) -> &'static str {
    match tool {
        Tool::ClaudeCode => "claude",
        Tool::Codex => "openai",
        Tool::Antigravity => "gemini",
        Tool::Pi => "pi",
        Tool::Kimi => "kimi",
    }
}

pub(super) fn section(title: &str) -> gtk::Label {
    let label = label(&["section-title"]);
    label.set_label(title);
    label
}

pub(super) fn section_header(
    title: &impl IsA<gtk::Widget>,
    trailing: &impl IsA<gtk::Widget>,
) -> gtk::Box {
    let header = hbox(8);
    header.add_css_class("section-header");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    trailing.set_halign(gtk::Align::End);
    header.append(title);
    header.append(trailing);
    header
}

pub(super) fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

pub(super) fn display_cost(cost: f64, available: bool) -> String {
    if available {
        format::cost(cost)
    } else {
        "—".to_owned()
    }
}

/// A large token count, compact like `374M` until clicked, then exact like
/// `373,720,902`. Clicking again switches back.
pub(super) struct Count {
    pub label: gtk::Label,
    tokens: Rc<Cell<u64>>,
    exact: Rc<Cell<bool>>,
}

impl Count {
    /// `exact` holds the chosen form, so it can outlive the label.
    pub fn new(class: &str, exact: Rc<Cell<bool>>) -> Count {
        let label = label(&[class]);
        label.set_cursor_from_name(Some("pointer"));
        label.set_tooltip_text(Some("Click to switch between short and exact counts"));
        let tokens: Rc<Cell<u64>> = Rc::default();
        let click = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_PRIMARY)
            .build();
        // The handler must not hold the label: it owns the controller.
        let (target, value, form) = (label.downgrade(), tokens.clone(), exact.clone());
        click.connect_released(move |_, _, _, _| {
            form.set(!form.get());
            if let Some(label) = target.upgrade() {
                Count::show(&label, value.get(), form.get());
            }
        });
        label.add_controller(click);
        Count {
            label,
            tokens,
            exact,
        }
    }

    pub fn set(&self, tokens: u64) {
        self.tokens.set(tokens);
        Count::show(&self.label, tokens, self.exact.get());
    }

    fn show(label: &gtk::Label, tokens: u64, exact: bool) {
        label.set_label(&if exact {
            format::grouped(tokens)
        } else {
            format::tokens(tokens)
        });
    }
}

/// A small captioned statistic.
struct Tile {
    root: gtk::Box,
    value: gtk::Label,
}

impl Tile {
    fn new(caption: &str, tooltip: &str) -> Tile {
        let root = vbox(2);
        root.add_css_class("tile");
        root.set_hexpand(true);
        root.set_tooltip_text(Some(tooltip));
        let heading = label(&["caption"]);
        heading.set_label(caption);
        let value = label(&["tile-value"]);
        root.append(&heading);
        root.append(&value);
        Tile { root, value }
    }
}

/// One rate-limit window: name, share left, bar and reset time.
struct LimitSlot {
    root: gtk::Box,
    name: gtk::Label,
    left: gtk::Label,
    bar: gtk::ProgressBar,
    reset: gtk::Label,
}

impl LimitSlot {
    fn new() -> LimitSlot {
        let root = vbox(3);
        root.set_hexpand(true);
        let top = hbox(6);
        let name = ellipsized(&["dim"]);
        let left = label(&["numeric"]);
        top.append(&name);
        top.append(&left);
        let bar = gtk::ProgressBar::new();
        let reset = label(&["caption"]);
        root.append(&top);
        root.append(&bar);
        root.append(&reset);
        LimitSlot {
            root,
            name,
            left,
            bar,
            reset,
        }
    }

    fn set(&self, window: Option<&LimitWindow>, now_s: i64) {
        let Some(window) = window else {
            self.root.set_visible(false);
            return;
        };
        self.root.set_visible(true);
        let reset_at = window.resets_at.filter(|at| *at > now_s);
        // A window whose reset time has passed starts empty.
        let used = if window.resets_at.is_some() && reset_at.is_none() {
            0.0
        } else {
            window.used_percent.clamp(0.0, 100.0)
        };
        self.name.set_label(&window_name(window.window_minutes));
        self.left.set_label(&format!("{:.0}% left", 100.0 - used));
        self.bar.set_fraction((100.0 - used) / 100.0);
        if used >= 80.0 {
            self.bar.add_css_class("low");
        } else {
            self.bar.remove_css_class("low");
        }
        self.reset.set_label(&match (window.resets_at, reset_at) {
            (_, Some(at)) => format!("Reset {}", format::duration_until(at - now_s)),
            (Some(_), None) => "Reset since update".to_owned(),
            (None, None) => String::new(),
        });
    }
}

pub fn window_name(minutes: u64) -> String {
    match minutes {
        10080 => "Weekly".to_owned(),
        1440 => "Daily".to_owned(),
        m if m > 0 && m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    }
}

/// One model, project or tool row.
struct BreakdownSlot {
    root: gtk::Box,
    dot: gtk::Box,
    name: gtk::Label,
    speed: gtk::Label,
    tokens: gtk::Label,
    share: gtk::Label,
}

impl BreakdownSlot {
    fn new() -> BreakdownSlot {
        let root = hbox(10);
        root.add_css_class("row");
        let dot = dot();
        let name = ellipsized(&[]);
        let speed = label(&["numeric", "dim"]);
        speed.set_xalign(1.0);
        let tokens = label(&["numeric"]);
        tokens.set_width_chars(6);
        tokens.set_xalign(1.0);
        let share = label(&["numeric", "dim"]);
        share.set_width_chars(4);
        share.set_xalign(1.0);
        root.append(&dot);
        root.append(&name);
        root.append(&speed);
        root.append(&tokens);
        root.append(&share);
        BreakdownSlot {
            root,
            dot,
            name,
            speed,
            tokens,
            share,
        }
    }
}

/// One recent session: title and tokens over model, subagents and age.
struct SessionSlot {
    root: gtk::Box,
    dot: gtk::Box,
    title: gtk::Label,
    tokens: gtk::Label,
    detail: gtk::Label,
}

impl SessionSlot {
    fn new() -> SessionSlot {
        let root = vbox(1);
        root.add_css_class("row");
        let top = hbox(10);
        let dot = dot();
        let title = ellipsized(&[]);
        let tokens = label(&["numeric"]);
        top.append(&dot);
        top.append(&title);
        top.append(&tokens);
        let detail = ellipsized(&["caption"]);
        detail.add_css_class("indented");
        root.append(&top);
        root.append(&detail);
        SessionSlot {
            root,
            dot,
            title,
            tokens,
            detail,
        }
    }

    fn set(&self, session: &SessionRow, now_ms: i64) {
        set_family(&self.dot, tool_family(session.tool));
        self.title.set_label(&session.title);
        self.tokens
            .set_label(&format::tokens(session.usage.tokens.total()));
        let mut detail = Vec::new();
        match session.models.as_slice() {
            [] => {}
            [(model, _)] => detail.push(model.clone()),
            models => detail.push(format!("{} models", models.len())),
        }
        match session.subagents {
            0 => {}
            1 => detail.push("1 subagent".to_owned()),
            count => detail.push(format!("{count} subagents")),
        }
        detail.push(format::ago(session.last_activity, now_ms));
        self.detail.set_label(&detail.join(" · "));
    }
}

pub struct Widget {
    pub root: gtk::Box,
    period_buttons: Vec<(Period, gtk::ToggleButton)>,
    live: gtk::Box,
    caption: gtk::Label,
    total: Count,
    detail: gtk::Label,
    notice: gtk::Label,
    speed: Tile,
    rate: Tile,
    cache: Tile,
    streak: Tile,
    sections: Sections,
    limits: gtk::Box,
    limits_title: gtk::Label,
    limits_plan: gtk::Label,
    limit_slots: [LimitSlot; 2],
    breakdown: Cell<Breakdown>,
    breakdown_tabs: Vec<(Breakdown, gtk::ToggleButton)>,
    rows: Vec<BreakdownSlot>,
    rows_empty: gtk::Label,
    /// Sessions listed while collapsed.
    session_limit: usize,
    sessions: RefCell<Vec<SessionSlot>>,
    sessions_list: gtk::Box,
    sessions_scroll: gtk::ScrolledWindow,
    sessions_more: gtk::ToggleButton,
    sessions_empty: gtk::Label,
    sessions_count: gtk::Label,
    detail_card: Detail,
    heatmap: Heatmap,
    active_days: gtk::Label,
    trend: Trend,
    peak: gtk::Label,
    /// Latest summary, kept to redraw the breakdown when its tab changes.
    last: RefCell<Option<Rc<Summary>>>,
}

impl Widget {
    /// `on_period` runs when the user picks a period in the switcher.
    pub fn new(
        selected: Period,
        settings: &config::Widget,
        on_period: impl Fn(Period) + 'static,
    ) -> Rc<Widget> {
        let sections = settings.sections;
        let sessions = settings.shown_sessions();
        let root = vbox(0);
        root.add_css_class("card");
        root.set_size_request(settings.width, -1);

        // Header: logo with live indicator, period switcher.
        let logo = hbox(6);
        let sigma = label(&["logo"]);
        sigma.set_label("Σ");
        let live = dot();
        live.add_css_class("live-dot");
        logo.append(&sigma);
        logo.append(&live);
        let switcher = hbox(0);
        switcher.add_css_class("switcher");
        let mut period_buttons = Vec::new();
        let on_period = Rc::new(on_period);
        for (period, text) in SWITCHER {
            let button = gtk::ToggleButton::with_label(text);
            button.set_tooltip_text(Some(period.label()));
            if let Some((_, first)) = period_buttons.first() {
                button.set_group(Some(first));
            }
            let callback = on_period.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    callback(period);
                }
            });
            switcher.append(&button);
            period_buttons.push((period, button));
        }
        let header = section_header(&logo, &switcher);
        header.add_css_class("top");
        root.append(&header);

        // Hero: total tokens and cost.
        let caption = label(&["caption", "hero-caption"]);
        let total = Count::new("hero", Rc::default());
        total.label.set_halign(gtk::Align::Start);
        let detail = label(&["dim"]);
        let notice = label(&["caption", "notice"]);
        notice.set_wrap(true);
        notice.set_max_width_chars(40);
        notice.set_visible(false);
        for widget in [&caption, &total.label, &detail, &notice] {
            root.append(widget);
        }

        // Statistics tiles.
        let speed = Tile::new(
            "SPEED",
            "Average output speed: output and reasoning tokens per second, \
             from each request to its last output",
        );
        let rate = Tile::new("LIVE", "Tokens per minute over the last five minutes");
        let cache = Tile::new("CACHE", "Share of prompt tokens served from cache");
        let streak = Tile::new("STREAK", "Consecutive days with usage");
        let tiles = hbox(8);
        tiles.add_css_class("tiles");
        tiles.set_homogeneous(true);
        for tile in [&speed, &rate, &cache, &streak] {
            tiles.append(&tile.root);
        }
        tiles.set_visible(sections.stats);
        root.append(&tiles);

        // Codex rate limits.
        let limits = vbox(6);
        limits.add_css_class("section");
        let limits_header = hbox(10);
        let limits_dot = dot();
        set_family(&limits_dot, "openai");
        let limits_title = ellipsized(&[]);
        let limits_plan = label(&["dim"]);
        limits_header.append(&limits_dot);
        limits_header.append(&limits_title);
        limits_header.append(&limits_plan);
        let limit_slots = [LimitSlot::new(), LimitSlot::new()];
        let windows = hbox(18);
        windows.set_homogeneous(true);
        windows.add_css_class("indented");
        for slot in &limit_slots {
            windows.append(&slot.root);
        }
        limits.append(&limits_header);
        limits.append(&windows);
        limits.set_visible(false);
        root.append(&limits);

        // Breakdown by model, project or tool.
        let breakdown_separator = separator();
        breakdown_separator.set_visible(sections.breakdown);
        root.append(&breakdown_separator);
        let tabs = hbox(0);
        tabs.add_css_class("tabs");
        let mut breakdown_tabs = Vec::new();
        for (kind, text) in [
            (Breakdown::Models, "MODELS"),
            (Breakdown::Projects, "PROJECTS"),
            (Breakdown::Tools, "TOOLS"),
        ] {
            let button = gtk::ToggleButton::with_label(text);
            if let Some((_, first)) = breakdown_tabs.first() {
                button.set_group(Some(first));
            }
            button.set_active(kind == Breakdown::Models);
            tabs.append(&button);
            breakdown_tabs.push((kind, button));
        }
        let breakdown = vbox(0);
        breakdown.add_css_class("section");
        breakdown.append(&tabs);
        let rows: Vec<BreakdownSlot> = (0..settings.rows.max(1))
            .map(|_| BreakdownSlot::new())
            .collect();
        for slot in &rows {
            slot.root.set_visible(false);
            breakdown.append(&slot.root);
        }
        let rows_empty = label(&["dim", "empty"]);
        rows_empty.set_label("No usage in this period");
        rows_empty.set_visible(false);
        breakdown.append(&rows_empty);
        breakdown.set_visible(sections.breakdown);
        root.append(&breakdown);

        // Recent sessions.
        let sessions_separator = separator();
        sessions_separator.set_visible(sessions > 0);
        root.append(&sessions_separator);
        let sessions_box = vbox(0);
        sessions_box.add_css_class("section");
        let sessions_count = label(&["caption"]);
        let sessions_more = gtk::ToggleButton::builder()
            .icon_name("pan-down-symbolic")
            .tooltip_text("Show more sessions")
            .css_classes(["icon"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let sessions_trailing = hbox(4);
        sessions_trailing.append(&sessions_count);
        sessions_trailing.append(&sessions_more);
        sessions_box.append(&section_header(&section("SESSIONS"), &sessions_trailing));
        // Expanded, the list scrolls within one row more than it shows
        // collapsed.
        let sessions_list = vbox(0);
        // The scrollbar takes its own column instead of covering the counts.
        let sessions_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .overlay_scrolling(false)
            .propagate_natural_height(true)
            .child(&sessions_list)
            .css_classes(["sessions-scroll"])
            .build();
        sessions_box.append(&sessions_scroll);
        let sessions_empty = label(&["dim", "empty"]);
        sessions_empty.set_label("No sessions in this period");
        sessions_empty.set_visible(false);
        sessions_box.append(&sessions_empty);
        sessions_box.set_visible(sessions > 0);
        root.append(&sessions_box);

        // Activity heatmap, and the trend under the same separator.
        let charts_separator = separator();
        charts_separator.set_visible(sections.activity || sections.trend);
        root.append(&charts_separator);
        let activity = vbox(6);
        activity.add_css_class("section");
        let active_days = label(&["caption"]);
        activity.append(&section_header(&section("ACTIVITY"), &active_days));
        let heatmap = Heatmap::new();
        activity.append(&heatmap.area);
        activity.set_visible(sections.activity);
        root.append(&activity);

        // Trend.
        let trend_box = vbox(6);
        trend_box.add_css_class("section");
        let peak = label(&["caption"]);
        trend_box.append(&section_header(&section("TREND · 30D"), &peak));
        let trend = Trend::new();
        trend_box.append(&trend.area);
        trend_box.set_visible(sections.trend);
        root.append(&trend_box);

        let detail_card = Detail::new(&root, settings.anchor, settings.width);
        let widget = Rc::new(Widget {
            detail_card,
            root,
            period_buttons,
            live,
            caption,
            total,
            detail,
            notice,
            speed,
            rate,
            cache,
            streak,
            sections,
            limits,
            limits_title,
            limits_plan,
            limit_slots,
            breakdown: Cell::new(Breakdown::Models),
            breakdown_tabs,
            rows,
            rows_empty,
            session_limit: sessions,
            sessions: RefCell::default(),
            sessions_list,
            sessions_scroll,
            sessions_more,
            sessions_empty,
            sessions_count,
            heatmap,
            active_days,
            trend,
            peak,
            last: RefCell::default(),
        });
        widget.select_period(selected);
        for _ in 0..sessions {
            widget.add_session_slot();
        }
        widget.connect_details();
        let weak = Rc::downgrade(&widget);
        widget.sessions_more.connect_toggled(move |button| {
            if let Some(widget) = weak.upgrade() {
                widget.set_expanded(button.is_active());
            }
        });
        for (kind, button) in &widget.breakdown_tabs {
            let kind = *kind;
            let weak = Rc::downgrade(&widget);
            button.connect_toggled(move |button| {
                if let (true, Some(widget)) = (button.is_active(), weak.upgrade()) {
                    widget.breakdown.set(kind);
                    let last = widget.last.borrow().clone();
                    if let Some(summary) = last {
                        widget.update_breakdown(&summary);
                    }
                }
            });
        }
        widget
    }

    /// Caps the detail card's height, like the card's own.
    pub fn set_max_height(&self, height: i32) {
        self.detail_card.set_max_height(height);
    }

    /// Opens detail cards from breakdown rows and chart days.
    fn connect_details(self: &Rc<Self>) {
        for (index, slot) in self.rows.iter().enumerate() {
            self.clickable(&slot.root, false, move |widget, summary, _, _| match widget
                .breakdown
                .get()
            {
                Breakdown::Models => summary
                    .by_model
                    .get(index)
                    .map(|row| Selection::Model(row.key.clone())),
                Breakdown::Projects => summary
                    .by_project
                    .get(index)
                    .map(|row| Selection::Project(row.key.clone())),
                Breakdown::Tools => summary
                    .by_tool
                    .get(index)
                    .map(|row| Selection::Tool(row.tool)),
            });
        }
        self.clickable(&self.heatmap.area, true, |widget, _, x, y| {
            widget.heatmap.date_at(x, y).map(Selection::Day)
        });
        self.clickable(&self.trend.area, false, |widget, _, x, _| {
            widget.trend.date_at(x).map(Selection::Day)
        });
    }

    /// Makes `target` open the detail card for what `pick` finds under the
    /// pointer. The item is picked when the button goes down, so a live
    /// update that reorders rows before the release cannot change it.
    /// `at_point` aligns the card with the click instead of all of `target`.
    fn clickable(
        self: &Rc<Self>,
        target: &impl IsA<gtk::Widget>,
        at_point: bool,
        pick: impl Fn(&Widget, &Summary, f64, f64) -> Option<Selection> + 'static,
    ) {
        target.add_css_class("clickable");
        target.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_PRIMARY)
            .build();
        // The handlers must not hold `target`: it owns the controller.
        let picked: Rc<RefCell<Option<(Selection, f64)>>> = Rc::default();
        let weak = Rc::downgrade(self);
        let state = picked.clone();
        click.connect_pressed(move |_, _, x, y| {
            let selection = weak.upgrade().and_then(|widget| {
                let summary = widget.last.borrow().clone()?;
                pick(&widget, &summary, x, y)
            });
            state.replace(selection.map(|selection| (selection, y)));
        });
        let state = picked.clone();
        click.connect_cancel(move |_, _| {
            state.take();
        });
        let weak = Rc::downgrade(self);
        click.connect_released(move |gesture, _, x, y_released| {
            let Some((selection, y)) = picked.take() else {
                return;
            };
            let (Some(widget), Some(source)) = (weak.upgrade(), gesture.widget()) else {
                return;
            };
            // Dragging off the item before releasing cancels the click.
            if !source.contains(x, y_released) {
                return;
            }
            let summary = widget.last.borrow().clone();
            if let Some(summary) = summary {
                widget.show_detail(selection, &source, at_point.then_some(y), &summary);
            }
        });
        target.add_controller(click);
    }

    /// Opens the detail card level with `source`, or with the point `y` in
    /// it.
    fn show_detail(
        &self,
        selection: Selection,
        source: &impl IsA<gtk::Widget>,
        y: Option<f64>,
        summary: &Summary,
    ) {
        let Some(bounds) = source.compute_bounds(&self.root) else {
            return;
        };
        let (top, height) = match y {
            Some(y) => (bounds.y() as f64 + y, 1.0),
            None => (bounds.y() as f64, bounds.height() as f64),
        };
        self.detail_card.show(selection, top, height, summary);
    }

    fn add_session_slot(self: &Rc<Self>) {
        let mut slots = self.sessions.borrow_mut();
        let index = slots.len();
        let slot = SessionSlot::new();
        slot.root.set_visible(false);
        self.sessions_list.append(&slot.root);
        self.clickable(&slot.root, false, move |_, summary, _, _| {
            let session = summary.sessions.get(index)?;
            Some(Selection::Session(session.tool, session.id.clone()))
        });
        slots.push(slot);
    }

    fn set_expanded(self: &Rc<Self>, expanded: bool) {
        self.sessions_more.set_icon_name(if expanded {
            "pan-up-symbolic"
        } else {
            "pan-down-symbolic"
        });
        self.sessions_more.set_tooltip_text(Some(if expanded {
            "Show fewer sessions"
        } else {
            "Show more sessions"
        }));
        if expanded {
            // Rows are one line each, so all have the first one's height.
            let row = self.sessions.borrow().first().map_or(0, |slot| {
                slot.root.measure(gtk::Orientation::Vertical, -1).1
            });
            self.sessions_scroll
                .set_max_content_height(row * (self.session_limit as i32 + 1));
            self.sessions_scroll
                .set_vscrollbar_policy(gtk::PolicyType::Automatic);
        } else {
            self.sessions_scroll
                .set_vscrollbar_policy(gtk::PolicyType::Never);
            self.sessions_scroll.set_max_content_height(-1);
            self.sessions_scroll.vadjustment().set_value(0.0);
        }
        let last = self.last.borrow().clone();
        if let Some(summary) = last {
            self.update_sessions(&summary, toktop::now_ms());
        }
    }

    pub fn select_period(&self, period: Period) {
        let mut matched = false;
        for (candidate, button) in &self.period_buttons {
            if *candidate == period {
                button.set_active(true);
                matched = true;
            }
        }
        if !matched {
            // A period without a button, chosen in the tray or config file.
            for (_, button) in &self.period_buttons {
                button.set_active(false);
            }
        }
    }

    pub fn update(self: &Rc<Self>, summary: &Rc<Summary>) {
        self.last.replace(Some(summary.clone()));
        let now = toktop::now_ms();
        let total = &summary.total;
        let available = summary.prices_available;

        self.caption.set_label(&format!(
            "TOKENS · {}",
            summary.period.label().to_uppercase()
        ));
        self.total.set(total.tokens.total());
        self.detail.set_label(&format!(
            "{} · {} requests",
            display_cost(total.cost, available),
            format::grouped(total.requests)
        ));
        let notice = if !available {
            Some("Costs hidden until the price list downloads".to_owned())
        } else if !summary.unpriced.is_empty() {
            Some(format!(
                "No price for {}; counted as $0",
                summary.unpriced.join(", ")
            ))
        } else {
            None
        };
        self.notice.set_visible(notice.is_some());
        self.notice.set_label(notice.as_deref().unwrap_or_default());

        let live = summary.rate_per_min > 0.0;
        if live {
            self.live.add_css_class("active");
        } else {
            self.live.remove_css_class("active");
        }
        self.live.set_tooltip_text(Some(&if live {
            format!(
                "Live: {} tokens/min",
                format::tokens(summary.rate_per_min as u64)
            )
        } else {
            "Idle for five minutes".to_owned()
        }));

        self.speed
            .value
            .set_label(&total.speed().map_or_else(|| "—".to_owned(), format::speed));
        self.rate.value.set_label(&if live {
            format!("{}/min", format::tokens(summary.rate_per_min as u64))
        } else {
            "idle".to_owned()
        });
        self.cache.value.set_label(
            &total
                .tokens
                .cache_hit_rate()
                .map_or_else(|| "—".to_owned(), format::percent),
        );
        self.streak.value.set_label(&match summary.streak_days {
            1 => "1 day".to_owned(),
            days => format!("{days} days"),
        });

        self.update_limits(summary, now);
        if self.sections.breakdown {
            self.update_breakdown(summary);
        }
        self.update_sessions(summary, now);
        self.update_charts(summary);
        self.detail_card.refresh(summary);
    }

    fn update_limits(&self, summary: &Summary, now_ms: i64) {
        let Some(limits) = summary
            .codex_limits
            .as_ref()
            .filter(|_| self.sections.limits)
        else {
            self.limits.set_visible(false);
            return;
        };
        self.limits.set_visible(true);
        self.limits_title.set_label("Codex");
        self.limits_plan
            .set_label(&limits.plan.as_deref().map(capitalize).unwrap_or_default());
        self.limits.set_tooltip_text(Some(&format!(
            "Codex rate limits, from the latest session log ({})",
            format::ago(limits.observed, now_ms)
        )));
        let now_s = now_ms / 1000;
        self.limit_slots[0].set(limits.primary.as_ref(), now_s);
        self.limit_slots[1].set(limits.secondary.as_ref(), now_s);
    }

    fn update_breakdown(&self, summary: &Summary) {
        let total = summary.total.tokens.total().max(1) as f64;
        let items: Vec<(&str, &str, &Usage)> = match self.breakdown.get() {
            Breakdown::Models => summary
                .by_model
                .iter()
                .map(|row: &Row| (model_family(&row.key), row.label.as_str(), &row.usage))
                .collect(),
            Breakdown::Projects => summary
                .by_project
                .iter()
                .map(|row| ("project", row.label.as_str(), &row.usage))
                .collect(),
            Breakdown::Tools => summary
                .by_tool
                .iter()
                .map(|row| (tool_family(row.tool), row.tool.label(), &row.usage))
                .collect(),
        };
        for (index, slot) in self.rows.iter().enumerate() {
            let Some((family, name, usage)) = items.get(index) else {
                slot.root.set_visible(false);
                continue;
            };
            slot.root.set_visible(true);
            set_family(&slot.dot, family);
            slot.name.set_label(name);
            slot.speed
                .set_label(&usage.speed().map_or_else(String::new, format::speed));
            slot.tokens.set_label(&format::tokens(usage.tokens.total()));
            slot.share
                .set_label(&format::percent(usage.tokens.total() as f64 / total));
        }
        self.rows_empty.set_visible(items.is_empty());
    }

    fn update_sessions(self: &Rc<Self>, summary: &Summary, now_ms: i64) {
        if self.session_limit == 0 {
            return;
        }
        let shown = if self.sessions_more.is_active() {
            summary.sessions.len().min(MAX_SESSIONS)
        } else {
            summary.sessions.len().min(self.session_limit)
        };
        while self.sessions.borrow().len() < shown {
            self.add_session_slot();
        }
        for (index, slot) in self.sessions.borrow().iter().enumerate() {
            match summary.sessions.get(index).filter(|_| index < shown) {
                Some(session) => {
                    slot.root.set_visible(true);
                    slot.set(session, now_ms);
                }
                None => slot.root.set_visible(false),
            }
        }
        self.sessions_more.set_visible(
            self.sessions_more.is_active() || summary.sessions.len() > self.session_limit,
        );
        self.sessions_empty.set_visible(summary.sessions.is_empty());
        self.sessions_count
            .set_label(&match summary.sessions.len() {
                0 => String::new(),
                1 => "1 session".to_owned(),
                count => format!("{count} sessions"),
            });
    }

    fn update_charts(&self, summary: &Summary) {
        let today = Local::now().date_naive();
        let available = summary.prices_available;
        let day = |date, usage: Option<&Usage>| chart::Day {
            date,
            tokens: usage.map_or(0, |usage| usage.tokens.total()),
            cost: available.then(|| usage.map_or(0.0, |usage| usage.cost)),
        };
        if self.sections.activity {
            self.heatmap.set(
                today,
                summary
                    .daily
                    .iter()
                    .map(|row| day(row.date, Some(&row.usage)))
                    .collect(),
            );
            self.active_days.set_label(&match summary.daily.len() {
                1 => "1 active day".to_owned(),
                days => format!("{days} active days"),
            });
        }
        if !self.sections.trend {
            return;
        }
        let trend: Vec<chart::Day> = (0..TREND_DAYS)
            .rev()
            .map(|back| {
                let date = today - Days::new(back);
                let usage = summary
                    .daily
                    .binary_search_by_key(&date, |row| row.date)
                    .ok()
                    .map(|index| &summary.daily[index].usage);
                day(date, usage)
            })
            .collect();
        let peak = trend.iter().map(|day| day.tokens).max().unwrap_or(0);
        self.peak.set_label(&if peak > 0 {
            format!("Peak {}", format::tokens(peak))
        } else {
            String::new()
        });
        self.trend.set(trend);
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
