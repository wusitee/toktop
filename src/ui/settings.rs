//! Settings window. Every change applies immediately through `change`.

use adw::prelude::*;
use std::rc::Rc;
use toktop::config::{Anchor, Config, Layer, Sections};
use toktop::model::Tool;
use toktop::paths;

/// A card section's flag in [`Sections`].
type Field = fn(&mut Sections) -> &mut bool;

/// Applies one edit to the settings.
pub type Change = Rc<dyn Fn(&dyn Fn(&mut Config))>;

const LAYERS: [(Layer, &str); 5] = [
    (Layer::Background, "Background"),
    (Layer::Bottom, "Bottom"),
    (Layer::Top, "Top"),
    (Layer::Overlay, "Overlay"),
    (Layer::Window, "Window"),
];

const ANCHORS: [(Anchor, &str); 9] = [
    (Anchor::TopLeft, "Top left"),
    (Anchor::Top, "Top"),
    (Anchor::TopRight, "Top right"),
    (Anchor::Left, "Left"),
    (Anchor::Center, "Center"),
    (Anchor::Right, "Right"),
    (Anchor::BottomLeft, "Bottom left"),
    (Anchor::Bottom, "Bottom"),
    (Anchor::BottomRight, "Bottom right"),
];

pub fn window(app: &adw::Application, config: &Config, change: Change) -> adw::Window {
    let page = adw::PreferencesPage::new();
    page.add(&sources(config, &change));
    page.add(&card(config, &change));
    page.add(&placement(config, &change));
    page.add(&general(config, &change));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    let window = adw::Window::builder()
        .application(app)
        .title("toktop Settings")
        .default_width(480)
        .default_height(720)
        .content(&view)
        .build();
    // Weak: the window owns this controller.
    let keys = gtk::EventControllerKey::new();
    let target = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape
            && let Some(window) = target.upgrade()
        {
            window.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window
}

fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build()
}

fn switch(
    title: &str,
    subtitle: &str,
    active: bool,
    on: impl Fn(bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build();
    row.connect_active_notify(move |row| on(row.is_active()));
    row
}

fn spin(title: &str, range: (f64, f64), value: f64, on: impl Fn(f64) + 'static) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(range.0, range.1, 1.0);
    row.set_title(title);
    row.set_value(value);
    row.connect_value_notify(move |row| on(row.value()));
    row
}

fn combo(
    title: &str,
    labels: &[&str],
    selected: usize,
    on: impl Fn(usize) + 'static,
) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(labels))
        .selected(selected as u32)
        .build();
    row.connect_selected_notify(move |row| on(row.selected() as usize));
    row
}

/// `path` with the home directory shortened to `~`.
fn display_path(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

fn sources(config: &Config, change: &Change) -> adw::PreferencesGroup {
    let group = group(
        "Sources",
        "Tools whose logs are read. A turned-off tool's files are not read or watched.",
    );
    for tool in Tool::ALL {
        let roots: Vec<String> = paths::source_roots(tool)
            .iter()
            .map(|path| display_path(path))
            .collect();
        let change = change.clone();
        group.add(&switch(
            tool.label(),
            &roots.join(", "),
            config.tools.contains(&tool),
            move |on| {
                change(&|config| {
                    // Keep the canonical order.
                    let enabled = |candidate: Tool| {
                        if candidate == tool {
                            on
                        } else {
                            config.tools.contains(&candidate)
                        }
                    };
                    config.tools = Tool::ALL.into_iter().filter(|t| enabled(*t)).collect();
                });
            },
        ));
    }
    group
}

fn card(config: &Config, change: &Change) -> adw::PreferencesGroup {
    let group = group("Card", "Parts of the widget that are shown.");
    let entries: [(&str, &str, Field); 5] = [
        ("Statistics", "Speed, live rate, cache and streak", |s| {
            &mut s.stats
        }),
        ("Codex limits", "Rate-limit windows from Codex logs", |s| {
            &mut s.limits
        }),
        ("Breakdown", "Models, projects and tools", |s| {
            &mut s.breakdown
        }),
        ("Activity", "Daily heatmap", |s| &mut s.activity),
        ("Trend", "Daily tokens over 30 days", |s| &mut s.trend),
    ];
    let mut sections = config.widget.sections;
    for (index, (title, subtitle, field)) in entries.into_iter().enumerate() {
        if index == 3 {
            // Also hidden by a count of zero.
            let change = change.clone();
            group.add(&switch(
                "Sessions",
                "Recent sessions",
                config.widget.shown_sessions() > 0,
                move |on| change(&|config| config.widget.show_sessions(on)),
            ));
        }
        let change = change.clone();
        group.add(&switch(title, subtitle, *field(&mut sections), move |on| {
            change(&|config| *field(&mut config.widget.sections) = on);
        }));
    }
    let rows = change.clone();
    group.add(&spin(
        "Breakdown rows",
        (1.0, 20.0),
        config.widget.rows as f64,
        move |value| rows(&|config| config.widget.rows = value as usize),
    ));
    let sessions = change.clone();
    group.add(&spin(
        "Sessions shown",
        (1.0, 20.0),
        config.widget.sessions.max(1) as f64,
        move |value| sessions(&|config| config.widget.set_session_count(value as usize)),
    ));
    group
}

fn placement(config: &Config, change: &Change) -> adw::PreferencesGroup {
    let widget = &config.widget;
    let group = group("Placement", "");
    let labels: Vec<&str> = LAYERS.iter().map(|(_, label)| *label).collect();
    let selected = LAYERS.iter().position(|(layer, _)| *layer == widget.layer);
    let layer = change.clone();
    let row = combo("Layer", &labels, selected.unwrap_or(1), move |index| {
        if let Some(&(value, _)) = LAYERS.get(index) {
            layer(&|config| config.widget.layer = value);
        }
    });
    row.set_subtitle("Background and Bottom sit below windows; Top and Overlay above them");
    group.add(&row);

    let labels: Vec<&str> = ANCHORS.iter().map(|(_, label)| *label).collect();
    let selected = ANCHORS
        .iter()
        .position(|(anchor, _)| *anchor == widget.anchor);
    let anchor = change.clone();
    group.add(&combo(
        "Position",
        &labels,
        selected.unwrap_or(2),
        move |index| {
            if let Some(&(value, _)) = ANCHORS.get(index) {
                anchor(&|config| config.widget.anchor = value);
            }
        },
    ));

    // "Automatic", then every connected monitor, keeping a configured but
    // disconnected one selectable.
    let mut outputs = vec![String::new()];
    if let Some(display) = gtk::gdk::Display::default() {
        let monitors = display.monitors();
        outputs.extend((0..monitors.n_items()).filter_map(|index| {
            let monitor = monitors.item(index)?.downcast::<gtk::gdk::Monitor>().ok()?;
            Some(monitor.connector()?.to_string())
        }));
    }
    if !outputs.contains(&widget.output) {
        outputs.push(widget.output.clone());
    }
    let labels: Vec<&str> = outputs
        .iter()
        .map(|name| if name.is_empty() { "Automatic" } else { name })
        .collect();
    let selected = outputs.iter().position(|name| *name == widget.output);
    let output = change.clone();
    let names = outputs.clone();
    group.add(&combo(
        "Monitor",
        &labels,
        selected.unwrap_or(0),
        move |index| {
            if let Some(name) = names.get(index) {
                output(&|config| config.widget.output = name.clone());
            }
        },
    ));

    let margin = change.clone();
    group.add(&spin(
        "Margin",
        (0.0, 400.0),
        widget.margin as f64,
        move |value| margin(&|config| config.widget.margin = value as i32),
    ));
    let width = change.clone();
    group.add(&spin(
        "Width",
        (280.0, 1200.0),
        widget.width as f64,
        move |value| width(&|config| config.widget.width = value as i32),
    ));
    group
}

fn general(config: &Config, change: &Change) -> adw::PreferencesGroup {
    let group = group("General", "");
    let change = change.clone();
    group.add(&switch(
        "Tray icon",
        "Takes effect at the next start",
        config.tray,
        move |on| change(&|config| config.tray = on),
    ));
    group
}
