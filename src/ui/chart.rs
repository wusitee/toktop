//! Daily token bar chart drawn with cairo.

use chrono::NaiveDate;
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

pub struct Chart {
    pub area: gtk::DrawingArea,
    days: Rc<RefCell<Vec<Day>>>,
}

impl Chart {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(88)
            .hexpand(true)
            .has_tooltip(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let days: Rc<RefCell<Vec<Day>>> = Rc::default();

        let data = days.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw(area, cr, width as f64, height as f64, &data.borrow());
        });
        let data = days.clone();
        area.connect_query_tooltip(move |area, x, _y, _keyboard, tooltip| {
            let days = data.borrow();
            if days.is_empty() || area.width() == 0 {
                return false;
            }
            let index = (x as f64 / area.width() as f64 * days.len() as f64) as usize;
            let Some(day) = days.get(index.min(days.len() - 1)) else {
                return false;
            };
            tooltip.set_text(Some(&format!(
                "{}\n{} tokens · {}",
                day.date.format("%a %b %-d"),
                format::tokens(day.tokens),
                day.cost.map_or_else(|| "—".to_owned(), format::cost)
            )));
            true
        });
        Chart { area, days }
    }

    pub fn set(&self, days: Vec<Day>) {
        if *self.days.borrow() != days {
            self.days.replace(days);
            self.area.queue_draw();
        }
    }
}

fn draw(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: f64, height: f64, days: &[Day]) {
    if days.is_empty() {
        return;
    }
    let fg = area.color();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let max = days.iter().map(|day| day.tokens).max().unwrap_or(0).max(1) as f64;
    let gap = 3.0;
    let count = days.len() as f64;
    let bar = ((width - gap * (count - 1.0)) / count).max(1.0);
    for (index, day) in days.iter().enumerate() {
        let x = index as f64 * (bar + gap);
        let (bar_height, color, alpha) = if day.tokens == 0 {
            (2.0, fg, 0.15)
        } else {
            let alpha = if index + 1 == days.len() { 1.0 } else { 0.6 };
            ((day.tokens as f64 / max * height).max(3.0), accent, alpha)
        };
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            alpha,
        );
        rounded_top(
            cr,
            x,
            height - bar_height,
            bar,
            bar_height,
            (bar / 3.0).min(3.0),
        );
        let _ = cr.fill();
    }
}

fn rounded_top(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(h).min(w / 2.0);
    cr.new_path();
    cr.move_to(x, y + h);
    cr.line_to(x, y + r);
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.line_to(x + w - r, y);
    cr.arc(x + w - r, y + r, r, 1.5 * std::f64::consts::PI, 0.0);
    cr.line_to(x + w, y + h);
    cr.close_path();
}
