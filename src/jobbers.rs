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
    TrophySection,
};
use crate::ships::{Ship, SHIPS};
use crate::utils::{offset_title, offset_title_width, text_similarity};

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

/// A Top Jobbers column: the skill(s) it ranks aboard jobbers by, plus an optional
/// explicit header. A single-skill column is the common case; a column with several
/// skills (e.g. Sail+Rig) ranks each jobber by their *best* of those skills and
/// flags which one with a marker letter — see [`rank_columns`].
pub struct JobberColumn {
    /// Header text; `None` derives it from the lone skill's short label.
    label: Option<&'static str>,
    /// The skills this column considers, in tie-irrelevant order; never empty.
    skills: &'static [Skill],
}

impl JobberColumn {
    /// The column header: the explicit label, else the single skill's short label.
    fn header(&self) -> &'static str {
        match self.label {
            Some(l) => l,
            None => self.skills[0].short_label(),
        }
    }

    /// Whether this column merges several skills (so its rows carry a marker).
    fn merged(&self) -> bool {
        self.skills.len() > 1
    }
}

/// Top Jobbers columns for a Pillage: gunners, navigators, and battle navigators.
/// Other voyage types prioritise different skills — see [`VoyageType::top_jobbers`].
const PILLAGE_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn { label: None, skills: &[Skill::Gunning] },
    JobberColumn { label: None, skills: &[Skill::Navigating] },
    JobberColumn { label: None, skills: &[Skill::BattleNavigation] },
];

/// Top Jobbers columns for an Atlantis run: treasure haulers, gunners, and battle
/// navigators.
const ATLANTIS_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn { label: None, skills: &[Skill::TreasureHaul] },
    JobberColumn { label: None, skills: &[Skill::Gunning] },
    JobberColumn { label: None, skills: &[Skill::BattleNavigation] },
];

/// Top Jobbers columns for a Cursed Isles run: foragers and battle navigators, the
/// two merged station columns Sail+Rig (Sailing/Rigging) and Carp+Patch
/// (Carpentry/Patching), and bilgers.
const CURSED_ISLES_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn { label: None, skills: &[Skill::Foraging] },
    JobberColumn { label: None, skills: &[Skill::BattleNavigation] },
    JobberColumn { label: Some("Sail+Rig"), skills: &[Skill::Sailing, Skill::Rigging] },
    JobberColumn { label: Some("Carp+Patch"), skills: &[Skill::Carpentry, Skill::Patching] },
    JobberColumn { label: None, skills: &[Skill::Bilging] },
];

/// Skills in each family, in yoweb display order. The pirate-stats popup renders
/// one table per family using these.
const PIRACY_SKILLS: &[Skill] = &[
    Skill::Sailing,
    Skill::Rigging,
    Skill::Carpentry,
    Skill::Patching,
    Skill::Bilging,
    Skill::Gunning,
    Skill::TreasureHaul,
    Skill::Navigating,
    Skill::BattleNavigation,
    Skill::Swordfighting,
    Skill::Rumble,
];
const CRAFTING_SKILLS: &[Skill] = &[
    Skill::Distilling,
    Skill::Alchemistry,
    Skill::Shipwrightery,
    Skill::Blacksmithing,
    Skill::Foraging,
    Skill::Weaving,
];
const CAROUSING_SKILLS: &[Skill] = &[
    Skill::Drinking,
    Skill::Spades,
    Skill::Hearts,
    Skill::TreasureDrop,
    Skill::Poker,
];

/// The kind of voyage being crewed. Pillage, Atlantis, and Cursed Isles are
/// implemented; the rest are picker stubs that fall back to a "coming soon"
/// placeholder. Each type drives its own Top Jobbers columns
/// ([`VoyageType::top_jobbers`]) and bottom panes ([`VoyageType::panes`]), so the
/// enum can grow without disturbing the existing data model.
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

    /// Whether the full jobbers layout (Top Jobbers + the panes) is wired up for
    /// this voyage type. Pillage, Atlantis, and Cursed Isles are, for now.
    pub fn implemented(self) -> bool {
        matches!(
            self,
            VoyageType::Pillage | VoyageType::Atlantis | VoyageType::CursedIsles
        )
    }

    /// The Top Jobbers columns this voyage type ranks, in display order. Each is
    /// one or more skills (merged columns rank by a jobber's best of them); new
    /// voyage types override this with the columns they need.
    pub fn top_jobbers(self) -> &'static [JobberColumn] {
        match self {
            VoyageType::Pillage => PILLAGE_TOP_JOBBERS,
            VoyageType::Atlantis => ATLANTIS_TOP_JOBBERS,
            VoyageType::CursedIsles => CURSED_ISLES_TOP_JOBBERS,
            // Unimplemented types fall back to Pillage's columns for now; they
            // render the "coming soon" placeholder instead of the panel anyway.
            // Give each its own columns as it gets wired up.
            VoyageType::Vampirates | VoyageType::Vikings => PILLAGE_TOP_JOBBERS,
        }
    }

    /// The bottom panes this voyage type shows, in left-to-right order. Pillage
    /// gets all three; Atlantis and Cursed Isles drop Greedy. Unimplemented types
    /// get none (they render the "coming soon" placeholder instead).
    pub fn panes(self) -> &'static [JobberPane] {
        match self {
            VoyageType::Pillage => PILLAGE_PANES,
            VoyageType::Atlantis => ATLANTIS_PANES,
            VoyageType::CursedIsles => CURSED_ISLES_PANES,
            VoyageType::Vampirates | VoyageType::Vikings => &[],
        }
    }

    /// Whether this voyage type spawns dragoons (so the Aboard pane should show
    /// the dragoon tallies). Only Atlantis does.
    pub fn tracks_dragoons(self) -> bool {
        matches!(self, VoyageType::Atlantis)
    }
}

/// Bottom panes for a Pillage: who's aboard, who's been greedy, who we planked.
const PILLAGE_PANES: &[JobberPane] =
    &[JobberPane::Aboard, JobberPane::Greedy, JobberPane::Planked];

/// Bottom panes for an Atlantis run: aboard and planked, no greedy tally.
const ATLANTIS_PANES: &[JobberPane] = &[JobberPane::Aboard, JobberPane::Planked];

/// Bottom panes for a Cursed Isles run: aboard and planked, same as Atlantis.
const CURSED_ISLES_PANES: &[JobberPane] = &[JobberPane::Aboard, JobberPane::Planked];

/// A pirate pane along the bottom of the layout. Which panes show is voyage-type
/// dependent ([`VoyageType::panes`]); Pillage shows all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobberPane {
    Aboard,
    Greedy,
    Planked,
}

// ---------------------------------------------------------------------------
// Global pirate stat cache
// ---------------------------------------------------------------------------

/// One yoweb page for a pirate. The background scheduler fetches at most one
/// page at a time, basic before trophies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiratePage {
    Basic,
    Trophies,
}

/// The single most important page to fetch right now, with the priority tier it
/// came from (lower = more urgent: 0 on-demand, 1 aboard, 2 planked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOrder {
    pub norm: String,
    pub page: PiratePage,
    pub tier: u8,
}

/// Yoweb stats fetched per pirate, keyed by normalized name, plus bookkeeping
/// so the background fetcher knows what's stale and what's been requested.
#[derive(Default)]
pub struct PirateCache {
    /// Successfully fetched pirates, keyed by normalized name. Each entry
    /// carries its basic profile, trophies, and per-part fetch timestamps.
    pub fetched: HashMap<String, CachedPirate>,
    /// Normalized names confirmed not to exist on yoweb this session (the page
    /// loaded but named no pirate). Never auto-fetched again; a force-requery
    /// clears the name so it can be retried.
    pub gone: HashSet<String>,
    /// Normalized names the user explicitly requested (clicked) — fetched at top
    /// priority and in full (both pages), ignoring staleness. Cleared once the
    /// trophies page (the last in the sequence) lands.
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

    /// Queue `name` for a full, top-priority re-query, ignoring staleness and any
    /// prior not-found result. Used by the on-demand path (clicking a pirate).
    /// Marks both pages stale so the (TTL-based) scheduler refetches them now, and
    /// raises the pirate to tier 0 until its trophies page lands.
    pub fn force_requery(&mut self, name: &str) {
        if let Ok(norm) = pirate::normalize_name(name) {
            self.gone.remove(&norm);
            if let Some(c) = self.fetched.get_mut(&norm) {
                c.basic_fetched_at = DateTime::<Utc>::MIN_UTC;
                c.trophies_fetched_at = DateTime::<Utc>::MIN_UTC;
            }
            self.forced.insert(norm);
        }
    }

    /// Decide which yoweb pages are due for an already-normalized `norm`, or
    /// `None` if nothing is. A never-seen pirate needs both pages; otherwise each
    /// page is due only once its TTL has elapsed. A forced re-query expresses
    /// itself by stale timestamps (see [`Self::force_requery`]), so it flows
    /// through this same TTL logic rather than a special case.
    pub fn fetch_plan(
        &self,
        norm: &str,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<FetchPlan> {
        match self.fetched.get(norm) {
            None => Some(FetchPlan { basic: true, trophies: true }),
            Some(c) => {
                let basic = now.signed_duration_since(c.basic_fetched_at) >= basic_ttl;
                let trophies = now.signed_duration_since(c.trophies_fetched_at) >= trophy_ttl;
                (basic || trophies).then_some(FetchPlan { basic, trophies })
            }
        }
    }

    /// Fold a completed single-page fetch for `norm` into the cache, swapping in
    /// whichever part came back fresh. An on-demand (`forced`) request is cleared
    /// only once its trophies page — the last in the basic→trophies sequence —
    /// lands, so a forced pirate fetches both pages before dropping off tier 0.
    pub fn apply_update(&mut self, norm: String, update: PirateUpdate) {
        match update {
            PirateUpdate::Refreshed { basic, trophies } => {
                // The trophies page is the tail of the sequence: once it lands an
                // on-demand request is satisfied.
                if trophies.is_some() {
                    self.forced.remove(&norm);
                }
                match self.fetched.get_mut(&norm) {
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
                    // A pirate's basic page is always fetched before its trophies,
                    // so for a brand-new entry `basic` is present; trophies lag
                    // behind, left stale (MIN_UTC) so the next pass fetches them.
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
                }
            }
            PirateUpdate::NotFound => {
                self.fetched.remove(&norm);
                self.forced.remove(&norm);
                self.gone.insert(norm);
            }
            // Transport/server error: keep whatever we have and give up the forced
            // request (don't hammer a failing page); natural staleness may retry.
            PirateUpdate::Error(_) => {
                self.forced.remove(&norm);
            }
        }
    }

    /// Which page (if any) is due for an already-normalized `norm`: basic before
    /// trophies. `None` means nothing is due.
    fn due_page(
        &self,
        norm: &str,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<PiratePage> {
        let plan = self.fetch_plan(norm, basic_ttl, trophy_ttl, now)?;
        if plan.basic {
            Some(PiratePage::Basic)
        } else if plan.trophies {
            Some(PiratePage::Trophies)
        } else {
            None
        }
    }

    /// The priority tier of an already-normalized `norm` against the current
    /// sets, or `None` if it's no longer relevant (so an in-flight fetch for it
    /// can be cancelled). 0 = on-demand, 1 = aboard/self, 2 = planked.
    pub fn tier_of(
        &self,
        norm: &str,
        aboard: &HashSet<String>,
        planked: &HashSet<String>,
    ) -> Option<u8> {
        if self.forced.contains(norm) {
            Some(0)
        } else if aboard.contains(norm) {
            Some(1)
        } else if planked.contains(norm) {
            Some(2)
        } else {
            None
        }
    }

    /// The single most important page to fetch right now, scanning tiers in
    /// priority order (on-demand ▸ aboard/self ▸ planked) and, within a tier,
    /// pirates in a stable order — basic before trophies per pirate. All name
    /// sets are already normalized. `None` means nothing is due.
    pub fn next_order(
        &self,
        aboard: &HashSet<String>,
        planked: &HashSet<String>,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<FetchOrder> {
        let due = |norm: &str| self.due_page(norm, basic_ttl, trophy_ttl, now);

        // Tier 0: on-demand. Stable order for determinism.
        let mut forced: Vec<&String> = self.forced.iter().collect();
        forced.sort_unstable();
        for norm in forced {
            if let Some(page) = due(norm) {
                return Some(FetchOrder { norm: norm.clone(), page, tier: 0 });
            }
        }

        // Tier 1: aboard + self (skipping gone / already-forced).
        let mut aboard_v: Vec<&String> = aboard.iter().collect();
        aboard_v.sort_unstable();
        for norm in aboard_v {
            if self.gone.contains(norm) || self.forced.contains(norm) {
                continue;
            }
            if let Some(page) = due(norm) {
                return Some(FetchOrder { norm: norm.clone(), page, tier: 1 });
            }
        }

        // Tier 2: planked (skipping gone / forced / already covered as aboard).
        let mut planked_v: Vec<&String> = planked.iter().collect();
        planked_v.sort_unstable();
        for norm in planked_v {
            if self.gone.contains(norm) || self.forced.contains(norm) || aboard.contains(norm) {
                continue;
            }
            if let Some(page) = due(norm) {
                return Some(FetchOrder { norm: norm.clone(), page, tier: 2 });
            }
        }

        None
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
    /// How many entries each Top Jobbers column shows. `None` (the default) shows
    /// the whole ranked list with no cap.
    pub leaderboard_size: Option<usize>,
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
    /// When `Some`, the pirate-stats popup is open for this pirate.
    pub pirate_popup: Option<PiratePopup>,
    /// When `Some`, the trophies popup is open (layered over the stats popup).
    pub trophy_popup: Option<TrophyPopup>,
}

/// State of the open pirate-stats popup: the pirate being viewed and which of its
/// two buttons ([See Trophies] / [Close]) is focused.
#[derive(Clone)]
pub struct PiratePopup {
    pub name: String,
    /// 0 = See Trophies, 1 = Close.
    pub button: usize,
}

/// State of the open trophies popup: whose trophies, the live search filter, the
/// vertical scroll offset (in rendered lines), and the last-rendered view height
/// (so Page Up/Down can scroll by half a page).
#[derive(Clone)]
pub struct TrophyPopup {
    pub name: String,
    pub search: String,
    pub offset: usize,
    pub view_h: usize,
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
            vec!["Enter: pirate stats \u{00b7} \u{2190}/\u{2192} panes \u{00b7} \u{2191}/\u{2193} select"]
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

    let implemented = ui.voyage_type.implemented();
    let panes = ui.voyage_type.panes();

    // The Unpoison button is only focusable while the vessel is poisoned, and a
    // pane is only focusable when this voyage type actually shows it — bounce
    // focus out of either when it no longer applies.
    let sel_poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    if !sel_poisoned && ui.focus == JobberFocus::Unpoison {
        ui.focus = JobberFocus::Vessels;
    }
    let focused_pane = match ui.focus {
        JobberFocus::Aboard => Some(JobberPane::Aboard),
        JobberFocus::Greedy => Some(JobberPane::Greedy),
        JobberFocus::Planked => Some(JobberPane::Planked),
        _ => None,
    };
    if focused_pane.is_some_and(|p| !panes.contains(&p)) {
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

    // ---- Top Jobbers + panes sizing (only used when the layout is implemented) ----
    // Top Jobbers caps at 5 per column; an explicit leaderboard size overrides it.
    let lb_limit = ui.leaderboard_size.or(Some(5));
    let top_columns = rank_columns(ui.voyage_type.top_jobbers(), &aboard_set, cache, lb_limit);
    let top_rows = top_columns.iter().map(|c| c.rows.len()).max().unwrap_or(0);
    let top_h = top_rows as u16 + 3;
    let top_panel_w = top_panel_width(&top_columns);

    // The Aboard pane gains dragoon tally footers on voyage types that spawn them.
    let dragoon_w = if ui.voyage_type.tracks_dragoons() {
        let d = vessel.map_or(0, |v| v.dragoons_aboard);
        let b = vessel.map_or(0, |v| v.dragoon_boardings);
        dragoons_footer(d).len().max(boardings_footer(b).len())
    } else {
        0
    };

    // Per-pane natural widths: content + borders(2) + padding(2), floored at title.
    let aboard_cw = aboard_set
        .iter()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .max(if swabbies > 0 {
            swabbie_footer(swabbies).len()
        } else {
            0
        })
        .max(dragoon_w);
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
    // Only the panes this voyage type shows contribute to the block width.
    let pane_widths: Vec<u16> = panes
        .iter()
        .map(|p| match p {
            JobberPane::Aboard => aboard_w,
            JobberPane::Greedy => greedy_w,
            JobberPane::Planked => planked_w,
        })
        .collect();
    let panes_w: u16 = pane_widths.iter().sum();

    // ---- Atlantis Stats box sizing (only on dragoon voyages) ----
    // A single `Dragoons Boarded | low to high` row; the range counts the lone
    // dragoons plus 3..6 per monster boarding party (we can't see the party size).
    let show_atlantis_stats = ui.voyage_type.tracks_dragoons();
    let (dragoon_low, dragoon_high) = {
        let d = vessel.map_or(0, |v| v.dragoons_aboard);
        let b = vessel.map_or(0, |v| v.dragoon_boardings);
        (
            d.saturating_add(b.saturating_mul(3)),
            d.saturating_add(b.saturating_mul(6)),
        )
    };
    let atlantis_w = if show_atlantis_stats {
        let value = dragoons_boarded_value(dragoon_low, dragoon_high);
        (("Dragoons Boarded".len() + 2 + value.len()) as u16 + 4)
            .max(offset_title_width("Atlantis Stats"))
    } else {
        0
    };
    let atlantis_h: u16 = if show_atlantis_stats { 3 } else { 0 };

    // ---- Block geometry: centered horizontally, full content height so the panes
    //      can run the whole way down. ----
    let block_w = if implemented {
        voyage_w.max(top_panel_w).max(panes_w).max(atlantis_w)
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

    // The pirate-stats / trophies popups own the screen, so hide the page tooltip
    // underneath them.
    let tooltip_lines = if ui.pirate_popup.is_some() || ui.trophy_popup.is_some() {
        Vec::new()
    } else {
        tooltip(state, ui)
    };
    let tip_h = tooltip_lines.len() as u16;

    let rows = if implemented {
        // The Atlantis Stats row sits between Voyage and Top Jobbers; it collapses
        // to zero height (rendering nothing) on voyage types without dragoons.
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Length(atlantis_h),
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

    let tip_area = if implemented {
        if show_atlantis_stats {
            render_atlantis_stats(frame, rows[1], dragoon_low, dragoon_high, focused);
        }
        render_top_panel(frame, rows[2], &top_columns, focused);
        render_panes(
            frame, rows[3], state, cache, selected.as_ref(), &aboard_set, &greedy, ui, focused,
            panes, &pane_widths, regions,
        );
        rows[4]
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

    // The pirate-stats popup, and the trophies popup layered over it.
    if ui.trophy_popup.is_none() {
        if let Some(pp) = ui.pirate_popup.clone() {
            render_pirate_popup(frame, &pp, cache, focused, regions);
        }
    }
    if let Some(tp) = ui.trophy_popup.as_mut() {
        render_trophy_popup(frame, tp, cache, regions);
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

/// The displayed "Dragoons Boarded" value. With no monster boardings the count is
/// exact (`low == high`), so we show a single number; otherwise each boarding party
/// hides 3..6 dragoons, so we report the span `low to high`.
fn dragoons_boarded_value(low: u32, high: u32) -> String {
    if low == high {
        low.to_string()
    } else {
        format!("{low} to {high}")
    }
}

/// The Atlantis Stats box: a single, non-selectable `Dragoons Boarded | value` row
/// spanning the box's inner width. See [`dragoons_boarded_value`] for the count vs.
/// range rule.
fn render_atlantis_stats(frame: &mut Frame, area: Rect, low: u32, high: u32, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::horizontal(1))
        .title(offset_title("Atlantis Stats").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let row = Rect::new(inner.x, inner.y, inner.width, 1);
    let cols = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(row);
    frame.render_widget(
        Paragraph::new(Span::styled("Dragoons Boarded", Style::default().bold())),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(dragoons_boarded_value(low, high)).right_aligned()),
        cols[1],
    );
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

/// The Aboard pane's swabbie footer, pluralised: "and 1 swabbie" / "and N
/// swabbies".
fn swabbie_footer(n: u32) -> String {
    if n == 1 {
        "and 1 swabbie".to_string()
    } else {
        format!("and {n} swabbies")
    }
}

/// The Aboard pane's lone-dragoon tally (Atlantis): "1 dragoon aboard" / "N
/// dragoons aboard".
fn dragoons_footer(n: u32) -> String {
    if n == 1 {
        "1 dragoon aboard".to_string()
    } else {
        format!("{n} dragoons aboard")
    }
}

/// The Aboard pane's monster-boarding tally (Atlantis): each is a party of 3/4/6
/// dragoons we can't count, so we report the party count.
fn boardings_footer(n: u32) -> String {
    if n == 1 {
        "1 monster boarding".to_string()
    } else {
        format!("{n} monster boardings")
    }
}

fn pane_focus_target(pane: JobberPane) -> ClickTarget {
    match pane {
        JobberPane::Aboard => ClickTarget::JobberAboardList,
        JobberPane::Greedy => ClickTarget::JobberGreedyList,
        JobberPane::Planked => ClickTarget::JobberPlankedList,
    }
}

/// Selectable pirate names in a pane, in the same display order `render_panes`
/// uses — Aboard & Planked alphabetical, Greedy by total strikes desc then name.
/// The single source of truth for mapping a pane selection index to a pirate.
pub fn pane_pirates(state: &GameState, key: &Arc<str>, pane: JobberPane) -> Vec<String> {
    match pane {
        JobberPane::Aboard => {
            let mut v: Vec<String> = state.aboard(key).into_iter().collect();
            v.sort_unstable();
            v
        }
        JobberPane::Greedy => {
            let mut g: Vec<(String, u32)> = state
                .vessels
                .get(key)
                .map(|v| v.greedy_by_pirate.iter().map(|(n, t)| (n.clone(), *t)).collect())
                .unwrap_or_default();
            g.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            g.into_iter().map(|(n, _)| n).collect()
        }
        JobberPane::Planked => {
            let mut v: Vec<String> = state
                .vessels
                .get(key)
                .map(|v| v.planked_by_us.clone())
                .unwrap_or_default();
            v.sort_unstable();
            v
        }
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
    panes: &[JobberPane],
    pane_widths: &[u16],
    regions: &mut Vec<ClickRegion>,
) {
    let n = panes.len();
    if n == 0 {
        return;
    }

    // Start from each pane's natural (content) width, then spread any slack — the
    // block may be wider than the panes combined when Top Jobbers or the Voyage
    // box is the widest piece — evenly so it doesn't all dump into the last pane.
    let natural: u16 = pane_widths.iter().sum();
    let slack = area.width.saturating_sub(natural);
    let add = slack / n as u16;
    let rem = slack % n as u16;
    let constraints: Vec<Constraint> = pane_widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if i + 1 == n {
                // The last pane absorbs the remainder so rounding leaves no gap.
                Constraint::Min(0)
            } else {
                Constraint::Length(w + add + u16::from((i as u16) < rem))
            }
        })
        .collect();
    let cols = Layout::horizontal(constraints).split(area);

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

    // Clamp every pane's selection up front, whether or not it's shown.
    ui.aboard_sel = clamp_sel(ui.aboard_sel, aboard_set.len());
    ui.greedy_sel = clamp_sel(ui.greedy_sel, greedy.len());
    let planked_n = vessel.map(|v| v.planked_by_us.len()).unwrap_or(0);
    ui.planked_sel = clamp_sel(ui.planked_sel, planked_n);

    // On dragoon voyages (Atlantis) the Aboard pane gains hostile tally footers.
    let show_dragoons = ui.voyage_type.tracks_dragoons();

    // Render only the panes this voyage type asks for, in order.
    for (i, pane) in panes.iter().enumerate() {
        let col = cols[i];
        match pane {
            // -- Aboard (alphabetical) + swabbie footer (non-selectable) --
            JobberPane::Aboard => {
                let mut aboard: Vec<&String> = aboard_set.iter().collect();
                aboard.sort_unstable();
                let mut rows: Vec<(Line, Option<usize>)> = aboard
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (Line::from(Span::styled((*n).clone(), style_for(n))), Some(i)))
                    .collect();
                let swabbies = vessel.map_or(0, |v| v.swabbies);
                if swabbies > 0 {
                    rows.push((
                        Line::from(Span::styled(swabbie_footer(swabbies), Style::default().italic())),
                        None,
                    ));
                }
                // Hostile dragoon tallies, in red, for Atlantis runs.
                if show_dragoons {
                    let hostile = Style::default().fg(Color::Red).italic();
                    let d = vessel.map_or(0, |v| v.dragoons_aboard);
                    let b = vessel.map_or(0, |v| v.dragoon_boardings);
                    rows.push((Line::from(Span::styled(dragoons_footer(d), hostile)), None));
                    rows.push((Line::from(Span::styled(boardings_footer(b), hostile)), None));
                }
                render_pane(
                    frame, col, "Aboard", rows, ui.aboard_sel, &mut ui.aboard_offset, focused,
                    ui.focus == JobberFocus::Aboard, JobberPane::Aboard, regions,
                );
            }
            // -- Greedy (total desc, then alphabetical) --
            JobberPane::Greedy => {
                let mut greedy_sorted: Vec<(&String, u32, u32)> = greedy.to_vec();
                greedy_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                let inner_w =
                    (col.width.saturating_sub(4) as usize).max(name_col_plus_value(&greedy_sorted));
                let rows: Vec<(Line, Option<usize>)> = greedy_sorted
                    .iter()
                    .enumerate()
                    .map(|(i, (name, total, current))| {
                        (greedy_line(name, *total, *current, inner_w, style_for(name)), Some(i))
                    })
                    .collect();
                render_pane(
                    frame, col, "Greedy", rows, ui.greedy_sel, &mut ui.greedy_offset, focused,
                    ui.focus == JobberFocus::Greedy, JobberPane::Greedy, regions,
                );
            }
            // -- Planked (alphabetical) --
            JobberPane::Planked => {
                let mut planked: Vec<String> =
                    vessel.map(|v| v.planked_by_us.clone()).unwrap_or_default();
                planked.sort_unstable();
                let rows: Vec<(Line, Option<usize>)> = planked
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (Line::from(Span::styled(n.clone(), style_for(n))), Some(i)))
                    .collect();
                render_pane(
                    frame, col, "Planked", rows, ui.planked_sel, &mut ui.planked_offset, focused,
                    ui.focus == JobberFocus::Planked, JobberPane::Planked, regions,
                );
            }
        }
    }
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

/// One jobber's standing within a column: their best skill among the column's set
/// (by standing, then experience), plus that skill's marker for merged columns.
struct RankedJobber {
    name: String,
    experience: Experience,
    standing: Standing,
    /// The winning skill's marker (e.g. `C`/`P`), set only for merged columns.
    marker: Option<char>,
}

/// A Top Jobbers column paired with its ranked jobbers — all the sizing and render
/// helpers need, so they take one slice instead of parallel column/row vecs.
struct RankedColumn {
    header: &'static str,
    /// Whether rows carry a marker letter (true for merged columns).
    marked: bool,
    rows: Vec<RankedJobber>,
}

/// Width of the per-row detail that trails a jobber's name, and whether anything
/// trails it at all. With codes shown it's `EEE/SSS` (+` X` marker on merged
/// columns); when the panel is too narrow we drop the code first, leaving only the
/// merged-column marker (or nothing). `0` means name-only — no trailing gap.
fn detail_width(marked: bool, show_codes: bool) -> usize {
    match (show_codes, marked) {
        (true, true) => CODE_LEN + 2, // EEE/SSS + " X"
        (true, false) => CODE_LEN,    // EEE/SSS
        (false, true) => 1,           // marker letter only
        (false, false) => 0,          // name only
    }
}

/// Rank the aboard jobbers for each column. Within a column a jobber is scored by
/// their *best* of the column's skills (by standing, then experience); a merged
/// column also records which skill won, via its marker. Pirates without any of a
/// column's skills fetched are skipped. `limit` caps each column; `None` is
/// uncapped.
fn rank_columns(
    columns: &[JobberColumn],
    aboard: &HashSet<String>,
    cache: &PirateCache,
    limit: Option<usize>,
) -> Vec<RankedColumn> {
    columns
        .iter()
        .map(|col| {
            let mut rows: Vec<RankedJobber> = aboard
                .iter()
                .filter_map(|n| {
                    let info = cache.get(n)?;
                    // Best of the column's skills for this pirate: highest standing,
                    // then experience.
                    let (skill, rec) = col
                        .skills
                        .iter()
                        .filter_map(|s| info.skills.get(s).map(|r| (s, r)))
                        .max_by(|a, b| {
                            a.1.standing
                                .cmp(&b.1.standing)
                                .then(a.1.experience.cmp(&b.1.experience))
                        })?;
                    Some(RankedJobber {
                        name: n.clone(),
                        experience: rec.experience,
                        standing: rec.standing,
                        marker: col.merged().then(|| skill.marker()),
                    })
                })
                .collect();
            rows.sort_by(|a, b| {
                b.standing
                    .cmp(&a.standing)
                    .then(b.experience.cmp(&a.experience))
                    .then_with(|| a.name.cmp(&b.name))
            });
            if let Some(n) = limit {
                rows.truncate(n);
            }
            RankedColumn { header: col.header(), marked: col.merged(), rows }
        })
        .collect()
}

/// Per-column outer widths for the Top Jobbers panel: each is the wider of its
/// header and its widest `name (+ gap + detail)` row, where the detail depends on
/// `show_codes` (see [`detail_width`]).
fn top_panel_col_widths(columns: &[RankedColumn], show_codes: bool) -> Vec<u16> {
    columns
        .iter()
        .map(|c| {
            let name_w = c.rows.iter().map(|j| j.name.chars().count()).max().unwrap_or(0);
            let detail = detail_width(c.marked, show_codes);
            let row_w = if detail > 0 { name_w + NAME_CODE_GAP + detail } else { name_w };
            row_w.max(c.header.chars().count()) as u16
        })
        .collect()
}

/// Total inner width (columns + gaps) the panel needs in a given mode.
fn top_panel_inner_width(columns: &[RankedColumn], show_codes: bool) -> u16 {
    let gaps = columns.len().saturating_sub(1) as u16 * COLUMN_GAP;
    top_panel_col_widths(columns, show_codes).iter().sum::<u16>() + gaps
}

/// The Top Jobbers panel's natural outer width (codes shown): columns + gaps +
/// padding + borders, with a floor so the title stays readable. This drives the
/// block sizing, so codes are dropped only when the terminal can't fit this.
fn top_panel_width(columns: &[RankedColumn]) -> u16 {
    // Floor so the title stays readable when no jobbers have fetched stats yet.
    const FLOOR: u16 = offset_title_width("Top Jobbers");
    (top_panel_inner_width(columns, true) + 4).max(FLOOR)
}

fn render_top_panel(frame: &mut Frame, region: Rect, columns: &[RankedColumn], focused: bool) {
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

    // Responsive degradation: show the EEE/SSS codes only while the full layout
    // fits the available width; when it doesn't, drop the codes first (names — and
    // the merged-column marker — stay). The block width comes from the natural
    // (codes-shown) width, so this only triggers when the terminal is too narrow.
    let show_codes = top_panel_inner_width(columns, true) <= inner.width;
    let col_w: Vec<u16> = top_panel_col_widths(columns, show_codes);

    // Interleave a gap between each pair of columns, with equal slack on both
    // sides so the column group sits centered when the panel is wider than its
    // content (e.g. when the panes below dictate the block width).
    let mut constraints: Vec<Constraint> = Vec::with_capacity(columns.len() * 2 + 1);
    constraints.push(Constraint::Fill(1));
    for (i, w) in col_w.iter().enumerate() {
        if i > 0 {
            constraints.push(Constraint::Length(COLUMN_GAP));
        }
        constraints.push(Constraint::Length(*w));
    }
    constraints.push(Constraint::Fill(1));
    let cols = Layout::horizontal(constraints).split(inner);

    for (ci, column) in columns.iter().enumerate() {
        let detail = detail_width(column.marked, show_codes);
        let name_w = (col_w[ci] as usize)
            .saturating_sub(if detail > 0 { NAME_CODE_GAP + detail } else { 0 });

        let mut lines: Vec<Line> = Vec::with_capacity(column.rows.len() + 1);
        lines.push(
            Line::from(Span::styled(column.header, Style::default().bold().underlined())).centered(),
        );
        for j in &column.rows {
            let mut spans = vec![Span::raw(format!("{:<name_w$}", truncate(&j.name, name_w)))];
            if show_codes {
                spans.push(Span::raw(" ".repeat(NAME_CODE_GAP)));
                spans.push(Span::styled(experience_abbr(j.experience), experience_style(j.experience)));
                spans.push(Span::raw("/"));
                spans.push(Span::styled(standing_abbr(j.standing), standing_style(j.standing)));
                // Merged columns flag which puzzle the jobber is strongest at.
                if let Some(m) = j.marker {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(m.to_string(), Style::default().fg(Color::DarkGray)));
                }
            } else if let Some(m) = j.marker {
                // Codes dropped for width, but the marker is "which puzzle", not a
                // standing/experience, so keep it.
                spans.push(Span::raw(" ".repeat(NAME_CODE_GAP)));
                spans.push(Span::styled(m.to_string(), Style::default().fg(Color::DarkGray)));
            }
            lines.push(Line::from(spans));
        }

        // Columns are laid out as [Fill, col, gap, col, gap, …, Fill] — the real
        // column areas start after the leading spacer at odd indices.
        frame.render_widget(Paragraph::new(lines), cols[1 + ci * 2]);
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

// ---------------------------------------------------------------------------
// Pirate-stats popup
// ---------------------------------------------------------------------------

/// One skill table row: skill name, then Experience and Standing spelled out in
/// their own columns, each emphasised like the Top Jobbers codes.
fn skill_row(
    skill_w: usize,
    exp_w: usize,
    sta_w: usize,
    name: &str,
    exp: Experience,
    standing: Standing,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{name:<skill_w$}")),
        Span::raw("  "),
        Span::styled(format!("{:<exp_w$}", exp.to_string()), experience_style(exp)),
        Span::raw("  "),
        Span::styled(format!("{:<sta_w$}", standing.to_string()), standing_style(standing)),
    ])
}

/// The pirate-stats popup: name, crew/flag boxes, three skill tables, and the
/// [See Trophies] / [Close] buttons. Sized to its content.
fn render_pirate_popup(
    frame: &mut Frame,
    pp: &PiratePopup,
    cache: &PirateCache,
    page_focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let screen = frame.area();
    let cached = cache.get_cached(&pp.name);

    // Buttons line (always present); compute its width up front.
    let see = "[ See Trophies ]";
    let close = "[ Close ]";
    const BTN_GAP: usize = 3;
    let buttons_w = see.len() + BTN_GAP + close.len();

    // --- Build the unboxed crew/flag columns (no header label). Each column's
    //     width is its widest line. ---
    let muted = Style::default().fg(Color::DarkGray);
    let (crew_lines, crew_w) = match cached.and_then(|c| c.basic.crew().map(|cr| (c, cr))) {
        Some((c, cr)) => {
            // Rank styled via CrewRank; the duty role (if any) renders plain. With
            // a role the affiliation spans three lines:
            //   [Rank] and / [Role] of / [Crew]
            // without one, two:  [Rank] of / [Crew].
            let rank_label = c.basic.crew_rank.trim().to_string();
            let name_w = cr.name.chars().count();
            match &cr.role {
                Some(role) => {
                    let l1_w = rank_label.chars().count() + " and".len();
                    let l1 = Line::from(vec![
                        Span::styled(rank_label, cr.rank.style()),
                        Span::raw(" and"),
                    ])
                    .centered();
                    let l2_text = format!("{role} of");
                    let l2_w = l2_text.chars().count();
                    let l2 = Line::from(l2_text).centered();
                    let l3 = Line::from(cr.name.clone()).centered();
                    (vec![l1, l2, l3], l1_w.max(l2_w).max(name_w) as u16)
                }
                None => {
                    let l1_w = rank_label.chars().count() + " of".len();
                    let l1 = Line::from(vec![
                        Span::styled(rank_label, cr.rank.style()),
                        Span::raw(" of"),
                    ])
                    .centered();
                    let l2 = Line::from(cr.name.clone()).centered();
                    (vec![l1, l2], l1_w.max(name_w) as u16)
                }
            }
        }
        None => (
            vec![Line::from(Span::styled("No crew", muted)).centered()],
            "No crew".len() as u16,
        ),
    };
    let (flag_lines, flag_w) = match cached.and_then(|c| c.basic.flag().map(|fl| (c, fl))) {
        Some((c, fl)) => {
            let title_label = c.basic.flag_rank.trim().to_string();
            let tail = " of".to_string();
            let l1_w = title_label.chars().count() + tail.chars().count();
            let l1 = Line::from(vec![
                Span::styled(title_label, fl.title.style()),
                Span::raw(tail),
            ])
            .centered();
            let l2 = Line::from(fl.name.clone()).centered();
            (vec![l1, l2], l1_w.max(fl.name.chars().count()) as u16)
        }
        None => (
            vec![Line::from(Span::styled("No flag", muted)).centered()],
            "No flag".len() as u16,
        ),
    };
    // The two columns split the row into equal halves with a 2-space gap. Each
    // side reserves the wider column's content width so they stay symmetric; when
    // the popup is wider they expand to fill, content centered within each half.
    const AFFIL_GAP: u16 = 2;
    let affil_w = 2 * crew_w.max(flag_w) + AFFIL_GAP;
    // The crew column may be 3 lines tall (with a role); the row fits the taller.
    let affil_h = crew_lines.len().max(flag_lines.len()) as u16;

    // --- Build the skill tables (one per family, in popup order). ---
    let sections: [(&str, &[Skill]); 3] = [
        ("Piracy Skills", PIRACY_SKILLS),
        ("Crafting Skills", CRAFTING_SKILLS),
        ("Carousing Skills", CAROUSING_SKILLS),
    ];
    // Column widths for the skill tables: skill name, then the spelled-out
    // Experience and Standing words.
    let (skill_w, exp_w, sta_w) = cached
        .map(|c| {
            c.basic.skills.iter().fold((0, 0, 0), |(sw, ew, stw), (s, r)| {
                (
                    sw.max(s.to_string().chars().count()),
                    ew.max(r.experience.to_string().chars().count()),
                    stw.max(r.standing.to_string().chars().count()),
                )
            })
        })
        .unwrap_or((0, 0, 0));

    let mut skill_lines: Vec<Line> = Vec::new();
    let mut table_title_w = 0usize;
    match cached {
        None => skill_lines.push(Line::from(Span::styled("Stats not loaded yet.", muted)).centered()),
        Some(c) => {
            let mut first = true;
            for (title, skills) in sections {
                let present: Vec<Skill> = skills
                    .iter()
                    .copied()
                    .filter(|s| c.basic.skills.contains_key(s))
                    .collect();
                if present.is_empty() {
                    continue;
                }
                if !first {
                    skill_lines.push(Line::from(""));
                }
                first = false;
                table_title_w = table_title_w.max(title.len());
                skill_lines.push(
                    Line::from(Span::styled(title, Style::default().bold().underlined())).centered(),
                );
                for s in present {
                    let rec = &c.basic.skills[&s];
                    skill_lines.push(skill_row(
                        skill_w,
                        exp_w,
                        sta_w,
                        &s.to_string(),
                        rec.experience,
                        rec.standing,
                    ));
                }
            }
            if skill_lines.is_empty() {
                skill_lines.push(Line::from(Span::styled("No skills recorded.", muted)).centered());
            }
        }
    }
    let skills_h = skill_lines.len() as u16;
    let skill_row_w = if skill_w > 0 {
        skill_w + 2 + exp_w + 2 + sta_w
    } else {
        0
    };
    // Width of the skills section as a block (widest row / title / fallback line),
    // so the whole section can be centered within the popup.
    let skills_block_w = skill_row_w
        .max(table_title_w)
        .max("Stats not loaded yet.".len())
        .max("No skills recorded.".len());

    // --- Geometry ---
    let content_w = (pp.name.chars().count())
        .max(affil_w as usize)
        .max(skills_block_w)
        .max(buttons_w) as u16;
    let box_w = (content_w + 4).min(screen.width.max(1));
    // name + gap + affil + gap + skills + gap + buttons, plus borders(2).
    let box_h = (1 + 1 + affil_h + 1 + skills_h + 1 + 1 + 2).min(screen.height.max(1));
    let x = screen.x + screen.width.saturating_sub(box_w) / 2;
    let y = screen.y + screen.height.saturating_sub(box_h) / 2;
    let popup = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Pirate").0);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1),       // name
        Constraint::Length(1),       // gap
        Constraint::Length(affil_h), // crew | flag
        Constraint::Length(1),       // gap
        Constraint::Min(0),          // skills
        Constraint::Length(1),       // gap
        Constraint::Length(1),       // buttons
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(pp.name.clone(), Style::default().bold())).centered()),
        rows[0],
    );

    // Crew | Flag (unboxed): two equal halves split by a 2-space gap, each filling
    // its side with the content centered.
    let affil_cols = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(AFFIL_GAP),
        Constraint::Fill(1),
    ])
    .split(rows[2]);
    frame.render_widget(Paragraph::new(crew_lines), affil_cols[0]);
    frame.render_widget(Paragraph::new(flag_lines), affil_cols[2]);

    // Center the whole skills section within the popup width.
    let sb_w = (skills_block_w as u16).min(rows[4].width);
    let sb_x = rows[4].x + rows[4].width.saturating_sub(sb_w) / 2;
    frame.render_widget(
        Paragraph::new(skill_lines),
        Rect::new(sb_x, rows[4].y, sb_w, rows[4].height),
    );

    // Buttons, centered; each gets a click region.
    let see_style = button_style(page_focused, pp.button == 0);
    let close_style = button_style(page_focused, pp.button == 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(see, see_style),
            Span::raw(" ".repeat(BTN_GAP)),
            Span::styled(close, close_style),
        ])
        .centered()),
        rows[6],
    );
    let start_x = rows[6].x + rows[6].width.saturating_sub(buttons_w as u16) / 2;
    regions.push(ClickRegion {
        rect: Rect::new(start_x, rows[6].y, see.len() as u16, 1),
        target: ClickTarget::JobberPirateSeeTrophies,
    });
    regions.push(ClickRegion {
        rect: Rect::new(
            start_x + (see.len() + BTN_GAP) as u16,
            rows[6].y,
            close.len() as u16,
            1,
        ),
        target: ClickTarget::JobberPirateClose,
    });
}

/// Button emphasis: highlighted when focused, bold otherwise.
fn button_style(page_focused: bool, active: bool) -> Style {
    if page_focused && active {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    }
}

// ---------------------------------------------------------------------------
// Trophies popup
// ---------------------------------------------------------------------------

/// Whether a (lowercased) trophy name matches the (lowercased) search needle:
/// a plain substring hit, or a fuzzy Jaro-Winkler match against the whole name or
/// any single word in it (so typos and partial words still surface trophies).
fn trophy_matches(name_lower: &str, needle: &str) -> bool {
    const FUZZY: f64 = 0.82;
    name_lower.contains(needle)
        || text_similarity(needle, name_lower) >= FUZZY
        || name_lower
            .split_whitespace()
            .any(|w| text_similarity(needle, w) >= FUZZY)
}

/// Center `s` within `width` columns (truncating if it somehow overflows).
fn center_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return truncate(s, width);
    }
    let left = (width - len) / 2;
    let right = width - len - left;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
}

/// Build a category's lines for the trophies popup: a centered name, then the
/// matching trophies laid out in 3 centered, word-wrapped columns. Returns an
/// empty vec when nothing in the category matches `search`.
fn trophy_section_lines(section: &TrophySection, search: &str, inner_w: usize) -> Vec<Line<'static>> {
    let needle = search.trim().to_lowercase();
    let mut names: Vec<&String> = section
        .trophies
        .iter()
        .filter(|t| needle.is_empty() || trophy_matches(&t.to_lowercase(), &needle))
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    names.sort_unstable();

    const GAP: usize = 2;
    let col_w = inner_w.saturating_sub(2 * GAP) / 3;
    let col_w = col_w.max(1);

    // Uncategorised trophies render under an italic "Ungrouped" heading.
    let (title_text, title_style) = if section.category.trim().is_empty() {
        ("Ungrouped".to_string(), Style::default().bold().underlined().italic())
    } else {
        (section.category.clone(), Style::default().bold().underlined())
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(title_text, title_style)).centered());

    // Lay out row-major in threes. The final short row centers oddly per spec:
    // two left → left & right columns; one left → the middle column. Each grid row
    // is followed by a blank line so trophies are visually separated.
    let mut i = 0;
    while i < names.len() {
        let remaining = names.len() - i;
        // Which column each of this row's cells lands in.
        let slots: [Option<&String>; 3] = if remaining == 1 {
            [None, Some(names[i]), None]
        } else if remaining == 2 {
            [Some(names[i]), None, Some(names[i + 1])]
        } else {
            [Some(names[i]), Some(names[i + 1]), Some(names[i + 2])]
        };
        let consumed = remaining.min(3);

        // Wrap each cell, then stack the columns to the tallest cell.
        let wrapped: Vec<Vec<String>> = slots
            .iter()
            .map(|s| s.map(|t| wrap_words(t, col_w)).unwrap_or_default())
            .collect();
        let rows = wrapped.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0..rows {
            let mut cells: Vec<String> = Vec::with_capacity(3);
            for col in &wrapped {
                let text = col.get(r).map(String::as_str).unwrap_or("");
                cells.push(center_to(text, col_w));
            }
            lines.push(Line::from(cells.join(&" ".repeat(GAP))));
        }
        lines.push(Line::from("")); // blank under each row
        i += consumed;
    }
    lines
}

/// The trophies popup: 80 wide, a pinned search box, then the pirate's trophy
/// categories (each a centered name + 3-column grid), vertically scrollable.
fn render_trophy_popup(
    frame: &mut Frame,
    tp: &mut TrophyPopup,
    cache: &PirateCache,
    regions: &mut Vec<ClickRegion>,
) {
    let screen = frame.area();
    let box_w = 80u16.min(screen.width.max(1));
    let box_h = screen.height.saturating_sub(2).max(3).min(screen.height.max(1));
    let x = screen.x + screen.width.saturating_sub(box_w) / 2;
    let y = screen.y + screen.height.saturating_sub(box_h) / 2;
    let popup = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Trophies").0);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1), // search
        Constraint::Min(0),    // scroll area
    ])
    .split(inner);

    // Search box (typed text shows live; the placeholder is muted).
    let search_line = if tp.search.is_empty() {
        Line::from(vec![
            Span::styled("Search: ", Style::default().bold()),
            Span::styled("type to filter…", Style::default().fg(Color::DarkGray).italic()),
        ])
    } else {
        Line::from(vec![
            Span::styled("Search: ", Style::default().bold()),
            Span::raw(tp.search.clone()),
        ])
    };
    frame.render_widget(Paragraph::new(search_line), rows[0]);

    // Build all visible category lines.
    let inner_w = rows[1].width as usize;
    let mut lines: Vec<Line> = Vec::new();
    match cache.get_cached(&tp.name) {
        None => lines.push(
            Line::from(Span::styled("Trophies not loaded yet.", Style::default().fg(Color::DarkGray)))
                .centered(),
        ),
        Some(c) => {
            // Two blank lines separate one group from the next.
            let mut first = true;
            for section in &c.trophies.sections {
                let sec = trophy_section_lines(section, &tp.search, inner_w);
                if sec.is_empty() {
                    continue;
                }
                if !first {
                    lines.push(Line::from(""));
                }
                first = false;
                lines.extend(sec);
            }
            if lines.is_empty() {
                lines.push(
                    Line::from(Span::styled("No matching trophies.", Style::default().fg(Color::DarkGray)))
                        .centered(),
                );
            }
        }
    }

    // Clamp scroll, then render the visible window. Record the view height so the
    // key handler can scroll by half a page.
    let view_h = rows[1].height as usize;
    tp.view_h = view_h;
    let max_off = lines.len().saturating_sub(view_h);
    if tp.offset > max_off {
        tp.offset = max_off;
    }
    let visible: Vec<Line> = lines.into_iter().skip(tp.offset).take(view_h).collect();
    frame.render_widget(Paragraph::new(visible), rows[1]);

    // The whole popup is a scroll target so the wheel works anywhere over it.
    regions.push(ClickRegion {
        rect: popup,
        target: ClickTarget::JobberTrophyArea,
    });
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
