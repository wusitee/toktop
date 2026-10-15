//! StatusNotifierItem tray icon.
//!
//! ksni runs the D-Bus service on its own thread and calls the [`ksni::Tray`]
//! methods there, so the tray only forwards [`Command`]s to the GTK main loop.
//! Tooltip updates are pushed from a small updater thread, because
//! `Handle::update` blocks until the service has applied them.

use ksni::blocking::TrayMethods;
use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Category, MenuItem, ToolTip};
use std::sync::mpsc;
use toktop::aggregate::Period;

pub enum Command {
    /// Show or hide the widget.
    Toggle,
    Show,
    Period(Period),
    RefreshPrices,
    Settings,
    Quit,
}

/// What the tray displays.
#[derive(Clone, Default, PartialEq)]
pub struct State {
    pub title: String,
    pub description: String,
    pub period: Period,
}

struct Item {
    icon: &'static str,
    commands: async_channel::Sender<Command>,
    state: State,
}

impl Item {
    fn send(&self, command: Command) {
        let _ = self.commands.send_blocking(command);
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "toktop".into()
    }

    fn title(&self) -> String {
        "toktop".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        self.icon.into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.state.title.clone(),
            description: self.state.description.clone(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let selected = Period::ALL
            .iter()
            .position(|period| *period == self.state.period)
            .unwrap_or(0);
        vec![
            StandardItem {
                label: "Show widget".into(),
                activate: Box::new(|item: &mut Self| item.send(Command::Show)),
                ..StandardItem::default()
            }
            .into(),
            SubMenu {
                label: "Period".into(),
                submenu: vec![
                    RadioGroup {
                        selected,
                        select: Box::new(|item: &mut Self, index| {
                            if let Some(&period) = Period::ALL.get(index) {
                                item.send(Command::Period(period));
                            }
                        }),
                        options: Period::ALL
                            .iter()
                            .map(|period| RadioItem {
                                label: period.label().into(),
                                ..RadioItem::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..SubMenu::default()
            }
            .into(),
            StandardItem {
                label: "Refresh prices".into(),
                activate: Box::new(|item: &mut Self| item.send(Command::RefreshPrices)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Settings".into(),
                icon_name: "preferences-system".into(),
                activate: Box::new(|item: &mut Self| item.send(Command::Settings)),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|item: &mut Self| item.send(Command::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // Keep the service so the icon returns when the panel restarts.
        true
    }
}

/// A registered tray icon.
pub struct Tray {
    updates: mpsc::Sender<State>,
    shown: State,
}

impl Tray {
    /// Registers the icon. Fails when no StatusNotifierItem host is running.
    pub fn spawn(
        commands: async_channel::Sender<Command>,
        icon: &'static str,
        state: State,
    ) -> Result<Tray, ksni::Error> {
        let handle = Item {
            icon,
            commands,
            state: state.clone(),
        }
        .spawn()?;
        let (updates, rx) = mpsc::channel::<State>();
        std::thread::Builder::new()
            .name("toktop-tray".into())
            .spawn(move || {
                while let Ok(mut state) = rx.recv() {
                    // Only the newest state matters.
                    while let Ok(newer) = rx.try_recv() {
                        state = newer;
                    }
                    if handle.update(|item| item.state = state).is_none() {
                        return;
                    }
                }
            })
            .map_err(|_| ksni::Error::WontShow)?;
        Ok(Tray {
            updates,
            shown: state,
        })
    }

    pub fn set(&mut self, state: State) {
        if state != self.shown {
            self.shown = state.clone();
            let _ = self.updates.send(state);
        }
    }
}
