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
use crate::utils::{offset_title, offset_title_width};

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

/// The kind of voyage being crewed. Only [`VoyageType::Pillage`] is implemented;
/// the rest are picker stubs that fall back to a "coming soon" placeholder. New
/// types only *augment* the Pillage layout with extra stats, so the enum can grow
/// without disturbing the existing data model.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoyageType {
    #[default]
    Pillage,
    Atlantis,
    CursedIsles,
    Vampirates,
    Vikings,
}

/// Every voyage type, in picker order.
pub const VOYAGE_TYPES: &[VoyageType] = &[
    VoyageType::Pillage,
    VoyageType::Atlantis,
    VoyageType::CursedIsles,
    VoyageType::Vampirates,
    VoyageType::Vikings,
];

impl VoyageType {
    /// Display name shown in the Voyage box and its picker.
    pub fn name(self) -> &'static str {
        match self {
            VoyageType::Pillage => "Pillage",
            VoyageType::Atlantis => "Atlantis",
            VoyageType::CursedIsles => "Cursed Isles",
            VoyageType::Vampirates => "Vampirates",
            VoyageType::Vikings => "Vikings",
        }
    }

    /// Whether the full jobbers layout (Top Jobbers + the three panes) is wired up
    /// for this voyage type. Only Pillage is, for now.
    pub fn implemented(self) -> bool {
        matches!(self, VoyageType::Pillage)
    }
}

/// One of the three pirate panes along the bottom of the Pillage layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobberPane {
    Aboard,
    Greedy,
    Planked,
}

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
/// reachable when the selected vessel is actually poisoned; the three panes only
/// exist on the Pillage layout.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum JobberFocus {
    #[default]
    Vessels,
    ShipType,
    VoyageType,
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
    /// Scroll offset of each pane, recomputed each frame to keep the pane's
    /// selection visible (the panes auto-scroll rather than scroll manually).
    pub aboard_offset: usize,
    pub greedy_offset: usize,
    pub planked_offset: usize,
    /// Selected pirate index within each pane (into that pane's pirate list).
    pub aboard_sel: usize,
    pub greedy_sel: usize,
    pub planked_sel: usize,
    /// The voyage type being crewed; gates the Pillage-only layout.
    pub voyage_type: VoyageType,
    /// Ship type chosen per vessel (index into [`SHIPS`]), keyed by vessel name.
    /// Lives here, not on the `Vessel`, so picks survive a relog state wipe.
    pub ship_types: HashMap<Arc<str>, usize>,
    /// When `Some`, the ship-type popup is open with this highlighted index;
    /// it applies to the currently-selected vessel.
    pub ship_popup: Option<usize>,
    /// When `Some`, the vessel picker popup is open with this highlighted index
    /// (into the latest-first vessel ordering).
    pub vessel_popup: Option<usize>,
    /// When `Some`, the voyage-type picker popup is open with this highlighted
    /// index (into [`VOYAGE_TYPES`]).
    pub voyage_popup: Option<usize>,
}

/// Bottom-bar tooltip lines for the current focus (empty when nothing to say).
pub fn tooltip(state: &GameState, ui: &JobbersUi) -> Vec<&'static str> {
    match ui.focus {
        JobberFocus::Vessels => vec!["Press Enter to pick a vessel."],
        JobberFocus::ShipType => vec!["Press Enter to pick this vessel's ship type."],
        JobberFocus::VoyageType => vec!["Press Enter to pick the voyage type."],
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
            vec!["\u{2190}/\u{2192} switch panes \u{00b7} \u{2191}/\u{2193} select pirate"]
        }
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
                .title(offset_title("Vessel's Jobbers").0),
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

    let pillage = ui.voyage_type.implemented();

    // The Unpoison button is only focusable while the vessel is poisoned, and the
    // three panes only exist on the Pillage layout — bounce focus out of either
    // when it no longer applies.
    let sel_poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    if !sel_poisoned && ui.focus == JobberFocus::Unpoison {
        ui.focus = JobberFocus::Vessels;
    }
    if !pillage
        && matches!(
            ui.focus,
            JobberFocus::Aboard | JobberFocus::Greedy | JobberFocus::Planked
        )
    {
        ui.focus = JobberFocus::VoyageType;
    }

    // ---- Voyage box sizing ----
    // Longest ship name, computed at compile time — the Ship Type row's value must
    // fit it.
    const MAX_SHIP_NAME: usize = {
        let mut max = 0usize;
        let mut i = 0;
        while i < SHIPS.len() {
            let len = SHIPS[i].name.len();
            if len > max {
                max = len;
            }
            i += 1;
        }
        max
    };
    let label_w = ["Vessels", "Ship Type", "Voyage Type"]
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(0) as u16;
    let vessel_name_w = selected.as_ref().map_or(0, |k| k.chars().count());
    let voyage_name_w = VOYAGE_TYPES.iter().map(|v| v.name().len()).max().unwrap_or(0);
    let value_w = vessel_name_w
        .max(MAX_SHIP_NAME)
        .max(voyage_name_w)
        .max("Select ship".len())
        .max("No vessel".len()) as u16;
    // label + 2-space gap + value, plus borders(2) + padding(2).
    let voyage_w = (label_w + 2 + value_w + 4).max(offset_title_width("Voyage"));

    // Staffing data + the warning the chosen ship implies.
    let vessel = selected.as_ref().and_then(|k| state.vessels.get(k));
    let aboard_set = selected.as_ref().map(|k| state.aboard(k)).unwrap_or_default();
    let ship_idx = selected.as_ref().and_then(|k| ui.ship_types.get(k).copied());
    let swabbies = vessel.map_or(0, |v| v.swabbies);
    let warn = ship_idx.and_then(|i| staffing(&SHIPS[i], aboard_set.len(), swabbies));

    // ---- Pillage-only sizing (Top Jobbers + the three panes) ----
    let ranked: Vec<Vec<(String, Experience, Standing)>> = SKILL_COLUMNS
        .iter()
        .map(|(skill, _)| rank_for_skill(&aboard_set, cache, skill))
        .collect();
    let top_rows = ranked.iter().map(Vec::len).max().unwrap_or(0);
    let top_h = top_rows as u16 + 3;
    let top_panel_w = top_panel_width(&ranked);

    // Per-pane natural widths: content + borders(2) + padding(2), floored at title.
    let aboard_cw = aboard_set
        .iter()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .max(if swabbies > 0 {
            format!("and {swabbies} swabbies").len()
        } else {
            0
        });
    let greedy: Vec<(&String, u32, u32)> = vessel
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
    let greedy_cw = name_col_plus_value(&greedy);
    let planked_cw = vessel
        .map(|v| {
            v.planked_by_us
                .iter()
                .map(|n| n.chars().count())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let pane_w = |cw: usize, title: &'static str| (cw as u16 + 4).max(offset_title_width(title));
    let aboard_w = pane_w(aboard_cw, "Aboard");
    let greedy_w = pane_w(greedy_cw, "Greedy");
    let planked_w = pane_w(planked_cw, "Planked");
    let panes_w = aboard_w + greedy_w + planked_w;

    // ---- Block geometry: centered horizontally, full content height so the panes
    //      can run the whole way down. ----
    let block_w = if pillage {
        voyage_w.max(top_panel_w).max(panes_w)
    } else {
        voyage_w.max(offset_title_width("Coming Soon"))
    };
    let block_w = block_w.min(area.width.max(1));
    let block = Rect::new(
        area.x + area.width.saturating_sub(block_w) / 2,
        area.y,
        block_w,
        area.height,
    );

    // Voyage box height: 3 rows + (blank + Unpoison) when poisoned + warning lines
    // + borders(2). Warnings wrap to the block's inner width.
    let warn_lines: Vec<String> = warn
        .map(|w| wrap_words(w.message(), block_w.saturating_sub(4) as usize))
        .unwrap_or_default();
    let unpoison_h: u16 = if sel_poisoned { 2 } else { 0 };
    let voyage_h = 3 + unpoison_h + warn_lines.len() as u16 + 2;

    let tooltip_lines = tooltip(state, ui);
    let tip_h = tooltip_lines.len() as u16;

    let rows = if pillage {
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Length(top_h),
            Constraint::Min(0),
            Constraint::Length(tip_h),
        ])
        .split(block)
    } else {
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Min(0),
            Constraint::Length(tip_h),
        ])
        .split(block)
    };

    render_voyage_box(
        frame, rows[0], ui, &selected, ship_idx, sel_poisoned, warn, &warn_lines, label_w,
        focused, regions,
    );

    let tip_area = if pillage {
        render_top_panel(frame, rows[1], &ranked, focused);
        render_panes(
            frame, rows[2], state, cache, selected.as_ref(), &aboard_set, &greedy, ui, focused,
            aboard_w, greedy_w, planked_w, regions,
        );
        rows[3]
    } else {
        render_placeholder(frame, rows[1], ui.voyage_type, focused);
        rows[2]
    };

    if !tooltip_lines.is_empty() {
        let text: Vec<Line> = tooltip_lines.iter().map(|l| Line::from(*l)).collect();
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            tip_area,
        );
    }

    // Modal popups, drawn last so they sit atop the page and their click regions
    // win the reverse-iterating hit test.
    if let Some(sel) = ui.ship_popup {
        render_ship_popup(frame, sel, regions);
    } else if let Some(sel) = ui.vessel_popup {
        render_vessel_popup(frame, sel, &ordered, state, regions);
    } else if let Some(sel) = ui.voyage_popup {
        render_voyage_popup(frame, sel, regions);
    }
}

// ---------------------------------------------------------------------------
// Voyage box: vessel / ship-type / voyage-type buttons + Unpoison
// ---------------------------------------------------------------------------

/// The Voyage box at the top of the page: a 2-column `label | value` table whose
/// three value cells are buttons (each opens its picker popup), plus a conditional
/// Unpoison button and any staffing warning, all framed in one bordered box.
#[allow(clippy::too_many_arguments)]
fn render_voyage_box(
    frame: &mut Frame,
    area: Rect,
    ui: &JobbersUi,
    selected: &Option<Arc<str>>,
    ship_idx: Option<usize>,
    poisoned: bool,
    warn: Option<Staffing>,
    warn_lines: &[String],
    label_w: u16,
    page_focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(page_focused))
        .padding(Padding::horizontal(1))
        .title(offset_title("Voyage").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Three label/value rows, then (when poisoned) a blank + Unpoison line, then
    // any warning lines, then slack.
    let mut constraints: Vec<Constraint> = vec![Constraint::Length(1); 3];
    if poisoned {
        constraints.push(Constraint::Length(1)); // blank
        constraints.push(Constraint::Length(1)); // unpoison
    }
    for _ in warn_lines {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let rows = Layout::vertical(constraints).split(inner);

    let vessel_value = selected
        .as_ref()
        .map(|k| k.to_string())
        .unwrap_or_else(|| "No vessel".to_string());
    let ship_value = ship_idx
        .map(|i| SHIPS[i].name.to_string())
        .unwrap_or_else(|| "Select ship".to_string());
    let voyage_value = ui.voyage_type.name().to_string();

    // The fourth field flags a placeholder value (nothing picked yet): it renders
    // greyed + italic when unfocused, matching the profits "Query Market first"
    // affordance.
    let entries: [(&str, String, bool, JobberFocus, ClickTarget); 3] = [
        (
            "Vessels",
            vessel_value,
            false,
            JobberFocus::Vessels,
            ClickTarget::JobberVesselButton,
        ),
        (
            "Ship Type",
            ship_value,
            ship_idx.is_none(),
            JobberFocus::ShipType,
            ClickTarget::JobberShipType,
        ),
        (
            "Voyage Type",
            voyage_value,
            false,
            JobberFocus::VoyageType,
            ClickTarget::JobberVoyageType,
        ),
    ];

    for (i, (label, value, placeholder, focus, target)) in entries.into_iter().enumerate() {
        let cols = Layout::horizontal([
            Constraint::Length(label_w),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(rows[i]);
        frame.render_widget(
            Paragraph::new(Span::styled(label, Style::default().bold())),
            cols[0],
        );
        let is_focused = page_focused && ui.focus == focus;
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else if placeholder {
            Style::default().fg(Color::DarkGray).italic()
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(value)).right_aligned()).style(value_style),
            cols[2],
        );
        regions.push(ClickRegion { rect: rows[i], target });
    }

    if poisoned {
        let btn_area = rows[4]; // rows: 0..2 table, 3 blank, 4 unpoison
        let btn_style = if page_focused && ui.focus == JobberFocus::Unpoison {
            Style::default().bg(Color::White).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::Red).bold()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("Unpoison", btn_style)).centered()),
            btn_area,
        );
        regions.push(ClickRegion {
            rect: btn_area,
            target: ClickTarget::JobberUnpoison,
        });
    }

    let warn_start = if poisoned { 5 } else { 3 };
    let warn_style = warn.map(Staffing::style).unwrap_or_default();
    for (j, w) in warn_lines.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(w.clone(), warn_style)).centered()),
            rows[warn_start + j],
        );
    }
}

/// Placeholder shown in place of the Pillage-only Top Jobbers + panes when the
/// selected voyage type isn't wired up yet.
fn render_placeholder(frame: &mut Frame, area: Rect, voyage_type: VoyageType, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(offset_title("Coming Soon").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(format!("{} voyages aren't supported yet.", voyage_type.name())).centered(),
        inner,
    );
}

// ---------------------------------------------------------------------------
// Bottom panes: Aboard | Greedy | Planked, side by side, with per-pane selection
// ---------------------------------------------------------------------------

/// Clamp a stored pane selection to the live pirate count.
fn clamp_sel(sel: usize, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        sel.min(n - 1)
    }
}

fn pane_focus_target(pane: JobberPane) -> ClickTarget {
    match pane {
        JobberPane::Aboard => ClickTarget::JobberAboardList,
        JobberPane::Greedy => ClickTarget::JobberGreedyList,
        JobberPane::Planked => ClickTarget::JobberPlankedList,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_panes(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    cache: &PirateCache,
    selected: Option<&Arc<str>>,
    aboard_set: &HashSet<String>,
    greedy: &[(&String, u32, u32)],
    ui: &mut JobbersUi,
    focused: bool,
    aboard_w: u16,
    greedy_w: u16,
    planked_w: u16,
    regions: &mut Vec<ClickRegion>,
) {
    // Start from each pane's natural (content) width, then spread any slack — the
    // block may be wider than the three panes combined when Top Jobbers or the
    // Voyage box is the widest piece — evenly so it doesn't all dump into Planked.
    let natural = aboard_w + greedy_w + planked_w;
    let slack = area.width.saturating_sub(natural);
    let add = slack / 3;
    let rem = slack % 3;
    let a = aboard_w + add + u16::from(rem > 0);
    let g = greedy_w + add + u16::from(rem > 1);
    let cols = Layout::horizontal([
        Constraint::Length(a),
        Constraint::Length(g),
        Constraint::Min(0),
    ])
    .split(area);

    // Same-crew / self emphasis, mirroring the old lists column.
    let my_crew: Option<String> = state
        .player_name
        .as_deref()
        .and_then(|me| cache.get(me))
        .map(|p| p.crew_name.clone())
        .filter(|c| !c.is_empty());
    let style_for = |name: &str| -> Style {
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

    let vessel = selected.and_then(|k| state.vessels.get(k));

    // -- Aboard (alphabetical) + swabbie footer (non-selectable) --
    let mut aboard: Vec<&String> = aboard_set.iter().collect();
    aboard.sort_unstable();
    let aboard_n = aboard.len();
    let mut aboard_rows: Vec<(Line, Option<usize>)> = aboard
        .iter()
        .enumerate()
        .map(|(i, n)| (Line::from(Span::styled((*n).clone(), style_for(n))), Some(i)))
        .collect();
    let swabbies = vessel.map_or(0, |v| v.swabbies);
    if swabbies > 0 {
        aboard_rows.push((
            Line::from(Span::styled(
                format!("and {swabbies} swabbies"),
                Style::default().italic(),
            )),
            None,
        ));
    }

    // -- Greedy (total desc, then alphabetical) --
    let mut greedy_sorted: Vec<(&String, u32, u32)> = greedy.to_vec();
    greedy_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let greedy_n = greedy_sorted.len();
    let greedy_inner_w =
        (cols[1].width.saturating_sub(4) as usize).max(name_col_plus_value(&greedy_sorted));
    let greedy_rows: Vec<(Line, Option<usize>)> = greedy_sorted
        .iter()
        .enumerate()
        .map(|(i, (name, total, current))| {
            (
                greedy_line(name, *total, *current, greedy_inner_w, style_for(name)),
                Some(i),
            )
        })
        .collect();

    // -- Planked (alphabetical) --
    let mut planked: Vec<String> = vessel.map(|v| v.planked_by_us.clone()).unwrap_or_default();
    planked.sort_unstable();
    let planked_n = planked.len();
    let planked_rows: Vec<(Line, Option<usize>)> = planked
        .iter()
        .enumerate()
        .map(|(i, n)| (Line::from(Span::styled(n.clone(), style_for(n))), Some(i)))
        .collect();

    ui.aboard_sel = clamp_sel(ui.aboard_sel, aboard_n);
    ui.greedy_sel = clamp_sel(ui.greedy_sel, greedy_n);
    ui.planked_sel = clamp_sel(ui.planked_sel, planked_n);

    render_pane(
        frame, cols[0], "Aboard", aboard_rows, ui.aboard_sel, &mut ui.aboard_offset, focused,
        ui.focus == JobberFocus::Aboard, JobberPane::Aboard, regions,
    );
    render_pane(
        frame, cols[1], "Greedy", greedy_rows, ui.greedy_sel, &mut ui.greedy_offset, focused,
        ui.focus == JobberFocus::Greedy, JobberPane::Greedy, regions,
    );
    render_pane(
        frame, cols[2], "Planked", planked_rows, ui.planked_sel, &mut ui.planked_offset, focused,
        ui.focus == JobberFocus::Planked, JobberPane::Planked, regions,
    );
}

/// Render a single pane: a bordered, auto-scrolling list of pirate rows. `rows`
/// pairs each display line with its pirate index (or `None` for non-selectable
/// rows like the swabbie footer). The selected pirate is highlighted and the
/// offset is nudged to keep it visible.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    frame: &mut Frame,
    area: Rect,
    title: &'static str,
    rows: Vec<(Line<'static>, Option<usize>)>,
    sel: usize,
    offset: &mut usize,
    page_focused: bool,
    active: bool,
    pane: JobberPane,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(page_focused, active))
        .padding(Padding::horizontal(1))
        .title(offset_title(title).0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-pane focus region first, so the per-row regions pushed below win the
    // reverse-iterating hit test on overlap.
    regions.push(ClickRegion {
        rect: area,
        target: pane_focus_target(pane),
    });

    let height = inner.height as usize;
    if height == 0 {
        return;
    }

    // Auto-scroll: keep the selected pirate's line within the visible window.
    if let Some(sel_line) = rows.iter().position(|(_, p)| *p == Some(sel)) {
        if sel_line < *offset {
            *offset = sel_line;
        } else if sel_line >= *offset + height {
            *offset = sel_line + 1 - height;
        }
    }
    let max_off = rows.len().saturating_sub(height);
    if *offset > max_off {
        *offset = max_off;
    }

    for (vis, (line, pidx)) in rows.iter().enumerate().skip(*offset).take(height) {
        let row_area = Rect::new(inner.x, inner.y + (vis - *offset) as u16, inner.width, 1);
        let is_sel = page_focused && active && *pidx == Some(sel);
        let para = if is_sel {
            Paragraph::new(line.clone()).style(Style::default().bg(Color::White).fg(Color::Black))
        } else {
            Paragraph::new(line.clone())
        };
        frame.render_widget(para, row_area);
        if let Some(idx) = pidx {
            regions.push(ClickRegion {
                rect: row_area,
                target: ClickTarget::JobberPirate { pane, idx: *idx },
            });
        }
    }
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
    // Floor so the title stays readable when no jobbers have fetched stats yet.
    const FLOOR: u16 = offset_title_width("Top Jobbers");
    let inner_w = top_panel_col_widths(ranked).iter().sum::<u16>() + 2 * COLUMN_GAP;
    (inner_w + 4).max(FLOOR)
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
        .title(offset_title("Top Jobbers").0);
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
        lines.push(
            Line::from(Span::styled(*header, Style::default().bold().underlined())).centered(),
        );
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
                .title(offset_title("Select Ship").0),
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

/// The vessel picker popup (replaces the old inline vessel list). Poisoned
/// vessels are listed in red, matching the old list styling.
fn render_vessel_popup(
    frame: &mut Frame,
    selected: usize,
    ordered: &[Arc<str>],
    state: &GameState,
    regions: &mut Vec<ClickRegion>,
) {
    let area = frame.area();

    let max_name = ordered
        .iter()
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(0)
        .max("No vessels".len());
    let w = max_name as u16 + 6; // +2 borders +2 padding +2 highlight symbol.
    let h = (ordered.len() as u16).max(1) + 2;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = if ordered.is_empty() {
        vec![ListItem::new("No vessels").style(Style::default().fg(Color::DarkGray))]
    } else {
        ordered
            .iter()
            .map(|k| {
                let poisoned = state.vessels.get(k).is_some_and(|v| v.poisoned);
                let item = ListItem::new(k.to_string());
                if poisoned {
                    item.style(Style::default().fg(Color::Red))
                } else {
                    item
                }
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Vessels").0),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let mut st = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, popup_area, &mut st);

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0..ordered.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberVesselItem(i),
        });
    }
}

/// The voyage-type picker popup. Unimplemented types are tagged "(soon)" and
/// muted, but can still be selected (they show the "coming soon" placeholder).
fn render_voyage_popup(frame: &mut Frame, selected: usize, regions: &mut Vec<ClickRegion>) {
    let area = frame.area();

    let labels: Vec<String> = VOYAGE_TYPES
        .iter()
        .map(|v| {
            if v.implemented() {
                v.name().to_string()
            } else {
                format!("{} (soon)", v.name())
            }
        })
        .collect();
    let max_name = labels.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let w = max_name as u16 + 6;
    let h = VOYAGE_TYPES.len() as u16 + 2;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = labels
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let item = ListItem::new(s.clone());
            if VOYAGE_TYPES[i].implemented() {
                item
            } else {
                item.style(Style::default().fg(Color::DarkGray))
            }
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Voyage Type").0),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let mut st = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, popup_area, &mut st);

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0..VOYAGE_TYPES.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberVoyageItem(i),
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
