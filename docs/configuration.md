# Configuration

Settings live in `$XDG_CONFIG_HOME/toktop/config.toml` (default `~/.config/toktop/config.toml`). The file is optional: missing keys use their defaults, and toktop creates the file the first time a setting changes in the app. A file that cannot be parsed is reported on stderr and ignored for the session; toktop never overwrites it.

```toml
period = "today"                               # today, week, 7d, month, 30d or all
tray = true                                    # show a tray icon when a panel supports it
tools = ["claude-code", "codex", "antigravity", "pi"]

[widget]
layer = "bottom"      # background, bottom, top, overlay or window
anchor = "top-right"  # top-left, top, top-right, left, center, right, bottom-left, bottom, bottom-right
margin = 16
output = ""           # monitor connector such as "DP-1"; empty lets the compositor choose
width = 380
rows = 5
sessions = 5
```

| Key | Default | Meaning |
|---|---|---|
| `period` | `"today"` | Period shown at startup. Changing the period in the widget or the tray menu saves it. |
| `tray` | `true` | Register a StatusNotifierItem tray icon. Needs a panel that hosts one, such as waybar, KDE Plasma, or GNOME with the AppIndicator extension. |
| `tools` | all four | Tools whose logs are read and watched. Disabled tools are not scanned. |
| `widget.layer` | `"bottom"` | Where the widget lives; see [Layers](#layers). |
| `widget.anchor` | `"top-right"` | Screen position of a layer widget. |
| `widget.margin` | `16` | Distance in pixels from the anchored edges. |
| `widget.output` | `""` | Monitor to show a layer widget on, by connector name (`hyprctl monitors`, `swaymsg -t get_outputs`). |
| `widget.width` | `380` | Card width in pixels. |
| `widget.rows` | `5` | Rows in the models, projects and tools list. |
| `widget.sessions` | `5` | Recent sessions shown; `0` hides the section. |

Changes to `tools`, `tray` and `widget` take effect at the next start. The `[window]` table of earlier versions is accepted and ignored.

## Layers

On compositors with wlr-layer-shell (Hyprland, Sway, KDE Plasma, niri and other wlroots-based compositors), the widget is a layer surface. It has no window decorations, does not tile and does not appear in window lists.

| Layer | Behavior |
|---|---|
| `background`, `bottom` | A desktop widget below all windows, visible on empty workspaces. Shown at startup, even with `--background`. |
| `top`, `overlay` | Above windows, like a panel popup. Hidden at startup with `--background`; summoned with `toktop --toggle` or the tray icon, dismissed with Escape. |
| `window` | An ordinary window. Also used when the compositor has no layer-shell support (GNOME, X11) or toktop is built without the `layer-shell` feature. |

A layer widget keeps toktop running when hidden, so `toktop --toggle` can bring it back. Right-click the widget for Refresh prices, Hide and Quit.

### Hyprland

```ini
# ~/.config/hypr/hyprland.conf
exec-once = toktop --background
bind = SUPER, T, exec, toktop --toggle
layerrule = blur on, ignore_alpha 0.5, match:namespace toktop
```

With the Lua configuration of newer Hyprland versions, the layer rule is `hl.layer_rule({ match = { namespace = "toktop" }, blur = true, ignore_alpha = 0.5 })`. The layer namespace is `toktop` on every compositor.

## Style

The widget is styled with GTK CSS. `$XDG_CONFIG_HOME/toktop/style.css`, if present, is loaded over the [built-in stylesheet](../src/ui/style.css) at user priority and read at startup; parse errors are reported on stderr. Colors are named, so most themes only redefine them:

```css
@define-color toktop_bg rgba(20, 20, 28, 0.75);
@define-color toktop_accent #ff7b72;
.card { border-radius: 8px; }
```

| Color | Used for |
|---|---|
| `toktop_bg`, `toktop_fg`, `toktop_dim`, `toktop_line` | Card background, text, secondary text, separators and borders |
| `toktop_accent` | Heatmap, trend line and limit bars (the charts draw in the CSS `color` of `.heatmap` and `.trend`) |
| `toktop_live`, `toktop_warn` | Live indicator; price notices and nearly exhausted limits |
| `toktop_claude`, `toktop_openai`, `toktop_gemini`, `toktop_deepseek`, `toktop_pi`, `toktop_other` | Model and tool dots (`.dot.claude`, …) |

## Command line

| Command | Effect |
|---|---|
| `toktop` | Start, or show the running instance's widget. |
| `toktop --background` | Start without showing a `top`, `overlay` or `window` widget; a `window` still opens when there is no tray icon to reopen it from. Does nothing if toktop is running. |
| `toktop --toggle` | Show or hide the running instance's widget; starts toktop if it is not running. |
| `toktop --print [period]` | Print a summary without the GUI. |

## Environment variables

| Variable | Effect |
|---|---|
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR` | Tool data locations; see [sources.md](sources.md). |
| `TOKTOP_DEBUG` | Log window and application lifecycle events to stderr; see [architecture.md](architecture.md#debugging). |
