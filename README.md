# toktop

A native Linux desktop monitor for AI coding tool token usage, written in Rust with GTK 4 and libadwaita. It is a lightweight reimplementation of [Token Monitor](https://github.com/Javis603/token-monitor) for Linux, running as a single process that updates within about a second of each turn.

Supported tools: Claude Code, Codex and Antigravity.

## Build

Requires Rust 1.92+, a C toolchain, `pkg-config`, and GTK 4.12+, libadwaita 1.6+, SQLite and OpenSSL development files.

```sh
# Arch Linux
sudo pacman -S --needed rust base-devel pkgconf gtk4 libadwaita sqlite openssl
# Debian / Ubuntu
sudo apt install cargo build-essential pkg-config libgtk-4-dev libadwaita-1-dev libsqlite3-dev libssl-dev

cargo build --release
```

Install the binary and desktop entry for your user:

```sh
install -Dm755 target/release/toktop ~/.local/bin/toktop
install -Dm644 data/io.github.wusitee.toktop.desktop ~/.local/share/applications/io.github.wusitee.toktop.desktop
```

## Usage

Run `toktop` to open the window. Pick the period from the header menu. The Overview page shows totals, per-tool usage, Codex rate limits and a 30-day chart; Models, Projects and Sessions break usage down further. Expand a row for its input, output, reasoning and cache split.

Print a summary without the GUI:

```sh
toktop --print [today|week|7d|month|30d|all]
```

## Documentation

- [Architecture](docs/architecture.md): threads, modules, data flow, watching, memory
- [Sources](docs/sources.md): where each tool's logs are read from
- [Token accounting](docs/accounting.md): per-tool parsing and deduplication rules
- [Pricing](docs/pricing.md): price source, model lookup and cost rules
