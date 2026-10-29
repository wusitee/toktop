# Configuration

Settings live in `$XDG_CONFIG_HOME/toktop/config.toml` (default `~/.config/toktop/config.toml`). The file is optional: missing keys use their defaults, and toktop creates the file the first time a setting changes in the app.

Most settings can also be changed in the settings window: right-click the widget and choose Settings, use the tray menu, or run `toktop --settings`. Changes there apply immediately, except `tray`. A file that cannot be parsed is reported on stderr and ignored for the session; toktop never overwrites it.

```toml
period = "24h"                                 # 24h, 7d, 30d or all
tray = true                                    # show a tray icon when a panel supports it
tools = ["claude-code", "codex", "antigravity", "pi", "kimi-code", "dsh"]

[widget]
layer = "bottom"      # background, bottom, top, overlay or window
anchor = "top-right"  # top-left, top, top-right, left, center, right, bottom-left, bottom, bottom-right
margin = 16
output = ""           # monitor connector such as "DP-1"; empty lets the compositor choose
width = 380
rows = 5
sessions = 5

[widget.sections]     # parts of the card that are shown
cost = true           # cost by input, cache read, cache write and output
stats = true          # recent speed, live rate, cache and streak tiles
limits = true         # Codex rate limits
breakdown = true      # models, projects and tools
sessions = true
activity = true       # heatmap
trend = true
```

| Key | Default | Meaning |
|---|---|---|
| `period` | `"24h"` | Period shown at startup. Changing the period in the widget or the tray menu saves it. |
| `tray` | `true` | Register a StatusNotifierItem tray icon. Needs a panel that hosts one, such as waybar, KDE Plasma, or GNOME with the AppIndicator extension. |
| `tools` | all six | Tools whose logs are read and watched. A disabled tool's directories are not scanned or watched, and its data is dropped. |
| `widget.layer` | `"bottom"` | Where the widget lives; see [Layers](#layers). |
| `widget.anchor` | `"top-right"` | Screen position of a layer widget. |
| `widget.margin` | `16` | Distance in pixels from the anchored edges. |
| `widget.output` | `""` | Monitor to show a layer widget on, by connector name (`hyprctl monitors`, `swaymsg -t get_outputs`). |
| `widget.width` | `380` | Card width in pixels. |
| `widget.rows` | `5` | Visible rows in the models, projects and tools list; scroll to reach up to 100 entries in the selected breakdown. |
| `widget.sessions` | `5` | Recent sessions shown when collapsed; `0` hides the section. The list starts expanded and scrolls through up to 100 sessions of the period in one more row's height. Use the chevron to collapse or expand it. |
| `widget.sections.*` | `true` | Whether each part of the card is shown. |

The legacy period keys `today`, `week` and `month` are accepted as aliases for `24h`, `7d` and `30d`. Saves use the new keys.

Edits to the file take effect at the next start; the settings window applies them immediately. The `[window]` table of earlier versions is accepted and ignored.

## Extra Codex homes and project groups

Both executables read these settings from their own host’s config:

```toml
codex_homes = ["~/code/nai-wai-bench/.scratch/*/codex-home"]

[[project_group]]
name = "nai-wai-bench"
match = ["/tmp/nwb-*"]
```

The default Codex home (`CODEX_HOME`, or `~/.codex`) is always included. `codex_homes` and colon-separated `TOKTOP_CODEX_HOMES` add homes; `~/` expands to the current user’s home. Glob patterns expand on each scan. Duplicate homes resolve to one source, including symlink aliases. The widget checks for newly matched homes every 30 seconds and watches their session directories and title indexes.

Project groups match full project paths using glob patterns (`*`, `?`, character classes and `**`). The first matching group wins. Widget breakdowns, session rows, `--print` and reports use the group name. Remote paths match without the host prefix. Original transcript paths remain available to report filters and in snapshots.

Codex quota snapshots stay separate by home. The widget’s bars show the most recently observed home; its tooltip identifies that home and lists the others. `--print` lists each home separately. Homes can use different accounts, and homes sharing an account can show the same account-wide quota.

## Remote usage over SSH

Install `toktop-collector` on each Linux host using the same release as the desktop. It can be built without GTK or libadwaita:

```sh
cargo build --release --locked --no-default-features --bin toktop-collector
install -Dm755 target/release/toktop-collector ~/.local/bin/toktop-collector
```

The collector still needs SQLite and zstd runtime libraries. Its executable must be on the remote non-interactive SSH `PATH`; check with `ssh dev-server 'toktop-collector --snapshot >/dev/null'`. The collector also reads its host’s `toktop/config.toml`, including extra Codex homes. Configure tool-specific environment variables for non-interactive SSH when needed. When invoked by the desktop, the collector reads only the tools enabled in the desktop settings.

Add SSH host aliases to the local config and restart toktop:

```toml
[[remote]]
host = "dev-server"

[[remote]]
host = "another-server"
```

Aliases may contain ASCII letters, digits, `.`, `_`, `-` and `@`, and must not begin with `-`. Existing SSH configuration, including jump hosts and identity settings, applies. First connect interactively to establish host trust and set up non-interactive authentication (for example an SSH agent). toktop uses SSH batch mode and cannot prompt for passwords or host-key confirmation.

toktop refreshes each host every 60 seconds, with a 60-second timeout and a 64 MiB response limit. A failed refresh retains that host's last successful snapshot for the lifetime of the app and displays an error with its age; it is not persisted across restarts. `--print` fetches hosts once and reports failures on stderr. Events from different hosts are counted independently, so copied session histories can be counted more than once. Remote Codex limit snapshots are not included in the local limits display.

The collector sends parsed token events, project paths, session titles and prompt excerpts over SSH; it does not send full transcripts. These metadata can contain sensitive information. Snapshots cover all available history, and scans and transfers may take longer on hosts with large logs.

## Layers

On compositors with wlr-layer-shell (Hyprland, Sway, KDE Plasma, niri and other wlroots-based compositors), the widget is a layer surface. It has no window decorations, does not tile and does not appear in window lists.

| Layer | Behavior |
|---|---|
| `background`, `bottom` | A desktop widget below all windows, visible on empty workspaces. Shown at startup, even with `--background`. |
| `top`, `overlay` | Above windows, like a panel popup. Hidden at startup with `--background`; summoned with `toktop --toggle` or the tray icon, dismissed with Escape. |
| `window` | An ordinary window. Also used when the compositor has no layer-shell support (GNOME, X11) or toktop is built without the `layer-shell` feature. |

A layer widget keeps toktop running when hidden, so `toktop --toggle` can bring it back. Right-click the widget for Settings, Refresh prices, Hide and Quit.

The card never grows taller than its monitor; longer content scrolls. On Hyprland, space reserved by panels such as waybar is subtracted too, read from Hyprland's IPC socket. Other compositors do not report it, so with a panel there, hide sections or lower `rows` and `sessions` until the card fits.

### Hyprland

```lua
-- ~/.config/hypr/hyprland.lua
hl.on("hyprland.start", function()
    hl.exec_cmd("toktop --background")
end)
hl.bind("SUPER + T", hl.dsp.exec_cmd("toktop --toggle"))
hl.layer_rule({ match = { namespace = "^toktop$" }, blur = true, ignore_alpha = 0.5 })
```

With the older hyprlang configuration (`hyprland.conf`):

```ini
exec-once = toktop --background
bind = SUPER, T, exec, toktop --toggle
layerrule = blur on, ignore_alpha 0.5, match:namespace ^toktop$
```

The layer namespace is `toktop` on every compositor.

## Style

The widget is styled with GTK CSS. `$XDG_CONFIG_HOME/toktop/style.css`, if present, is loaded over the [built-in stylesheet](https://github.com/wusitee/toktop/blob/main/src/ui/style.css) at user priority and read at startup; parse errors are reported on stderr. Colors are named, so most themes only redefine them:

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
| `toktop_claude`, `toktop_openai`, `toktop_gemini`, `toktop_deepseek`, `toktop_pi`, `toktop_kimi`, `toktop_other` | Model and tool dots (`.dot.claude`, …) |
| `toktop_input`, `toktop_cache_read`, `toktop_cache_write`, `toktop_output` | Token kinds in the cost-by-type bar and the detail cards' token and cost bars (each bar draws in the CSS `color` of its `.kind-swatch` legend marks) |

## Command line

| Command | Effect |
|---|---|
| `toktop` | Start, or show the running instance's widget. |
| `toktop --background` | Start without showing a `top`, `overlay` or `window` widget; a `window` still opens when there is no tray icon to reopen it from. Does nothing if toktop is running. |
| `toktop --toggle` | Show or hide the running instance's widget; starts toktop if it is not running. |
| `toktop --settings` | Open the settings window; starts toktop if it is not running. |
| `toktop --print [period]` | Print a summary without the GUI. |
| `toktop report …`, `toktop-collector report …` | Print a filtered, priced accounting report; see [Reports](reports.md). |

## Environment variables

| Variable | Effect |
|---|---|
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, `KIMI_CODE_HOME`, `DSH_HOME` | Tool data locations; see [sources.md](sources.md). |
| `TOKTOP_CODEX_HOMES` | Colon-separated extra Codex home paths or globs, in addition to config entries and the default home. |
| `TOKTOP_DEBUG` | Log window and application lifecycle events to stderr; see [architecture.md](architecture.md#debugging). |
