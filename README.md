# Rusty Quartermaster

[![CI](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml/badge.svg)](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml)

A terminal toolkit for [Yohoho! Puzzle Pirates](https://www.puzzlepirates.com/) players. It runs entirely in your terminal — navigate with the keyboard and/or the mouse — and bundles several tools behind one top bar. Optionally point it at your in-game chat log and it tracks your voyages live.

## Tools

Switch between these from the top bar (plus **Exit**):

- **Profits** — Work out whether a pillage paid off. List the commodities you carried, fill in how many units you restocked, held, and plundered, and enter their buy/sell prices; the tool returns a full profit breakdown — gross plunder, jobber cuts, the booty chest (gross and net), goods value, restock cost, pre-voyage stocking, and the bottom-line split between the hold and the divvy. Your inventory and inputs persist between runs. With `--clipboard`, copy your vessel's hold to the clipboard (the game's hold JSON) and the tool offers to fill the **Stock** column from it — you confirm before anything changes, and the Booty column is never touched.

- **Damage** — A sea-battle damage calculator. Pick both hull types (yours and the foe's) and enter the shots, rocks, and rams each ship took; it shows the morale and hull damage for each side, how many more shots each can absorb before losing morale or sinking, and a crew-strength advantage inferred from the two hulls.

- **Jobbers** — Scouts the crew jobbing on your vessel by looking up their public [Puzzle Pirates](https://www.puzzlepirates.com/) web profiles (needs your pirate name and an attached chat log). For the chosen voyage type — Pillage, Atlantis, Cursed Isles, Vampirates, or Vikings — it ranks everyone aboard by the skills that matter for that run so you can see who's best suited to each station and tracks who's aboard, how many a pirate has bashed greedy brigands, and who's been planked. Pirate cards, a trophy list, and a skill-distribution scatterplot are available too. Profiles are cached and refreshed on a schedule.

- **Voyage Statistics** — Reconstructs each sail-to-port run from the chat log and reports on it: a per-fight sea-battle log (each with an advantage-over-time graph and damage breakdown), timing, loot and PoE, the booty divvy, an enemy tally, crew and damage advantage, and consumption (cannonballs, rum, and rum spice). Finished runs can be saved to a history file and reloaded read-only, with win-rate and PoE charts drawn across your saved voyages.

- **Map** — The selected ocean's map, drawn in the terminal: diamonds are islands, circles are open-sea league points, solid lines are leagues on routes whose chart can be bought, dotted lines are leagues that have to be sailed from memory. A cursor sails league by league with the keys laid out like a compass (`q`/`e`/`z`/`c` for the diagonals, `a`/`d` west and east, `w`/`x` north and south), and the view scrolls to follow it. A key whose exact heading has no league takes the one diagonal on that side instead, and does nothing when both diagonals exist, so a press never guesses; **Space** marks the league point under the cursor as memorized (the glyph fills in), and once both ends of a league are memorized that league is highlighted as sailable. `/` searches for an island by name. Memorized points are saved per ocean in the cache. Maps are compiled into the binary from `src/data/maps/`; Emerald ships today, and `scripts/extract_map.py` turns any other ocean's YPPedia map page into a map file.

## Prerequisites

You need the Rust toolchain installed. If you don't have it yet:

### Linux / macOS

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the on-screen instructions, then restart your terminal (or run `source ~/.cargo/env`).

### Windows

Download and run the installer from <https://rustup.rs>. You will also need the Visual Studio C++ Build Tools — the installer will tell you if they're missing. After installation, open a new Command Prompt or PowerShell window so `cargo` is on your PATH.

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

- Linux / macOS: `target/release/rusty_quartermaster`
- Windows: `target\release\rusty_quartermaster.exe`

You can also run it directly:

```sh
cargo run --release -- [OPTIONS] [CHAT_LOG]
```

Prebuilt binaries for each tagged release are attached to the [GitHub Releases](https://github.com/f5xs-0000a/rusty_quartermaster/releases) page.

## Running

```sh
rusty_quartermaster [OPTIONS] [CHAT_LOG]
```

`CHAT_LOG` is an optional path to your Puzzle Pirates client chat log. When given, the log is read in full and then tailed live, which powers Voyage Statistics and the live crew tracking in Jobbers. Without it, those features stay idle. (Enable chat logging in the game client's options first; the client writes the log into its own `logs` folder.)

### Options

All options are optional.

| Flag | Description |
|---|---|
| `--cache <PATH>` | Save/load the unified cache JSON: your inventory, commodity list, and fetched pirate stats. Loaded on startup, saved on exit. Defaults to `ypp_cache.json` beside the executable. |
| `--voyages <PATH>` | Save/load the voyage-history JSON (completed voyages across all your pirates). Loaded on startup, appended on save. Defaults to `ypp_voyages.json` beside the executable. |
| `--user <NAME>` | Your pirate name. Attributes planks to you in the chat log and enables Jobbers pirate-stat lookups. |
| `--ocean <OCEAN>` | Ocean (server) to use, case-insensitive. One of the seven live oceans: Emerald, Meridian, Cerulean, Obsidian, Opal, Jade, Ice. |
| `--pirate-ttl-days <DAYS>` | Days before a cached pirate's basic profile is treated as stale and re-fetched in the background. Default: 3. |
| `--trophy-ttl-days <DAYS>` | Days before a cached pirate's trophies are treated as stale and re-fetched in the background. Default: 7. |
| `--donate-to-crew` | Reveal the "Crew Donation Share Rate" row in Profits and deduct that donation in the breakdown. Off by default. |
| `--ypp-query-rate <SECONDS>` | Minimum seconds between pirate-stat/trophy lookups. Higher is gentler on the server. Default: 60. |
| `--clipboard` | Watch the clipboard for a copied hold and offer to fill the Profits **Stock** column from it (you confirm first). Off by default: without it the clipboard is never read. |

### Example

```sh
# Track a live session on Emerald, caching data between runs
rusty_quartermaster \
  --ocean Emerald \
  --user YourPirate \
  --cache cache.json \
  --voyages voyages.json \
  "/path/to/puzzle_pirates/chat.log"
```

On Windows, quote any path that contains spaces:

```powershell
.\rusty_quartermaster.exe --ocean Emerald --user YourPirate --cache cache.json "C:\path\to\chat.log"
```

## Navigation

When you start the app, the top bar lists the available tools; the selected tool fills the rest of the screen.

- **Arrow keys** move focus between elements (top bar, fields, table cells, buttons).
- **Enter** activates buttons, confirms selections, or submits input.
- **Esc** goes back (content to the top bar, or closes a popup). Pressing Esc on the top bar selects **Exit**.
- **Mouse** is fully supported — click anything interactive, and the scroll wheel works in lists and tables.

## Troubleshooting

- **Display looks garbled** — make sure your terminal supports ANSI escape codes and is at least 80 columns wide. On Windows, use Windows Terminal or PowerShell rather than the legacy `cmd.exe`.

- **The hold prompt never appears** — the watcher is opt-in, so make sure you passed `--clipboard`. The clipboard is then checked about once a second, and only a *change* triggers the prompt: whatever was on the clipboard when the tool started is ignored, so copy the hold again. On Linux the clipboard needs an X11 session or a Wayland compositor that supports the data-control protocol (over SSH or in a bare TTY there is no clipboard at all).

## License

Released under the [MIT License](LICENSE).

Puzzle Pirates is a trademark of Grey Havens, LLC. Rusty Quartermaster is an unofficial fan tool and is not affiliated with, endorsed by, or sponsored by Grey Havens, Three Rings Design, or Sega. The license covers only this project's source code and grants no rights in any third-party trademark or game content.

The ocean maps in `src/data/maps/` are derived from the map templates on [YPPedia](https://yppedia.puzzlepirates.com/), whose content is available under the [Creative Commons Attribution 2.5](https://creativecommons.org/licenses/by/2.5/) license; each map file records the page it came from.
