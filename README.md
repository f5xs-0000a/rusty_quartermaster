# Rusty Quartermaster

[![CI](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml/badge.svg)](https://github.com/f5xs-0000a/rusty_quartermaster/actions/workflows/ci.yml)

A terminal toolkit for [Yohoho! Puzzle Pirates](https://www.puzzlepirates.com/) players. It runs entirely in your terminal — navigate with the keyboard and/or the mouse — and bundles several tools behind one top bar. Optionally point it at your in-game chat log and it tracks your voyages live.

## Tools

Switch between these from the top bar (plus **Exit**):

- **Profits** — Work out whether a pillage paid off. List the commodities you carried, fill in how many units you restocked, held, and plundered, and enter their buy/sell prices; the tool returns a full profit breakdown — gross plunder, jobber cuts, the booty chest (gross and net), goods value, restock cost, pre-voyage stocking, and the bottom-line split between the hold and the divvy. Your inventory and inputs persist between runs. With `--clipboard`, copy your vessel's hold to the clipboard (the game's hold JSON) and the tool offers to fill the **Stock** column from it — you confirm before anything changes, and the Booty column is never touched.

- **Damage** — A sea-battle damage calculator. Pick both hull types (yours and the foe's) and enter the shots, rocks, and rams each ship took; it shows the morale and hull damage for each side, how many more shots each can absorb before losing morale or sinking, and a crew-strength advantage inferred from the two hulls.

- **Jobbers** — Scouts the crew jobbing on your vessel by looking up their public [Puzzle Pirates](https://www.puzzlepirates.com/) web profiles (needs your pirate name and an attached chat log). For the chosen voyage type — Pillage, Vampirates, Vikings, a flotilla, a blockade, Atlantis, the Haunted Seas, or Cursed Isles (a flotilla, a blockade and the Haunted Seas are laid out as an Atlantis run is, nothing being tracked that is their own yet) — it ranks everyone aboard by the skills that matter for that run so you can see who's best suited to each station and tracks who's aboard, how many a pirate has bashed greedy brigands, and who's been planked. Pirate cards, a trophy list, and a skill-distribution scatterplot are available too. Profiles are cached and refreshed on a schedule.

- **Voyage Statistics** — Reconstructs each sail-to-port run from the chat log and reports on it: a per-fight sea-battle log (each with an advantage-over-time graph and damage breakdown), timing, loot and PoE, the booty divvy, an enemy tally, crew and damage advantage, and consumption (cannonballs, rum, and rum spice). Finished runs can be saved to your persistence file and reloaded read-only, with win-rate and PoE charts drawn across your saved voyages.

- **Map** — The selected ocean's map, drawn in the terminal: circles are islands, four-pointed stars are open-sea league points, and a line is a league between two of them. A line's colour says what its chart is worth: white for a chart sold in game, grey for one no shipyard sells (it has to be come by some other way), red for a league whose ends you have both memorized. The sea is drawn as YPPedia draws it, so a league that no chart covers at all is left out of the drawing to keep it readable - the keys still sail it, and it appears on the map once both its ends are memorized. A cursor sails league by league with the keys laid out like a compass (`q`/`e`/`z`/`c` for the diagonals, `a`/`d` west and east, `w`/`x` north and south), and the view scrolls to follow it. A key whose exact heading has no league takes the one diagonal on that side instead, and does nothing when both diagonals exist, so a press never guesses; **Space** marks the league point under the cursor as memorized (the glyph fills in), and once both ends of a league are memorized that league is highlighted as sailable. `/` searches for an island by name, and `?` opens a help popup with the keys and the glyph legend. In a terminal 100 columns or wider, a column on the right describes the island under the cursor (size and status, memorized state, its governor, ruling flag, property tax and exports, what its archipelago forages, and the gems its palace buys at full price when that is known), while the foot of the map's own frame tallies how many of the ocean's league points you have memorized. The governor, flag, tax and exports come from the ocean's island list on the Puzzle Pirates website, fetched the first time you open the Map page and again once the cached copy is a week old; the request shares the same throttle as the pirate lookups, so it may take up to a minute to arrive. Memorized points are one pirate's knowledge of one ocean, so they are saved in your persistence file under your pirate (`--user`) within the ocean; without a pirate name the Map page shows the ocean but cannot memorize. Where you left the cursor is remembered too, but in the cache and per ocean rather than per pirate, so the page reopens on the stretch of sea you were last looking at. All seven oceans' maps are compiled into the binary from `src/data/maps/`, each a transcript of that ocean's YPPedia map page (named in the file's `source`) kept by hand since. Each island and archipelago in a map file may carry the shorter name the chart draws it under - `Kent` for Isle of Kent - and a name too wide for one row wraps onto the next. A map file holds the leagues the wiki draws, which are the charted ones; the build adds every remaining pair of points a single league apart, since those can be sailed too, and the Map page sails them without drawing them.

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
| `--cache <PATH>` | Save/load the unified cache JSON: your inventory, commodity list, fetched pirate stats, and the Map page's last view per ocean. Loaded on startup, saved on exit. Defaults to `ypp_cache.json` beside the executable. |
| `--persistence <PATH>` | Save/load your own data as JSON: the completed-voyage history and what each of your pirates has memorized of each ocean, across all your pirates. Loaded on startup, written as you save a voyage and on exit. Defaults to `ypp_persistence.json` beside the executable, falling back to an `ypp_voyages.json` left there by an earlier version. Also accepted as `--voyages`. |
| `--user <NAME>` | Your pirate name. Attributes planks to you in the chat log, enables Jobbers pirate-stat lookups, and is who the Map page's memorized league points belong to. |
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
  --persistence persistence.json \
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

The ocean maps in `src/data/maps/` and the island geography and gem prices in `src/data/bare_cache.json` are derived from [YPPedia](https://yppedia.puzzlepirates.com/) (the map templates, island pages, and the [Gem](https://yppedia.puzzlepirates.com/Gem) price guide), whose content is available under the [Creative Commons Attribution 2.5](https://creativecommons.org/licenses/by/2.5/) license; each map file records the page it came from.
