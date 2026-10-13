# Configuration

Settings live in `$XDG_CONFIG_HOME/toktop/config.toml` (default `~/.config/toktop/config.toml`). The file is optional: missing keys use their defaults, and toktop creates the file the first time a setting changes in the app. A file that cannot be parsed is reported on stderr and ignored for the session; toktop never overwrites it.

```toml
period = "today"                               # today, week, 7d, month, 30d or all
tray = true                                    # show a tray icon when a panel supports it
tools = ["claude-code", "codex", "antigravity"]

[window]
width = 420
height = 680
maximized = false
```

| Key | Default | Meaning |
|---|---|---|
| `period` | `"today"` | Period shown at startup. Changing the period in the window or the tray menu saves it. |
| `tray` | `true` | Register a StatusNotifierItem tray icon. Needs a panel that hosts one, such as waybar, KDE Plasma, or GNOME with the AppIndicator extension. |
| `tools` | all three | Tools whose logs are read and watched. Disabled tools are not scanned. |
| `window.width`, `window.height` | `420`, `680` | Window size, saved when the window closes. |
| `window.maximized` | `false` | Whether the window opens maximized. |

Changes to `tools` and `tray` take effect at the next start.

## Environment variables

| Variable | Effect |
|---|---|
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | Tool data locations; see [sources.md](sources.md). |
| `TOKTOP_DEBUG` | Log window and application lifecycle events to stderr; see [architecture.md](architecture.md#debugging). |
