# toktop

A native Linux desktop monitor for AI coding tool token usage, written in Rust. It is a lightweight reimplementation of [Token Monitor](https://github.com/Javis603/token-monitor) for Linux.

Supported tools: Claude Code, Codex and Antigravity.

## Build

Requires Rust 1.85+, a C toolchain, `pkg-config`, and SQLite and OpenSSL development files.

```sh
# Arch Linux
sudo pacman -S --needed rust base-devel pkgconf sqlite openssl
# Debian / Ubuntu
sudo apt install cargo build-essential pkg-config libsqlite3-dev libssl-dev

cargo build --release
```

## Usage

Print a usage summary:

```sh
toktop --print [today|week|7d|month|30d|all]
```

## Documentation

- [Architecture](docs/architecture.md): modules, data flow, incremental reads
- [Sources](docs/sources.md): where each tool's logs are read from
- [Token accounting](docs/accounting.md): per-tool parsing and deduplication rules
- [Pricing](docs/pricing.md): price source, model lookup and cost rules
