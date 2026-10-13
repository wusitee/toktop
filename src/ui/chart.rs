//! Cairo charts: a weekly activity heatmap and a daily trend line.
//!
//! Both draw in their widget's CSS `color`, so a user stylesheet can
//! restyle them like any other text.

use chrono::{Datelike, Days, NaiveDate};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use toktop::format;

#[derive(Clone, PartialEq)]
pub struct Day {
    pub date: NaiveDate,
    pub tokens: u64,
    pub cost: Option<f64>,
}

fn tooltip_text(day: &Day) -> String {
    format!(
        "{}\n{} tokens · {}",
        day.date.format("%a %b %-d, %Y"),
        format::grouped(day.tokens),
        day.cost.map_or_else(|| "—".to_owned(), format::cost)
    )
}

fn set_source(cr: &gtk::cairo::Context, color: &gtk::gdk::RGBA, alpha: f64) {
    cr.set_source_rgba(
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        color.alpha() as f64 * alpha,
    );
}

#[derive(Default, PartialEq)]
struct HeatmapData {
    today: Option<NaiveDate>,
    /// Days with usage, sorted by date.
    days: Vec<Day>,
}

/// Grid geometry for a given allocation.
struct Grid {
    cell: f64,
    gap: f64,
    weeks: i64,
    /// x of the first column.
    left: f64,
}

const GAP: f64 = 3.0;
const LABEL_HEIGHT: f64 = 16.0;

impl Grid {
    fn new(width: f64, height: f64) -> Grid {
        let cell = ((height - LABEL_HEIGHT - 6.0 * GAP) / 7.0).max(2.0);
        let weeks = (((width + GAP) / (cell + GAP)).floor() as i64).max(1);
        let used = weeks as f64 * (cell + GAP) - GAP;
        Grid {
            cell,
            gap: GAP,
            weeks,
            left: width - used,
        }
    }

    fn origin(&self, column: i64, row: i64) -> (f64, f64) {
        (
            self.left + column as f64 * (self.cell + self.gap),
            row as f64 * (self.cell + self.gap),
        )
    }
}

/// Monday of the week containing `date`.
fn week_start(date: NaiveDate) -> NaiveDate {
    date - Days::new(date.weekday().num_days_from_monday() as u64)
}

/// GitHub-style grid of days: one column per week, Monday at the top, the
/// current week on the right. As many weeks as fit are shown.
pub struct Heatmap {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<HeatmapData>>,
}

impl Heatmap {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height((7.0 * 11.0 + 6.0 * GAP + LABEL_HEIGHT) as i32)
            .hexpand(true)
            .has_tooltip(true)
            .css_classes(["heatmap"])
            .build();
        let data: Rc<RefCell<HeatmapData>> = Rc::default();
        let state = data.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw_heatmap(area, cr, width as f64, height as f64, &state.borrow());
        });
        let state = data.clone();
        area.connect_query_tooltip(move |area, x, y, _keyboard, tooltip| {
            let data = state.borrow();
            let Some(today) = data.today else {
                return false;
            };
            let grid = Grid::new(area.width() as f64, area.height() as f64);
            let pitch = grid.cell + grid.gap;
            let (column, row) = (
                ((x as f64 - grid.left) / pitch).floor() as i64,
                (y as f64 / pitch).floor() as i64,
            );
            if !(0..grid.weeks).contains(&column) || !(0..7).contains(&row) {
                return false;
            }
            let monday = week_start(today) - Days::new(((grid.weeks - 1 - column) * 7) as u64);
            let date = monday + Days::new(row as u64);
            if date > today {
                return false;
            }
            let day = match data.days.binary_search_by_key(&date, |day| day.date) {
                Ok(index) => data.days[index].clone(),
                Err(_) => Day {
                    date,
                    tokens: 0,
                    cost: None,
                },
            };
            tooltip.set_text(Some(&tooltip_text(&day)));
            true
        });
        Heatmap { area, data }
    }

    pub fn set(&self, today: NaiveDate, days: Vec<Day>) {
        let data = HeatmapData {
            today: Some(today),
            days,
        };
        if *self.data.borrow() != data {
            self.data.replace(data);
            self.area.queue_draw();
        }
    }
}

fn draw_heatmap(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: f64,
    height: f64,
    data: &HeatmapData,
) {
    let Some(today) = data.today else { return };
    let color = area.color();
    let grid = Grid::new(width, height);
    let first = week_start(today) - Days::new(((grid.weeks - 1) * 7) as u64);
    let visible: Vec<&Day> = data
        .days
        .iter()
        .filter(|day| day.date >= first && day.date <= today && day.tokens > 0)
        .collect();
    // Quartiles of the visible active days set the four intensity levels.
    let mut values: Vec<u64> = visible.iter().map(|day| day.tokens).collect();
    values.sort_unstable();
    let quartile = |q: usize| values.get(values.len() * q / 4).copied().unwrap_or(0);
    let thresholds = [quartile(1), quartile(2), quartile(3)];
    let radius = (grid.cell / 4.0).min(2.5);

    let mut active = visible.into_iter().peekable();
    for column in 0..grid.weeks {
        let monday = first + Days::new((column * 7) as u64);
        for row in 0..7 {
            let date = monday + Days::new(row as u64);
            if date > today {
                break;
            }
            while active.peek().is_some_and(|day| day.date < date) {
                active.next();
            }
            let tokens = active
                .peek()
                .filter(|day| day.date == date)
                .map_or(0, |day| day.tokens);
            let alpha = if tokens == 0 {
                0.08
            } else {
                match thresholds.iter().filter(|limit| tokens > **limit).count() {
                    0 => 0.3,
                    1 => 0.5,
                    2 => 0.75,
                    _ => 1.0,
                }
            };
            let (x, y) = grid.origin(column, row);
            set_source(cr, &color, alpha);
            rounded_rect(cr, x, y, grid.cell, grid.cell, radius);
            let _ = cr.fill();
        }
    }

    // Month names under the first column of each month.
    let label_y = 7.0 * (grid.cell + grid.gap) + LABEL_HEIGHT - 4.0;
    let mut last_end = f64::MIN;
    set_source(cr, &color, 0.55);
    cr.select_font_face(
        "monospace",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    cr.set_font_size(10.0);
    for column in 0..grid.weeks {
        let monday = first + Days::new((column * 7) as u64);
        if monday.day() > 7 {
            continue;
        }
        let (x, _) = grid.origin(column, 0);
        let name = monday.format("%b").to_string();
        let Ok(extents) = cr.text_extents(&name) else {
            continue;
        };
        if x < last_end + 4.0 || x + extents.x_advance() > width {
            continue;
        }
        last_end = x + extents.x_advance();
        cr.move_to(x, label_y);
        let _ = cr.show_text(&name);
    }
}

/// Daily totals as a filled line, oldest on the left.
pub struct Trend {
    pub area: gtk::DrawingArea,
    days: Rc<RefCell<Vec<Day>>>,
}

impl Trend {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(44)
            .hexpand(true)
            .has_tooltip(true)
            .css_classes(["trend"])
            .build();
        let days: Rc<RefCell<Vec<Day>>> = Rc::default();
        let data = days.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw_trend(area, cr, width as f64, height as f64, &data.borrow());
        });
        let data = days.clone();
        area.connect_query_tooltip(move |area, x, _y, _keyboard, tooltip| {
            let days = data.borrow();
            if days.len() < 2 || area.width() == 0 {
                return false;
            }
            let step = area.width() as f64 / (days.len() - 1) as f64;
            let index = ((x as f64 / step).round() as usize).min(days.len() - 1);
            tooltip.set_text(Some(&tooltip_text(&days[index])));
            true
        });
        Trend { area, days }
    }

    pub fn set(&self, days: Vec<Day>) {
        if *self.days.borrow() != days {
            self.days.replace(days);
            self.area.queue_draw();
        }
    }
}

fn draw_trend(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: f64,
    height: f64,
    days: &[Day],
) {
    if days.len() < 2 {
        return;
    }
    let color = area.color();
    let max = days.iter().map(|day| day.tokens).max().unwrap_or(0).max(1) as f64;
    let (top, bottom) = (3.0, height - 1.5);
    let step = width / (days.len() - 1) as f64;
    let point = |index: usize, tokens: u64| {
        (
            index as f64 * step,
            bottom - tokens as f64 / max * (bottom - top),
        )
    };

    cr.new_path();
    for (index, day) in days.iter().enumerate() {
        let (x, y) = point(index, day.tokens);
        cr.line_to(x, y);
    }
    let line = cr.copy_path();
    cr.line_to(width, height);
    cr.line_to(0.0, height);
    cr.close_path();
    let fill = gtk::cairo::LinearGradient::new(0.0, top, 0.0, height);
    fill.add_color_stop_rgba(
        0.0,
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        0.35,
    );
    fill.add_color_stop_rgba(
        1.0,
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        0.0,
    );
    let _ = cr.set_source(&fill);
    let _ = cr.fill();

    cr.new_path();
    if let Ok(line) = line {
        cr.append_path(&line);
    }
    set_source(cr, &color, 1.0);
    cr.set_line_width(1.5);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    let _ = cr.stroke();

    let last = days.len() - 1;
    let (x, y) = point(last, days[last].tokens);
    cr.arc(x - 2.5, y, 2.5, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}
