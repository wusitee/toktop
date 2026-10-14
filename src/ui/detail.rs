//! Detail card for a model, project, tool, session or day, shown beside the
//! main card when its row or chart cell is clicked.
//!
//! The card is a popover rebuilt from each summary while it is open, so it
//! stays live; closed, it holds no widgets. It stays open until closed with
//! its button or by clicking the same item again, so clicking another item
//! switches it directly.

use super::widget::{
    dot, ellipsized, hbox, label, model_family, section, section_header, set_family, tool_family,
    vbox,
};
use chrono::{DateTime, Local, NaiveDate};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use toktop::aggregate::{DayRow, Split, Summary, Usage, project_labels};
use toktop::config::Anchor;
use toktop::format;
use toktop::model::Tool;

/// Entries shown per list before the rest are summarized.
const LIST_ROWS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Model(String),
    /// Full project path.
    Project(String),
    Tool(Tool),
    Session(Tool, String),
    Day(NaiveDate),
}

pub struct Detail {
    popover: gtk::Popover,
    kind: gtk::Label,
    content: gtk::Box,
    scroll: gtk::ScrolledWindow,
    selected: Rc<RefCell<Option<Selection>>>,
}

impl Detail {
    /// A detail card attached to `card`, opening on the side away from the
    /// screen edge the card is anchored to.
    pub fn new(card: &gtk::Box, anchor: Anchor, width: i32) -> Detail {
        // The header stays while the body is rebuilt, so a refresh cannot
        // cancel a click on the close button.
        let kind = label(&["caption", "detail-kind"]);
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close")
            .css_classes(["icon"])
            .valign(gtk::Align::Center)
            .build();
        let content = vbox(0);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&content)
            .build();
        let frame = vbox(0);
        frame.set_size_request(width, -1);
        frame.append(&section_header(&kind, &close));
        frame.append(&scroll);
        let popover = gtk::Popover::builder()
            .has_arrow(false)
            .autohide(false)
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
        let selected: Rc<RefCell<Option<Selection>>> = Default::default();
        let state = selected.clone();
        let body = content.clone();
        popover.connect_closed(move |_| {
            state.replace(None);
            clear(&body);
        });
        Detail {
            popover,
            kind,
            content,
            scroll,
            selected,
        }
    }

    /// Caps the card's height; a taller body scrolls.
    pub fn set_max_height(&self, height: i32) {
        // Leaves room for the header and padding.
        self.scroll.set_max_content_height((height - 60).max(100));
    }

    /// Shows `selection` level with the card rows from `y` to `y + height`
    /// (in card coordinates). Showing the open item again closes the card.
    pub fn show(&self, selection: Selection, y: f64, height: f64, summary: &Summary) {
        if self.selected.borrow().as_ref() == Some(&selection) || !self.render(&selection, summary)
        {
            self.popover.popdown();
            return;
        }
        self.selected.replace(Some(selection));
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
    pub fn refresh(&self, summary: &Summary) {
        let selected = self.selected.borrow().clone();
        if let Some(selection) = selected
            && !self.render(&selection, summary)
        {
            self.popover.popdown();
        }
    }

    /// Fills the card; false when the selection is not in the summary.
    fn render(&self, selection: &Selection, summary: &Summary) -> bool {
        let Some(info) = Info::find(selection, summary) else {
            return false;
        };
        self.kind.set_label(&info.kind);
        clear(&self.content);
        info.build(&self.content, summary.prices_available);
        true
    }
}

impl Drop for Detail {
    fn drop(&mut self) {
        self.popover.unparent();
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

struct Item {
    family: &'static str,
    name: String,
    /// Full name, such as a project's path, for the tooltip.
    full: String,
    tokens: u64,
}

/// Everything a detail card shows, independent of what was selected.
struct Info {
    family: &'static str,
    kind: String,
    title: String,
    usage: Usage,
    tiles: Vec<(&'static str, String)>,
    lists: Vec<(&'static str, Vec<Item>)>,
    notes: Vec<String>,
}

fn models(split: &Split) -> Vec<Item> {
    split
        .models
        .iter()
        .map(|(model, tokens)| Item {
            family: model_family(model),
            name: model.clone(),
            full: model.clone(),
            tokens: *tokens,
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
        })
        .collect()
}

/// Projects with labels that tell apart the ones in this list.
fn projects(split: &Split) -> Vec<Item> {
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
        let period = summary.period.label().to_uppercase();
        let total = summary.total.tokens.total().max(1) as f64;
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
            Selection::Model(key) => {
                let row = summary.by_model.iter().find(|row| &row.key == key)?;
                Info {
                    family: model_family(key),
                    kind: format!("MODEL · {period}"),
                    title: row.label.clone(),
                    usage: row.usage.clone(),
                    tiles: row_tiles(&row.usage, &row.split),
                    lists: vec![
                        ("TOOLS", tools(&row.split)),
                        ("PROJECTS", projects(&row.split)),
                    ],
                    notes: Vec::new(),
                }
            }
            Selection::Project(key) => {
                let row = summary.by_project.iter().find(|row| &row.key == key)?;
                Info {
                    family: "project",
                    kind: format!("PROJECT · {period}"),
                    title: row.label.clone(),
                    usage: row.usage.clone(),
                    tiles: row_tiles(&row.usage, &row.split),
                    lists: vec![("MODELS", models(&row.split)), ("TOOLS", tools(&row.split))],
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
                        ("MODELS", models(&row.split)),
                        ("PROJECTS", projects(&row.split)),
                    ],
                    notes: Vec::new(),
                }
            }
            Selection::Session(tool, id) => {
                let session = summary
                    .sessions
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
                let mut tiles = usage_tiles(&day.usage);
                tiles.push(("SESSIONS", split.sessions.to_string()));
                Info {
                    family: "day",
                    kind: "DAY".to_owned(),
                    title: date.format("%A, %b %-d, %Y").to_string(),
                    usage: day.usage.clone(),
                    tiles,
                    lists: vec![
                        ("MODELS", models(split)),
                        ("TOOLS", tools(split)),
                        ("PROJECTS", projects(split)),
                    ],
                    notes: Vec::new(),
                }
            }
        };
        Some(info)
    }

    fn build(self, card: &gtk::Box, prices_available: bool) {
        let usage = &self.usage;
        let total = usage.tokens.total();

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
        card.append(&header);

        let hero = label(&["detail-hero"]);
        hero.set_label(&format::grouped(total));
        card.append(&hero);
        let summary = label(&["dim"]);
        summary.set_label(&format!(
            "{} · {} requests",
            super::widget::display_cost(usage.cost, prices_available),
            format::grouped(usage.requests)
        ));
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

        // Token composition.
        let composition = vbox(0);
        composition.add_css_class("section");
        composition.append(&section_header(&section("TOKENS"), &label(&[])));
        let tokens = &usage.tokens;
        for (name, value) in [
            ("Input", tokens.input),
            ("Output", tokens.output),
            ("Reasoning", tokens.reasoning),
            ("Cache read", tokens.cache_read),
            ("Cache write", tokens.cache_write),
        ] {
            if value == 0 {
                continue;
            }
            let share = value as f64 / total as f64;
            let row = hbox(10);
            row.add_css_class("row");
            let name_label = label(&["dim"]);
            name_label.set_label(name);
            name_label.set_width_chars(11);
            let bar = gtk::ProgressBar::builder()
                .fraction(share)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            let amount = label(&["numeric"]);
            amount.set_label(&format::tokens(value));
            amount.set_width_chars(6);
            amount.set_xalign(1.0);
            let percent = label(&["numeric", "dim"]);
            percent.set_label(&format::percent(share));
            percent.set_width_chars(4);
            percent.set_xalign(1.0);
            row.append(&name_label);
            row.append(&bar);
            row.append(&amount);
            row.append(&percent);
            composition.append(&row);
        }
        card.append(&composition);

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
                let row = hbox(10);
                row.add_css_class("row");
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
                row.append(&mark);
                row.append(&name);
                row.append(&amount);
                row.append(&percent);
                list.append(&row);
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
