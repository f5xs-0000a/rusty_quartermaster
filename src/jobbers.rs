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

use chrono::{DateTime, Duration, Utc};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::chatlog::GameState;
use crate::clickmap::{ClickRegion, ClickTarget};
use crate::pirate::{
    self, BasicInfo, CachedPirate, Experience, FetchPlan, PirateUpdate, Skill, Standing,
};
use crate::ships::{Ship, SHIPS};

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
/// so the background fetcher never requests the same pirate twice and knows when
/// a cached entry has gone stale.
#[derive(Default)]
pub struct PirateCache {
    /// Successfully fetched pirates, keyed by normalized name. Each entry
    /// carries its basic profile, trophies, and per-part fetch timestamps.
    pub fetched: HashMap<String, CachedPirate>,
    /// Normalized names currently being fetched — dedups the worklist and, via
    /// its length, bounds in-flight concurrency.
    pub in_flight: HashSet<String>,
    /// Normalized names confirmed not to exist on yoweb this session (the page
    /// loaded but named no pirate). Never auto-fetched again; a force-requery
    /// clears the name so it can be retried.
    pub gone: HashSet<String>,
    /// Normalized names queued for a full re-query regardless of staleness, set
    /// by the force-refresh UI. Drained as each is dispatched.
    pub forced: HashSet<String>,
}

impl PirateCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a pirate's basic profile by (un-normalized) name.
    pub fn get(&self, name: &str) -> Option<&BasicInfo> {
        self.get_cached(name).map(|c| &c.basic)
    }

    /// Look up a cached pirate, including trophies and fetch timestamps.
    pub fn get_cached(&self, name: &str) -> Option<&CachedPirate> {
        pirate::normalize_name(name)
            .ok()
            .and_then(|n| self.fetched.get(&n))
    }

    /// Queue `name` for a full re-query on the next worklist pass, ignoring
    /// staleness and any prior not-found result. Used by the force-refresh UI
    /// (still to be wired up).
    #[allow(dead_code)]
    pub fn force_requery(&mut self, name: &str) {
        if let Ok(norm) = pirate::normalize_name(name) {
            self.gone.remove(&norm);
            self.forced.insert(norm);
        }
    }

    /// Decide which yoweb pages to (re)fetch for an already-normalized `norm`,
    /// or `None` if nothing is due. A forced or never-seen pirate fetches both
    /// pages; otherwise each page is fetched only once its TTL has elapsed.
    pub fn fetch_plan(
        &self,
        norm: &str,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<FetchPlan> {
        if self.forced.contains(norm) {
            return Some(FetchPlan { basic: true, trophies: true });
        }
        match self.fetched.get(norm) {
            None => Some(FetchPlan { basic: true, trophies: true }),
            Some(c) => {
                let basic = now.signed_duration_since(c.basic_fetched_at) >= basic_ttl;
                let trophies = now.signed_duration_since(c.trophies_fetched_at) >= trophy_ttl;
                (basic || trophies).then_some(FetchPlan { basic, trophies })
            }
        }
    }

    /// Fold a completed fetch for `norm` into the cache, swapping in whichever
    /// parts came back fresh, and clear the name's in-flight/forced bookkeeping.
    pub fn apply_update(&mut self, norm: String, update: PirateUpdate) {
        self.in_flight.remove(&norm);
        self.forced.remove(&norm);
        match update {
            PirateUpdate::Refreshed { basic, trophies } => match self.fetched.get_mut(&norm) {
                Some(entry) => {
                    if let Some((info, at)) = basic {
                        entry.basic = info;
                        entry.basic_fetched_at = at;
                    }
                    if let Some((t, at)) = trophies {
                        entry.trophies = t;
                        entry.trophies_fetched_at = at;
                    }
                }
                // A brand-new pirate always fetches the basic page first, so
                // `basic` is present here; trophies may lag behind, left stale so
                // the next pass retries them.
                None => {
                    if let Some((info, basic_at)) = basic {
                        let (trophies, trophies_at) = match trophies {
                            Some((t, at)) => (t, at),
                            None => (Default::default(), DateTime::<Utc>::MIN_UTC),
                        };
                        self.fetched.insert(
                            norm,
                            CachedPirate {
                                basic: info,
                                trophies,
                                basic_fetched_at: basic_at,
                                trophies_fetched_at: trophies_at,
                            },
                        );
                    }
                }
            },
            PirateUpdate::NotFound => {
                self.fetched.remove(&norm);
                self.gone.insert(norm);
            }
            // Transport/server error: keep whatever we have; the name stays
            // eligible for a later requery.
            PirateUpdate::Error(_) => {}
        }
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
    ShipType,
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
    /// Ship type chosen per vessel (index into [`SHIPS`]), keyed by vessel name.
    /// Lives here, not on the `Vessel`, so picks survive a relog state wipe.
    pub ship_types: HashMap<Arc<str>, usize>,
    /// When `Some`, the ship-type popup is open with this highlighted index;
    /// it applies to the currently-selected vessel.
    pub ship_popup: Option<usize>,
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
        JobberFocus::ShipType => vec!["Press Enter to pick this vessel's ship type."],
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
// Ship staffing
// ---------------------------------------------------------------------------

/// Staffing verdict for the selected vessel against the chosen ship.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Staffing {
    /// Room for more crew *and* mercenaries still available — hire some.
    Understaffed,
    /// More swabbies or pirates aboard than the ship can hold: wrong ship.
    Invalid,
}

impl Staffing {
    fn message(self) -> &'static str {
        match self {
            Staffing::Understaffed => "Understaffed. Hire jobbers.",
            Staffing::Invalid => "Invalid ship selected.",
        }
    }

    fn style(self) -> Style {
        match self {
            Staffing::Understaffed => Style::default().fg(Color::Yellow),
            Staffing::Invalid => Style::default().fg(Color::Red),
        }
    }
}

/// Compare the crew aboard against the chosen ship's capacity.
///
/// * `players` — named pirates aboard (the Aboard list).
/// * `swabbies` — the anonymous swabbie/mercenary tally aboard.
///
/// Overstaffed takes priority: exceeding the mercenary cap *or* the total
/// pirate cap means the wrong ship was probably picked. Otherwise it's
/// understaffed only while both there's room for more crew and the mercenary
/// cap hasn't been hit.
fn staffing(ship: &Ship, players: usize, swabbies: u32) -> Option<Staffing> {
    let total = players as u64 + swabbies as u64;
    if swabbies > ship.max_mercenaries as u32 || total > ship.max_pirates as u64 {
        Some(Staffing::Invalid)
    } else if swabbies < ship.max_mercenaries as u32 && total < ship.max_pirates as u64 {
        Some(Staffing::Understaffed)
    } else {
        None
    }
}

/// Word-wrap `text` to `width` columns, hard-breaking any single word longer
/// than the line so a narrow column never overflows.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for mut word in text.split_whitespace() {
        // A word that can't fit on its own line is chopped to width.
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            let consumed = head.len();
            lines.push(head);
            word = &word[consumed..];
        }
        if word.is_empty() {
            continue;
        }
        let need = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if need > width {
            lines.push(std::mem::take(&mut cur));
        } else if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
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

    // The tooltip (focus-dependent help) rides at the bottom of the block so it
    // shares the block's width rather than spanning the whole content area.
    let tooltip_lines = tooltip(state, ui);
    let tip_h = tooltip_lines.len() as u16;

    // Total block height = Top Jobbers + the taller of the two bottom columns,
    // plus the tooltip strip beneath them.
    let top_rows = ranked.iter().map(Vec::len).max().unwrap_or(0);
    let top_h = top_rows as u16 + 3;
    let vessel = selected.as_ref().and_then(|k| state.vessels.get(k));

    // Ship-type widget: the ship chosen for this vessel (if any) and the
    // staffing warning it implies. Wrapped here so the box can be sized to fit.
    let ship_idx = selected.as_ref().and_then(|k| ui.ship_types.get(k).copied());
    let players_aboard = aboard_set.len();
    let swabbies = vessel.map_or(0, |v| v.swabbies);
    let warn = ship_idx.and_then(|i| staffing(&SHIPS[i], players_aboard, swabbies));
    // Box has no padding, so inner text width = box width - borders(2).
    let warn_lines: Vec<String> = warn
        .map(|w| wrap_words(w.message(), vessel_w.saturating_sub(2) as usize))
        .unwrap_or_default();
    let ship_h = warn_lines.len() as u16 + 3; // ship-name row + warning + borders(2)

    let list_h = |n: usize| (n as u16 + 2).max(3);
    // Left column: vessels box + ship-type box + unpoison button.
    let left_h = (ordered.len() as u16 + 2).max(3) + ship_h + 3;
    // The Aboard box gains one extra row for the "and n swabbies" footer.
    let aboard_rows = aboard_set.len() + usize::from(vessel.is_some_and(|v| v.swabbies > 0));
    let lists_h = list_h(aboard_rows)
        + list_h(vessel.map_or(0, |v| v.greedy_by_pirate.len()))
        + list_h(vessel.map_or(0, |v| v.planked_by_us.len()));
    let block_h = top_h + left_h.max(lists_h) + tip_h;

    // Center the whole block in the content area (matches the Damage/Profit
    // calculators), bounded by the available space.
    let block_w = block_w.min(area.width);
    let block_h = block_h.min(area.height);
    let bx = area.x + area.width.saturating_sub(block_w) / 2;
    let by = area.y + area.height.saturating_sub(block_h) / 2;
    let block = Rect::new(bx, by, block_w, block_h);

    // Vertical: Top Jobbers (content height) over the bottom region, with the
    // tooltip strip beneath them.
    let top_h = top_h.min(block.height);
    let rows = Layout::vertical([
        Constraint::Length(top_h),
        Constraint::Min(0),
        Constraint::Length(tip_h),
    ])
    .split(block);
    render_top_panel(frame, rows[0], &ranked, focused);

    // Bottom: left column (vessels + unpoison) beside the stacked lists, which
    // widen to fill the rest of the block.
    let bottom = Layout::horizontal([Constraint::Length(vessel_w), Constraint::Min(0)]).split(rows[1]);

    render_left_column(
        frame, bottom[0], state, &ordered, &selected, ui.focus, focused,
        ship_idx, warn, &warn_lines, ship_h, regions,
    );
    render_lists_column(frame, bottom[1], state, cache, selected.as_ref(), &aboard_set, ui, focused, regions);

    // Tooltip: spans the block width, directly under the bottom region.
    if !tooltip_lines.is_empty() {
        let text: Vec<Line> = tooltip_lines.iter().map(|l| Line::from(*l)).collect();
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            rows[2],
        );
    }

    // Ship-type popup, drawn last so it sits atop the page and its click
    // regions win the reverse-iterating hit test.
    if let Some(sel) = ui.ship_popup {
        render_ship_popup(frame, sel, regions);
    }
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
    ship_idx: Option<usize>,
    warn: Option<Staffing>,
    warn_lines: &[String],
    ship_h: u16,
    regions: &mut Vec<ClickRegion>,
) {
    // Vessels list as tall as its content; Ship Type below it; Unpoison below
    // that; empty slack beneath. Leave room for the ship-type box + 3-tall button.
    let reserve = ship_h + 3;
    let vessels_h =
        (ordered.len() as u16 + 2).clamp(3, area.height.saturating_sub(reserve).max(3));
    let chunks = Layout::vertical([
        Constraint::Length(vessels_h),
        Constraint::Length(ship_h),
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

    // -- Ship Type --
    // First row is the chosen ship (a button that opens the select popup);
    // any staffing warning is wrapped beneath it inside the same box.
    let st_focused = focus == JobberFocus::ShipType;
    let label_style = if focused && st_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else if ship_idx.is_some() {
        Style::default().bold()
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let ship_label = ship_idx
        .map(|i| crate::ships::SHIPS[i].name.to_string())
        .unwrap_or_else(|| "Select ship".to_string());
    let mut st_lines: Vec<Line> = Vec::with_capacity(1 + warn_lines.len());
    st_lines.push(Line::from(Span::styled(ship_label, label_style)).centered());
    let warn_style = warn.map(Staffing::style).unwrap_or_default();
    for w in warn_lines {
        st_lines.push(Line::from(Span::styled(w.clone(), warn_style)));
    }
    let st_block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(focused, st_focused))
        .title("─── Ship Type ");
    frame.render_widget(Paragraph::new(st_lines).block(st_block), chunks[1]);
    regions.push(ClickRegion {
        rect: chunks[1],
        target: ClickTarget::JobberShipType,
    });

    // -- Unpoison button --
    // When the vessel isn't poisoned the button can't be used, so both the box
    // and the word are grayed out; otherwise the border follows normal focus.
    let poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    let disabled = Style::default().fg(Color::DarkGray);
    let (btn_style, border) = if !poisoned {
        (disabled, disabled)
    } else if focus == JobberFocus::Unpoison {
        (
            Style::default().bg(Color::White).fg(Color::Black).bold(),
            box_border(focused, true),
        )
    } else {
        (
            Style::default().fg(Color::Red).bold(),
            box_border(focused, false),
        )
    };
    let button = Paragraph::new("Unpoison").centered().style(btn_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border),
    );
    frame.render_widget(button, chunks[2]);
    regions.push(ClickRegion {
        rect: chunks[2],
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
    let mut aboard_lines: Vec<Line> = aboard
        .iter()
        .map(|n| Line::from(Span::styled((*n).clone(), name_style(n))))
        .collect();
    // Footer: how many swabbies (NPC crew) are aboard, italicized.
    let swabbies = vessel.map_or(0, |v| v.swabbies);
    if swabbies > 0 {
        aboard_lines.push(Line::from(Span::styled(
            format!("   and {swabbies} swabbies"),
            Style::default().italic(),
        )));
    }
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

/// The ship-type select popup: same list as the Damage calculator's, minus the
/// "View" affordance. `selected` is the highlighted ship index.
fn render_ship_popup(frame: &mut Frame, selected: usize, regions: &mut Vec<ClickRegion>) {
    let area = frame.area();

    let max_name = SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0);
    // +2 borders +2 padding +2 highlight symbol.
    let w = max_name as u16 + 6;
    let h = SHIPS.len() as u16 + 2; // +2 borders
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = SHIPS.iter().map(|s| ListItem::new(s.name)).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Select Ship "),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, popup_area, &mut state);

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0..SHIPS.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberShipItem(i),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sloop: max_mercenaries = 6, max_pirates = 7.
    fn sloop() -> &'static Ship {
        SHIPS.iter().find(|s| s.name == "Sloop").unwrap()
    }

    #[test]
    fn staffing_flags_understaffed_when_room_and_mercs_left() {
        // 2 players + 2 swabbies = 4 < 7 pirates, 2 < 6 mercs.
        assert_eq!(staffing(sloop(), 2, 2), Some(Staffing::Understaffed));
    }

    #[test]
    fn staffing_is_clear_when_full_or_mercs_capped() {
        // Exactly at the pirate cap: not understaffed, not invalid.
        assert_eq!(staffing(sloop(), 1, 6), None);
        // Mercenary cap reached even with a free pirate slot: don't nag to hire.
        assert_eq!(staffing(sloop(), 0, 6), None);
    }

    #[test]
    fn staffing_is_invalid_when_over_either_cap() {
        // Too many swabbies for the merc cap.
        assert_eq!(staffing(sloop(), 0, 7), Some(Staffing::Invalid));
        // Too many bodies for the pirate cap (overstaffed beats understaffed).
        assert_eq!(staffing(sloop(), 5, 4), Some(Staffing::Invalid));
    }

    #[test]
    fn wrap_words_breaks_on_spaces() {
        assert_eq!(
            wrap_words("Understaffed. Hire jobbers.", 20),
            vec!["Understaffed. Hire", "jobbers."],
        );
    }

    #[test]
    fn wrap_words_hard_breaks_overlong_words() {
        assert_eq!(wrap_words("Understaffed.", 5), vec!["Under", "staff", "ed."]);
    }
}
