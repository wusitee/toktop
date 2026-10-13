//! GTK4 / libadwaita window and tray icon.
//!
//! [`Shell`] lives for the whole process: it owns the worker, the latest
//! summary, the settings and the tray icon. The window is created on demand
//! and destroyed when closed, so a tray-only process does not keep its
//! widgets in memory.

mod chart;
mod tray;

use adw::prelude::*;
use chrono::{Days, Local};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use toktop::aggregate::{Period, Row, SessionRow, Summary, Usage};
use toktop::config::{self, Config};
use toktop::format;
use toktop::model::LimitWindow;
use toktop::worker::{self, Msg};

const APP_ID: &str = "io.github.wusitee.toktop";
const CHART_DAYS: u64 = 30;
const MAX_SESSIONS: usize = 100;

pub struct Options {
    /// Start without a window when the tray icon is available.
    pub background: bool,
}

pub fn run(options: Options) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    let background = options.background;
    let shell = Rc::new(Shell::new(&app, options));
    // `register` emits `startup` on the primary instance.
    let started = shell.clone();
    app.connect_startup(move |_| started.start());
    if let Err(error) = app.register(gio::Cancellable::NONE) {
        eprintln!("toktop: {error}");
        return glib::ExitCode::FAILURE;
    }
    if app.is_remote() {
        // Another instance is running: show its window unless asked not to.
        if !background {
            app.activate();
        }
        return glib::ExitCode::SUCCESS;
    }
    let activated = shell.clone();
    app.connect_activate(move |_| activated.activate());
    app.connect_shutdown(|_| debug_event("application shutdown"));
    app.connect_name_lost(|_| {
        debug_event("GApplication name-lost");
        false
    });
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| {
            debug_event("app.quit activation");
            app.quit();
        })
        .build();
    app.add_action_entries([quit]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    // Command-line arguments are handled before GTK starts.
    app.run_with_args::<&str>(&[])
}

fn debug_event(event: &str) {
    if std::env::var_os("TOKTOP_DEBUG").is_some() {
        eprintln!("toktop: {} {event}", chrono::Utc::now().to_rfc3339());
    }
}

fn display_cost(cost: f64, available: bool) -> String {
    if available {
        format::cost(cost)
    } else {
        "—".to_owned()
    }
}

/// Process-wide state on the GTK main thread.
struct Shell {
    app: adw::Application,
    options: Options,
    config: RefCell<Config>,
    /// False when the config file exists but could not be read; it is then
    /// never overwritten.
    config_writable: bool,
    worker: RefCell<Option<Sender<Msg>>>,
    latest: RefCell<Option<Summary>>,
    ui: RefCell<Option<Rc<Ui>>>,
    tray: RefCell<Option<tray::Tray>>,
    /// Keeps the application running while the tray icon exists.
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    activated: Cell<bool>,
}

impl Shell {
    fn new(app: &adw::Application, options: Options) -> Self {
        let (config, config_writable) = match Config::load(&config::path()) {
            Ok(config) => (config, true),
            Err(error) => {
                eprintln!(
                    "toktop: ignoring {} ({error}); using defaults",
                    config::path().display()
                );
                (Config::default(), false)
            }
        };
        Shell {
            app: app.clone(),
            options,
            config: RefCell::new(config),
            config_writable,
            worker: RefCell::default(),
            latest: RefCell::default(),
            ui: RefCell::default(),
            tray: RefCell::default(),
            hold: RefCell::default(),
            activated: Cell::new(false),
        }
    }

    fn start(self: &Rc<Self>) {
        let (period, tools, want_tray) = {
            let config = self.config.borrow();
            (config.period, config.tools.clone(), config.tray)
        };
        let (tx, rx) = async_channel::unbounded::<Summary>();
        match worker::spawn(period, tools, move |summary| {
            let _ = tx.send_blocking(summary);
        }) {
            Ok(worker) => {
                self.worker.replace(Some(worker));
            }
            Err(error) => eprintln!("toktop: could not start worker: {error}"),
        }
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(mut summary) = rx.recv().await {
                // Only the newest summary matters.
                while let Ok(newer) = rx.try_recv() {
                    summary = newer;
                }
                let Some(shell) = shell.upgrade() else { return };
                shell.show_summary(summary);
            }
        });

        if want_tray {
            self.start_tray(period);
        }
    }

    fn start_tray(self: &Rc<Self>, period: Period) {
        let (tx, rx) = async_channel::unbounded();
        let state = tray::State {
            title: "toktop".into(),
            description: String::new(),
            period,
        };
        match tray::Tray::spawn(tx, state) {
            Ok(tray) => {
                self.tray.replace(Some(tray));
                self.hold.replace(Some(self.app.hold()));
            }
            Err(error) => {
                eprintln!("toktop: no tray icon ({error})");
                return;
            }
        }
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(command) = rx.recv().await {
                let Some(shell) = shell.upgrade() else { return };
                match command {
                    tray::Command::Toggle => shell.toggle_window(),
                    tray::Command::Show => shell.show_window(),
                    tray::Command::Period(period) => shell.set_period(period),
                    tray::Command::RefreshPrices => shell.send(Msg::RefreshPrices),
                    tray::Command::Quit => shell.app.quit(),
                }
            }
        });
    }

    fn activate(self: &Rc<Self>) {
        let first = !self.activated.replace(true);
        if first && self.options.background {
            if self.tray.borrow().is_some() {
                return;
            }
            eprintln!("toktop: no tray icon available; opening the window");
        }
        self.show_window();
    }

    fn send(&self, msg: Msg) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            let _ = worker.send(msg);
        }
    }

    fn show_summary(&self, summary: Summary) {
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.update(&summary);
        }
        if let Some(tray) = self.tray.borrow_mut().as_mut() {
            tray.set(tray_state(&summary));
        }
        self.latest.replace(Some(summary));
    }

    fn show_window(self: &Rc<Self>) {
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.window.present();
            return;
        }
        let ui = {
            let config = self.config.borrow();
            Rc::new(Ui::new(&self.app, &config.window, config.period))
        };
        let shell = Rc::downgrade(self);
        ui.period.connect_selected_notify(move |dropdown| {
            if let (Some(shell), Some(&period)) = (
                shell.upgrade(),
                Period::ALL.get(dropdown.selected() as usize),
            ) {
                shell.set_period(period);
            }
        });
        let shell = Rc::downgrade(self);
        ui.window.connect_close_request(move |window| {
            debug_event("window close-request");
            if let Some(shell) = shell.upgrade() {
                shell.close_window(window);
            }
            glib::Propagation::Proceed
        });
        if let Some(summary) = self.latest.borrow().as_ref() {
            ui.update(summary);
        }
        ui.window.present();
        self.ui.replace(Some(ui));
    }

    /// Saves the window size and drops the widgets; GTK destroys the window
    /// after this returns.
    fn close_window(&self, window: &adw::ApplicationWindow) {
        let (width, height) = window.default_size();
        let maximized = window.is_maximized();
        self.update_config(|config| {
            if !maximized && width > 0 && height > 0 {
                config.window.width = width;
                config.window.height = height;
            }
            config.window.maximized = maximized;
        });
        self.ui.replace(None);
        // Return freed widget memory once the window is gone.
        glib::idle_add_local_once(worker::trim_heap);
    }

    fn toggle_window(self: &Rc<Self>) {
        let open = self.ui.borrow().as_ref().map(|ui| ui.window.clone());
        match open {
            Some(window) if window.is_active() => window.close(),
            _ => self.show_window(),
        }
    }

    fn set_period(&self, period: Period) {
        if self.config.borrow().period == period {
            return;
        }
        self.update_config(|config| config.period = period);
        self.send(Msg::Period(period));
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.select_period(period);
        }
    }

    fn update_config(&self, change: impl FnOnce(&mut Config)) {
        let mut config = self.config.borrow_mut();
        let before = config.clone();
        change(&mut config);
        if *config == before || !self.config_writable {
            return;
        }
        if let Err(error) = config.save(&config::path()) {
            eprintln!("toktop: could not save settings: {error}");
        }
    }
}

fn period_index(period: Period) -> u32 {
    Period::ALL
        .iter()
        .position(|candidate| *candidate == period)
        .unwrap_or(0) as u32
}

/// Tooltip text for the tray icon.
fn tray_state(summary: &Summary) -> tray::State {
    let total = &summary.total;
    let mut title = format!(
        "{}: {} tokens",
        summary.period.label(),
        format::tokens(total.tokens.total())
    );
    if summary.prices_available {
        title.push_str(&format!(" · {}", format::cost(total.cost)));
    }
    let mut lines = Vec::new();
    if summary.rate_per_min > 0.0 {
        lines.push(format!(
            "Live: {} tokens/min",
            format::tokens(summary.rate_per_min as u64)
        ));
    }
    if let Some(limits) = &summary.codex_limits {
        let now_s = toktop::now_ms() / 1000;
        let windows: Vec<String> = [&limits.primary, &limits.secondary]
            .into_iter()
            .flatten()
            .map(|window| {
                let used = if window.resets_at.is_some_and(|at| at <= now_s) {
                    0.0
                } else {
                    window.used_percent
                };
                format!(
                    "{} {used:.0}%",
                    window_name(window.window_minutes).to_lowercase()
                )
            })
            .collect();
        if !windows.is_empty() {
            lines.push(format!("Codex {}", windows.join(" · ")));
        }
    }
    tray::State {
        title,
        description: lines.join("\n"),
        period: summary.period,
    }
}

/// A boxed list inside a titled group.
fn list_group(title: &str) -> (adw::PreferencesGroup, gtk::ListBox) {
    let group = adw::PreferencesGroup::builder().title(title).build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let placeholder = gtk::Label::builder()
        .label("No usage in this period")
        .css_classes(["dim-label"])
        .margin_top(18)
        .margin_bottom(18)
        .build();
    list.set_placeholder(Some(&placeholder));
    group.add(&list);
    (group, list)
}

fn page(name: &str, groups: &[&adw::PreferencesGroup]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().name(name).build();
    for group in groups {
        page.add(*group);
    }
    page
}

fn numeric(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["numeric"])
        .build()
}

struct Ui {
    window: adw::ApplicationWindow,
    period: gtk::DropDown,
    banner: adw::Banner,
    total: gtk::Label,
    detail: gtk::Label,
    rate: gtk::Label,
    tools: UsageList,
    limits_group: adw::PreferencesGroup,
    limits: gtk::ListBox,
    chart: chart::Chart,
    models: UsageList,
    projects: UsageList,
    sessions: SessionList,
    /// Stable keys of expanded breakdown rows, kept across list rebuilds.
    expanded: Rc<RefCell<HashSet<String>>>,
}

/// Token buckets shown in an expanded usage row.
const BUCKETS: [&str; 5] = ["Input", "Output", "Reasoning", "Cache read", "Cache write"];

fn bucket_values(tokens: &toktop::model::Tokens) -> [u64; 5] {
    [
        tokens.input,
        tokens.output,
        tokens.reasoning,
        tokens.cache_read,
        tokens.cache_write,
    ]
}

/// Widgets of one usage row whose text can change in place.
struct UsageRowWidgets {
    row: adw::ExpanderRow,
    total: gtk::Label,
    /// One label per nonzero bucket, in `BUCKETS` order.
    buckets: Vec<gtk::Label>,
    hit_rate: Option<gtk::Label>,
}

/// Row structure that can be updated without rebuilding: key, nonzero
/// buckets and whether a cache hit rate is shown.
type UsageShape = (String, [bool; 5], bool);

/// A breakdown list. Updates change row text in place while the rows keep the
/// same keys, order and detail rows, so focus, scroll and expansion survive
/// live updates; anything else rebuilds the list.
struct UsageList {
    list: gtk::ListBox,
    kind: &'static str,
    shape: RefCell<Vec<UsageShape>>,
    rows: RefCell<Vec<UsageRowWidgets>>,
}

struct SessionRowWidgets {
    row: adw::ActionRow,
    tokens: gtk::Label,
    cost: gtk::Label,
}

/// Recent sessions, updated in place while their identities and order hold.
struct SessionList {
    list: gtk::ListBox,
    shape: RefCell<Vec<(toktop::model::Tool, String, Option<String>)>>,
    rows: RefCell<Vec<SessionRowWidgets>>,
}

impl Ui {
    fn new(app: &adw::Application, size: &config::Window, selected: Period) -> Self {
        let labels: Vec<&str> = Period::ALL.iter().map(|period| period.label()).collect();
        let period = gtk::DropDown::from_strings(&labels);
        period.set_tooltip_text(Some("Period"));
        period.set_selected(period_index(selected));

        let stack = adw::ViewStack::new();
        let header = adw::HeaderBar::new();
        header.pack_start(&period);
        let switcher = adw::ViewSwitcherBar::builder()
            .stack(&stack)
            .reveal(true)
            .build();
        let banner = adw::Banner::new("");

        // Overview
        let total = gtk::Label::builder()
            .css_classes(["title-1", "numeric"])
            .build();
        let detail = gtk::Label::builder().css_classes(["dim-label"]).build();
        let rate = gtk::Label::builder()
            .css_classes(["caption", "accent"])
            .build();
        let hero = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_bottom(6)
            .build();
        hero.append(&total);
        hero.append(&detail);
        hero.append(&rate);
        let hero_group = adw::PreferencesGroup::new();
        hero_group.add(&hero);

        let (tools_group, tools) = list_group("Tools");
        let (limits_group, limits) = list_group("Codex limits");
        limits_group.set_visible(false);
        let chart = chart::Chart::new();
        let chart_group = adw::PreferencesGroup::builder()
            .title("Last 30 days")
            .build();
        let chart_frame = gtk::Frame::builder().child(&chart.area).build();
        chart_frame.add_css_class("view");
        chart_group.add(&chart_frame);
        let overview = page(
            "overview",
            &[&hero_group, &tools_group, &limits_group, &chart_group],
        );

        let (models_group, models) = list_group("Models");
        let (projects_group, projects) = list_group("Projects");
        let (sessions_group, sessions) = list_group("Recent sessions");

        for (child, name, title, icon) in [
            (&overview, "overview", "Overview", "go-home-symbolic"),
            (
                &page("models", &[&models_group]),
                "models",
                "Models",
                "applications-science-symbolic",
            ),
            (
                &page("projects", &[&projects_group]),
                "projects",
                "Projects",
                "folder-symbolic",
            ),
            (
                &page("sessions", &[&sessions_group]),
                "sessions",
                "Sessions",
                "document-open-recent-symbolic",
            ),
        ] {
            stack.add_titled_with_icon(child, Some(name), title, icon);
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(&stack);
        stack.set_vexpand(true);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&switcher);
        toolbar.set_content(Some(&content));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("toktop")
            .default_width(size.width)
            .default_height(size.height)
            .maximized(size.maximized)
            .content(&toolbar)
            .build();

        Ui {
            window,
            period,
            banner,
            total,
            detail,
            rate,
            tools: UsageList::new(tools, "tool"),
            limits_group,
            limits,
            chart,
            models: UsageList::new(models, "model"),
            projects: UsageList::new(projects, "project"),
            sessions: SessionList {
                list: sessions,
                shape: RefCell::default(),
                rows: RefCell::default(),
            },
            expanded: Rc::default(),
        }
    }

    fn select_period(&self, period: Period) {
        self.period.set_selected(period_index(period));
    }

    fn update(&self, summary: &Summary) {
        let now = toktop::now_ms();
        let total = &summary.total;
        self.total
            .set_label(&format!("{} tokens", format::tokens(total.tokens.total())));
        let mut detail = format!(
            "{} · {} requests",
            display_cost(total.cost, summary.prices_available),
            total.requests
        );
        if let Some(rate) = total.tokens.cache_hit_rate() {
            detail.push_str(&format!(" · {} cached", format::percent(rate)));
        }
        self.detail.set_label(&detail);
        self.rate.set_visible(summary.rate_per_min > 0.0);
        self.rate.set_label(&format!(
            "Live: {} tokens/min",
            format::tokens(summary.rate_per_min as u64)
        ));

        if !summary.prices_available {
            self.banner
                .set_title("Prices unavailable; costs hidden until the price list downloads");
            self.banner.set_revealed(true);
        } else if summary.unpriced.is_empty() {
            self.banner.set_revealed(false);
        } else {
            self.banner.set_title(&format!(
                "No price for {}; counted as $0",
                summary.unpriced.join(", ")
            ));
            self.banner.set_revealed(true);
        }

        let total_tokens = total.tokens.total().max(1) as f64;
        let available = summary.prices_available;
        self.tools.set(
            summary.by_tool.iter().map(|(tool, usage)| {
                (
                    format!("{tool:?}"),
                    tool.label().to_owned(),
                    usage,
                    usage.tokens.total() as f64 / total_tokens,
                )
            }),
            available,
            &self.expanded,
        );
        self.models.set(
            rows(&summary.by_model, total_tokens),
            available,
            &self.expanded,
        );
        self.projects.set(
            rows(&summary.by_project, total_tokens),
            available,
            &self.expanded,
        );
        self.sessions.set(&summary.sessions, now, available);

        self.update_limits(summary, now);

        let today = Local::now().date_naive();
        let days = (0..CHART_DAYS)
            .rev()
            .map(|back| {
                let date = today - Days::new(back);
                let usage = summary
                    .daily
                    .binary_search_by_key(&date, |day| day.date)
                    .ok()
                    .map(|index| &summary.daily[index].usage);
                chart::Day {
                    date,
                    tokens: usage.map_or(0, |usage| usage.tokens.total()),
                    cost: summary
                        .prices_available
                        .then(|| usage.map_or(0.0, |usage| usage.cost)),
                }
            })
            .collect();
        self.chart.set(days);
    }

    fn update_limits(&self, summary: &Summary, now_ms: i64) {
        self.limits.remove_all();
        let Some(limits) = &summary.codex_limits else {
            self.limits_group.set_visible(false);
            return;
        };
        self.limits_group.set_visible(true);
        self.limits_group.set_title(&match &limits.plan {
            Some(plan) => format!("Codex limits · {plan}"),
            None => "Codex limits".to_owned(),
        });
        self.limits_group.set_description(Some(&format!(
            "Updated {}",
            format::ago(limits.observed, now_ms)
        )));
        for window in [&limits.primary, &limits.secondary].into_iter().flatten() {
            self.limits.append(&limit_row(window, now_ms / 1000));
        }
    }
}

fn rows(rows: &[Row], total: f64) -> impl Iterator<Item = (String, String, &Usage, f64)> {
    rows.iter().map(move |row| {
        (
            row.key.clone(),
            row.label.clone(),
            &row.usage,
            row.usage.tokens.total() as f64 / total,
        )
    })
}

fn usage_subtitle(usage: &Usage, share: f64, prices_available: bool) -> String {
    format!(
        "{} · {} · {} req",
        format::percent(share),
        display_cost(usage.cost, prices_available),
        usage.requests
    )
}

fn usage_shape(key: &str, usage: &Usage) -> UsageShape {
    (
        key.to_owned(),
        bucket_values(&usage.tokens).map(|value| value > 0),
        usage.tokens.cache_hit_rate().is_some(),
    )
}

fn usage_row(title: &str, usage: &Usage, share: f64, prices_available: bool) -> UsageRowWidgets {
    let row = adw::ExpanderRow::builder()
        .title(title)
        .use_markup(false)
        .subtitle(usage_subtitle(usage, share, prices_available))
        .build();
    let total = numeric(&format::tokens(usage.tokens.total()));
    row.add_suffix(&total);
    let mut buckets = Vec::new();
    for (name, value) in BUCKETS.into_iter().zip(bucket_values(&usage.tokens)) {
        if value > 0 {
            let detail = adw::ActionRow::builder().title(name).build();
            let label = numeric(&format::tokens(value));
            detail.add_suffix(&label);
            row.add_row(&detail);
            buckets.push(label);
        }
    }
    let hit_rate = usage.tokens.cache_hit_rate().map(|rate| {
        let detail = adw::ActionRow::builder().title("Cache hit rate").build();
        let label = numeric(&format::percent(rate));
        detail.add_suffix(&label);
        row.add_row(&detail);
        label
    });
    UsageRowWidgets {
        row,
        total,
        buckets,
        hit_rate,
    }
}

impl UsageList {
    fn new(list: gtk::ListBox, kind: &'static str) -> Self {
        UsageList {
            list,
            kind,
            shape: RefCell::default(),
            rows: RefCell::default(),
        }
    }

    fn set<'a>(
        &self,
        items: impl Iterator<Item = (String, String, &'a Usage, f64)>,
        prices_available: bool,
        expanded: &Rc<RefCell<HashSet<String>>>,
    ) {
        let items: Vec<_> = items.collect();
        let shape: Vec<UsageShape> = items
            .iter()
            .map(|(key, _, usage, _)| usage_shape(key, usage))
            .collect();
        if *self.shape.borrow() == shape {
            for ((_, title, usage, share), widgets) in items.iter().zip(self.rows.borrow().iter()) {
                widgets.row.set_title(title);
                widgets
                    .row
                    .set_subtitle(&usage_subtitle(usage, *share, prices_available));
                widgets
                    .total
                    .set_label(&format::tokens(usage.tokens.total()));
                let values = bucket_values(&usage.tokens)
                    .into_iter()
                    .filter(|value| *value > 0);
                for (label, value) in widgets.buckets.iter().zip(values) {
                    label.set_label(&format::tokens(value));
                }
                if let (Some(label), Some(rate)) =
                    (&widgets.hit_rate, usage.tokens.cache_hit_rate())
                {
                    label.set_label(&format::percent(rate));
                }
            }
            return;
        }

        self.list.remove_all();
        let mut rows = Vec::with_capacity(items.len());
        for (key, title, usage, share) in &items {
            let key = format!("{}:{key}", self.kind);
            let widgets = usage_row(title, usage, *share, prices_available);
            widgets.row.set_expanded(expanded.borrow().contains(&key));
            let expanded = expanded.clone();
            widgets.row.connect_expanded_notify(move |row| {
                let mut set = expanded.borrow_mut();
                if row.is_expanded() {
                    set.insert(key.clone());
                } else {
                    set.remove(&key);
                }
            });
            self.list.append(&widgets.row);
            rows.push(widgets);
        }
        self.rows.replace(rows);
        self.shape.replace(shape);
    }
}

fn session_subtitle(session: &SessionRow, now_ms: i64) -> String {
    let mut subtitle = vec![session.tool.label().to_owned()];
    if let Some(project) = &session.project {
        subtitle.push(toktop::aggregate::project_label(project));
    }
    subtitle.push(format::ago(session.last_activity, now_ms));
    subtitle.join(" · ")
}

fn session_row(session: &SessionRow, now_ms: i64, prices_available: bool) -> SessionRowWidgets {
    let row = adw::ActionRow::builder()
        .title(&session.title)
        .use_markup(false)
        .title_lines(1)
        .subtitle(session_subtitle(session, now_ms))
        .subtitle_lines(1)
        .build();
    if let Some(project) = &session.project {
        row.set_tooltip_text(Some(project));
    }
    let numbers = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    let tokens = numeric(&format::tokens(session.usage.tokens.total()));
    tokens.set_halign(gtk::Align::End);
    let cost = numeric(&display_cost(session.usage.cost, prices_available));
    cost.set_halign(gtk::Align::End);
    cost.add_css_class("dim-label");
    cost.add_css_class("caption");
    numbers.append(&tokens);
    numbers.append(&cost);
    row.add_suffix(&numbers);
    SessionRowWidgets { row, tokens, cost }
}

impl SessionList {
    fn set(&self, sessions: &[SessionRow], now_ms: i64, prices_available: bool) {
        let sessions = &sessions[..sessions.len().min(MAX_SESSIONS)];
        let shape: Vec<_> = sessions
            .iter()
            .map(|session| (session.tool, session.title.clone(), session.project.clone()))
            .collect();
        if *self.shape.borrow() == shape {
            for (session, widgets) in sessions.iter().zip(self.rows.borrow().iter()) {
                widgets.row.set_subtitle(&session_subtitle(session, now_ms));
                widgets
                    .tokens
                    .set_label(&format::tokens(session.usage.tokens.total()));
                widgets
                    .cost
                    .set_label(&display_cost(session.usage.cost, prices_available));
            }
            return;
        }
        self.list.remove_all();
        let rows: Vec<_> = sessions
            .iter()
            .map(|session| {
                let widgets = session_row(session, now_ms, prices_available);
                self.list.append(&widgets.row);
                widgets
            })
            .collect();
        self.rows.replace(rows);
        self.shape.replace(shape);
    }
}

fn window_name(minutes: u64) -> String {
    match minutes {
        10080 => "Weekly".to_owned(),
        1440 => "Daily".to_owned(),
        m if m > 0 && m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    }
}

fn limit_row(window: &LimitWindow, now_s: i64) -> adw::ActionRow {
    let reset = window.resets_at.filter(|at| *at > now_s);
    let subtitle = match (window.resets_at, reset) {
        (_, Some(at)) => format!("Resets in {}", format::duration_until(at - now_s)),
        (Some(_), None) => "Reset since last update".to_owned(),
        (None, None) => String::new(),
    };
    let used = if window.resets_at.is_some() && reset.is_none() {
        0.0
    } else {
        window.used_percent.clamp(0.0, 100.0)
    };
    let row = adw::ActionRow::builder()
        .title(format!("{} window", window_name(window.window_minutes)))
        .subtitle(subtitle)
        .build();
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(100.0)
        .value(used)
        .width_request(96)
        .valign(gtk::Align::Center)
        .build();
    // Higher is worse for a quota; drop the default "full is good" offsets.
    bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_LOW));
    bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_HIGH));
    bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_FULL));
    row.add_suffix(&bar);
    row.add_suffix(&numeric(&format!("{used:.0}%")));
    row
}
