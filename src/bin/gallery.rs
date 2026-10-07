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
    bare,
    damage::{BattlePrompt, ShipSelectPopup, Side},
    islands::{CachedIslands, IslandInfo, parse_island_list},
    jobbers::{
        BoardTab,
        JobberFocus,
        NoteFocus,
        NotePopup,
        PerFightPopup,
        PiratePopup,
        SkillDistPopup,
        TrophyPopup,
        VoyageType,
    },
    map::{
        data::{Chart, Heading, Map},
        ui::caption_points,
    },
    ocean::Ocean,
    profits::{Focus, HoldImport, InventoryRow, PopupKind, ProfitResult},
    utils::{FieldKind, PromptField},
    voyage::{
        AxisMode,
        BattleSnapshot,
        ui::{SaveFocus, SavePrompt},
    },
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

/// Something the app draws that isn't one of its pages, and so is drawn once
/// at a size of its own rather than at every size asked for.
///
/// Two things qualify. The setup screen runs before the app exists and owns
/// its own loop, so it never passes through [`AppShell`] at all. The
/// window-too-small notice only appears in a window too small for the app,
/// which is not a size worth rendering every page at.
struct Screen {
    slug: &'static str,
    description: &'static str,
    size: (u16, u16),
    draw: Box<dyn Fn(&mut ratatui::Frame)>,
}

fn screen_state(
    slug: &'static str,
    description: &'static str,
    size: (u16, u16),
    draw: impl Fn(&mut ratatui::Frame) + 'static,
) -> Screen {
    Screen {
        slug,
        description,
        size,
        draw: Box::new(draw),
    }
}

/// The screens outside the page model, in the order the user meets them.
fn screens() -> Vec<Screen> {
    vec![
        screen_state(
            "startup-setup",
            "Startup, the setup screen as it opens",
            (80, 24),
            |frame| {
                rusty_quartermaster::startup::preview(
                    frame,
                    Ocean::Emerald,
                    "Playerone",
                    None,
                )
            },
        ),
        screen_state(
            "startup-setup-nameless",
            "Startup, the tooltip for going without a name",
            (80, 24),
            |frame| {
                rusty_quartermaster::startup::preview(
                    frame,
                    Ocean::Emerald,
                    "",
                    None,
                )
            },
        ),
        screen_state(
            "startup-setup-no-such-pirate",
            "Startup, a name yoweb could not find",
            (80, 24),
            |frame| {
                rusty_quartermaster::startup::preview(
                    frame,
                    Ocean::Emerald,
                    "Playerone",
                    Some(
                        "Arr, no 'Playerone' to be found on the Emerald \
                         ocean. Check yer spelling, or Esc to skip.",
                    ),
                )
            },
        ),
        // One entry per form the refusal takes: the frame itself not fitting
        // either way or only across, and a page that fits the frame but not
        // the window, short of width or of height.
        screen_state(
            "window-tiny",
            "Too small for the app's own frame, both ways",
            (40, 2),
            |frame| attached_shell().render(frame),
        ),
        screen_state(
            "window-too-narrow-for-the-app",
            "Too narrow for the app's own frame",
            (40, 10),
            |frame| attached_shell().render(frame),
        ),
        screen_state(
            "window-too-narrow-for-the-page",
            "Too narrow for the open page",
            (50, 30),
            |frame| {
                let mut shell = attached_shell();
                open(&mut shell, AppId::Chatlog, true);
                shell.render(frame);
            },
        ),
        screen_state(
            "window-too-short-for-the-page",
            "Too short for the open page",
            (60, 20),
            |frame| attached_shell().render(frame),
        ),
    ]
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

/// Jobbers to send aboard on top of a run's own crew, enough of them that the
/// panes and the Skill Leaderboard both have more rows than they can show.
const LONG_ROSTER: [&str; 10] = [
    "Matea", "Mateb", "Matec", "Mated", "Matee", "Matef", "Mateg", "Mateh",
    "Matei", "Matej",
];

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
    "[01:13:00] Thieving Stickleback has grappled Test Vessel. A melee breaks \
     out between the crews!",
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

/// An Atlantis run, left mid-encounter: two lone dragoons splash aboard, the
/// monster lands a party, and one dragoon is driven off again. The tells
/// only mark the encounter — the game keeps the dragoon count itself.
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

/// Two intervals of the Atlantis run above, as the game copies them: tokens
/// off the maneuvering stations and chests off the haul. Two reports rather
/// than one because a run's figures are summed over the reports it produced,
/// and one report would never show that.
const ATLANTIS_REPORTS: [&str; 2] = [
    r#"{"sail":{"Matetwo":{"performance":4,
                           "maneuver_tokens":[12,7,3,2,0,0,0]},
               "Playerone":{"performance":3,
                            "maneuver_tokens":[5,9,1,4,0,0,0]}},
        "rigging":{"Matethree":{"performance":4,
                                "maneuver_tokens":[4,2,8,0,0,0,0]}},
        "haul":{"Matethree":{"performance":5,"m.treasure_hauled":[6,3,1]},
                "Matetwo":{"performance":2,"m.treasure_hauled":[2,1,0]}}}"#,
    r#"{"sail":{"Playerone":{"performance":5,
                             "maneuver_tokens":[9,14,2,6,0,0,0]},
               "Matethree":{"performance":2,
                            "maneuver_tokens":[3,1,0,1,0,0,0]}},
        "gunnery":{"Matetwo":{"performance":4,"cannons_loaded":31}},
        "haul":{"Playerone":{"performance":3,"m.treasure_hauled":[4,2,2]},
                "Matethree":{"performance":4,"m.treasure_hauled":[5,1,0]}}}"#,
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
    [
        "Rum",
        "Iron",
        "Hemp",
        "Wood",
        "Cloth",
        // consumables, for the Voyage Statistics consumption section
        "Small cannon balls",
        "Grog",
        "Fine rum",
        "Rum spice",
    ]
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

/// Put `name` in the pirate cache as a fully-fetched pirate, so the popups
/// that show a pirate's stats and trophies draw their contents instead of
/// saying they are not loaded yet. The skills cover all three families and the
/// trophies all three section shapes (two named groups and the ungrouped
/// remainder), which is what makes these states show the scrolling that a real
/// pirate's pages need.
fn cache_pirate(shell: &mut AppShell, name: &str, shift: usize) {
    cache_jobber(shell, name, Some(shift));
}

/// Put `name` in the cache as a greenie: no duty puzzle has carried them as far
/// as a Broad, and nothing on their trophy shelf was awarded for skill. Their
/// name is drawn green wherever a roster names them.
fn cache_greenie(shell: &mut AppShell, name: &str) {
    cache_jobber(shell, name, None);
}

/// Bring a roster aboard holding every crew rank, with one pirate of another
/// crew and one jobbing with ours, and cache them all — including the pirate
/// this run planks, so the Planked pane has a crewmate in it. Our own pirate is
/// cached too: their crew is what a rank tag is measured against, and the whole
/// roster goes untagged while the cache is without them.
fn crew_roster(shell: &mut AppShell) {
    const CREW: &str = "The Example Crew";
    const RANKS: [(&str, &str, &str); 8] = [
        ("Matea", "Captain", CREW),
        ("Mateb", "Senior Officer", CREW),
        ("Matec", "Fleet Officer", CREW),
        ("Mated", "Officer", CREW),
        ("Matee", "Pirate", CREW),
        ("Matef", "Cabin Person", CREW),
        ("Mateg", "Jobbing Pirate", CREW),
        ("Mateh", "Officer", "The Other Crew"),
    ];

    cache_pirate(shell, ME, 0);
    set_crew(shell, ME, "Fleet Officer", CREW);
    for (i, (name, rank, crew)) in RANKS.iter().enumerate() {
        shell.chatlog.process_line(&format!(
            "[01:00:30] {name} has come aboard."
        ));
        cache_pirate(shell, name, i);
        set_crew(shell, name, rank, crew);
    }
    cache_pirate(shell, "Matefour", 0);
    set_crew(shell, "Matefour", "Pirate", CREW);
}

/// Re-crew a cached pirate, which is what the Aboard pane's rank tags read. A
/// pirate the cache has not got yet is left alone, there being nothing to crew.
fn set_crew(shell: &mut AppShell, name: &str, rank: &str, crew: &str) {
    use rusty_quartermaster::pirate::normalize_name;

    let Ok(norm) = normalize_name(name) else {
        return;
    };
    if let Some(entry) = shell.pirate_cache.fetched.get_mut(&norm) {
        entry.basic.crew_rank = rank.to_owned();
        entry.basic.crew_name = crew.to_owned();
    }
}

/// The body of both: `shift` starts the pirate that far along the skill
/// ladders, or, as `None`, holds every skill at a greenie's.
fn cache_jobber(shell: &mut AppShell, name: &str, shift: Option<usize>) {
    use std::collections::HashMap;

    use chrono::Utc;
    use rusty_quartermaster::pirate::{
        BasicInfo,
        CachedPirate,
        Experience,
        Skill,
        SkillRecord,
        Standing,
        Trophies,
        TrophySection,
        normalize_name,
    };

    // Each skill gets a different pair so no two rows read alike; the ladders
    // are walked in step rather than being picked for any particular pirate.
    // `shift` starts a pirate further along them, which is what ranks a roster
    // of them against each other.
    const LADDER: [(Experience, Standing); 6] = [
        (Experience::Narrow, Standing::Able),
        (Experience::Broad, Standing::Proficient),
        (Experience::Solid, Standing::Respected),
        (Experience::Expert, Standing::Master),
        (Experience::Sublime, Standing::Legendary),
        (
            Experience::Transcendent,
            Standing::Ultimate,
        ),
    ];
    const SKILLS: [Skill; 12] = [
        Skill::Sailing,
        Skill::Carpentry,
        Skill::Bilging,
        Skill::Gunning,
        Skill::TreasureHaul,
        Skill::Swordfighting,
        Skill::Distilling,
        Skill::Alchemistry,
        Skill::Shipwrightery,
        Skill::Blacksmithing,
        Skill::Drinking,
        Skill::Poker,
    ];
    const TROPHIES: [(&str, &[&str]); 3] = [
        (
            "Sailing",
            &[
                "First Voyage Home",
                "A Hundred Leagues",
                "Sloop Sprint",
                "Brigantine Brace",
                "Galleon Gauntlet",
                "Tide Turner",
                "Longest Haul",
                "Lost Then Found",
                "Rigged in Full",
                "Bilge Bailer",
                "Patched Twice",
                "Gunner's Eye",
            ],
        ),
        (
            "Carousing",
            &[
                "Table of Ten",
                "Shuffled Deck",
                "Double Dare",
                "Last Call",
                "Spades Sweep",
                "Hearts Hoarder",
            ],
        ),
        (
            "",
            &["Unlabelled Keepsake", "Odd Memento"],
        ),
    ];

    let skills: HashMap<Skill, SkillRecord> = SKILLS
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            // A greenie is shy of a Broad in every puzzle a duty report rates,
            // so there is no ladder to walk for one.
            let (experience, standing) = match shift {
                Some(shift) => LADDER[(i + shift) % LADDER.len()],
                None => (Experience::Narrow, Standing::Able),
            };
            (
                s,
                SkillRecord {
                    experience,
                    standing,
                    archipelago: None,
                },
            )
        })
        .collect();
    let entry = CachedPirate {
        basic: BasicInfo {
            name: name.to_owned(),
            crew_rank: "Officer".to_owned(),
            crew_role: None,
            crew_name: "Test Crew".to_owned(),
            flag_rank: "Royalty".to_owned(),
            flag_name: "Example Flag".to_owned(),
            reputation: HashMap::new(),
            skills,
        },
        trophies: Trophies {
            sections: TROPHIES
                .iter()
                .map(|(category, trophies)| {
                    TrophySection {
                        category: (*category).to_owned(),
                        trophies: trophies
                            .iter()
                            .map(|t| (*t).to_owned())
                            .collect(),
                    }
                })
                .collect(),
        },
        basic_fetched_at: Utc::now(),
        trophies_fetched_at: Utc::now(),
    };
    shell.pirate_cache.fetched.insert(
        normalize_name(name).expect("a pirate name"),
        entry,
    );
}

/// The ocean the map states draw. Emerald unless `RQ_DUMP_OCEAN` says
/// otherwise, matching the dump tests in `src/map/ui.rs`.
fn dump_ocean() -> &'static Map {
    let name =
        std::env::var("RQ_DUMP_OCEAN").unwrap_or_else(|_| "Emerald".to_owned());
    Map::for_ocean(&name).unwrap_or_else(|| panic!("{name} map"))
}

/// The ocean's colonized islands written out the way yoweb writes them, so
/// the Map states show what the parser makes of a fetched page rather than a
/// list assembled by hand. Uninhabited islands are absent, as they are on
/// yoweb. The facts are walked off short ladders so no two islands read
/// alike, and the outposts are left without a governor or a ruling flag: a
/// colony yoweb says less about is the shape that proves the metadata column
/// leaves out what it has nothing to say about.
fn island_list_page(geo: &bare::Ocean) -> String {
    const GOVERNORS: [&str; 4] =
        ["Playertwo", "Playerthree", "Playerfour", "Playerfive"];
    // the last flag is longer than the metadata column is wide, since a flag
    // name may be as long as its founders liked. It falls to the island with
    // the longest export list, so one state carries both stresses
    const FLAGS: [&str; 6] = [
        "Example Flag",
        "Test Flag",
        "Sample & Sons",
        "Example Fleet",
        "Test Armada",
        "Sample Flag of the Example Fleet",
    ];
    // enough goods that the longest list is as long as a large island's, which
    // is what asks the Island column to scroll
    const EXPORTS: [&str; 12] = [
        "Hemp",
        "Iron",
        "Sugar cane",
        "Wood",
        "Fine black cloth",
        "Kraken's ink",
        "Lacquer",
        "Varnish",
        "Blue dye",
        "Broad cloth",
        "Fine sail cloth",
        "Nitramine",
    ];

    // yoweb's own entities, so the names and flags reach the parser the way
    // they reach it over the wire
    let encode = |text: &str| text.replace('&', "&amp;").replace('\'', "&#39;");
    let colonized = geo
        .archipelagos
        .iter()
        .flat_map(|arch| arch.islands.iter().map(move |isle| (arch, isle)))
        .filter(|(_, isle)| isle.status != bare::Status::Uninhabited);

    let mut page =
        String::from("<center><img src=\"/yoweb/images/header.png\"><br>\n");
    for (n, (arch, isle)) in colonized.enumerate() {
        page.push_str(&format!(
            "<center><font size=\"+1\">{}</font><br>\nPopulation: \
             {}<br>\nLocated in the {} archipelago.<br>\n",
            encode(&isle.name),
            100 + n * 37,
            arch.name,
        ));
        if isle.size != bare::Size::Outpost {
            page.push_str(&format!(
                "Governor: <a \
                 href=\"/yoweb/pirate.wm?target={gov}\">{gov}</a><br>\n",
                gov = GOVERNORS[n % GOVERNORS.len()],
            ));
        }
        page.push_str(&format!(
            "Property tax: {}%<br>\n</center>\n",
            5 * (n % 5),
        ));
        if isle.size != bare::Size::Outpost {
            page.push_str(&format!(
                "Ruled by <a \
                 href=\"/yoweb/flag/info.wm?flagid={}\">{}</a><br>\n",
                n + 1,
                encode(FLAGS[n % FLAGS.len()]),
            ));
        }
        let exports: Vec<&str> = (0 ..= n % EXPORTS.len())
            .map(|k| EXPORTS[(n + k) % EXPORTS.len()])
            .collect();
        page.push_str(&format!(
            "Exports: {} <br><br>\n",
            exports.join(" , "),
        ));
    }
    page
}

/// Put the ocean's island list in the shell as a finished fetch would, by
/// parsing the page above.
fn cache_islands(shell: &mut AppShell) {
    let Some(geo) = shell.ocean_geo() else {
        return;
    };
    shell.islands = Some(CachedIslands {
        fetched_at: chrono::Utc::now(),
        islands: parse_island_list(&island_list_page(geo)),
    });
    shell.island_list_wanted = false;
}

/// Move the cursor to the island on the map that `rank` scores highest among
/// those the fetched list names, leaving it where it is if none are. The
/// states pick an island by the shape of its entry rather than by name, so
/// they hold for whichever ocean is dumped.
fn cursor_on_island(
    shell: &mut AppShell,
    rank: impl Fn(&IslandInfo, &bare::Archipelago, &bare::Island) -> Option<usize>,
) {
    let geo = shell.ocean_geo();
    let list = shell.islands.as_ref().expect("a fetched island list");
    let point = dump_ocean()
        .islands
        .iter()
        .filter_map(|place| {
            let info = list.get(place.name)?;
            let (arch, isle) = geo?.island(place.name)?;
            Some((rank(info, arch, isle)?, place.at()))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, point)| point);
    if let Some(point) = point {
        shell.map.cursor = Some(point);
    }
}

/// A shell sitting on the Map page of `dump_ocean()`, cursor on an island,
/// with the ocean's island list already fetched.
fn map_shell() -> AppShell {
    map_shell_of(Ocean::Emerald)
}

/// A shell sitting on the Map page of one ocean, cursor on its first island,
/// with that ocean's island list already fetched.
fn map_shell_of(ocean: Ocean) -> AppShell {
    let mut shell = attached_shell();
    shell.ocean = Some(ocean);
    open(&mut shell, AppId::Map, true);
    let map = Map::for_ocean(ocean.name())
        .unwrap_or_else(|| panic!("{} map", ocean.name()));
    let first = map.islands.first().expect("an island");
    shell.map.cursor = Some((first.x, first.y));
    // A pirate is what the memorization tally is keyed to; without one the
    // chart asks for it instead, which `map-no-pirate` covers.
    shell.map.pirate = Some(ME.to_owned());
    cache_islands(&mut shell);
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
    // Market querying off (no ocean) shows the manual Sell/Buy price columns;
    // with a supported ocean those columns give way to the place fields.
    states.push(state(
        "profits-empty-manual",
        "Profits, empty, no ocean (manual price columns)",
        |shell| open(shell, AppId::Profits, true),
    ));
    states.push(state(
        "profits-empty-market",
        "Profits, empty, market ocean (no price columns)",
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
    // More commodities than any window over them: the Inventory is the one box
    // on the page that scrolls, so this is where its scrollbar shows.
    states.push(state(
        "profits-long-list",
        "Profits with more commodities than the Inventory can show",
        |shell| {
            shell.ocean = Some(Ocean::Emerald);
            shell.query_market = true;
            for (i, name) in [
                "Sugar cane",
                "Cocoa",
                "Coffee",
                "Tea",
                "Madder",
                "Indigo",
                "Lobster",
                "Swill",
                "Grog",
                "Nibs",
                "Broad cloth",
                "Fine cloth",
                "Kelp",
                "Lacquer",
                "Nails",
                "Oak",
                "Pine",
                "Sailcloth",
                "Stone",
                "Varnish",
            ]
            .into_iter()
            .enumerate()
            {
                shell.commodities.push(Commodity {
                    id: i as u64 + 10,
                    name: name.to_owned(),
                });
            }
            for id in (1 ..= 5).chain(10 .. 30) {
                let mut row = InventoryRow::new(id);
                row.stock = (10 * id).to_string();
                shell.profits.rows.push(row);
            }
            open(shell, AppId::Profits, true);
            shell.profits.focus = Focus::Table;
            shell.profits.table_state.select(Some(0));
        },
    ));
    // A table too wide for its box: the columns keep their widths and the
    // view scrolls to the selected one instead.
    states.push(state(
        "profits-wide-table",
        "Profits, inventory wider than its box (scrolled)",
        |shell| {
            shell.commodities.push(Commodity {
                id: 99,
                name: "Fine enchanted midnight broadcloth".to_owned(),
            });
            for id in [99, 1, 2] {
                let mut row = InventoryRow::new(id);
                row.stock = "120".to_owned();
                row.sell = "450".to_owned();
                row.buy = "380".to_owned();
                shell.profits.rows.push(row);
            }
            open(shell, AppId::Profits, true);
            shell.profits.focus = Focus::Table;
            shell.profits.table_state.select(Some(0));
            shell.profits.table_state.select_column(Some(5));
        },
    ));

    states.push(state(
        "profits-submit-failed",
        "Profits with a commodity search that resolves to nothing",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Input;
            // A query nothing answers is left in the box to be corrected, and
            // the row says as much where a name would be completed.
            shell.profits.search.value = "Nosuchgood".to_owned();
            shell.profits.search.cursor = shell.profits.search.value.len();
        },
    ));
    states.push(state(
        "profits-search-completed",
        "Profits completing a commodity name in the search row",
        move |shell| {
            populate(shell);
            shell.profits.focus = Focus::Input;
            shell.profits.search.value = "Ru".to_owned();
            shell.profits.search.cursor = shell.profits.search.value.len();
        },
    ));
    states.push(state(
        "profits-search-closed",
        "Profits inviting a search while the cursor is elsewhere",
        move |shell| {
            populate(shell);
            shell.profits.focus_table_top();
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
            shell.profits.focus = Focus::Popup;
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
            shell.profits.focus = Focus::Popup;
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
            shell.profits.focus = Focus::Popup;
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
            shell.profits.focus = Focus::Popup;
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
            shell.profits.focus = Focus::Popup;
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
            shell.profits.focus = Focus::Popup;
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
                note: None,
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
        // No tell says a flotilla, a blockade or the Haunted Seas is under
        // way, so the log is an ordinary pillage and the voyage type is the
        // quartermaster's word for it.
        (
            "jobbers-flotilla",
            "Jobbers, Flotilla layout (Atlantis's, picked by hand)",
            PILLAGE,
            VoyageType::Flotilla,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-blockade",
            "Jobbers, Blockade layout (Atlantis's, less the treasure haulers)",
            PILLAGE,
            VoyageType::Blockade,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-atlantis",
            "Jobbers, Atlantis layout (Aboard/Planked)",
            ATLANTIS,
            VoyageType::Atlantis,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-haunted-seas",
            "Jobbers, Haunted Seas layout (Atlantis's, picked by hand)",
            PILLAGE,
            VoyageType::HauntedSeas,
            JobberFocus::Aboard,
        ),
        (
            "jobbers-cursed-isles",
            "Jobbers, Cursed Isles layout (Enthralled + Fight Statistics)",
            CURSED_ISLES,
            VoyageType::CursedIsles,
            JobberFocus::Enthralled,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            feed(shell, log);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = voyage_type;
            shell.jobbers_ui.focus = focus;
        }));
    }

    // The Atlantis run once its duty reports have carried figures: the panes
    // stand one over the other and the Tokens and Chests box takes the column
    // beside them. One state per board, since each ranks its own figures.
    for (slug, description, tab) in [
        (
            "jobbers-atlantis-tokens",
            "Jobbers, Atlantis token leaderboard",
            BoardTab::Tokens,
        ),
        (
            "jobbers-atlantis-treasures",
            "Jobbers, Atlantis treasure leaderboard",
            BoardTab::Treasures,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            feed(shell, ATLANTIS);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = VoyageType::Atlantis;
            for text in ATLANTIS_REPORTS {
                let report =
                    rusty_quartermaster::duty::parse(text).expect("report");
                shell.take_duty_report(&report, chrono::Utc::now());
            }
            shell.jobbers_ui.board_tab = tab;
            shell.jobbers_ui.focus = JobberFocus::Board;
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

    states.push(state(
        "jobbers-long-roster",
        "Jobbers with more aboard than the pane and the leaderboard can show",
        |shell| {
            feed(shell, PILLAGE);
            for (i, name) in LONG_ROSTER.iter().enumerate() {
                shell.chatlog.process_line(&format!(
                    "[01:00:30] {name} has come aboard."
                ));
                // Stats are what the leaderboard ranks, so a roster without
                // them would leave it empty however long the roster is.
                cache_pirate(shell, name, i);
            }
            open(shell, AppId::Chatlog, true);
        },
    ));

    // The Aboard and Planked panes tag our own crew with each pirate's rank.
    // Every rank is aboard here, with one pirate of another crew and one
    // jobbing with ours — neither of which a tag speaks for. Nobody aboard has
    // been planked, so the pane spends no column on the plank mark: this is the
    // narrower of the two Aboard panes.
    states.push(state(
        "jobbers-crew-ranks",
        "Jobbers, crew ranks tagged on the roster",
        |shell| {
            feed(shell, PILLAGE);
            crew_roster(shell);
            open(shell, AppId::Chatlog, true);
        },
    ));

    // The same roster with the plank mark's column up, which puts every shape a
    // row's tag strip takes in one dump: a crewmate we planked and took back
    // aboard wears both tags, a planked pirate of another crew wears the mark
    // alone with the rank's columns left empty beside it, a crewmate we never
    // planked wears the rank alone, and the rest wear neither.
    states.push(state(
        "jobbers-plank-marks",
        "Jobbers, plank marks on pirates taken back aboard",
        |shell| {
            feed(shell, PILLAGE);
            crew_roster(shell);
            // This run planked Matefour, one of ours. Mateh sails with another
            // crew, so the mark is all the strip has to say about them.
            shell.chatlog.process_line(
                "[01:51:00] Playerone forced Mateh to walk the plank.",
            );
            for (at, name) in [("01:52:00", "Matefour"), ("01:53:00", "Mateh")]
            {
                shell.chatlog.process_line(&format!(
                    "[{at}] {name} has come aboard."
                ));
            }
            open(shell, AppId::Chatlog, true);
        },
    ));

    // Greenies among the crew: every other name on the roster is one, so the
    // green reads against the names beside it — in the Aboard pane and in the
    // ranking, where a greenie sits at the foot of every column.
    states.push(state(
        "jobbers-greenies",
        "Jobbers with greenies among the roster",
        |shell| {
            feed(shell, PILLAGE);
            for (i, name) in LONG_ROSTER.iter().enumerate() {
                shell.chatlog.process_line(&format!(
                    "[01:00:30] {name} has come aboard."
                ));
                if i % 2 == 0 {
                    cache_greenie(shell, name);
                } else {
                    cache_pirate(shell, name, i);
                }
            }
            open(shell, AppId::Chatlog, true);
        },
    ));

    // Vikings stands the leaderboard beside the panes, so its height is the
    // panes' and a long ranking is what it holds. The crew's Gunnery is known
    // here, which is also what the Vikings Statistics box breaks down.
    states.push(state(
        "jobbers-vikings-ranked",
        "Jobbers, Vikings leaderboard beside the panes, crew ranked",
        |shell| {
            feed(shell, PILLAGE);
            for (i, name) in LONG_ROSTER.iter().enumerate() {
                shell.chatlog.process_line(&format!(
                    "[01:00:30] {name} has come aboard."
                ));
                cache_pirate(shell, name, i);
            }
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = VoyageType::Vikings;
            shell.jobbers_ui.focus = JobberFocus::Leaderboard;
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
                button: 2, // Close, as a freshly-opened popup marks
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
        },
    ));
    states.push(state(
        "jobbers-popup-pirate-stats",
        "Jobbers, pirate stats popup with the stats fetched",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 2, // Close, as a freshly-opened popup marks
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
        },
    ));
    // A note read on the pirate it is about: the Note section sits above the
    // standings and the button offers to edit rather than to add. The note
    // wraps to the popup's width, which the note itself never widens.
    const NOTE: &str = "Fine gunner, but drifts off station when the fight \
                        runs long. Jobbed with us twice; planked once for \
                        taking the helm uninvited.";
    states.push(state(
        "jobbers-popup-pirate-note",
        "Jobbers, pirate stats popup with a note written down",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 0,
                offset: 0,
                view_h: 0,
                note: Some(NOTE.to_owned()),
            });
        },
    ));
    // The editor itself, as wide as the trophies popup beside it and at least
    // four lines tall, with the caret at the end of what has been written.
    states.push(state(
        "jobbers-popup-note",
        "Jobbers, the note editor over a pirate",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 0,
                offset: 0,
                view_h: 0,
                note: Some(NOTE.to_owned()),
            });
            let mut field = PromptField::new("Note", FieldKind::Paragraph);
            field.value = NOTE.to_owned();
            field.cursor = field.value.len();
            shell.jobbers_ui.note_popup = Some(NotePopup {
                name: "Matetwo".to_owned(),
                field,
                focus: NoteFocus::Text,
                offset: 0,
                wrap_w: 0,
                view_h: 0,
            });
        },
    ));
    // Nothing written yet: the box says what it is for, and keeps its four
    // lines so it does not open as a slit.
    states.push(state(
        "jobbers-popup-note-empty",
        "Jobbers, the note editor with nothing written yet",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 0,
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
            shell.jobbers_ui.note_popup = Some(NotePopup {
                name: "Matetwo".to_owned(),
                field: PromptField::new("Note", FieldKind::Paragraph),
                focus: NoteFocus::Text,
                offset: 0,
                wrap_w: 0,
                view_h: 0,
            });
        },
    ));
    // A note long enough to outgrow the box it opened at: the editor grew to
    // the screen and the text scrolls within it, with the keys on Save
    // after a step down off the last line.
    states.push(state(
        "jobbers-popup-note-long",
        "Jobbers, the note editor on a note longer than the box",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            let long = [
                "Fine gunner, but drifts off station when the fight runs long.",
                "Jobbed with us twice; planked once for taking the helm \
                 uninvited.",
                "Sails a sloop better than anything larger, and says so at \
                 every chance.",
                "Asked after the crew twice; told them we would think on it.",
                "Keeps a tidy hold and will restock without being asked, \
                 which is worth the rest of this.",
                "Will not take the guns on a frigate; says the reload is a \
                 carpenter's job and leaves it at that.",
                "Good in a fray, better in a rumble, and will swap station to \
                 be in one.",
                "Turned up for the Vikings run two hours late and bought the \
                 rum for it afterwards.",
                "Knows the Emerald archipelagos well enough to navigate \
                 without the chart, which is rarer than it sounds.",
                "Keeps asking after a sloop of their own; would job less if \
                 they had one.",
                "Owes the crew nothing and says so often enough that it is \
                 worth writing down.",
                "Bilges without being told when the hold is taking water, and \
                 says nothing about it afterwards.",
                "Would make an officer if they turned up when they said they \
                 would.",
            ]
            .join("\n");
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 0,
                offset: 0,
                view_h: 0,
                note: Some(long.clone()),
            });
            let mut field = PromptField::new("Note", FieldKind::Paragraph);
            field.value = long;
            shell.jobbers_ui.note_popup = Some(NotePopup {
                name: "Matetwo".to_owned(),
                field,
                focus: NoteFocus::Save,
                offset: 0,
                wrap_w: 0,
                view_h: 0,
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
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
            shell.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: "Matetwo".to_owned(),
                search: None,
                offset: 0,
                view_h: 10,
            });
        },
    ));
    states.push(state(
        "jobbers-popup-trophy-list",
        "Jobbers, trophy browser with the trophies fetched",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 1,
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
            shell.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: "Matetwo".to_owned(),
                search: None,
                offset: 0,
                view_h: 10,
            });
        },
    ));
    states.push(state(
        "jobbers-popup-trophy-filtered",
        "Jobbers, trophy browser with a filter typed into it",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 1,
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
            let mut search = PromptField::new("Search", FieldKind::Text);
            search.value = "Gun".to_owned();
            search.cursor = search.value.len();
            shell.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: "Matetwo".to_owned(),
                search: Some(search),
                offset: 0,
                view_h: 10,
            });
        },
    ));
    states.push(state(
        "jobbers-popup-trophy-nomatch",
        "Jobbers, trophy browser whose filter matches nothing",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            cache_pirate(shell, "Matetwo", 0);
            shell.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: "Matetwo".to_owned(),
                button: 1,
                offset: 0,
                view_h: 0,
                note: Some(String::new()),
            });
            let mut search = PromptField::new("Search", FieldKind::Text);
            search.value = "Nosuchtrophy".to_owned();
            search.cursor = search.value.len();
            shell.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: "Matetwo".to_owned(),
                search: Some(search),
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
    // The same graph asked for before a fight has been had: a run with no
    // wave fought yet, so the popup has nothing to plot.
    states.push(state(
        "jobbers-popup-per-fight-empty",
        "Jobbers, per-fight advantage graph with no fight yet",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            shell.jobbers_ui.voyage_type = VoyageType::CursedIsles;
            shell.jobbers_ui.per_fight_popup = Some(PerFightPopup {
                idx: 0,
                axis: AxisMode::Time,
            });
        },
    ));
    // A copied duty report that shares almost nobody with the vessel we think
    // we're on, which is what makes it ask rather than fold itself in. The
    // gallery hands the log straight to the parser, so the clipboard watcher
    // that would normally deliver the report is stood in for here.
    states.push(state(
        "jobbers-popup-roster",
        "Jobbers, duty report roster prompt",
        |shell| {
            feed(shell, PILLAGE);
            open(shell, AppId::Chatlog, true);
            let rated: Vec<String> = [
                "Matetwo",
                "Strangerone",
                "Strangertwo",
                "Strangerthree",
                "Strangerfour",
                "Strangerfive",
            ]
            .iter()
            .map(|n| format!("{n:?}:{{\"performance\":3}}"))
            .collect();
            let text = format!("{{\"sail\":{{{}}}}}", rated.join(","));
            let report =
                rusty_quartermaster::duty::parse(&text).expect("report");
            // the copy's time never reaches the screen, and the gallery has
            // no persistence file for it to be written to
            shell.take_duty_report(&report, chrono::Utc::now());
            shell.surface_roster_import();
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
    // Consumption is a Profits stock delta, so the section only appears for a
    // run whose hold came back lighter. The hold carried swill it never drank,
    // which is why no swill row shows.
    states.push(state(
        "voyage-consumption",
        "Voyage Statistics on a pillage that emptied its hold",
        |shell| {
            feed(shell, PILLAGE);
            // (commodity id, restock, stock): balls, grog, fine rum, spice.
            for (id, restock, stock) in [
                (6, "200", "50"),
                (7, "100", "40"),
                (8, "20", "5"),
                (9, "30", "12"),
            ] {
                let mut used = InventoryRow::new(id);
                used.restock = restock.to_owned();
                used.stock = stock.to_owned();
                shell.profits.rows.push(used);
            }
            open(shell, AppId::Voyage, true);
            // The window follows the focus, so name the field rather than
            // counting rows down to it.
            shell.voyage_ui.pending_focus_key = Some("Rum spice".to_owned());
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
        "Voyage, save prompt as it opens",
        |shell| {
            saveable_run(shell);
            shell.voyage_ui.prompt = Some(SavePrompt::default());
        },
    ));
    // Declining the engagements takes the two parts of a fight's record with
    // it: they dim, and the focus can no longer reach them.
    states.push(state(
        "voyage-popup-save-partial",
        "Voyage, save prompt with parts declined",
        |shell| {
            saveable_run(shell);
            let mut prompt = SavePrompt {
                focus: SaveFocus::Part(0),
                ..SavePrompt::default()
            };
            prompt.toggle(0); // engagements off; damage + melee go inert
            prompt.focus = SaveFocus::Part(4);
            prompt.toggle(4); // pillaged goods off
            shell.voyage_ui.prompt = Some(prompt);
        },
    ));
    // The goods are the one part of the prompt with no ceiling. A hold this
    // full outgrows an 80x24 page, which is what puts a bar inside the popup;
    // the same run fits whole at 120x40, where no bar is drawn. The second
    // state is the window run to the end of the list.
    for (slug, description, end) in [
        (
            "voyage-popup-save-many-goods",
            "Voyage, save prompt over a holdful of goods",
            false,
        ),
        (
            "voyage-popup-save-scrolled",
            "Voyage, save prompt scrolled to the last of the goods",
            true,
        ),
    ] {
        states.push(state(slug, description, move |shell| {
            saveable_run(shell);
            if let Some(voy) = shell.chatlog.current_pillage_voyage_mut() {
                voy.booty_goods = Some(
                    [
                        ("Sugar cane", 310),
                        ("Hemp", 120),
                        ("Iron", 95),
                        ("Wood", 240),
                        ("Stone", 60),
                        ("Hemp oil", 35),
                        ("Varnish", 18),
                        ("Lacquer", 12),
                        ("Kraken's ink", 4),
                        ("Broadcloth", 55),
                        ("Cowhide", 70),
                        ("Dye", 25),
                        ("Madder", 16),
                        ("Indigo", 9),
                        ("Lobelia", 7),
                    ]
                    .into_iter()
                    .map(|(name, qty)| (name.to_owned(), qty))
                    .collect(),
                );
            }
            shell.voyage_ui.prompt = Some(SavePrompt {
                // The render clamps the far end, so any offset past the list
                // lands on its last screenful.
                pan: end.then_some(u16::MAX),
                ..SavePrompt::default()
            });
        }));
    }
}

/// A ported pillage with something to show under every part of the save prompt:
/// fights carrying a melee timeline and a damage snapshot, a hold that came
/// back lighter, and a divvy that won goods.
///
/// The gallery hands the log straight to the parser, so the app steps that
/// normally run beside it — pinning the Damage calculator onto a resolved
/// fight, freezing the booty at the divvy — are stood in for here.
fn saveable_run(shell: &mut AppShell) {
    // (commodity id, restock, stock): the consumables the hold spent.
    for (id, restock, stock) in [
        (6, "200", "50"),
        (7, "100", "40"),
        (8, "20", "5"),
        (9, "30", "12"),
    ] {
        let mut row = InventoryRow::new(id);
        row.restock = restock.to_owned();
        row.stock = stock.to_owned();
        shell.profits.rows.push(row);
    }
    feed(shell, PILLAGE);
    open(shell, AppId::Voyage, true);
    if let Some(voy) = shell.chatlog.current_pillage_voyage_mut() {
        // Two of the three fights were tracked in the calculator; the third
        // went unrecorded, as one left to itself does.
        for battle in voy.battles.iter_mut().take(2) {
            battle.recorded = true;
            battle.snapshot = Some(BattleSnapshot {
                our_ship: 0,
                foe_ship: 0,
                our_hits: [3, 1],
                foe_hits: [8, 2],
                rams: 1,
                our_pirates: 5,
            });
        }
        voy.booty_chest = Some(1_200);
        voy.booty_goods = Some(vec![
            ("Iron".to_owned(), 30),
            ("Hemp".to_owned(), 12),
        ]);
    }
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
        "map-island-colony",
        "Map, cursor on a capital with its island info fetched",
        |shell| {
            *shell = map_shell();
            cursor_on_island(shell, |info, _, isle| {
                (info.governor.is_some()
                    && isle.status == bare::Status::Capital)
                    .then_some(0)
            });
        },
    ));
    states.push(state(
        "map-island-exports",
        "Map, cursor on the colony yoweb says the most about",
        |shell| {
            *shell = map_shell();
            cursor_on_island(shell, |info, _, _| {
                info.governor.is_some().then_some(info.exports.len())
            });
        },
    ));
    states.push(state(
        "map-island-partial",
        "Map, cursor on an island yoweb names no governor for",
        |shell| {
            *shell = map_shell();
            cursor_on_island(shell, |info, _, _| {
                info.governor.is_none().then_some(info.exports.len())
            });
        },
    ));
    states.push(state(
        "map-island-long-archipelago",
        "Map, cursor where the archipelago wraps the column's head",
        |shell| {
            *shell = map_shell();
            cursor_on_island(shell, |_, arch, _| {
                Some(arch.name.chars().count())
            });
        },
    ));
    states.push(state(
        "map-island-scrolled",
        "Map, Island column scrolled to the end of what it says",
        |shell| {
            *shell = map_shell();
            cursor_on_island(shell, |info, _, _| {
                info.governor.is_some().then_some(info.exports.len())
            });
            // past the end: the render clamps it to the last line
            shell.map.info_scroll = usize::MAX;
        },
    ));
    states.push(state(
        "map-island-fetching",
        "Map waiting on the island list",
        |shell| {
            *shell = map_shell();
            shell.islands = None;
            shell.island_list_wanted = true;
        },
    ));
    states.push(state(
        "map-memorized",
        "Map with memorized league points tallied for a pirate",
        |shell| {
            *shell = map_shell();
            shell.map.pirate = Some(ME.to_owned());
            // An island and everything within two leagues of it, so the
            // routes memorized out of it are on show and not only
            // the marks at their ends. The island is one whose own routes are
            // of both kinds, so a bought chart, a booty chart and a memorized
            // route are all on the one screen.
            let map = dump_ocean();
            let headings = [
                Heading::E,
                Heading::W,
                Heading::Ne,
                Heading::Nw,
                Heading::Se,
                Heading::Sw,
            ];
            let island = map
                .islands
                .iter()
                .find(|i| {
                    let kinds = headings
                        .iter()
                        .filter_map(|h| map.neighbour(i.at(), *h))
                        .map(|(_, league)| league.chart)
                        .collect::<Vec<Chart>>();
                    kinds.contains(&Chart::Sold)
                        && kinds.contains(&Chart::Unsold)
                })
                .unwrap_or_else(|| map.islands.first().expect("an island"));
            shell.map.cursor = Some(island.at());
            let mut known = vec![island.at()];
            for _ in 0 .. 2 {
                for from in known.clone() {
                    for heading in headings {
                        if let Some((to, _)) = map.neighbour(from, heading)
                            && !known.contains(&to)
                        {
                            known.push(to);
                        }
                    }
                }
            }
            shell.map.memorized.extend(known);
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
        "Map search completing an island's name",
        |shell| {
            *shell = map_shell();
            let island = dump_ocean().islands.first().expect("an island");
            let mut field = PromptField::new("Search", FieldKind::Text);
            // The first few letters, so the rest of the name is the completion
            // the row draws for them.
            field.value = island.name.chars().take(5).collect();
            field.cursor = field.value.len();
            shell.map.search = Some(field);
        },
    ));
    states.push(state(
        "map-search-miss",
        "Map search box with no match",
        |shell| {
            *shell = map_shell();
            let mut field = PromptField::new("Search", FieldKind::Text);
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
    states.push(state(
        "map-help-scrolled",
        "Map help popup read to the end, in a window too short for it",
        |shell| {
            *shell = map_shell();
            shell.map.help = true;
            // past the end: the render clamps it to the last line
            shell.map.help_scroll = usize::MAX;
        },
    ));
    archipelago_states(states);
}

/// One state per archipelago, the view centred on where its name is drawn, so
/// every caption on an ocean can be read where it landed. The German and
/// Spanish oceans are left out: their maps are the English ones' twins, and
/// the placing is what these states are for.
fn archipelago_states(states: &mut Vec<State>) {
    const OCEANS: [Ocean; 5] = [
        Ocean::Emerald,
        Ocean::Meridian,
        Ocean::Cerulean,
        Ocean::Obsidian,
        Ocean::Ice,
    ];
    for ocean in OCEANS {
        let map = Map::for_ocean(ocean.name())
            .unwrap_or_else(|| panic!("{} map", ocean.name()));
        for (arch, point) in caption_points(map) {
            // a slug and a description outlive the gallery run either way, so
            // leaking the two strings costs nothing and keeps `State` plain
            let slug: &'static str = Box::leak(
                format!(
                    "map-arch-{}-{}",
                    slug_of(ocean.name()),
                    slug_of(arch)
                )
                .into_boxed_str(),
            );
            let description: &'static str = Box::leak(
                format!(
                    "Map of {}, centred on {arch}",
                    ocean.name()
                )
                .into_boxed_str(),
            );
            states.push(state(slug, description, move |shell| {
                *shell = map_shell_of(ocean);
                shell.map.cursor = Some(point);
            }));
        }
    }
}

/// A name as it goes into a file name: lowercase, one dash for each run of
/// anything else.
fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
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

            // The gallery must stay offline: it renders states for inspection
            // and has no business touching the market or yoweb. `loading` is
            // raised the moment a fetch is asked for, so it catches a state
            // that starts one however indirectly.
            if shell.loading {
                return Err(format!(
                    "state {:?} asked for a network fetch",
                    state.slug,
                )
                .into());
            }

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

    // The screens outside the page model, each at its own size. Their style
    // reports go in one sheet of their own, since they share no size with the
    // pages or with each other.
    let mut styles = String::new();
    for screen_state in screens() {
        let (width, height) = screen_state.size;
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| (screen_state.draw)(frame))?;
        let stem = format!("{width}x{height}-{}", screen_state.slug);
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
            screen_state.description
        ));
        styles.push_str(&format!(
            "=== {stem}.txt -- {}\n",
            screen_state.description
        ));
        styles.push_str(&style_report(buffer));
        styles.push('\n');
        sheet.push_str(&format!(
            "<section id=\"{stem}\"><h2>{stem}</h2><p>{}</p><img \
             src=\"{stem}.svg\" alt=\"{stem}\"></section>\n",
            escape_xml(screen_state.description),
        ));
        nav.push_str(&format!(
            "<li><a href=\"#{stem}\">{}</a></li>\n",
            escape_xml(screen_state.slug),
        ));
        written += 1;
    }
    fs::write(
        args.out.join("STYLES-screens.txt"),
        styles,
    )?;

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
