//! Detail card for a model, project, tool, session or day, shown beside the
//! main card when its row or chart cell is clicked.
//!
//! The card is a popover rebuilt from each summary while it is open, so it
//! stays live; closed, it holds no widgets. It stays open until closed with
//! its button or by clicking the same item again, so clicking another item
//! switches it directly. Day drill-downs use a secondary popover, keeping
//! the day's overview visible.

use super::chart::{self, KindSplit};
use super::widget::{
    Count, dot, ellipsized, hbox, label, model_family, section, section_header, set_family,
    tool_family, vbox,
};
use chrono::{DateTime, Local, NaiveDate};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use toktop::aggregate::{DayRow, SPEED_DAYS, Split, Summary, Usage, project_labels};
use toktop::config::Anchor;
use toktop::format;
use toktop::model::Tool;

/// Entries shown per list before the rest are summarized.
const LIST_ROWS: usize = 6;
/// Day breakdown and session rows shown before their lists scroll.
const DAY_ROWS: usize = 5;
/// Most entries rendered in a day's scrolling lists.
const DAY_MAX_ROWS: usize = 100;

/// Group switcher of a day's breakdown, like the main card's tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DayGroup {
    Models,
    Projects,
    Tools,
}

impl DayGroup {
    const ALL: [(DayGroup, &'static str); 3] = [
        (DayGroup::Models, "MODELS"),
        (DayGroup::Projects, "PROJECTS"),
        (DayGroup::Tools, "TOOLS"),
    ];

    fn index(self) -> usize {
        match self {
            DayGroup::Models => 0,
            DayGroup::Projects => 1,
            DayGroup::Tools => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Model(String),
    /// Full project path.
    Project(String),
    Tool(Tool),
    Session(Tool, String),
    Day(NaiveDate),
    DayModel(NaiveDate, String),
    DayProject(NaiveDate, String),
    DaySession(NaiveDate, Tool, String),
}

impl Selection {
    fn day(&self) -> Option<NaiveDate> {
        match self {
            Self::Day(date)
            | Self::DayModel(date, _)
            | Self::DayProject(date, _)
            | Self::DaySession(date, _, _) => Some(*date),
            _ => None,
        }
    }
}

pub struct Detail {
    popover: gtk::Popover,
    kind: gtk::Label,
    frame: gtk::Box,
    anchor: Anchor,
    /// Title, rebuilt with each render.
    top: gtk::Box,
    /// Token count, kept across renders so a refresh cannot drop a click on
    /// it.
    hero: Count,
    token_caption: gtk::Label,
    costs: gtk::Box,
    cost: gtk::Label,
    /// Everything below the count, rebuilt with each render.
    content: gtk::Box,
    scroll: gtk::ScrolledWindow,
    selected: RefCell<Option<Selection>>,
    latest: RefCell<Option<Rc<Summary>>>,
    /// Tab selected in a day card's breakdown.
    day_group: Cell<DayGroup>,
    on_day: Box<dyn Fn(Option<NaiveDate>)>,
    secondary: RefCell<Option<Rc<Detail>>>,
}

impl Detail {
    /// A detail card attached to `card`, opening on the side away from the
    /// screen edge the card is anchored to.
    pub fn new(
        card: &gtk::Box,
        anchor: Anchor,
        width: i32,
        on_day: impl Fn(Option<NaiveDate>) + 'static,
    ) -> Rc<Detail> {
        // The header stays while the body is rebuilt, so a refresh cannot
        // cancel a click on the close button.
        let kind = label(&["caption", "detail-kind"]);
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close")
            .css_classes(["icon"])
            .valign(gtk::Align::Center)
            .build();
        let top = vbox(0);
        let hero = Count::new("detail-hero");
        let token_caption = label(&["caption", "hero-caption"]);
        token_caption.set_label("TOKENS");
        let tokens = vbox(0);
        tokens.set_hexpand(true);
        tokens.append(&token_caption);
        tokens.append(&hero.label);
        let cost_caption = label(&["caption", "hero-caption"]);
        cost_caption.set_label("TOTAL COST");
        cost_caption.set_xalign(1.0);
        let cost = label(&["hero"]);
        cost.set_xalign(1.0);
        let costs = vbox(0);
        costs.append(&cost_caption);
        costs.append(&cost);
        let totals = hbox(16);
        totals.append(&tokens);
        totals.append(&costs);
        let content = vbox(0);
        let column = vbox(0);
        column.set_size_request(width, -1);
        column.append(&top);
        column.append(&totals);
        column.append(&content);
        let body = vbox(0);
        body.add_css_class("detail-body");
        body.append(&column);
        // Reserve the scrollbar gutter so scrolling never narrows the content.
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Always)
            .overlay_scrolling(false)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .child(&body)
            .css_classes(["list-scroll"])
            .build();
        let scrollbar = scroll.vscrollbar();
        scrollbar.set_opacity(0.0);
        let scrollbar = scrollbar.downgrade();
        scroll.vadjustment().connect_changed(move |adjustment| {
            if let Some(scrollbar) = scrollbar.upgrade() {
                scrollbar.set_opacity(if adjustment.upper() > adjustment.page_size() {
                    1.0
                } else {
                    0.0
                });
            }
        });
        let frame = vbox(0);
        frame.set_size_request(width, -1);
        let header = section_header(&kind, &close);
        header.add_css_class("detail-header");
        frame.append(&header);
        frame.append(&scroll);
        let popover = gtk::Popover::builder()
            .has_arrow(false)
            .autohide(false)
            .cascade_popdown(false)
            .child(&frame)
            .css_classes(["detail"])
            .build();
        let target = popover.downgrade();
        close.connect_clicked(move |_| {
            if let Some(popover) = target.upgrade() {
                popover.popdown();
            }
        });
        let (_, _, left, right) = anchor.edges();
        popover.set_position(if left && !right {
            gtk::PositionType::Right
        } else {
            gtk::PositionType::Left
        });
        popover.set_parent(card);
        let detail = Rc::new(Detail {
            popover,
            kind,
            frame,
            anchor,
            top,
            hero,
            token_caption,
            costs,
            cost,
            content,
            scroll,
            selected: RefCell::default(),
            latest: RefCell::default(),
            day_group: Cell::new(DayGroup::Models),
            on_day: Box::new(on_day),
            secondary: RefCell::default(),
        });
        let weak = Rc::downgrade(&detail);
        detail.popover.connect_closed(move |_| {
            if let Some(detail) = weak.upgrade() {
                detail.close_secondary();
                if detail
                    .selected
                    .take()
                    .and_then(|selection| selection.day())
                    .is_some()
                {
                    (detail.on_day)(None);
                }
                detail.latest.take();
                clear(&detail.top);
                clear(&detail.content);
            }
        });
        detail
    }

    /// Caps the card's height; a taller body scrolls.
    pub fn set_max_height(&self, height: i32) {
        // Leaves room for the header and padding.
        self.scroll.set_max_content_height((height - 60).max(100));
        if let Some(secondary) = self.secondary.borrow().as_ref() {
            secondary.set_max_height(height);
        }
    }

    /// Shows `selection` level with the card rows from `y` to `y + height`
    /// (in card coordinates). Showing the open item again closes the card.
    pub fn show(self: &Rc<Self>, selection: Selection, y: f64, height: f64, summary: &Rc<Summary>) {
        if self.selected.borrow().as_ref() == Some(&selection) || !self.render(&selection, summary)
        {
            self.popover.popdown();
            return;
        }
        self.close_secondary();
        self.latest.replace(Some(summary.clone()));
        self.select(selection);
        // The card's full width, so the popover clears it also when GTK flips
        // it to the other side; the gap is the popover's CSS margin. Widget
        // coordinates start inside the padding; the bounds include it.
        let Some(card) = self
            .popover
            .parent()
            .and_then(|card| card.compute_bounds(&card))
        else {
            return;
        };
        self.popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            card.x() as i32,
            y as i32,
            card.width() as i32,
            (height as i32).max(1),
        )));
        self.popover.popup();
    }

    /// Redraws an open card from a new summary.
    pub fn refresh(self: &Rc<Self>, summary: &Rc<Summary>) {
        let selected = self.selected.borrow().clone();
        if let Some(selection) = selected {
            self.latest.replace(Some(summary.clone()));
            if !self.render(&selection, summary) {
                self.popover.popdown();
            }
            let secondary = self.secondary.borrow().clone();
            if let Some(secondary) = secondary {
                secondary.refresh(summary);
            }
        }
    }

    fn select(&self, selection: Selection) {
        let day = selection.day();
        let previous = self
            .selected
            .replace(Some(selection))
            .and_then(|selection| selection.day());
        if previous != day {
            (self.on_day)(day);
        }
    }

    fn close_secondary(&self) {
        if let Some(secondary) = self.secondary.take() {
            secondary.popover.popdown();
        }
    }

    fn navigate(self: &Rc<Self>, selection: Selection, source: &gtk::Button) {
        let summary = self.latest.borrow().clone();
        let Some(summary) = summary else { return };
        if matches!(self.selected.borrow().as_ref(), Some(Selection::Day(_))) {
            let Some(bounds) = source.compute_bounds(&self.frame) else {
                return;
            };
            let secondary = self
                .secondary
                .borrow_mut()
                .get_or_insert_with(|| {
                    let secondary =
                        Detail::new(&self.frame, self.anchor, self.frame.width_request(), |_| {});
                    secondary
                        .scroll
                        .set_max_content_height(self.scroll.max_content_height());
                    secondary
                })
                .clone();
            secondary.show(
                selection,
                bounds.y() as f64,
                bounds.height() as f64,
                &summary,
            );
        } else if self.render(&selection, &summary) {
            self.select(selection);
            self.scroll.vadjustment().set_value(0.0);
        }
    }

    /// Fills the card; false when the selection is not in the summary.
    fn render(self: &Rc<Self>, selection: &Selection, summary: &Summary) -> bool {
        let Some(info) = Info::find(selection, summary) else {
            return false;
        };
        self.kind.set_label(&info.kind);
        let day = matches!(selection, Selection::Day(_));
        self.token_caption.set_visible(day);
        self.costs.set_visible(day);
        self.hero
            .label
            .set_css_classes(&[if day { "hero" } else { "detail-hero" }]);
        self.cost.set_label(&super::widget::display_cost(
            info.usage.cost.total(),
            summary.prices_available,
        ));
        clear(&self.top);
        clear(&self.content);
        self.hero.set(info.usage.tokens.total());
        let weak = Rc::downgrade(self);
        let groups = Rc::downgrade(self);
        info.build(
            &self.top,
            &self.content,
            summary.prices_available,
            day.then(|| self.day_group.get()),
            move |selection, source| {
                if let Some(detail) = weak.upgrade() {
                    detail.navigate(selection, source);
                }
            },
            move |group| {
                if let Some(detail) = groups.upgrade() {
                    detail.set_day_group(group);
                }
            },
        );
        true
    }

    /// Switches a day card's breakdown list, rebuilding its rows.
    fn set_day_group(self: &Rc<Self>, group: DayGroup) {
        if self.day_group.get() == group {
            return;
        }
        self.day_group.set(group);
        let selected = self.selected.borrow().clone();
        let latest = self.latest.borrow().clone();
        if let (Some(selection @ Selection::Day(_)), Some(summary)) = (selected, latest) {
            self.render(&selection, &summary);
        }
    }
}

impl Drop for Detail {
    fn drop(&mut self) {
        self.close_secondary();
        if self
            .selected
            .get_mut()
            .as_ref()
            .and_then(Selection::day)
            .is_some()
        {
            (self.on_day)(None);
        }
        self.popover.unparent();
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// A list that scrolls within `rows` rows and keeps the scrollbar in its own
/// column, like the main card's lists. The bar is hidden while the list fits,
/// but its column is always reserved so rows never change width.
fn item_list(
    items: &[Item],
    rows: usize,
    row: &impl Fn(&Item) -> gtk::Widget,
) -> gtk::ScrolledWindow {
    let list = vbox(0);
    for item in items.iter().take(DAY_MAX_ROWS) {
        list.append(&row(item));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Always)
        .overlay_scrolling(false)
        .propagate_natural_height(true)
        .child(&list)
        .css_classes(["list-scroll", "detail-list"])
        .build();
    let bar = scroll.vscrollbar();
    bar.set_opacity(0.0);
    let bar = bar.downgrade();
    scroll.vadjustment().connect_changed(move |adjustment| {
        if let Some(bar) = bar.upgrade() {
            bar.set_opacity(if adjustment.upper() > adjustment.page_size() {
                1.0
            } else {
                0.0
            });
        }
    });
    // Cap the natural height at `rows` rows. Measuring the first row keeps
    // this right across font sizes.
    if items.len() > rows
        && let Some(row) = list.first_child()
    {
        let height = row
            .height()
            .max(row.measure(gtk::Orientation::Vertical, -1).1);
        scroll.set_max_content_height((height.max(1) * rows as i32).max(1));
    }
    scroll
}

struct Item {
    family: &'static str,
    name: String,
    /// Full name, such as a project's path, for the tooltip.
    full: String,
    tokens: u64,
    selection: Option<Selection>,
}

/// Everything a detail card shows, independent of what was selected.
struct Info {
    family: &'static str,
    kind: String,
    title: String,
    usage: Usage,
    tiles: Vec<(&'static str, String)>,
    /// Breakdown lists, or a day's tabs in models-projects-tools order.
    lists: Vec<(&'static str, Vec<Item>)>,
    /// A day's sessions, shown in their own scrolling list.
    sessions: Vec<Item>,
    /// Daily output speeds over the last [`SPEED_DAYS`] days.
    daily_speed: Vec<(NaiveDate, f64)>,
    notes: Vec<String>,
}

fn models(split: &Split, date: Option<NaiveDate>) -> Vec<Item> {
    split
        .models
        .iter()
        .map(|(model, tokens)| Item {
            family: model_family(model),
            name: model.clone(),
            full: model.clone(),
            tokens: *tokens,
            selection: date.map(|date| Selection::DayModel(date, model.clone())),
        })
        .collect()
}

fn tools(split: &Split) -> Vec<Item> {
    split
        .tools
        .iter()
        .map(|(tool, tokens)| Item {
            family: tool_family(*tool),
            name: tool.label().to_owned(),
            full: tool.label().to_owned(),
            tokens: *tokens,
            selection: None,
        })
        .collect()
}

/// Projects with labels that tell apart the ones in this list.
fn projects(split: &Split, date: Option<NaiveDate>) -> Vec<Item> {
    let paths: Vec<&str> = split
        .projects
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    split
        .projects
        .iter()
        .zip(project_labels(&paths))
        .map(|((path, tokens), name)| Item {
            family: "project",
            name,
            full: path.clone(),
            tokens: *tokens,
            selection: date.map(|date| Selection::DayProject(date, path.clone())),
        })
        .collect()
}

fn optional(value: Option<f64>, show: impl Fn(f64) -> String) -> String {
    value.map_or_else(|| "—".to_owned(), show)
}

/// Speed and cache tiles, shared by every kind.
fn usage_tiles(usage: &Usage) -> Vec<(&'static str, String)> {
    vec![
        ("SPEED", optional(usage.speed(), format::speed)),
        (
            "CACHE",
            optional(usage.tokens.cache_hit_rate(), format::percent),
        ),
    ]
}

fn local_time(ts_ms: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(ts_ms).map(|time| time.with_timezone(&Local))
}

impl Info {
    fn find(selection: &Selection, summary: &Summary) -> Option<Info> {
        let date = match selection {
            Selection::DayModel(date, _)
            | Selection::DayProject(date, _)
            | Selection::DaySession(date, _, _) => Some(*date),
            _ => None,
        };
        let (by_model, by_project, period, total) = if let Some(date) = date {
            let detail = summary
                .day_detail
                .as_ref()
                .filter(|detail| detail.date == date)?;
            let index = summary
                .daily
                .binary_search_by_key(&date, |day| day.date)
                .ok()?;
            (
                &detail.by_model,
                &detail.by_project,
                date.format("%b %-d, %Y").to_string().to_uppercase(),
                &summary.daily[index].usage,
            )
        } else {
            (
                &summary.by_model,
                &summary.by_project,
                summary.period.label().to_uppercase(),
                &summary.total,
            )
        };
        let total = total.tokens.total().max(1) as f64;
        // Tiles for a share of the period's usage.
        let row_tiles = |usage: &Usage, split: &Split| {
            let mut tiles = usage_tiles(usage);
            tiles.push((
                "SHARE",
                format::percent(usage.tokens.total() as f64 / total),
            ));
            tiles.push(("SESSIONS", split.sessions.to_string()));
            tiles
        };
        let info = match selection {
            Selection::Model(key) | Selection::DayModel(_, key) => {
                let row = by_model.iter().find(|row| &row.key == key)?;
                Info {
                    family: model_family(key),
                    kind: format!("MODEL · {period}"),
                    title: row.label.clone(),
                    usage: row.usage.clone(),
                    tiles: row_tiles(&row.usage, &row.split),
                    lists: vec![
                        ("TOOLS", tools(&row.split)),
                        ("PROJECTS", projects(&row.split, date)),
                    ],
                    sessions: Vec::new(),
                    daily_speed: row.daily_speed.clone(),
                    notes: Vec::new(),
                }
            }
            Selection::Project(key) | Selection::DayProject(_, key) => {
                let row = by_project.iter().find(|row| &row.key == key)?;
                Info {
                    family: "project",
                    kind: format!("PROJECT · {period}"),
                    title: row.label.clone(),
                    usage: row.usage.clone(),
                    tiles: row_tiles(&row.usage, &row.split),
                    lists: vec![
                        ("MODELS", models(&row.split, date)),
                        ("TOOLS", tools(&row.split)),
                    ],
                    sessions: Vec::new(),
                    daily_speed: Vec::new(),
                    notes: vec![row.key.clone()],
                }
            }
            Selection::Tool(tool) => {
                let row = summary.by_tool.iter().find(|row| row.tool == *tool)?;
                Info {
                    family: tool_family(*tool),
                    kind: format!("TOOL · {period}"),
                    title: tool.label().to_owned(),
                    usage: row.usage.clone(),
                    tiles: row_tiles(&row.usage, &row.split),
                    lists: vec![
                        ("MODELS", models(&row.split, None)),
                        ("PROJECTS", projects(&row.split, None)),
                    ],
                    sessions: Vec::new(),
                    daily_speed: Vec::new(),
                    notes: Vec::new(),
                }
            }
            Selection::Session(tool, id) | Selection::DaySession(_, tool, id) => {
                let sessions = if date.is_some() {
                    &summary.day_detail.as_ref()?.sessions
                } else {
                    &summary.sessions
                };
                let session = sessions
                    .iter()
                    .find(|session| session.tool == *tool && &session.id == id)?;
                let mut tiles = usage_tiles(&session.usage);
                tiles.push((
                    "SPAN",
                    format::duration_until((session.last_activity - session.first_activity) / 1000),
                ));
                tiles.push(("SUBAGENTS", session.subagents.to_string()));
                let models = session
                    .models
                    .iter()
                    .map(|(model, tokens)| Item {
                        family: model_family(model),
                        name: model.clone(),
                        full: model.clone(),
                        tokens: *tokens,
                        selection: date.map(|date| Selection::DayModel(date, model.clone())),
                    })
                    .collect();
                let now = toktop::now_ms();
                let mut notes = Vec::new();
                if let Some(project) = &session.project {
                    notes.push(project.clone());
                }
                if let Some(start) = local_time(session.first_activity) {
                    notes.push(format!(
                        "{} · {} · last {}",
                        tool.label(),
                        start.format("Started %b %-d, %H:%M"),
                        format::ago(session.last_activity, now)
                    ));
                }
                notes.push(format!("ID {id}"));
                Info {
                    family: tool_family(*tool),
                    kind: format!("SESSION · {period}"),
                    title: session.title.clone(),
                    usage: session.usage.clone(),
                    tiles,
                    lists: vec![("MODELS", models)],
                    sessions: Vec::new(),
                    daily_speed: Vec::new(),
                    notes,
                }
            }
            Selection::Day(date) => {
                // A day without usage shows as empty.
                let empty = DayRow {
                    date: *date,
                    usage: Usage::default(),
                    split: Split::default(),
                };
                let day = summary
                    .daily
                    .binary_search_by_key(date, |day| day.date)
                    .map_or(&empty, |index| &summary.daily[index]);
                let split = &day.split;
                let date_link = summary
                    .day_detail
                    .as_ref()
                    .filter(|detail| detail.date == *date)
                    .map(|_| *date);
                let mut tiles = usage_tiles(&day.usage);
                tiles.push(("SESSIONS", split.sessions.to_string()));
                let lists = vec![
                    ("MODELS", models(split, date_link)),
                    ("PROJECTS", projects(split, date_link)),
                    ("TOOLS", tools(split)),
                ];
                let sessions = summary
                    .day_detail
                    .as_ref()
                    .filter(|detail| detail.date == *date)
                    .map(|detail| {
                        detail
                            .sessions
                            .iter()
                            .map(|session| Item {
                                family: tool_family(session.tool),
                                name: session.title.clone(),
                                full: session.title.clone(),
                                tokens: session.usage.tokens.total(),
                                selection: Some(Selection::DaySession(
                                    *date,
                                    session.tool,
                                    session.id.clone(),
                                )),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Info {
                    family: "day",
                    kind: "DAY".to_owned(),
                    title: date.format("%A, %b %-d, %Y").to_string(),
                    usage: day.usage.clone(),
                    tiles,
                    lists,
                    sessions,
                    daily_speed: Vec::new(),
                    notes: Vec::new(),
                }
            }
        };
        Some(info)
    }

    /// Fills `top` with the title and `card` with what follows the count.
    fn build(
        self,
        top: &gtk::Box,
        card: &gtk::Box,
        prices_available: bool,
        day_group: Option<DayGroup>,
        navigate: impl Fn(Selection, &gtk::Button) + 'static,
        on_group: impl Fn(DayGroup) + 'static,
    ) {
        let navigate = Rc::new(navigate);
        let on_group = Rc::new(on_group);
        let day = day_group.is_some();
        let usage = &self.usage;
        let total = usage.tokens.total();

        // One row, wrapped in a button when it opens another card.
        let row = |item: &Item| -> gtk::Widget {
            let content = hbox(10);
            content.add_css_class("row");
            let mark = dot();
            set_family(&mark, item.family);
            let name = ellipsized(&[]);
            name.set_label(&item.name);
            name.set_tooltip_text(Some(&item.full));
            let amount = label(&["numeric"]);
            amount.set_label(&format::tokens(item.tokens));
            amount.set_width_chars(6);
            amount.set_xalign(1.0);
            let percent = label(&["numeric", "dim"]);
            percent.set_label(&format::percent(item.tokens as f64 / total as f64));
            percent.set_width_chars(4);
            percent.set_xalign(1.0);
            content.append(&mark);
            content.append(&name);
            content.append(&amount);
            content.append(&percent);
            if let Some(selection) = &item.selection {
                let button = gtk::Button::builder()
                    .child(&content)
                    .css_classes(["flat", "detail-link"])
                    .tooltip_text(format!("{} · View usage for this day", item.full))
                    .build();
                button.set_cursor_from_name(Some("pointer"));
                let selection = selection.clone();
                let navigate = navigate.clone();
                button.connect_clicked(move |button| navigate(selection.clone(), button));
                button.upcast()
            } else {
                content.upcast()
            }
        };

        let header = hbox(8);
        let mark = dot();
        set_family(&mark, self.family);
        mark.set_valign(gtk::Align::Start);
        mark.add_css_class("detail-dot");
        let title = label(&["detail-title"]);
        title.set_label(&self.title);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_lines(3);
        title.set_max_width_chars(1);
        title.set_hexpand(true);
        header.append(&mark);
        header.append(&title);
        top.append(&header);

        let summary = label(&["dim"]);
        summary.set_label(&if day {
            format!("{} requests", format::grouped(usage.requests))
        } else {
            format!(
                "{} · {} requests",
                super::widget::display_cost(usage.cost.total(), prices_available),
                format::grouped(usage.requests)
            )
        });
        card.append(&summary);
        if total == 0 {
            let empty = label(&["dim", "empty"]);
            empty.set_label("No usage");
            card.append(&empty);
            return;
        }

        let tiles = hbox(6);
        tiles.add_css_class("tiles");
        tiles.set_homogeneous(true);
        for (caption, value) in &self.tiles {
            let tile = vbox(2);
            tile.add_css_class("tile");
            let heading = label(&["caption"]);
            heading.set_label(caption);
            let text = label(&["tile-value"]);
            text.set_label(value);
            tile.append(&heading);
            tile.append(&text);
            tiles.append(&tile);
        }
        card.append(&tiles);

        // Token and cost composition, by the same kinds.
        let reasoning = label(&["caption"]);
        if usage.tokens.reasoning > 0 {
            reasoning.set_label(&format!(
                "output incl. {} reasoning",
                format::tokens(usage.tokens.reasoning)
            ));
        }
        let mut splits = vec![(
            "TOKENS",
            reasoning,
            chart::token_amount as fn(f64) -> String,
            chart::token_parts(&usage.tokens),
        )];
        if prices_available && usage.cost.total() > 0.0 {
            splits.push((
                "COST",
                label(&[]),
                format::cost,
                chart::cost_parts(&usage.cost),
            ));
        }
        for (title, trailing, amount, parts) in splits {
            let section_box = vbox(0);
            section_box.add_css_class("section");
            section_box.append(&section_header(&section(title), &trailing));
            let split = KindSplit::new(amount);
            split.set(parts);
            section_box.append(&split.root);
            card.append(&section_box);
        }

        if !self.daily_speed.is_empty() {
            let speed = vbox(6);
            speed.add_css_class("section");
            let (low, high) = self
                .daily_speed
                .iter()
                .fold((f64::MAX, 0.0f64), |(low, high), (_, speed)| {
                    (low.min(*speed), high.max(*speed))
                });
            let range = label(&["caption"]);
            range.set_label(&if self.daily_speed.len() > 1 {
                // The low end without its unit: `9–92 tok/s`.
                let low = format::speed(low);
                format!("{}–{}", low.trim_end_matches(" tok/s"), format::speed(high))
            } else {
                format::speed(high)
            });
            speed.append(&section_header(
                &section(&format!("SPEED · {SPEED_DAYS}D")),
                &range,
            ));
            speed.append(&chart::speed_line(
                Local::now().date_naive(),
                SPEED_DAYS,
                self.daily_speed,
            ));
            card.append(&speed);
        }

        if let Some(day_group) = day_group {
            // A day groups its breakdown like the main card: one tab per
            // kind, with the rows in a list that scrolls within a fixed
            // height. Sessions get their own scrolling list below.
            let tabs = hbox(0);
            tabs.add_css_class("tabs");
            let mut buttons = Vec::new();
            for (group, title) in DayGroup::ALL {
                let button = gtk::ToggleButton::with_label(title);
                if let Some((_, first)) = buttons.first() {
                    button.set_group(Some(first));
                }
                if group == day_group {
                    button.set_active(true);
                }
                buttons.push((group, button));
            }
            for (group, button) in &buttons {
                tabs.append(button);
                let group = *group;
                let on_group = on_group.clone();
                button.connect_toggled(move |button| {
                    if button.is_active() {
                        on_group(group);
                    }
                });
            }
            let breakdown = vbox(0);
            breakdown.add_css_class("section");
            breakdown.append(&tabs);
            let items = self
                .lists
                .get(day_group.index())
                .map_or(&[][..], |(_, items)| items.as_slice());
            if items.is_empty() {
                let empty = label(&["dim", "empty"]);
                empty.set_label("No usage");
                breakdown.append(&empty);
            } else {
                breakdown.append(&item_list(items, DAY_ROWS, &row));
            }
            card.append(&breakdown);

            if !self.sessions.is_empty() {
                let sessions = vbox(0);
                sessions.add_css_class("section");
                let count = label(&["caption"]);
                count.set_label(&match self.sessions.len() {
                    1 => "1 session".to_owned(),
                    count => format!("{count} sessions"),
                });
                sessions.append(&section_header(&section("SESSIONS"), &count));
                sessions.append(&item_list(&self.sessions, DAY_ROWS, &row));
                card.append(&sessions);
            }
        } else {
            for (title, items) in &self.lists {
                if items.is_empty() {
                    continue;
                }
                let list = vbox(0);
                list.add_css_class("section");
                let count = label(&["caption"]);
                count.set_label(&items.len().to_string());
                list.append(&section_header(&section(title), &count));
                for item in items.iter().take(LIST_ROWS) {
                    list.append(&row(item));
                }
                if items.len() > LIST_ROWS {
                    let rest: u64 = items[LIST_ROWS..].iter().map(|item| item.tokens).sum();
                    let more = label(&["caption", "indented"]);
                    more.set_label(&format!(
                        "+{} more · {}",
                        items.len() - LIST_ROWS,
                        format::tokens(rest)
                    ));
                    list.append(&more);
                }
                card.append(&list);
            }
        }

        for note in &self.notes {
            let text = label(&["caption", "detail-note"]);
            text.set_label(note);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_max_width_chars(1);
            text.set_hexpand(true);
            card.append(&text);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/ui/detail.rs"]
mod tests;
