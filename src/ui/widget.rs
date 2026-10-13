//! The toktop card: totals, rate limits, top models and recent sessions,
//! activity heatmap and trend.
//!
//! Every list has a fixed number of row slots built once; updates only
//! change label text and visibility, so a live update never rebuilds
//! widgets and stays cheap.

use super::chart::{self, Heatmap, Trend};
use chrono::{Days, Local};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use toktop::aggregate::{Period, Row, SessionRow, Summary, Usage};
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

#[derive(Clone, Copy, PartialEq)]
enum Breakdown {
    Models,
    Projects,
    Tools,
}

fn label(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A label that shrinks with an ellipsis instead of widening the card.
fn ellipsized(classes: &[&str]) -> gtk::Label {
    let label = label(classes);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_hexpand(true);
    // Keeps the natural width small so long text cannot widen the card.
    label.set_max_width_chars(1);
    label
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// Small colored mark for a model vendor or tool.
fn dot() -> gtk::Box {
    gtk::Box::builder()
        .css_classes(["dot"])
        .valign(gtk::Align::Center)
        .build()
}

fn set_family(dot: &gtk::Box, family: &str) {
    dot.set_css_classes(&["dot", family]);
}

/// Style class for a model id, by vendor.
fn model_family(model: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    if name.contains("claude") {
        "claude"
    } else if name.contains("gemini") || name.starts_with("gemma") {
        "gemini"
    } else if name.contains("deepseek") {
        "deepseek"
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

fn tool_family(tool: Tool) -> &'static str {
    match tool {
        Tool::ClaudeCode => "claude",
        Tool::Codex => "openai",
        Tool::Antigravity => "gemini",
    }
}

fn section(title: &str) -> gtk::Label {
    let label = label(&["section-title"]);
    label.set_label(title);
    label
}

fn section_header(title: &impl IsA<gtk::Widget>, trailing: &impl IsA<gtk::Widget>) -> gtk::Box {
    let header = hbox(8);
    header.add_css_class("section-header");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    trailing.set_halign(gtk::Align::End);
    header.append(title);
    header.append(trailing);
    header
}

fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

fn display_cost(cost: f64, available: bool) -> String {
    if available {
        format::cost(cost)
    } else {
        "—".to_owned()
    }
}

/// Multi-line usage details for a tooltip.
fn usage_tooltip(title: &str, usage: &Usage, prices_available: bool) -> String {
    let tokens = &usage.tokens;
    let mut lines = vec![
        title.to_owned(),
        format!(
            "{} tokens · {} · {} requests",
            format::grouped(tokens.total()),
            display_cost(usage.cost, prices_available),
            format::grouped(usage.requests)
        ),
    ];
    let parts: Vec<String> = [
        ("Input", tokens.input),
        ("Output", tokens.output),
        ("Reasoning", tokens.reasoning),
        ("Cache read", tokens.cache_read),
        ("Cache write", tokens.cache_write),
    ]
    .iter()
    .filter(|(_, value)| *value > 0)
    .map(|(name, value)| format!("{name} {}", format::tokens(*value)))
    .collect();
    if !parts.is_empty() {
        lines.push(parts.join(" · "));
    }
    let mut extra = Vec::new();
    if let Some(rate) = tokens.cache_hit_rate() {
        extra.push(format!("{} cache hits", format::percent(rate)));
    }
    if let Some(speed) = usage.speed() {
        extra.push(format::speed(speed));
    }
    if !extra.is_empty() {
        lines.push(extra.join(" · "));
    }
    lines.join("\n")
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
    tokens: gtk::Label,
    share: gtk::Label,
}

impl BreakdownSlot {
    fn new() -> BreakdownSlot {
        let root = hbox(10);
        root.add_css_class("row");
        let dot = dot();
        let name = ellipsized(&[]);
        let tokens = label(&["numeric"]);
        let share = label(&["numeric", "dim"]);
        share.set_width_chars(4);
        share.set_xalign(1.0);
        root.append(&dot);
        root.append(&name);
        root.append(&tokens);
        root.append(&share);
        BreakdownSlot {
            root,
            dot,
            name,
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

    fn set(&self, session: &SessionRow, now_ms: i64, prices_available: bool) {
        set_family(&self.dot, tool_family(session.tool));
        self.title.set_label(&session.title);
        self.tokens
            .set_label(&format::tokens(session.usage.tokens.total()));
        let mut detail = Vec::new();
        match session.models.as_slice() {
            [] => {}
            [model] => detail.push(model.clone()),
            models => detail.push(format!("{} models", models.len())),
        }
        match session.subagents {
            0 => {}
            1 => detail.push("1 subagent".to_owned()),
            count => detail.push(format!("{count} subagents")),
        }
        detail.push(format::ago(session.last_activity, now_ms));
        self.detail.set_label(&detail.join(" · "));
        let mut tooltip = usage_tooltip(&session.title, &session.usage, prices_available);
        if let Some(project) = &session.project {
            tooltip.push('\n');
            tooltip.push_str(project);
        }
        if session.models.len() > 1 {
            tooltip.push('\n');
            tooltip.push_str(&session.models.join(", "));
        }
        self.root.set_tooltip_text(Some(&tooltip));
    }
}

pub struct Widget {
    pub root: gtk::Box,
    period_buttons: Vec<(Period, gtk::ToggleButton)>,
    live: gtk::Box,
    caption: gtk::Label,
    total: gtk::Label,
    detail: gtk::Label,
    notice: gtk::Label,
    speed: Tile,
    rate: Tile,
    cache: Tile,
    streak: Tile,
    limits: gtk::Box,
    limits_title: gtk::Label,
    limits_plan: gtk::Label,
    limit_slots: [LimitSlot; 2],
    breakdown: Cell<Breakdown>,
    breakdown_tabs: Vec<(Breakdown, gtk::ToggleButton)>,
    rows: Vec<BreakdownSlot>,
    rows_empty: gtk::Label,
    sessions: Vec<SessionSlot>,
    sessions_empty: gtk::Label,
    sessions_count: gtk::Label,
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
        width: i32,
        rows: usize,
        sessions: usize,
        on_period: impl Fn(Period) + 'static,
    ) -> Rc<Widget> {
        let root = vbox(0);
        root.add_css_class("card");
        root.set_size_request(width, -1);

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
        let total = label(&["hero"]);
        let detail = label(&["dim"]);
        let notice = label(&["caption", "notice"]);
        notice.set_wrap(true);
        notice.set_max_width_chars(40);
        notice.set_visible(false);
        for widget in [&caption, &total, &detail, &notice] {
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
        root.append(&separator());
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
        let rows: Vec<BreakdownSlot> = (0..rows.max(1)).map(|_| BreakdownSlot::new()).collect();
        for slot in &rows {
            slot.root.set_visible(false);
            breakdown.append(&slot.root);
        }
        let rows_empty = label(&["dim", "empty"]);
        rows_empty.set_label("No usage in this period");
        rows_empty.set_visible(false);
        breakdown.append(&rows_empty);
        root.append(&breakdown);

        // Recent sessions.
        root.append(&separator());
        let sessions_box = vbox(0);
        sessions_box.add_css_class("section");
        let sessions_count = label(&["caption"]);
        sessions_box.append(&section_header(&section("SESSIONS"), &sessions_count));
        let session_slots: Vec<SessionSlot> = (0..sessions).map(|_| SessionSlot::new()).collect();
        for slot in &session_slots {
            slot.root.set_visible(false);
            sessions_box.append(&slot.root);
        }
        let sessions_empty = label(&["dim", "empty"]);
        sessions_empty.set_label("No sessions in this period");
        sessions_empty.set_visible(false);
        sessions_box.append(&sessions_empty);
        sessions_box.set_visible(sessions > 0);
        root.append(&sessions_box);

        // Activity heatmap.
        root.append(&separator());
        let activity = vbox(6);
        activity.add_css_class("section");
        let active_days = label(&["caption"]);
        activity.append(&section_header(&section("ACTIVITY"), &active_days));
        let heatmap = Heatmap::new();
        activity.append(&heatmap.area);
        root.append(&activity);

        // Trend.
        let trend_box = vbox(6);
        trend_box.add_css_class("section");
        let peak = label(&["caption"]);
        trend_box.append(&section_header(&section("TREND · 30D"), &peak));
        let trend = Trend::new();
        trend_box.append(&trend.area);
        root.append(&trend_box);

        let widget = Rc::new(Widget {
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
            limits,
            limits_title,
            limits_plan,
            limit_slots,
            breakdown: Cell::new(Breakdown::Models),
            breakdown_tabs,
            rows,
            rows_empty,
            sessions: session_slots,
            sessions_empty,
            sessions_count,
            heatmap,
            active_days,
            trend,
            peak,
            last: RefCell::default(),
        });
        widget.select_period(selected);
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

    pub fn update(&self, summary: &Rc<Summary>) {
        self.last.replace(Some(summary.clone()));
        let now = toktop::now_ms();
        let total = &summary.total;
        let available = summary.prices_available;

        self.caption.set_label(&format!(
            "TOKENS · {}",
            summary.period.label().to_uppercase()
        ));
        let tokens = total.tokens.total();
        self.total.set_label(&if tokens >= 1_000_000_000_000 {
            format::tokens(tokens)
        } else {
            format::grouped(tokens)
        });
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
        self.update_breakdown(summary);
        self.update_sessions(summary, now);
        self.update_charts(summary);
    }

    fn update_limits(&self, summary: &Summary, now_ms: i64) {
        let Some(limits) = &summary.codex_limits else {
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
        let available = summary.prices_available;
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
                .map(|(tool, usage)| (tool_family(*tool), tool.label(), usage))
                .collect(),
        };
        let projects = self.breakdown.get() == Breakdown::Projects;
        for (index, slot) in self.rows.iter().enumerate() {
            let Some((family, name, usage)) = items.get(index) else {
                slot.root.set_visible(false);
                continue;
            };
            slot.root.set_visible(true);
            set_family(&slot.dot, family);
            slot.name.set_label(name);
            slot.tokens.set_label(&format::tokens(usage.tokens.total()));
            slot.share
                .set_label(&format::percent(usage.tokens.total() as f64 / total));
            let mut tooltip = usage_tooltip(name, usage, available);
            if projects {
                // The label may be shortened; the tooltip has the full path.
                if let Some(row) = summary.by_project.get(index) {
                    tooltip.push('\n');
                    tooltip.push_str(&row.key);
                }
            }
            slot.root.set_tooltip_text(Some(&tooltip));
        }
        self.rows_empty.set_visible(items.is_empty());
    }

    fn update_sessions(&self, summary: &Summary, now_ms: i64) {
        for (index, slot) in self.sessions.iter().enumerate() {
            match summary.sessions.get(index) {
                Some(session) => {
                    slot.root.set_visible(true);
                    slot.set(session, now_ms, summary.prices_available);
                }
                None => slot.root.set_visible(false),
            }
        }
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
