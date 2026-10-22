//! Application shell, widget window and tray icon.
//!
//! [`Shell`] lives for the whole process: it owns the worker, the latest
//! summary, the settings and the tray icon. The widget window is created on
//! demand and destroyed when hidden, so a hidden toktop does not keep its
//! widgets in memory.

mod chart;
mod detail;
mod hyprland;
mod layer;
mod settings;
mod tray;
mod widget;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use toktop::aggregate::{Period, Summary};
use toktop::config::{self, Config, Layer};
use toktop::format;
use toktop::worker::{self, Msg};

const APP_ID: &str = "io.github.wusitee.toktop";

pub struct Options {
    /// Start hidden, unless the widget is part of the desktop.
    pub background: bool,
    /// Show or hide the widget of a running instance.
    pub toggle: bool,
    /// Open the settings window.
    pub settings: bool,
}

pub fn run(options: Options) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    let (background, toggle, open_settings) =
        (options.background, options.toggle, options.settings);
    let shell = Rc::new(Shell::new(&app, options));
    // `register` emits `startup` on the primary instance.
    let started = shell.clone();
    app.connect_startup(move |_| started.start());
    if let Err(error) = app.register(gio::Cancellable::NONE) {
        eprintln!("toktop: {error}");
        return glib::ExitCode::FAILURE;
    }
    if app.is_remote() {
        // Another instance is running: forward the request to it.
        if toggle {
            app.activate_action("toggle", None);
        } else if open_settings {
            app.activate_action("settings", None);
        } else if !background {
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
    let weak = Rc::downgrade(&shell);
    let toggle_action = gio::ActionEntry::builder("toggle")
        .activate(move |_: &adw::Application, _, _| {
            if let Some(shell) = weak.upgrade() {
                shell.toggle_window();
            }
        })
        .build();
    let weak = Rc::downgrade(&shell);
    let refresh = gio::ActionEntry::builder("refresh-prices")
        .activate(move |_: &adw::Application, _, _| {
            if let Some(shell) = weak.upgrade() {
                shell.send(Msg::RefreshPrices);
            }
        })
        .build();
    let weak = Rc::downgrade(&shell);
    let settings_action = gio::ActionEntry::builder("settings")
        .activate(move |_: &adw::Application, _, _| {
            if let Some(shell) = weak.upgrade() {
                shell.open_settings();
            }
        })
        .build();
    app.add_action_entries([quit, toggle_action, refresh, settings_action]);
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

/// Built-in style, then the user's stylesheet over it.
fn load_style() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let builtin = gtk::CssProvider::new();
    builtin.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &builtin,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let path = config::style_path();
    if path.exists() {
        let user = gtk::CssProvider::new();
        user.connect_parsing_error(|_, section, error| {
            eprintln!("toktop: style.css {}: {error}", section.to_str());
        });
        user.load_from_path(&path);
        gtk::style_context_add_provider_for_display(
            &display,
            &user,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
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
    /// Whether the widget is a layer-shell surface rather than a window.
    layered: Cell<bool>,
    worker: RefCell<Option<Sender<Msg>>>,
    latest: RefCell<Option<Rc<Summary>>>,
    ui: RefCell<Option<Ui>>,
    settings: RefCell<Option<adw::Window>>,
    tray: RefCell<Option<tray::Tray>>,
    /// Keeps the application running while the widget is hidden.
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    activated: Cell<bool>,
}

/// An open widget window.
struct Ui {
    window: gtk::ApplicationWindow,
    widget: Rc<widget::Widget>,
    menu: gtk::PopoverMenu,
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.menu.unparent();
    }
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
            layered: Cell::new(false),
            worker: RefCell::default(),
            latest: RefCell::default(),
            ui: RefCell::default(),
            settings: RefCell::default(),
            tray: RefCell::default(),
            hold: RefCell::default(),
            activated: Cell::new(false),
        }
    }

    fn start(self: &Rc<Self>) {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        load_style();
        let (period, tools, want_tray, layer) = {
            let config = self.config.borrow();
            (
                config.period,
                config.tools.clone(),
                config.tray,
                config.widget.layer,
            )
        };
        if layer != Layer::Window {
            if layer::is_supported() {
                self.layered.set(true);
            } else {
                eprintln!(
                    "toktop: the compositor has no layer-shell support{}; using a window",
                    if cfg!(feature = "layer-shell") {
                        ""
                    } else {
                        " (built without the layer-shell feature)"
                    }
                );
            }
        }

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
        self.update_hold();
    }

    /// Keeps toktop running while a hidden widget can be summoned again:
    /// a layer widget with `toktop --toggle`, any widget from the tray.
    fn update_hold(&self) {
        let needed = self.layered.get() || self.tray.borrow().is_some();
        if needed != self.hold.borrow().is_some() {
            self.hold.replace(needed.then(|| self.app.hold()));
        }
    }

    fn start_tray(self: &Rc<Self>, period: Period) {
        let (tx, rx) = async_channel::unbounded();
        let state = tray::State {
            title: "toktop".into(),
            description: String::new(),
            period,
        };
        // Panels look the icon up by name, so fall back to a stock icon when
        // toktop's own is not installed into an icon theme.
        let installed = gdk::Display::default()
            .is_some_and(|display| gtk::IconTheme::for_display(&display).has_icon(APP_ID));
        let icon = if installed {
            APP_ID
        } else {
            "utilities-system-monitor"
        };
        match tray::Tray::spawn(tx, icon, state) {
            Ok(tray) => {
                self.tray.replace(Some(tray));
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
                    tray::Command::Settings => shell.open_settings(),
                    tray::Command::Quit => shell.app.quit(),
                }
            }
        });
    }

    fn activate(self: &Rc<Self>) {
        let first = !self.activated.replace(true);
        if first && self.options.settings {
            self.open_settings();
        }
        if first && (self.options.background || self.options.settings) {
            let desktop = self.layered.get() && self.config.borrow().widget.layer.is_desktop();
            if !desktop {
                // The settings window keeps toktop running.
                if self.hold.borrow().is_some() || self.options.settings {
                    return;
                }
                eprintln!("toktop: no tray icon available; opening the window");
            }
        }
        self.show_window();
    }

    fn send(&self, msg: Msg) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            let _ = worker.send(msg);
        }
    }

    fn show_summary(&self, summary: Summary) {
        let summary = Rc::new(summary);
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.widget.update(&summary);
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
        let ui = self.build_window();
        if let Some(summary) = self.latest.borrow().as_ref() {
            ui.widget.update(summary);
        }
        ui.window.present();
        self.ui.replace(Some(ui));
    }

    fn build_window(self: &Rc<Self>) -> Ui {
        let config = self.config.borrow().clone();
        let settings = &config.widget;
        let shell = Rc::downgrade(self);
        let day_shell = shell.clone();
        let widget = widget::Widget::new(
            config.period,
            settings,
            move |period| {
                if let Some(shell) = shell.upgrade() {
                    shell.set_period(period);
                }
            },
            move |day| {
                if let Some(shell) = day_shell.upgrade() {
                    shell.send(Msg::DetailDay(day));
                }
            },
        );

        let window = gtk::ApplicationWindow::builder()
            .application(&self.app)
            .title("toktop")
            .resizable(false)
            .build();
        window.add_css_class("toktop");
        let layered = self.layered.get();
        let configured = layer::find_monitor(&settings.output);
        if layered {
            layer::place(&window, settings, configured.as_ref());
            // Hyprland focuses an on-demand layer when it maps; keep the
            // focus in an open settings window.
            if self.settings.borrow().is_some() {
                layer::set_keyboard(&window, false);
            }
        } else {
            window.add_css_class("windowed");
        }

        // Scroll only when the card is taller than the screen.
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .child(&widget.root)
            .build();
        // Panels reserve space the widget must not grow under; only
        // Hyprland reports it.
        // Hyprland's focused monitor is where an unplaced widget appears,
        // also when the configured one is disconnected.
        let output = configured.as_ref().and_then(|monitor| monitor.connector());
        let reserved = layered
            .then(|| hyprland::reserved(output.as_deref()))
            .flatten();
        let monitor = configured
            .or_else(|| layer::find_monitor(&reserved.as_ref()?.connector))
            .or_else(|| {
                gdk::Display::default()?
                    .monitors()
                    .item(0)?
                    .downcast::<gdk::Monitor>()
                    .ok()
            });
        if let Some(monitor) = monitor {
            let used = match &reserved {
                _ if !layered => 96,
                Some(reserved) => 2 * settings.margin + reserved.top + reserved.bottom,
                None => 2 * settings.margin,
            };
            let height = (monitor.geometry().height() - used).max(200);
            scrolled.set_max_content_height(height);
            widget.set_max_height(height);
        }
        window.set_child(Some(&scrolled));

        // Right-click menu.
        let model = gio::Menu::new();
        model.append(Some("Settings"), Some("app.settings"));
        model.append(Some("Refresh prices"), Some("app.refresh-prices"));
        model.append(Some("Hide"), Some("app.toggle"));
        model.append(Some("Quit"), Some("app.quit"));
        let menu = gtk::PopoverMenu::from_model(Some(&model));
        menu.set_has_arrow(false);
        menu.set_parent(&widget.root);
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let popover = menu.clone();
        click.connect_pressed(move |_, _, x, y| {
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        });
        widget.root.add_controller(click);

        // Escape dismisses a widget the user summoned over other windows.
        if !layered || !settings.layer.is_desktop() {
            let keys = gtk::EventControllerKey::new();
            // Weak: the window owns this controller.
            let target = window.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    if let Some(window) = target.upgrade() {
                        window.close();
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            window.add_controller(keys);
        }

        let shell = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            debug_event("window close-request");
            if let Some(shell) = shell.upgrade() {
                shell.close_window();
            }
            glib::Propagation::Proceed
        });
        Ui {
            window,
            widget,
            menu,
        }
    }

    /// Drops the widgets; GTK destroys the window after this returns.
    fn close_window(&self) {
        self.ui.replace(None);
        // Return freed widget memory once the window is gone.
        glib::idle_add_local_once(worker::trim_heap);
    }

    fn toggle_window(self: &Rc<Self>) {
        let open = self.ui.borrow().as_ref().map(|ui| ui.window.clone());
        match open {
            // An ordinary window in the background is raised first.
            Some(window) if !self.layered.get() && !window.is_active() => window.present(),
            Some(window) => window.close(),
            None => self.show_window(),
        }
    }

    fn set_period(&self, period: Period) {
        if self.config.borrow().period == period {
            return;
        }
        self.update_config(|config| config.period = period);
        self.send(Msg::Period(period));
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.widget.select_period(period);
        }
    }

    /// Applies `change` and saves it. Returns the settings from before.
    fn update_config(&self, change: impl FnOnce(&mut Config)) -> Config {
        let mut config = self.config.borrow_mut();
        let before = config.clone();
        change(&mut config);
        if *config != before
            && self.config_writable
            && let Err(error) = config.save(&config::path())
        {
            eprintln!("toktop: could not save settings: {error}");
        }
        before
    }

    fn open_settings(self: &Rc<Self>) {
        if let Some(window) = self.settings.borrow().as_ref() {
            window.present();
            return;
        }
        let shell = Rc::downgrade(self);
        let change: settings::Change = Rc::new(move |edit| {
            if let Some(shell) = shell.upgrade() {
                shell.apply_settings(edit);
            }
        });
        let window = settings::window(&self.app, &self.config.borrow(), change);
        let shell = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(shell) = shell.upgrade() {
                shell.settings.replace(None);
                if let (Some(ui), true) = (shell.ui.borrow().as_ref(), shell.layered.get()) {
                    layer::set_keyboard(&ui.window, true);
                }
            }
            glib::Propagation::Proceed
        });
        window.present();
        self.settings.replace(Some(window));
    }

    /// Applies an edit from the settings window right away.
    fn apply_settings(self: &Rc<Self>, edit: &dyn Fn(&mut Config)) {
        let before = self.update_config(edit);
        let config = self.config.borrow().clone();
        if config.tools != before.tools {
            self.send(Msg::Tools(config.tools.clone()));
        }
        if config.widget == before.widget {
            return;
        }
        let layered = config.widget.layer != Layer::Window && layer::is_supported();
        self.layered.set(layered);
        self.update_hold();
        // Rebuild an open widget with the new settings. A layer widget does
        // not take the focus while settings are open (see `build_window`);
        // an ordinary window is presented first, then settings over it.
        let open = self.ui.borrow().as_ref().map(|ui| ui.window.clone());
        if let Some(window) = open {
            window.close();
            self.show_window();
            if !layered && let Some(settings) = self.settings.borrow().as_ref() {
                settings.present();
            }
        }
    }
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
        title.push_str(&format!(" · {}", format::cost(total.cost.total())));
    }
    let mut lines = Vec::new();
    if summary.rate_per_min > 0.0 {
        lines.push(format!(
            "Live: {} tokens/min",
            format::tokens(summary.rate_per_min as u64)
        ));
    }
    if let Some(speed) = total.speed() {
        lines.push(format!("Average speed: {}", format::speed(speed)));
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
                    widget::window_name(window.window_minutes).to_lowercase()
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
