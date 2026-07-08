# Rusty Quartermaster

[![CI](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml/badge.svg)](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml)

A terminal application for [Yohoho! Puzzle Pirates](https://www.puzzlepirates.com/) players. It runs entirely in your terminal and you navigate with the keyboard and/or mouse.

## Prerequisites

You need the Rust toolchain installed. If you don't have it yet:

### Linux / macOS

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the on-screen instructions, then restart your terminal (or run `source ~/.cargo/env`).

### Windows

Download and run the installer from <https://rustup.rs>. You will also need the Visual Studio C++ Build Tools -- the installer will tell you if they're missing.

After installation, open a new Command Prompt or PowerShell window so `cargo` is on your PATH.

### Verify

```sh
cargo --version
```

If this prints a version number, you're ready.

## Building

Clone or download this repository, then from the project directory:

```sh
cargo build --release
```

The compiled binary will be at:

- Linux / macOS: `target/release/ypp_quartermaster`
- Windows: `target\release\ypp_quartermaster.exe`

You can also run it directly without a separate build step:

```sh
cargo run --release
```

## Running

```sh
ypp_quartermaster [OPTIONS]
```

### Options

| Flag | Description |
|---|---|
| `--inventory <PATH>` | Path to a JSON file for saving/loading your commodity inventory. Loaded on startup, saved on exit. |
| `--market-cache <PATH>` | Path to a JSON file for caching market price data. Avoids re-fetching prices every run. |

Both flags are optional. Without `--market-cache`, the app fetches commodity data on every startup, which requires an internet connection.

### Example

```sh
# First run -- fetches prices and saves them for later
ypp_quartermaster --inventory my_inventory.json --market-cache market.json

# Subsequent runs -- loads cached prices, much faster startup
ypp_quartermaster --inventory my_inventory.json --market-cache market.json
```

On Windows, use backslashes or quote the paths if they contain spaces:

```powershell
.\ypp_quartermaster.exe --inventory my_inventory.json --market-cache market.json
```

## How to use

When you start the app, you'll see a sidebar on the left listing the available tools and the currently selected tool on the right.

### Navigation

- **Arrow keys** move focus between elements (sidebar, fields, table cells, buttons).
- **Enter** activates buttons, confirms selections, or submits input.
- **Esc** goes back (content to sidebar, or closes popups). Pressing Esc in the sidebar exits the app.
- **Mouse** is fully supported -- click on anything interactive. Scroll wheel works in lists and tables.

## Troubleshooting

- **Display looks garbled** -- make sure your terminal supports ANSI escape codes and is at least 80 columns wide. On Windows, use Windows Terminal or PowerShell (not the legacy `cmd.exe` in older Windows versions).
- **Ship images don't open** -- the `v` key in the ship popup writes a temporary PNG and opens it with `xdg-open` (Linux). On Windows or macOS, this won't work automatically. The damage calculator itself functions fine without it.
