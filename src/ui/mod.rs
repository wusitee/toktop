//! GTK4 / libadwaita window.

mod chart;

use adw::prelude::*;
use chrono::{Days, Local};
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use toktop::aggregate::{Period, Row, SessionRow, Summary, Usage};
use toktop::format;
use toktop::model::LimitWindow;
use toktop::worker::{self, Msg};

const APP_ID: &str = "io.github.wusitee.toktop";
const CHART_DAYS: u64 = 30;
const MAX_SESSIONS: usize = 100;

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(activate);
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

fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let ui = Rc::new(Ui::new(app));
    let (tx, rx) = async_channel::unbounded::<Summary>();
    match worker::spawn(Period::default(), move |summary| {
        let _ = tx.send_blocking(summary);
    }) {
        Ok(worker) => ui.connect_period(worker),
        Err(error) => {
            ui.banner.set_title(&format!("Could not start: {error}"));
            ui.banner.set_revealed(true);
        }
    }
    let target = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(mut summary) = rx.recv().await {
            // Only the newest summary matters.
            while let Ok(newer) = rx.try_recv() {
                summary = newer;
            }
            target.update(&summary);
        }
    });
    ui.window.present();
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
    tools: gtk::ListBox,
    limits_group: adw::PreferencesGroup,
    limits: gtk::ListBox,
    chart: chart::Chart,
    models: gtk::ListBox,
    projects: gtk::ListBox,
    sessions: gtk::ListBox,
    /// Stable keys of expanded breakdown rows, kept across list rebuilds.
    expanded: Rc<RefCell<HashSet<String>>>,
    /// Last rendered summary; lists are rebuilt only when their data changes.
    shown: RefCell<Option<Summary>>,
}

impl Ui {
    fn new(app: &adw::Application) -> Self {
        let labels: Vec<&str> = Period::ALL.iter().map(|period| period.label()).collect();
        let period = gtk::DropDown::from_strings(&labels);
        period.set_tooltip_text(Some("Period"));

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
            .default_width(420)
            .default_height(680)
            .content(&toolbar)
            .build();

        window.connect_close_request(|_| {
            debug_event("window close-request");
            glib::Propagation::Proceed
        });

        Ui {
            window,
            period,
            banner,
            total,
            detail,
            rate,
            tools,
            limits_group,
            limits,
            chart,
            models,
            projects,
            sessions,
            expanded: Rc::default(),
            shown: RefCell::default(),
        }
    }

    fn connect_period(&self, worker: Sender<Msg>) {
        self.period.connect_selected_notify(move |dropdown| {
            if let Some(&period) = Period::ALL.get(dropdown.selected() as usize) {
                let _ = worker.send(Msg::Period(period));
            }
        });
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

        let previous = self.shown.replace(Some(summary.clone()));
        let previous = previous.as_ref();
        let total_tokens = total.tokens.total().max(1) as f64;
        let context_changed = previous.is_none_or(|before| {
            before.total.tokens.total() != total.tokens.total()
                || before.prices_available != summary.prices_available
        });
        if context_changed || previous.is_none_or(|before| before.by_tool != summary.by_tool) {
            self.fill(
                &self.tools,
                summary.by_tool.iter().map(|(tool, usage)| {
                    (
                        format!("{tool:?}"),
                        tool.label().to_owned(),
                        usage,
                        usage.tokens.total() as f64 / total_tokens,
                    )
                }),
                "tool",
                summary.prices_available,
            );
        }
        if context_changed || previous.is_none_or(|before| before.by_model != summary.by_model) {
            self.fill(
                &self.models,
                rows(&summary.by_model, total_tokens),
                "model",
                summary.prices_available,
            );
        }
        if context_changed || previous.is_none_or(|before| before.by_project != summary.by_project)
        {
            self.fill(
                &self.projects,
                rows(&summary.by_project, total_tokens),
                "project",
                summary.prices_available,
            );
        }
        // Session rows show relative times, so they also refresh on the tick.
        self.sessions.remove_all();
        for session in summary.sessions.iter().take(MAX_SESSIONS) {
            self.sessions
                .append(&session_row(session, now, summary.prices_available));
        }

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

    fn fill<'a>(
        &self,
        list: &gtk::ListBox,
        rows: impl Iterator<Item = (String, String, &'a Usage, f64)>,
        kind: &str,
        prices_available: bool,
    ) {
        list.remove_all();
        for (key, title, usage, share) in rows {
            let key = format!("{kind}:{key}");
            let row = usage_row(&title, usage, share, prices_available);
            row.set_expanded(self.expanded.borrow().contains(&key));
            let expanded = self.expanded.clone();
            row.connect_expanded_notify(move |row| {
                let mut set = expanded.borrow_mut();
                if row.is_expanded() {
                    set.insert(key.clone());
                } else {
                    set.remove(&key);
                }
            });
            list.append(&row);
        }
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

fn usage_row(title: &str, usage: &Usage, share: f64, prices_available: bool) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(title)
        .use_markup(false)
        .subtitle(format!(
            "{} · {} · {} req",
            format::percent(share),
            display_cost(usage.cost, prices_available),
            usage.requests
        ))
        .build();
    row.add_suffix(&numeric(&format::tokens(usage.tokens.total())));
    let tokens = &usage.tokens;
    for (name, value) in [
        ("Input", tokens.input),
        ("Output", tokens.output),
        ("Reasoning", tokens.reasoning),
        ("Cache read", tokens.cache_read),
        ("Cache write", tokens.cache_write),
    ] {
        if value > 0 {
            let detail = adw::ActionRow::builder().title(name).build();
            detail.add_suffix(&numeric(&format::tokens(value)));
            row.add_row(&detail);
        }
    }
    if let Some(rate) = tokens.cache_hit_rate() {
        let detail = adw::ActionRow::builder().title("Cache hit rate").build();
        detail.add_suffix(&numeric(&format::percent(rate)));
        row.add_row(&detail);
    }
    row
}

fn session_row(session: &SessionRow, now_ms: i64, prices_available: bool) -> adw::ActionRow {
    let mut subtitle = vec![session.tool.label().to_owned()];
    if let Some(project) = &session.project {
        subtitle.push(toktop::aggregate::project_label(project));
    }
    subtitle.push(format::ago(session.last_activity, now_ms));
    let row = adw::ActionRow::builder()
        .title(&session.title)
        .use_markup(false)
        .title_lines(1)
        .subtitle(subtitle.join(" · "))
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
    row
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
