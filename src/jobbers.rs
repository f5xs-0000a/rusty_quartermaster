//! The "Vessel's Jobbers" page: pick a vessel on the left, see its jobbers'
//! skills and activity on the right.
//!
//! Two pieces of state feed this page:
//!   * [`crate::chatlog::GameState`] — per-vessel crew/greedy/plank tallies from
//!     the chat log (wiped on relog).
//!   * [`PirateCache`] — yoweb stats fetched per pirate name. This is *global*:
//!     a pirate's stats are intrinsic to them, not to any vessel, and they don't
//!     change on relog, so the cache is never wiped.
//!
//! [`JobbersUi`] holds the view state (selected vessel + per-list scroll).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph};

use crate::chatlog::GameState;
use crate::clickmap::{ClickRegion, ClickTarget};
use crate::pirate::{self, Experience, Pirate, Skill, Standing};

const TOP_N: usize = 4;
/// Width of the `EEE/SSS` experience/standing code.
const CODE_LEN: usize = 7;
/// Spaces between a jobber's name and their code in the Top Jobbers panel.
const NAME_CODE_GAP: usize = 2;
/// Spaces between Top Jobbers skill columns.
const COLUMN_GAP: u16 = 3;

/// Warning shown at the bottom of the app while the Unpoison button is focused.
pub const UNPOISON_TOOLTIP: [&str; 2] = [
    "You left the ship and you might have missed logs that were important.",
    "Press Enter to ignore the warnings.",
];

/// Skill columns shown in the top panel, with their display headers.
const SKILL_COLUMNS: &[(Skill, &str)] = &[
    (Skill::Gunning, "Gunnery"),
    (Skill::Navigating, "Navigation"),
    (Skill::BattleNavigation, "B. Navigation"),
];

// ---------------------------------------------------------------------------
// Global pirate stat cache
// ---------------------------------------------------------------------------

/// Yoweb stats fetched per pirate, keyed by normalized name, plus bookkeeping
/// so the background fetcher never requests the same pirate twice.
#[derive(Default)]
pub struct PirateCache {
    /// Successfully fetched pirates, keyed by normalized name.
    pub fetched: HashMap<String, Pirate>,
    /// Names already queued/in-flight/done — dedups the fetch worklist.
    pub requested: HashSet<String>,
    /// Fetches currently in flight, for throttling.
    pub in_flight: usize,
}

impl PirateCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a pirate by (un-normalized) name.
    pub fn get(&self, name: &str) -> Option<&Pirate> {
        pirate::normalize_name(name)
            .ok()
            .and_then(|n| self.fetched.get(&n))
    }
}

// ---------------------------------------------------------------------------
// View state
// ---------------------------------------------------------------------------

/// Which widget on the page has keyboard focus. The Unpoison button is only
/// reachable when the selected vessel is actually poisoned.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum JobberFocus {
    #[default]
    Vessels,
    Unpoison,
    Aboard,
    Greedy,
    Planked,
}

#[derive(Default)]
pub struct JobbersUi {
    /// Vessel currently shown; resolved against the live vessel set each frame.
    pub selected: Option<Arc<str>>,
    pub focus: JobberFocus,
    pub aboard_offset: usize,
    pub greedy_offset: usize,
    pub planked_offset: usize,
}

/// Bottom-bar tooltip lines for the current focus (empty when nothing to say).
pub fn tooltip(state: &GameState, ui: &JobbersUi) -> Vec<&'static str> {
    match ui.focus {
        JobberFocus::Unpoison => {
            let poisoned = ui
                .selected
                .as_ref()
                .and_then(|k| state.vessels.get(k))
                .is_some_and(|v| v.poisoned);
            if poisoned {
                UNPOISON_TOOLTIP.to_vec()
            } else {
                Vec::new()
            }
        }
        JobberFocus::Aboard | JobberFocus::Greedy | JobberFocus::Planked => {
            vec!["Shift+Up/Down: scroll this list."]
        }
        JobberFocus::Vessels => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Styling helpers
// ---------------------------------------------------------------------------

/// Three-letter standing code (e.g. `Mas` for Master).
fn standing_abbr(s: Standing) -> &'static str {
    match s {
        Standing::Able => "Abl",
        Standing::Proficient => "Pro",
        Standing::Distinguished => "Dis",
        Standing::Respected => "Res",
        Standing::Master => "Mas",
        Standing::Renowned => "Ren",
        Standing::GrandMaster => "Gra",
        Standing::Legendary => "Leg",
        Standing::Ultimate => "Ult",
    }
}

/// Three-letter experience code (e.g. `Bro` for Broad).
fn experience_abbr(e: Experience) -> &'static str {
    match e {
        Experience::Novice => "Nov",
        Experience::Neophyte => "Neo",
        Experience::Apprentice => "App",
        Experience::Narrow => "Nar",
        Experience::Broad => "Bro",
        Experience::Solid => "Sol",
        Experience::Weighty => "Wei",
        Experience::Expert => "Exp",
        Experience::Paragon => "Par",
        Experience::Illustrious => "Ill",
        Experience::Sublime => "Sub",
        Experience::Revered => "Rev",
        Experience::Exalted => "Exa",
        Experience::Transcendent => "Tra",
    }
}

/// Standing emphasis: bold from Proficient, bold+italic from Renowned.
fn standing_style(s: Standing) -> Style {
    if s >= Standing::Renowned {
        Style::default().bold().italic()
    } else if s >= Standing::Proficient {
        Style::default().bold()
    } else {
        Style::default()
    }
}

/// Experience emphasis: bold from Broad, bold+italic from Sublime.
fn experience_style(e: Experience) -> Style {
    if e >= Experience::Sublime {
        Style::default().bold().italic()
    } else if e >= Experience::Broad {
        Style::default().bold()
    } else {
        Style::default()
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Border for a focusable widget: bright only when the page is focused *and*
/// this widget is the active one.
fn box_border(page_focused: bool, active: bool) -> Style {
    if page_focused && active {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Truncate to `max` columns, marking elision with `…`.
fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Render entry point
// ---------------------------------------------------------------------------

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    cache: &PirateCache,
    ui: &mut JobbersUi,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    if !state.attached {
        let msg = Paragraph::new(
            "No chat log attached. Pass --chat-log <PATH> (and --user <NAME>) to monitor a game log.",
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(focused))
                .title("─── Vessel's Jobbers "),
        );
        frame.render_widget(msg, area);
        return;
    }

    // Resolve the selected vessel against the current (latest-first) ordering.
    let ordered = state.vessels_by_recency();
    let selected: Option<Arc<str>> = ui
        .selected
        .as_ref()
        .filter(|k| state.vessels.contains_key(*k))
        .cloned()
        .or_else(|| ordered.first().cloned());
    ui.selected = selected.clone();

    // The Unpoison button is only focusable while the vessel is poisoned.
    let sel_poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    if !sel_poisoned && ui.focus == JobberFocus::Unpoison {
        ui.focus = JobberFocus::Vessels;
    }

    // Rank currently-aboard jobbers per skill (reused for the panel + sizing).
    let aboard_set = selected.as_ref().map(|k| state.aboard(k)).unwrap_or_default();
    let ranked: Vec<Vec<(String, Experience, Standing)>> = SKILL_COLUMNS
        .iter()
        .map(|(skill, _)| rank_for_skill(&aboard_set, cache, skill))
        .collect();

    // The Top Jobbers panel is the widest piece, so its natural width sets the
    // width of the whole Jobbers block; the bottom row spans that same width.
    let max_name = ordered.iter().map(|k| k.chars().count()).max().unwrap_or(0);
    // chrome = borders(2) + padding(2) + highlight "> "(2); also fit "Unpoison".
    let vessel_w = ((max_name + 6).max(12)).min(28) as u16;
    let block_w = top_panel_width(&ranked).max(vessel_w + 20);

    // Total block height = Top Jobbers + the taller of the two bottom columns.
    let top_rows = ranked.iter().map(Vec::len).max().unwrap_or(0);
    let top_h = top_rows as u16 + 3;
    let vessel = selected.as_ref().and_then(|k| state.vessels.get(k));
    let list_h = |n: usize| (n as u16 + 2).max(3);
    let left_h = (ordered.len() as u16 + 2).max(3) + 3; // vessels box + unpoison
    let lists_h = list_h(aboard_set.len())
        + list_h(vessel.map_or(0, |v| v.greedy_by_pirate.len()))
        + list_h(vessel.map_or(0, |v| v.planked_by_us.len()));
    let block_h = top_h + left_h.max(lists_h);

    // Center the whole block in the content area (matches the Damage/Profit
    // calculators), bounded by the available space.
    let block_w = block_w.min(area.width);
    let block_h = block_h.min(area.height);
    let bx = area.x + area.width.saturating_sub(block_w) / 2;
    let by = area.y + area.height.saturating_sub(block_h) / 2;
    let block = Rect::new(bx, by, block_w, block_h);

    // Vertical: Top Jobbers (content height) over the bottom region.
    let top_h = top_h.min(block.height);
    let rows = Layout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).split(block);
    render_top_panel(frame, rows[0], &ranked, focused);

    // Bottom: left column (vessels + unpoison) beside the stacked lists, which
    // widen to fill the rest of the block.
    let bottom = Layout::horizontal([Constraint::Length(vessel_w), Constraint::Min(0)]).split(rows[1]);

    render_left_column(frame, bottom[0], state, &ordered, &selected, ui.focus, focused, regions);
    render_lists_column(frame, bottom[1], state, cache, selected.as_ref(), &aboard_set, ui, focused, regions);
}

// ---------------------------------------------------------------------------
// Left column: vessel selector + unpoison button (both content-sized)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_left_column(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    ordered: &[Arc<str>],
    selected: &Option<Arc<str>>,
    focus: JobberFocus,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    // Vessels list as tall as its content; Unpoison directly below; empty slack
    // beneath. Leave room for the 3-tall button.
    let vessels_h = (ordered.len() as u16 + 2).clamp(3, area.height.saturating_sub(3).max(3));
    let chunks = Layout::vertical([
        Constraint::Length(vessels_h),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(area);

    // -- Vessel list --
    // inner content width = width - borders(2) - padding(2) - highlight "> "(2).
    let name_w = chunks[0].width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = ordered
        .iter()
        .map(|key| {
            let poisoned = state.vessels.get(key).is_some_and(|v| v.poisoned);
            let item = ListItem::new(truncate(key, name_w));
            if poisoned {
                item.style(Style::default().fg(Color::Red))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(box_border(focused, focus == JobberFocus::Vessels))
                .padding(Padding::horizontal(1))
                .title("─── Vessels "),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let sel_idx = selected
        .as_ref()
        .and_then(|s| ordered.iter().position(|k| k == s));
    let mut list_state = ListState::default().with_selected(sel_idx);
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    // Click regions for each visible vessel row.
    let inner_x = chunks[0].x + 1;
    let inner_y = chunks[0].y + 1;
    let inner_w = chunks[0].width.saturating_sub(2);
    let inner_h = chunks[0].height.saturating_sub(2);
    for i in 0..ordered.len().min(inner_h as usize) {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberVessel(i),
        });
    }

    // -- Unpoison button --
    let poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    let btn_style = if !poisoned {
        Style::default().fg(Color::DarkGray)
    } else if focus == JobberFocus::Unpoison {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().fg(Color::Red).bold()
    };
    let button = Paragraph::new("Unpoison").centered().style(btn_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(box_border(focused, focus == JobberFocus::Unpoison)),
    );
    frame.render_widget(button, chunks[1]);
    regions.push(ClickRegion {
        rect: chunks[1],
        target: ClickTarget::JobberUnpoison,
    });
}

// ---------------------------------------------------------------------------
// Lists column: Aboard / Greedy / Planked, stacked, each content-sized
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_lists_column(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    cache: &PirateCache,
    selected: Option<&Arc<str>>,
    aboard_set: &HashSet<String>,
    ui: &mut JobbersUi,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let Some(key) = selected else {
        let h = 3.min(area.height);
        let msg = Paragraph::new("No vessels boarded yet this session.").block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(focused))
                .title("─── Jobbers "),
        );
        frame.render_widget(msg, Rect::new(area.x, area.y, area.width, h));
        return;
    };

    // The player's crew, used to bold same-crew jobbers.
    let my_crew: Option<String> = state
        .player_name
        .as_deref()
        .and_then(|me| cache.get(me))
        .map(|p| p.crew_name.clone())
        .filter(|c| !c.is_empty());

    let name_style = |name: &str| -> Style {
        let is_player = state
            .player_name
            .as_deref()
            .is_some_and(|me| name.eq_ignore_ascii_case(me));
        if is_player {
            return Style::default().bold().italic();
        }
        let is_crewmate = match (&my_crew, cache.get(name)) {
            (Some(mine), Some(p)) => p.crew_name.eq_ignore_ascii_case(mine),
            _ => false,
        };
        if is_crewmate {
            Style::default().bold()
        } else {
            Style::default()
        }
    };

    let vessel = state.vessels.get(key);
    let mut y = area.y;

    // -- Aboard (alphabetical) --
    let mut aboard: Vec<&String> = aboard_set.iter().collect();
    aboard.sort_unstable();
    let aboard_lines: Vec<Line> = aboard
        .iter()
        .map(|n| Line::from(Span::styled((*n).clone(), name_style(n))))
        .collect();
    place_list_vertical(
        frame, area, &mut y, "─── Aboard ", aboard_lines, &mut ui.aboard_offset,
        box_border(focused, ui.focus == JobberFocus::Aboard),
        ClickTarget::JobberAboardList, regions,
    );

    // -- Greedy strikes (by total desc, then alphabetical), shown as
    //    "(before this battle) + (this/last battle)". --
    let mut greedy: Vec<(&String, u32, u32)> = vessel
        .map(|v| {
            v.greedy_by_pirate
                .iter()
                .map(|(n, total)| {
                    let current = v.greedy_current.get(n).copied().unwrap_or(0);
                    (n, *total, current)
                })
                .collect()
        })
        .unwrap_or_default();
    greedy.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    // The box fills the column, so right-align the values to its inner width
    // (column width minus borders(2) and horizontal padding(2)).
    let content_w = name_col_plus_value(&greedy);
    let greedy_w = (area.width.saturating_sub(4) as usize).max(content_w);
    let greedy_lines: Vec<Line> = greedy
        .iter()
        .map(|(name, total, current)| {
            greedy_line(name, *total, *current, greedy_w, name_style(name))
        })
        .collect();
    place_list_vertical(
        frame, area, &mut y, "─── Greedy ", greedy_lines, &mut ui.greedy_offset,
        box_border(focused, ui.focus == JobberFocus::Greedy),
        ClickTarget::JobberGreedyList, regions,
    );

    // -- Planked by us (alphabetical, unformatted) --
    let mut planked: Vec<String> = vessel.map(|v| v.planked_by_us.clone()).unwrap_or_default();
    planked.sort_unstable();
    let planked_lines: Vec<Line> = planked.iter().map(|n| Line::from(n.clone())).collect();
    place_list_vertical(
        frame, area, &mut y, "─── Planked ", planked_lines, &mut ui.planked_offset,
        box_border(focused, ui.focus == JobberFocus::Planked),
        ClickTarget::JobberPlankedList, regions,
    );
}

/// Render a content-sized list box at `*y`, then advance `*y` past it so the
/// next box stacks directly below. Width and height are bounded by `region`.
#[allow(clippy::too_many_arguments)]
fn place_list_vertical(
    frame: &mut Frame,
    region: Rect,
    y: &mut u16,
    title: &str,
    lines: Vec<Line>,
    offset: &mut usize,
    border: Style,
    target: ClickTarget,
    regions: &mut Vec<ClickRegion>,
) {
    let remaining_h = (region.y + region.height).saturating_sub(*y);
    if remaining_h == 0 {
        return;
    }
    // Width fills the column (set from the Top Jobbers panel back in `render`);
    // height = rows + borders(2), bounded by the available space.
    let box_h = (lines.len() as u16 + 2).clamp(3, remaining_h);
    let area = Rect::new(region.x, *y, region.width, box_h);
    render_scroll_list(frame, area, title, lines, offset, border, target, regions);
    *y = y.saturating_add(box_h);
}

/// Minimum width that keeps every greedy row's name and value from colliding:
/// widest name + 2-space gap + widest `before + current` value.
fn name_col_plus_value(greedy: &[(&String, u32, u32)]) -> usize {
    let name_col = greedy.iter().map(|(n, _, _)| n.chars().count()).max().unwrap_or(0);
    let val_col = greedy
        .iter()
        .map(|(_, t, c)| format!("{} + {}", t.saturating_sub(*c), c).len())
        .max()
        .unwrap_or(0);
    name_col + 2 + val_col
}

/// Build a greedy row: name left, `before + current` strikes right-aligned.
fn greedy_line(name: &str, total: u32, current: u32, width: usize, style: Style) -> Line<'static> {
    let before = total.saturating_sub(current);
    let value = format!("{before} + {current}");
    let name_max = width.saturating_sub(value.len() + 1);
    let nm = truncate(name, name_max);
    let pad = width.saturating_sub(nm.chars().count() + value.len());
    Line::from(vec![
        Span::styled(nm, style),
        Span::raw(" ".repeat(pad)),
        Span::raw(value),
    ])
}

/// Top-N currently-aboard jobbers for a skill, by standing then experience then
/// name. Pirates without fetched stats are skipped.
fn rank_for_skill(
    aboard: &HashSet<String>,
    cache: &PirateCache,
    skill: &Skill,
) -> Vec<(String, Experience, Standing)> {
    let mut ranked: Vec<(String, Experience, Standing)> = aboard
        .iter()
        .filter_map(|n| {
            cache
                .get(n)
                .and_then(|p| p.skills.get(skill))
                .map(|r| (n.clone(), r.experience, r.standing))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then(b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(TOP_N);
    ranked
}

/// Per-skill column widths for the Top Jobbers panel: each is the wider of its
/// header and its widest `name + gap + code` row.
fn top_panel_col_widths(ranked: &[Vec<(String, Experience, Standing)>]) -> Vec<u16> {
    SKILL_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, (_, header))| {
            let name_w = ranked[i]
                .iter()
                .map(|(n, _, _)| n.chars().count())
                .max()
                .unwrap_or(0);
            (name_w + NAME_CODE_GAP + CODE_LEN).max(header.chars().count()) as u16
        })
        .collect()
}

/// The Top Jobbers panel's natural outer width: columns + gaps + padding +
/// borders, with a floor so the title stays readable.
fn top_panel_width(ranked: &[Vec<(String, Experience, Standing)>]) -> u16 {
    let inner_w = top_panel_col_widths(ranked).iter().sum::<u16>() + 2 * COLUMN_GAP;
    (inner_w + 4).max(18)
}

fn render_top_panel(
    frame: &mut Frame,
    region: Rect,
    ranked: &[Vec<(String, Experience, Standing)>],
    focused: bool,
) {
    // Size each column to its content: max(header, widest name + gap + code).
    let col_w: Vec<u16> = top_panel_col_widths(ranked);

    // The panel fills its region: the region's width was derived from this
    // panel's natural width back in `render`, so it already hugs the content.
    let area = region;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::horizontal(1))
        .title("─── Top Jobbers ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::horizontal([
        Constraint::Length(col_w[0]),
        Constraint::Length(COLUMN_GAP),
        Constraint::Length(col_w[1]),
        Constraint::Length(COLUMN_GAP),
        Constraint::Length(col_w[2]),
        Constraint::Min(0),
    ])
    .split(inner);
    let col_areas = [cols[0], cols[2], cols[4]];

    for (ci, (_, header)) in SKILL_COLUMNS.iter().enumerate() {
        let name_w = (col_w[ci] as usize).saturating_sub(NAME_CODE_GAP + CODE_LEN);

        let mut lines: Vec<Line> = Vec::with_capacity(ranked[ci].len() + 1);
        lines.push(Line::from(Span::styled(
            *header,
            Style::default().bold().underlined(),
        )));
        for (name, exp, standing) in &ranked[ci] {
            lines.push(Line::from(vec![
                Span::raw(format!("{:<name_w$}", truncate(name, name_w))),
                Span::raw(" ".repeat(NAME_CODE_GAP)),
                Span::styled(experience_abbr(*exp), experience_style(*exp)),
                Span::raw("/"),
                Span::styled(standing_abbr(*standing), standing_style(*standing)),
            ]));
        }

        frame.render_widget(Paragraph::new(lines), col_areas[ci]);
    }
}

/// Render a vertically-scrollable list inside a bordered box, registering the
/// box as a scroll target. `offset` is clamped to the content here.
#[allow(clippy::too_many_arguments)]
fn render_scroll_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line>,
    offset: &mut usize,
    border: Style,
    target: ClickTarget,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = inner.height as usize;
    let max_off = lines.len().saturating_sub(rows);
    if *offset > max_off {
        *offset = max_off;
    }

    let visible: Vec<Line> = lines.into_iter().skip(*offset).take(rows).collect();
    frame.render_widget(Paragraph::new(visible), inner);

    regions.push(ClickRegion { rect: area, target });
}
