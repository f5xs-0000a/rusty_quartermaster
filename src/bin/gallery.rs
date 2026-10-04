//! Offline UI gallery: renders interface states to text and SVG.
//!
//! Every state is drawn through the same [`AppShell::render`] the real app
//! uses, against ratatui's `TestBackend`, and written out twice: as the plain
//! character grid the terminal would show, and as an SVG. Reading the text is
//! how layout is audited without a terminal — labels drawn whole, columns
//! aligned, nothing clipped at the right margin — but it keeps only the
//! characters, so focus, selection and every other colour-borne cue vanish
//! from it. The SVG keeps them, and `STYLES-<size>.txt` lists them in prose.
//!
//! ```sh
//! cargo run --bin gallery -- --out tmp/ui-gallery --prune
//! ```
//!
//! `index.html` beside the dumps shows every SVG on one page.
//!
//! State is reached the way the app reaches it: pages via the top bar's
//! arrow keys, page content by feeding chat-log lines through the real
//! parser. The log lines below are synthetic but follow the formats the
//! parser's own tests use, so a page populated here is populated the same way
//! a game session populates it.
//!
//! Deleting is opt-in: without `--prune` a re-run overwrites the states it
//! produces and leaves everything else in the directory alone.

use std::{fs, path::PathBuf, sync::Arc};

use clap::Parser;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use rusty_quartermaster::{
    api::Commodity,
    app::{APP_LIST, AppId, AppShell},
    damage::{BattlePrompt, ShipSelectPopup, Side},
    jobbers::{
        JobberFocus,
        PerFightPopup,
        PiratePopup,
        SkillDistPopup,
        TrophyPopup,
        VoyageType,
    },
    map::data::Map,
    ocean::Ocean,
    profits::{Focus, HoldImport, InventoryRow, PopupKind, ProfitResult},
    utils::{FieldKind, PromptField},
    voyage::{AxisMode, ui::SaveChoice},
};

#[derive(Parser)]
#[command(about = "Render the TUI's interface states to text and SVG")]
struct Args {
    /// Directory to write the dumps into (created if missing).
    #[arg(long, default_value = "tmp/ui-gallery")]
    out: PathBuf,

    /// Terminal size to render at, as `COLSxROWS`. Repeat for several sizes;
    /// defaults to the 80x24 floor and the roomier 120x40.
    #[arg(long = "size", value_parser = parse_size)]
    sizes: Vec<(u16, u16)>,

    /// Delete the dumps a previous run left behind before writing. Only files
    /// named like this tool's own output are touched; without it, dumps for
    /// states that no longer exist linger and read as current.
    #[arg(long)]
    prune: bool,

    /// Colour scheme for the SVGs.
    #[arg(long, default_value = "dark")]
    theme: String,

    /// Font family for the SVGs. Needs the box-drawing and block glyphs, or
    /// the frames and the Map canvas come out as blanks.
    #[arg(long, default_value = "monospace")]
    font: String,

    /// Font size in pixels for the SVGs; the cell grid is derived from it.
    #[arg(long, default_value_t = 16.0)]
    font_size: f64,
}

/// Whether `name` is one of this tool's dumps
/// (`<cols>x<rows>-<slug>.txt` or `.svg`), which is the only thing `--prune`
/// is allowed to delete.
fn is_dump_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_suffix(".txt")
        .or_else(|| name.strip_suffix(".svg"))
    else {
        return false;
    };
    let Some((size, slug)) = rest.split_once('-') else {
        return false;
    };
    let Some((w, h)) = size.split_once('x') else {
        return false;
    };
    !slug.is_empty()
        && !w.is_empty()
        && !h.is_empty()
        && w.bytes().all(|b| b.is_ascii_digit())
        && h.bytes().all(|b| b.is_ascii_digit())
}

fn parse_size(raw: &str) -> Result<(u16, u16), String> {
    let (w, h) = raw
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected COLSxROWS, got {raw:?}"))?;
    let parse = |s: &str, what: &str| {
        s.trim()
            .parse::<u16>()
            .map_err(|e| format!("bad {what} in {raw:?}: {e}"))
    };
    Ok((parse(w, "width")?, parse(h, "height")?))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The rendered screen as text: one line per terminal row, full width, with
/// trailing blanks kept so column alignment survives into the file.
fn screen(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0 .. area.height {
        for x in 0 .. area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// A horizontal stretch of cells on one row sharing a single style. The unit
/// both the style report and the SVG are built from, so neither can disagree
/// with the other about where a highlight begins or ends.
struct Run {
    row: u16,
    col: u16,
    text: String,
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

impl Run {
    /// Whether the run carries no styling at all, and so says nothing about
    /// highlighting.
    fn is_plain(&self) -> bool {
        self.fg == Color::Reset
            && self.bg == Color::Reset
            && self.modifier.is_empty()
    }

    fn width(&self) -> u16 {
        self.text.chars().count() as u16
    }
}

/// Split the screen into styled runs, row by row, covering every cell.
fn runs(buffer: &Buffer) -> Vec<Run> {
    let area = buffer.area;
    let mut out = Vec::new();
    for row in 0 .. area.height {
        let mut col = 0;
        while col < area.width {
            let cell = &buffer[(col, row)];
            let (fg, bg, modifier) = (cell.fg, cell.bg, cell.modifier);
            let start = col;
            let mut text = String::new();
            while col < area.width {
                let cell = &buffer[(col, row)];
                if cell.fg != fg || cell.bg != bg || cell.modifier != modifier {
                    break;
                }
                text.push_str(cell.symbol());
                col += 1;
            }
            out.push(Run {
                row,
                col: start,
                text,
                fg,
                bg,
                modifier,
            });
        }
    }
    out
}

/// Where the screen is styled away from the terminal default: one line per run
/// of cells sharing a colour or modifier. Focus, selection and highlighting
/// are drawn with style alone, so two states can share a character grid and
/// differ only here.
fn style_report(buffer: &Buffer) -> String {
    let mut out = String::new();
    for run in runs(buffer).iter().filter(|r| !r.is_plain()) {
        out.push_str(&format!(
            "row {:>3}  cols {:>3}-{:<3}  fg={:?} bg={:?} mod={:?}  {:?}\n",
            run.row,
            run.col,
            run.col + run.width() - 1,
            run.fg,
            run.bg,
            run.modifier,
            run.text.trim_end(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// SVG export
// ---------------------------------------------------------------------------

/// A terminal colour scheme: the sixteen ANSI slots plus the default
/// foreground and background a `Color::Reset` resolves to.
struct Theme {
    name: &'static str,
    fg: &'static str,
    bg: &'static str,
    ansi: [&'static str; 16],
}

const THEMES: &[Theme] = &[
    Theme {
        name: "dark",
        fg: "#c5c8c6",
        bg: "#1d1f21",
        ansi: [
            "#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb",
            "#8abeb7", "#c5c8c6", "#666666", "#d54e53", "#b9ca4a", "#e7c547",
            "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
        ],
    },
    Theme {
        name: "light",
        fg: "#2e3436",
        bg: "#ffffff",
        ansi: [
            "#000000", "#cc0000", "#4e9a06", "#c4a000", "#3465a4", "#75507b",
            "#06989a", "#d3d7cf", "#555753", "#ef2929", "#8ae234", "#fce94f",
            "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",
        ],
    },
];

fn theme_named(name: &str) -> Result<&'static Theme, String> {
    THEMES.iter().find(|t| t.name == name).ok_or_else(|| {
        let known: Vec<_> = THEMES.iter().map(|t| t.name).collect();
        format!(
            "unknown theme {name:?}; known: {}",
            known.join(", ")
        )
    })
}

/// Resolve one ratatui colour against `theme`. `Reset` means "whatever the
/// terminal defaults to", which differs for text and for background.
fn resolve(color: Color, theme: &Theme, is_fg: bool) -> String {
    let ansi = |i: usize| theme.ansi[i].to_owned();
    match color {
        Color::Reset => {
            if is_fg {
                theme.fg.to_owned()
            } else {
                theme.bg.to_owned()
            }
        }
        Color::Black => ansi(0),
        Color::Red => ansi(1),
        Color::Green => ansi(2),
        Color::Yellow => ansi(3),
        Color::Blue => ansi(4),
        Color::Magenta => ansi(5),
        Color::Cyan => ansi(6),
        Color::Gray => ansi(7),
        Color::DarkGray => ansi(8),
        Color::LightRed => ansi(9),
        Color::LightGreen => ansi(10),
        Color::LightYellow => ansi(11),
        Color::LightBlue => ansi(12),
        Color::LightMagenta => ansi(13),
        Color::LightCyan => ansi(14),
        Color::White => ansi(15),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => xterm_256(i, theme),
    }
}

/// The xterm 256-colour palette: the sixteen ANSI slots, then a 6x6x6 RGB
/// cube, then a 24-step greyscale ramp.
fn xterm_256(i: u8, theme: &Theme) -> String {
    match i {
        0 ..= 15 => theme.ansi[i as usize].to_owned(),
        16 ..= 231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = i as usize - 16;
            let (r, g, b) = (
                LEVELS[i / 36],
                LEVELS[(i / 6) % 6],
                LEVELS[i % 6],
            );
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        _ => {
            let v = 8 + 10 * (i as u16 - 232);
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Draw the screen as an SVG: a background rectangle per styled stretch and a
/// text element per stretch of glyphs. Unlike the text dump this keeps every
/// colour and modifier, so highlighting and focus survive into the file.
///
/// Each text run is pinned to its grid width with `textLength`, so the cells
/// stay aligned under any font whose advance width isn't exactly `0.6em`.
fn svg(buffer: &Buffer, theme: &Theme, font: &str, font_size: f64) -> String {
    let area = buffer.area;
    let cell_w = font_size * 0.6;
    let cell_h = font_size * 1.2;
    let (width, height) = (
        cell_w * area.width as f64,
        cell_h * area.height as f64,
    );

    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" \
         height=\"{height:.0}\" viewBox=\"0 0 {width:.2} \
         {height:.2}\">\n<rect width=\"100%\" height=\"100%\" \
         fill=\"{}\"/>\n<g font-family=\"{}\" font-size=\"{font_size}px\" \
         xml:space=\"preserve\">\n",
        theme.bg,
        escape_xml(font),
    );

    let runs = runs(buffer);

    // Backgrounds first, so every glyph lands on top of its own cell colour.
    for run in &runs {
        let (_, bg) = reversed_pair(run);
        let fill = resolve(bg, theme, false);
        if fill == theme.bg {
            continue;
        }
        out.push_str(&format!(
            "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" \
             fill=\"{fill}\"/>\n",
            run.col as f64 * cell_w,
            run.row as f64 * cell_h,
            run.width() as f64 * cell_w,
            cell_h,
        ));
    }

    for run in &runs {
        if run.text.trim().is_empty() || run.modifier.contains(Modifier::HIDDEN)
        {
            continue;
        }
        let (fg, _) = reversed_pair(run);
        let mut attrs = format!("fill=\"{}\"", resolve(fg, theme, true));
        if run.modifier.contains(Modifier::BOLD) {
            attrs.push_str(" font-weight=\"bold\"");
        }
        if run.modifier.contains(Modifier::ITALIC) {
            attrs.push_str(" font-style=\"italic\"");
        }
        if run.modifier.contains(Modifier::DIM) {
            attrs.push_str(" opacity=\"0.6\"");
        }
        if run.modifier.contains(Modifier::UNDERLINED) {
            attrs.push_str(" text-decoration=\"underline\"");
        }
        if run.modifier.contains(Modifier::CROSSED_OUT) {
            attrs.push_str(" text-decoration=\"line-through\"");
        }
        out.push_str(&format!(
            "<text x=\"{:.2}\" y=\"{:.2}\" textLength=\"{:.2}\" \
             lengthAdjust=\"spacing\" {attrs}>{}</text>\n",
            run.col as f64 * cell_w,
            // Baseline, not the cell top: most of the glyph sits above it.
            run.row as f64 * cell_h + font_size * 0.95,
            run.width() as f64 * cell_w,
            escape_xml(&run.text),
        ));
    }

    out.push_str("</g>\n</svg>\n");
    out
}

/// A run's drawing colours, with `REVERSED` applied — the terminal swaps the
/// pair itself, so an exported image has to do it too.
fn reversed_pair(run: &Run) -> (Color, Color) {
    if run.modifier.contains(Modifier::REVERSED) {
        (run.bg, run.fg)
    } else {
        (run.fg, run.bg)
    }
}

/// One gallery entry: a slug for the filename, a description for the index,
/// and the state to put the shell in before drawing.
struct State {
    slug: &'static str,
    description: &'static str,
    build: Box<dyn Fn(&mut AppShell)>,
}

fn state(
    slug: &'static str,
    description: &'static str,
    build: impl Fn(&mut AppShell) + 'static,
) -> State {
    State {
        slug,
        description,
        build: Box::new(build),
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Walk the top bar to `app` and leave focus there. The bar starts on Profits,
/// so this is a run of right-arrows; `enter` then descends into the page.
fn open(shell: &mut AppShell, app: AppId, enter: bool) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let steps = APP_LIST.iter().position(|a| *a == app).unwrap_or(0);
    for _ in 0 .. steps {
        shell.handle_key(key(KeyCode::Right), &tx);
    }
    if enter {
        shell.handle_key(key(KeyCode::Enter), &tx);
    }
}

// ---------------------------------------------------------------------------
// Synthetic chat log
// ---------------------------------------------------------------------------

/// Our own pirate, as the app learns it from `--user`.
const ME: &str = "Playerone";

/// A full pillaging run: board, crew aboard, sail, three fights (win, loss,
/// win) with loot and a greedy hit each, a plank, port, divvy.
const PILLAGE: &[&str] = &[
    "====== 2026/06/16 ======",
    "[01:00:00] Going aboard the Test Vessel...",
    "[01:00:01] Matetwo has come aboard.",
    "[01:00:02] Matethree has come aboard.",
    "[01:00:03] 3 swabbies have come aboard.",
    "[01:00:05] This vessel is now Pillaging, Average to Hard Barbarians.",
    "[01:00:06] Playerone issued an order to set the vessel to sail.",
    // fight one: a win, with a greedy hit
    "[01:05:00] You intercepted the War Frigate 'Modest Sild'!",
    "[01:06:00] Test Vessel has grappled Modest Sild. A melee breaks out \
     between the crews!",
    "[01:06:10] Matethree delivers an overwhelming barrage against Hunched \
     Alice, causing some treasure to fall from their grip!",
    "[01:06:12] Brawny Brigand is eliminated!",
    "[01:06:14] Matetwo is eliminated!",
    "[01:06:20] Grizzled Brigand is eliminated!",
    "[01:07:00] Game over.  Winners: Playerone, Matetwo.",
    "[01:07:05] The victors plundered 7,756 pieces of eight and 9 units of \
     goods from the defeated vessel.",
    "[01:07:06] Ye received 576 pieces of eight as your initial cut of the \
     booty!",
    // fight two: a loss
    "[01:12:00] You have been intercepted by the Xebec 'Thieving Stickleback'!",
    "[01:13:00] Thieving Stickleback has grappled Test Vessel. A melee \
     breaks out between the crews!",
    "[01:13:20] Playerone is eliminated!",
    "[01:14:00] Game over.  Winners: Nervy Hugh, Insane Yang.",
    "[01:14:05] The victors plundered 3,207 pieces of eight and 15 units of \
     goods from the defeated vessel.",
    // fight three: a win, and a mercenary revealed by the winners roster
    "[01:20:00] You intercepted the Sloop 'Boring Gar'!",
    "[01:21:00] Test Vessel has grappled Boring Gar. A melee breaks out \
     between the crews!",
    "[01:21:10] Matetwo executes a masterful strike against Demented Carlos, \
     who drops some treasure in surprise!",
    "[01:21:20] Scurvy Brigand is eliminated!",
    "[01:22:00] Game over.  Winners: Playerone, Matetwo, Matethree, Luka \
     Merciless, A swabbie.",
    "[01:22:05] The victors plundered 5,400 pieces of eight and 20 units of \
     goods from the defeated vessel.",
    "[01:25:00] Playerone forced Matefour to walk the plank.",
    "[01:29:00] Playerone issued an order to put into port.",
    "[01:30:00] The booty has been divided!",
];

/// An Atlantis run, left mid-encounter so both dragoon counters show: two
/// lone splashes, one monster party, one dragoon driven off.
const ATLANTIS: &[&str] = &[
    "====== 2026/06/17 ======",
    "[02:00:00] Going aboard the Abyssal Grunion...",
    "[02:00:01] Matetwo has come aboard.",
    "[02:00:02] Matethree has come aboard.",
    "[02:00:05] This vessel is now Atlantis.",
    "[02:00:06] Playerone issued an order to set the vessel to sail.",
    "[02:01:00] Ye hear a splash, and the sound of foreign footsteps.",
    "[02:01:30] Ye hear a splash, and the sound of foreign footsteps.",
    "[02:02:00] Dragoons from the monster took advantage of their proximity \
     to board yer vessel!",
    "[02:02:10] Playerone has driven Bellator from the ship!",
];

/// A Cursed Isles run: the fog tell, a raft phase, then two island waves so
/// the Enthralled pane and Fight Statistics both have numbers.
const CURSED_ISLES: &[&str] = &[
    "====== 2026/06/18 ======",
    "[03:00:00] Going aboard the Cursed Tuna...",
    "[03:00:01] Matetwo has come aboard.",
    "[03:00:02] Matethree has come aboard.",
    "[03:00:06] Playerone issued an order to set the vessel to sail.",
    "[03:00:10] The crew inhales the noxious fog, and starts to lose fine \
     motor control.",
    "[03:01:00] Boarders from the raft clamber onto yer vessel as theirs \
     sinks to the depths.",
    "[03:01:01] Boarders from the raft clamber onto yer vessel as theirs \
     sinks to the depths.",
    "[03:01:10] Playerone has taken control of a zombie.",
    "[03:01:11] Matetwo has taken control of a zombie.",
    "[03:01:20] Playerone has driven Controlled Zombie from the ship!",
    "[03:05:00] Ye land on the island, but an angry mob of its inhabitants \
     stands between ye and yer rightful plunderin'!",
    "[03:05:10] Servile Zombie is eliminated!",
    "[03:05:13] Enlightened One is eliminated!",
    "[03:05:14] Cursed Zombie is eliminated!",
    "[03:05:15] Mindless Zombie is eliminated!",
    "[03:06:00] Game over.  Winners: Playerone, Matetwo, Playerone's Thrall.",
    "[03:06:10] Berserk Cultist is eliminated!",
    "[03:06:11] Foaming Homunculus is eliminated!",
    "[03:07:00] Game over.  Winners: Playerone, Matetwo.",
];

/// A vampirate lair: two waves, the second ending in an all-vampire winners
/// list so the lair closes.
const VAMPIRATES: &[&str] = &[
    "====== 2026/06/19 ======",
    "[04:00:00] Going aboard the Thin Tigerfish...",
    "[04:00:01] Matetwo has come aboard.",
    "[04:00:02] Matethree has come aboard.",
    "[04:00:06] Playerone issued an order to set the vessel to sail.",
    "[04:01:00] Welcome to the vampire sanctum. ",
    "[04:01:10] Stygian Lilith is eliminated!",
    "[04:01:11] Matethree is eliminated!",
    "[04:01:12] Immortal Schreck is eliminated!",
    "[04:01:13] Craving Silvia is eliminated!",
    "[04:02:00] Game over.  Winners: Playerone, Matetwo.",
    "[04:02:10] Sunless Collins is eliminated!",
    "[04:03:00] Game over.  Winners: Revenant Drac, Gloaming Lucy.",
];

/// A player-versus-player fight: an enemy real player goes down, which is
/// what tips the battle into the PvP category.
const PVP: &[&str] = &[
    "====== 2026/06/20 ======",
    "[05:00:00] Going aboard the Enchanting Pike...",
    "[05:00:01] Matetwo has come aboard.",
    "[05:00:06] Playerone issued an order to set the vessel to sail.",
    "[05:01:00] You intercepted the War Frigate 'Bloody Nightmare'!",
    "[05:02:00] Enchanting Pike has grappled Bloody Nightmare. A melee breaks \
     out between the crews!",
    "[05:02:30] Enemyone is eliminated!",
    "[05:02:40] Sea Lawyer is eliminated!",
    "[05:04:00] Game over.  Winners: Playerone, Matetwo.",
    "[05:05:00] Playerone issued an order to put into port.",
];

/// A few goods to name the inventory rows. Ids only have to agree with the
/// rows that reference them.
fn commodities() -> Vec<Commodity> {
    ["Rum", "Iron", "Hemp", "Wood", "Cloth"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            Commodity {
                id: i as u64 + 1,
                name: name.to_owned(),
            }
        })
        .collect()
}

/// A shell with a chat log attached and our pirate known, but nothing parsed.
fn attached_shell() -> AppShell {
    let mut shell = AppShell::new(commodities());
    shell.chatlog.attached = true;
    shell.chatlog.player_name = Some(Arc::from(ME));
    shell
}

/// Feed lines through the parser without the live-navigation side effects, so
/// a state lands exactly where the gallery puts it.
fn feed(shell: &mut AppShell, lines: &[&str]) {
    for line in lines {
        shell.chatlog.process_line(line);
    }
}

/// The ocean the map states draw. Emerald unless `RQ_DUMP_OCEAN` says
/// otherwise, matching the dump tests in `src/map/ui.rs`.
fn dump_ocean() -> &'static Map {
    let name =
        std::env::var("RQ_DUMP_OCEAN").unwrap_or_else(|_| "Emerald".to_owned());
    Map::for_ocean(&name).unwrap_or_else(|| panic!("{name} map"))
}

/// A shell sitting on the Map page of `dump_ocean()`, cursor on an island.
fn map_shell() -> AppShell {
    let mut shell = attached_shell();
    shell.ocean = Some(Ocean::Emerald);
    open(&mut shell, AppId::Map, true);
    let first = dump_ocean().islands.first().expect("an island");
    shell.map.cursor = Some((first.x, first.y));
    // A pirate is what the memorization tally is keyed to; without one the
    // column asks for it instead, which `map-no-pirate` covers.
    shell.map.pirate = Some(ME.to_owned());
    shell
}

// ---------------------------------------------------------------------------
// The gallery
// ---------------------------------------------------------------------------

fn top_bar_states(states: &mut Vec<State>) {
    for app in APP_LIST {
        let (slug, description) = match app {
            AppId::Profits => {
                (
                    "bar-profits",
                    "top bar, Profits selected",
                )
            }
            AppId::Damage => ("bar-damage", "top bar, Damage selected"),
            AppId::Chatlog => {
                (
                    "bar-jobbers",
                    "top bar, Jobbers selected",
                )
            }
            AppId::Voyage => {
                (
                    "bar-voyage",
                    "top bar, Voyage Statistics selected",
                )
            }
            AppId::Map => ("bar-map", "top bar, Map selected"),
            AppId::Exit => ("bar-exit", "the Exit page"),
        };
        let app = *app;
        states.push(state(slug, description, move |shell| {
            open(shell, app, false)
        }));
    }
}

fn profits_states(states: &mut Vec<State>) {
    // Market off (no ocean) shows the manual Sell/Buy price columns; with
    // a supported ocean those columns give way to the place fields.
    states.push(state(
        "profits-empty-manual",
        "Profits, empty, no ocean (manual price columns)",
        |shell| open(shell, AppId::Profits, true),
    ));
    states.push(state(
        "profits-empty-market",
        "Profits, empty, Market ocean (no price columns)",
        |shell| {
            shell.ocean = Some(Ocean::Emerald);
            shell.query_market = true;
            open(shell, AppId::Profits, true);
        },
    ));

    let populate = |shell: &mut AppShell| {
        shell.ocean = Some(Ocean::Emerald);
        shell.query_market = true;
        for (i, (restock, stock, booty)) in
            [("100", "250", "40"), ("50", "0", "12"), ("", "80", "")]
                .into_iter()
                .enumerate()
        {
            let mut row = InventoryRow::new(i as u64 + 1);
            row.restock = restock.to_owned();
            row.stock = stock.to_owned();
            row.booty = booty.to_owned();
            shell.profits.rows.push(row);
        }
        shell.profits.panel[0].value = "Admiral Island".to_owned();
        open(shell, AppId::Profits, true);
    };

    states.push(state(
        "profits-rows",
        "Profits with inventory rows, table focused",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Table;
        },
    ));
    states.push(state(
        "profits-panel-focus",
        "Profits, a parameter field focused",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Panel(1);
        },
    ));
    states.push(state(
        "profits-button-focus",
        "Profits, Calculate button focused",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Button;
        },
    ));
    states.push(state(
        "profits-submit-failed",
        "Profits with a rejected commodity search",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Input;
            shell.profits.input = "Nosuchgood".to_owned();
            shell.profits.submit_failed =
                Some("No commodity matches \"Nosuchgood\".".to_owned());
        },
    ));

    // -- the six popups --
    states.push(state(
        "profits-popup-requery",
        "Profits, re-query confirmation",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::ReQueryConfirm {
                yes_focused: true,
            });
        },
    ));
    states.push(state(
        "profits-popup-delete",
        "Profits, delete-row confirmation",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::DeleteConfirm {
                row_idx: 0,
                name: "Rum".to_owned(),
                yes_focused: false,
            });
        },
    ));
    states.push(state(
        "profits-popup-restock-warning",
        "Profits, restock place not recognized",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::RestockWarning {
                missing: vec!["Hemp".to_owned(), "Cloth".to_owned()],
                ocean_wide_focused: true,
            });
        },
    ));
    states.push(state(
        "profits-popup-price-block",
        "Profits, blocked on missing prices",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::PriceBlock {
                need_buy: vec!["Iron".to_owned()],
                need_sell: vec!["Hemp".to_owned(), "Cloth".to_owned()],
            });
        },
    ));
    states.push(state(
        "profits-popup-result",
        "Profits, the calculated breakdown",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::ProfitResult(ProfitResult {
                gross_plundered: 16_363,
                restock_reserve: 2_000,
                immediate_cuts: 1_200,
                chest_gross: 8_181,
                stolen: 0,
                chest_net: 8_181,
                goods_value: 4_400,
                restock_value: 1_750,
                add_to_restocking: 2_000,
                stocking: 500,
                total_gained: 9_331,
                co_cut: 933,
                crew_donation: 466,
                add_to_booty: 7_932,
            }));
        },
    ));
    states.push(state(
        "profits-popup-hold-import",
        "Profits, clipboard hold import prompt",
        move |shell| {
            populate(shell);
            shell.profits.popup = Some(PopupKind::HoldImport(HoldImport {
                goods: vec![(1, 240), (2, 80)],
                unknown: vec!["Blackpowder".to_owned()],
                yes_focused: true,
                resume: Focus::Table,
            }));
        },
    ));
}

fn damage_states(states: &mut Vec<State>) {
    states.push(state(
        "damage-default",
        "Damage calculator, unseeded",
        |shell| open(shell, AppId::Damage, true),
    ));

    let seeded = |shell: &mut AppShell| {
        shell.damage.left_ship = 2;
        shell.damage.right_ship = 4;
        shell.damage.left = [3, 2];
        shell.damage.right = [1, 4];
        shell.damage.rams = 1;
        open(shell, AppId::Damage, true);
    };

    // Only rows up to `LAST_INTERACTIVE_ROW` take the cursor; Shots Left,
    // Damage and Manpower Advantage are derived readouts it never lands on,
    // so focusing them is not a state the app can be in.
    for (slug, description, row) in [
        (
            "damage-focus-ship",
            "Damage, Ship row focused",
            0usize,
        ),
        (
            "damage-focus-shots-taken",
            "Damage, Shots Taken row focused",
            1,
        ),
        (
            "damage-focus-rocks",
            "Damage, Rocks Banged row focused",
            2,
        ),
        (
            "damage-focus-rams",
            "Damage, Times Rammed row focused",
            3,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            seeded(shell);
            shell.damage.focus_row = row;
            shell.damage.focus_side = Side::Right;
        }));
    }

    states.push(state(
        "damage-popup-ship",
        "Damage, ship picker open",
        move |shell| {
            seeded(shell);
            shell.damage.popup = Some(ShipSelectPopup {
                side: Side::Left,
                selected: 3,
            });
        },
    ));
    states.push(state(
        "damage-popup-reset",
        "Damage, reset confirmation",
        move |shell| {
            seeded(shell);
            shell.damage.reset_prompt = Some(true);
        },
    ));
    states.push(state(
        "damage-popup-battle",
        "Damage, new-battle prompt",
        move |shell| {
            seeded(shell);
            shell.damage.battle_prompt = Some(BattlePrompt {
                ship_name: Some("Modest Sild".to_owned()),
                foe_ship: Some(4),
                note: Some("Previous fight was saved.".to_owned()),
                prev_saved: true,
                apply: true,
            });
        },
    ));
    // The same prompt as the live tailer raises it, straight off an
    // interception line. No keypress follows: the live path navigates to the
    // calculator itself, and an Enter here would answer the prompt away.
    states.push(state(
        "damage-live-battle-prompt",
        "Damage, battle prompt raised by a live interception",
        |shell| {
            feed(shell, PILLAGE);
            for line in [
                "[06:00:00] Playerone issued an order to set the vessel to \
                 sail.",
                "[06:01:00] You have been intercepted by the Grand Frigate \
                 'Sea Lord'!",
            ] {
                shell.feed_chat_line(line);
            }
        },
    ));
}

fn jobbers_states(states: &mut Vec<State>) {
    states.push(state(
        "jobbers-detached",
        "Jobbers with no chat log attached",
        |shell| {
            shell.chatlog.attached = false;
            open(shell, AppId::Chatlog, true);
        },
    ));
    states.push(state(
        "jobbers-attached-empty",
        "Jobbers, log attached but nothing parsed",
        |shell| open(shell, AppId::Chatlog, true),
    ));

    // -- one state per voyage-type layout, each on a matching run --
    for (slug, description, log, voyage_type, focus) in [
        (
            "jobbers-pillage",
            "Jobbers, Pillage layout (Aboard/Greedy/Planked)",
            PILLAGE,
            VoyageType::Pillage,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-atlantis",
            "Jobbers, Atlantis layout with dragoon counters",
            ATLANTIS,
            VoyageType::Atlantis,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-cursed-isles",
            "Jobbers, Cursed Isles layout (Enthralled + Fight Statistics)",
            CURSED_ISLES,
            VoyageType::CursedIsles,
            JobberFocus::Enthralled,
        ),
        (
            "jobbers-vampirates",
            "Jobbers, Vampirates layout (filling leaderboard)",
            VAMPIRATES,
            VoyageType::Vampirates,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-vikings",
            "Jobbers, Vikings layout (panes beside the leaderboard)",
            PILLAGE,
            VoyageType::Vikings,
            JobberFocus::Planked,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            feed(shell, log);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = voyage_type;
            shell.jobbers_ui.focus = focus;
        }));
    }

    states.push(state(
        "jobbers-greedy-focus",
        "Jobbers, Greedy pane focused",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.focus = JobberFocus::Greedy;
        },
    ));
    states.push(state(
        "jobbers-leaderboard-focus",
        "Jobbers, skill leaderboard focused (cache empty)",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.focus = JobberFocus::Leaderboard;
        },
    ));
    states.push(state(
        "jobbers-leaderboard-capped",
        "Jobbers, leaderboard capped to five rows",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.leaderboard_size = Some(5);
        },
    ));

    // -- popups --
    states.push(state(
        "jobbers-popup-vessel",
        "Jobbers, vessel picker",
        |shell| {
            feed(shell, PILLAGE);
            feed(shell, ATLANTIS);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.vessel_popup = Some(0);
        },
    ));
    states.push(state(
        "jobbers-popup-ship-type",
        "Jobbers, ship-type picker",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.ship_popup = Some(3);
        },
    ));
    states.push(state(
        "jobbers-popup-voyage-type",
        "Jobbers, voyage-type picker",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_popup = Some(2);
        },
    ));
    states.push(state(
        "jobbers-popup-pirate",
        "Jobbers, pirate stats popup (not queried)",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 0,
            });
        },
    ));
    states.push(state(
        "jobbers-popup-trophy",
        "Jobbers, trophy browser layered over pirate stats",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 1,
            });
            shell.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: "Matetwo".to_owned(),
                search: String::new(),
                offset: 0,
                view_h: 10,
            });
        },
    ));
    states.push(state(
        "jobbers-popup-skill-dist",
        "Jobbers, skill distribution scatterplot",
        |shell| {
            feed(shell, VAMPIRATES);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = VoyageType::Vampirates;
            shell.jobbers_ui.skill_dist_popup = Some(SkillDistPopup {
                cursor: (50, 50),
            });
        },
    ));
    states.push(state(
        "jobbers-popup-per-fight",
        "Jobbers, per-fight advantage graph",
        |shell| {
            feed(shell, CURSED_ISLES);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = VoyageType::CursedIsles;
            shell.jobbers_ui.per_fight_popup = Some(PerFightPopup {
                idx: 0,
                axis: AxisMode::Time,
            });
        },
    ));

    // The live path navigates itself: entering a lair, or the Cursed Isles
    // fog tell, switches the page and its layout with no keypress at all.
    states.push(state(
        "jobbers-live-lair",
        "Jobbers, lair entry switching the layout live",
        |shell| {
            for line in &VAMPIRATES[.. 7] {
                shell.feed_chat_line(line);
            }
        },
    ));
    states.push(state(
        "jobbers-live-cursed-isles",
        "Jobbers, the fog tell switching the layout live",
        |shell| {
            for line in &CURSED_ISLES[.. 7] {
                shell.feed_chat_line(line);
            }
        },
    ));
}

fn voyage_states(states: &mut Vec<State>) {
    states.push(state(
        "voyage-empty",
        "Voyage Statistics with no voyage tracked",
        |shell| open(shell, AppId::Voyage, true),
    ));
    states.push(state(
        "voyage-pillage",
        "Voyage Statistics on a finished pillage",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Voyage, true);
        },
    ));
    states.push(state(
        "voyage-pvp",
        "Voyage Statistics on a PvP fight",
        |shell| {
            feed(shell, PVP);
            open(shell, AppId::Voyage, true);
        },
    ));
    states.push(state(
        "voyage-scrolled",
        "Voyage Statistics scrolled down to the charts",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Voyage, true);
            // The offset is derived from the focused item, so focus the last
            // one and let the body scroll to it.
            shell.voyage_ui.focus = usize::MAX;
        },
    ));

    for (slug, description, idx) in [
        (
            "voyage-popup-winrate",
            "Voyage, enlarged ship winrate chart",
            0,
        ),
        (
            "voyage-popup-chart",
            "Voyage, enlarged PoE-per-fight chart",
            1,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Voyage, true);
            shell.voyage_ui.chart_popup = Some(idx);
        }));
    }

    states.push(state(
        "voyage-popup-battles",
        "Voyage, Sea Battles popup with its embedded calculator",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Voyage, true);
            shell.voyage_ui.battles_popup = Some(0);
        },
    ));
    states.push(state(
        "voyage-popup-save",
        "Voyage, save-or-discard prompt",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Voyage, true);
            shell.voyage_ui.prompt = Some(SaveChoice::Save);
        },
    ));
}

fn map_states(states: &mut Vec<State>) {
    states.push(state(
        "map-no-ocean",
        "Map with no ocean selected",
        |shell| open(shell, AppId::Map, true),
    ));
    states.push(state(
        "map-ocean",
        "Map of the ocean, cursor on an island",
        |shell| {
            *shell = map_shell();
        },
    ));
    states.push(state(
        "map-memorized",
        "Map with memorized league points tallied for a pirate",
        |shell| {
            *shell = map_shell();
            shell.map.pirate = Some(ME.to_owned());
            let island = dump_ocean().islands.first().expect("an island");
            shell.map.memorized.insert((island.x, island.y));
            if let Some(next) = dump_ocean().islands.get(1) {
                shell.map.memorized.insert((next.x, next.y));
            }
        },
    ));
    states.push(state(
        "map-no-pirate",
        "Map asking for a pirate before it tallies",
        |shell| {
            *shell = map_shell();
            shell.map.pirate = None;
        },
    ));
    states.push(state(
        "map-search-hit",
        "Map search box with a match",
        |shell| {
            *shell = map_shell();
            let island = dump_ocean().islands.first().expect("an island");
            let mut field = PromptField::new("Find", FieldKind::Text);
            field.value = island.name.to_owned();
            field.cursor = field.value.chars().count();
            shell.map.search = Some(field);
        },
    ));
    states.push(state(
        "map-search-miss",
        "Map search box with no match",
        |shell| {
            *shell = map_shell();
            let mut field = PromptField::new("Find", FieldKind::Text);
            field.value = "Nowhere".to_owned();
            field.cursor = field.value.chars().count();
            shell.map.search = Some(field);
        },
    ));
    states.push(state(
        "map-help",
        "Map help popup",
        |shell| {
            *shell = map_shell();
            shell.map.help = true;
        },
    ));
}

fn states() -> Vec<State> {
    let mut states = Vec::new();
    top_bar_states(&mut states);
    profits_states(&mut states);
    damage_states(&mut states);
    jobbers_states(&mut states);
    voyage_states(&mut states);
    map_states(&mut states);
    states
}

// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let sizes = if args.sizes.is_empty() {
        vec![(80, 24), (120, 40)]
    } else {
        args.sizes.clone()
    };

    fs::create_dir_all(&args.out)?;

    let mut pruned = 0usize;
    if args.prune {
        for entry in fs::read_dir(&args.out)? {
            let entry = entry?;
            if is_dump_name(&entry.file_name().to_string_lossy()) {
                fs::remove_file(entry.path())?;
                pruned += 1;
            }
        }
    }

    let theme = theme_named(&args.theme)?;
    let states = states();
    let (mut nav, mut sheet) = (String::new(), String::new());
    let mut index = String::from(
        "Interface states rendered by `cargo run --bin gallery`.\n\nEach dump \
         is the plain character grid of one screen. Focus and selection\nare \
         drawn with colour rather than characters, so states that \
         differ\nonly in focus share a grid and are told apart in \
         STYLES-*.txt.\n",
    );
    let mut written = 0usize;

    for (width, height) in &sizes {
        index.push_str(&format!("\n{width}x{height}\n"));
        let mut styles = String::new();
        for state in &states {
            let mut shell = attached_shell();
            (state.build)(&mut shell);

            let mut terminal =
                Terminal::new(TestBackend::new(*width, *height))?;
            terminal.draw(|frame| shell.render(frame))?;

            let stem = format!("{width}x{height}-{}", state.slug);
            let buffer = terminal.backend().buffer();

            fs::write(
                args.out.join(format!("{stem}.txt")),
                screen(buffer),
            )?;
            fs::write(
                args.out.join(format!("{stem}.svg")),
                svg(
                    buffer,
                    theme,
                    &args.font,
                    args.font_size,
                ),
            )?;
            index.push_str(&format!(
                "  {stem}.txt / .svg  -- {}\n",
                state.description
            ));

            styles.push_str(&format!(
                "=== {stem}.txt -- {}\n",
                state.description
            ));
            styles.push_str(&style_report(buffer));
            styles.push('\n');

            sheet.push_str(&format!(
                "<section id=\"{stem}\"><h2>{stem}</h2><p>{}</p><img \
                 src=\"{stem}.svg\" alt=\"{stem}\"></section>\n",
                escape_xml(state.description),
            ));
            nav.push_str(&format!(
                "<li><a href=\"#{stem}\">{}</a></li>\n",
                escape_xml(state.slug),
            ));
            written += 1;
        }
        fs::write(
            args.out.join(format!("STYLES-{width}x{height}.txt")),
            styles,
        )?;
    }

    fs::write(args.out.join("INDEX.txt"), &index)?;
    fs::write(
        args.out.join("index.html"),
        contact_sheet(&nav, &sheet),
    )?;
    if pruned > 0 {
        println!("pruned {pruned} file(s) from an earlier run");
    }
    println!(
        "wrote {written} states to {} (open index.html to browse)",
        args.out.display()
    );
    Ok(())
}

/// One page listing every SVG, for browsing the whole gallery at once instead
/// of opening files one at a time.
fn contact_sheet(nav: &str, sections: &str) -> String {
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Interface \
         gallery</title>\n<style>\nbody {{ margin: 0; display: flex; font: \
         14px system-ui, sans-serif; background: #15171a; color: #c5c8c6; \
         }}\nnav {{ position: sticky; top: 0; align-self: flex-start; height: \
         100vh; overflow-y: auto; min-width: 20em; padding: 1em; background: \
         #1d1f21; }}\nnav ul {{ list-style: none; margin: 0; padding: 0; \
         }}\nnav a {{ color: #81a2be; text-decoration: none; }}\nnav a:hover \
         {{ text-decoration: underline; }}\nmain {{ flex: 1; padding: 1em \
         2em; min-width: 0; }}\nsection {{ margin-bottom: 3em; }}\nh2 {{ \
         font: 600 13px ui-monospace, monospace; color: #b5bd68; }}\np {{ \
         margin: 0 0 .75em; color: #969896; }}\nimg {{ max-width: 100%; \
         border: 1px solid #373b41; \
         }}\n</style>\n<nav><ul>\n{nav}</ul></nav>\n<main>\n{sections}</main>\\
         \
         n"
    )
}
