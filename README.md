# toktop

A native Linux desktop widget for AI coding tool token usage, written in Rust with GTK 4 and libadwaita. It is a lightweight reimplementation of [Token Monitor](https://github.com/Javis603/token-monitor) for Linux, running as a single process that updates within about a second of each turn.

Supported tools: Claude Code, Codex, Antigravity, [Pi](https://github.com/earendil-works/pi) and Kimi Code.

## Build

Requires Rust 1.92+, a C toolchain, `pkg-config`, and GTK 4.12+, libadwaita 1.6+, gtk4-layer-shell, SQLite and OpenSSL development files. Build with `--no-default-features` to drop the gtk4-layer-shell dependency; the widget is then always a window.

```sh
# Arch Linux
sudo pacman -S --needed rust base-devel pkgconf gtk4 libadwaita gtk4-layer-shell sqlite openssl
# Debian / Ubuntu
sudo apt install cargo build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev libsqlite3-dev libssl-dev

cargo build --release
```

Install the binary and desktop entry for your user:

```sh
install -Dm755 target/release/toktop ~/.local/bin/toktop
install -Dm644 data/io.github.wusitee.toktop.desktop ~/.local/share/applications/io.github.wusitee.toktop.desktop
```

## Usage

Run `toktop` to show the widget. On Hyprland, Sway, KDE Plasma and other compositors with wlr-layer-shell, it is a desktop widget pinned to the top-right corner below your windows; elsewhere it opens as a small window. It shows:

- total tokens, cost and requests for the selected period (DAY, WEEK, MONTH, ALL)
- average output speed (tok/s), live tokens per minute, cache hit rate and daily streak
- Codex rate limits left and their reset times
- top models, projects or tools with their speed, and recent sessions with their models and subagents
- an activity heatmap and a 30-day trend

Click a model, project, tool, session or heatmap day to open a detail card beside the widget: token composition, speed, cache rate and how the usage divides among models, tools and projects. Clicking another item switches the card; the same item or its close button closes it. The chevron next to the session count expands the list into a scrollable one, one row taller. Right-click for Settings, Refresh prices, Hide and Quit. The settings window (also `toktop --settings`) turns tools on or off, chooses which parts of the card are shown, and sets layer, position, monitor and size. The look can be changed with a `style.css`; see [docs/configuration.md](docs/configuration.md).

On Hyprland, in `~/.config/hypr/hyprland.lua`:

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("toktop --background")
end)
hl.bind("SUPER + T", hl.dsp.exec_cmd("toktop --toggle"))
hl.layer_rule({ match = { namespace = "^toktop$" }, blur = true, ignore_alpha = 0.5 })
```

For the older `hyprland.conf` syntax, see [docs/configuration.md](docs/configuration.md#hyprland).

With a panel that hosts tray icons (waybar, KDE Plasma, GNOME with the AppIndicator extension), toktop also shows a tray icon: click it to show or hide the widget, or use its menu to change the period, open settings or quit. To start at login on other desktops:

```sh
install -Dm644 data/io.github.wusitee.toktop-autostart.desktop ~/.config/autostart/io.github.wusitee.toktop.desktop
```

Print a summary without the GUI:

```sh
toktop --print [today|week|7d|month|30d|all]
```

## Documentation

- [Architecture](docs/architecture.md): threads, application lifecycle, modules, watching, memory
- [Sources](docs/sources.md): where each tool's logs are read from
- [Token accounting](docs/accounting.md): per-tool parsing and deduplication rules, response times, session titles
- [Pricing](docs/pricing.md): price source, model lookup and cost rules
- [Configuration](docs/configuration.md): settings file, widget layers, Hyprland setup, style and command line
