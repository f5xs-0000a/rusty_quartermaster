use std::collections::HashMap;

use crossterm::event::{
    KeyCode,
    KeyEvent,
    MouseButton,
    MouseEvent,
    MouseEventKind,
};
use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

use crate::{
    aliases,
    api::{CachedOffers, Commodity, fetch_offers_for},
    bare,
    chatlog::GameState,
    clickmap::{self, ClickRegion, ClickTarget},
    damage::DamageApp,
    jobbers::{
        self,
        JobberFocus,
        JobberPane,
        JobbersUi,
        PirateCache,
        PiratePopup,
        SkillDistPopup,
        TrophyPopup,
        VOYAGE_TYPES,
        VoyageType,
    },
    map::{
        MapApp,
        data::{Map, Point},
    },
    ocean::Ocean,
    profits::ProfitsApp,
    utils::text_similarity,
};

// ---------------------------------------------------------------------------
// App routing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum AppId {
    Profits,
    Damage,
    Chatlog,
    Voyage,
    Map,
    Exit,
}

impl AppId {
    /// Two-line top-bar label. Single-word apps sit on the upper line with an
    /// empty lower line; only "Voyage Statistics" wraps onto both lines.
    fn bar_lines(self) -> (&'static str, &'static str) {
        match self {
            AppId::Profits => ("Profits", ""),
            AppId::Damage => ("Damage", ""),
            AppId::Chatlog => ("Jobbers", ""),
            AppId::Voyage => ("Voyage", "Statistics"),
            AppId::Map => ("Map", ""),
            AppId::Exit => ("Exit", ""),
        }
    }

    /// Columns the label occupies: the wider of its two lines.
    fn label_width(self) -> u16 {
        let (upper, lower) = self.bar_lines();
        upper.len().max(lower.len()) as u16
    }
}

pub const APP_LIST: &[AppId] = &[
    AppId::Profits,
    AppId::Damage,
    AppId::Chatlog,
    AppId::Voyage,
    AppId::Map,
    AppId::Exit,
];

/// Index of the Exit app in `APP_LIST` (where the universal Esc lands).
fn exit_index() -> usize {
    APP_LIST.iter().position(|a| *a == AppId::Exit).unwrap()
}

/// Whether `target` is one of the Damage-calculator click variants (so the Sea
/// Battles popup can claim them for its embedded editor).
fn is_damage_target(target: &ClickTarget) -> bool {
    matches!(
        target,
        ClickTarget::DamageCell { .. }
            | ClickTarget::DamageIncrement { .. }
            | ClickTarget::DamageDecrement { .. }
            | ClickTarget::DamageRam
            | ClickTarget::DamageRamIncrement
            | ClickTarget::DamageRamDecrement
            | ClickTarget::DamageShipItem(_)
            | ClickTarget::DamageResetYes
            | ClickTarget::DamageResetNo
    )
}

/// Top bar: two label lines, no border (a shaded strip).
const TOPBAR_HEIGHT: u16 = 2;

/// Blank columns each side of a top-bar label, the padding every widget keeps
/// between its contents and its edge.
const TOPBAR_PADDING: u16 = 1;

/// Shortest terminal that can hold the bar and a page at all. Whether the page
/// that is open fits the rows left over is its own call (see
/// [`crate::utils::too_short`]); this is only the floor below which there is no
/// room to say so.
const MIN_HEIGHT: u16 = TOPBAR_HEIGHT + 1;

/// Columns the top bar needs before a label would be clipped: every label with
/// its padding. Slots are allowed to differ in width; a label is never clipped
/// to keep them equal.
fn topbar_min_width() -> u16 {
    APP_LIST
        .iter()
        .map(|app| app.label_width() + 2 * TOPBAR_PADDING)
        .sum()
}

/// Width of each top-bar slot at `width`: its label and padding, plus an even
/// share of whatever is left over so the bar still spans the full width.
fn topbar_slots(width: u16) -> Vec<u16> {
    let mut slots: Vec<u16> = APP_LIST
        .iter()
        .map(|app| app.label_width() + 2 * TOPBAR_PADDING)
        .collect();
    let mut slack = width.saturating_sub(slots.iter().sum());
    let share = slack / slots.len() as u16;
    for slot in slots.iter_mut() {
        *slot += share;
        slack -= share;
    }
    // Whatever doesn't divide evenly goes out a column at a time.
    for slot in slots.iter_mut().take(slack as usize) {
        *slot += 1;
    }
    slots
}

#[derive(PartialEq)]
enum GlobalFocus {
    TopBar,
    Content,
}

// ---------------------------------------------------------------------------
// InputResult — sub-app → AppShell communication
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum FetchPurpose {
    Islands,
    Profits,
}

pub enum InputResult {
    Consumed,
    Exit,
    StartFetch(FetchPurpose),
    RebuildIslands,
}

// ---------------------------------------------------------------------------
// Shared state bundle (passed to sub-app methods)
// ---------------------------------------------------------------------------

pub struct SharedState<'a> {
    pub commodities: &'a [Commodity],
    pub cached_offers: &'a HashMap<String, CachedOffers>,
    pub available_islands: &'a [String],
    /// The selected ocean's geography (archipelago → island membership) from
    /// the baked-in [bare cache](crate::bare), used to resolve a
    /// Restocking-field query that names an archipelago rather than a
    /// single island. `None` when no ocean is selected or it isn't present
    /// in the bare cache.
    pub ocean_geo: Option<&'static bare::Ocean>,
    pub loading: bool,
    /// Whether the selected ocean has Market market data (profit calc
    /// works).
    pub market_supported: bool,
    /// Gross PoE plundered, PoE stolen from us, and the retained booty chest
    /// (per-fight halves) over the current pillage, from the battle ledger.
    /// Drive the auto-deduced booty-chest figure on the Profits page. See
    /// [`crate::chatlog::GameState::current_pillage_poe`].
    pub pillage_gross: u64,
    pub pillage_stolen: u64,
    pub pillage_chest: u64,
}

/// Bundle of inputs for [`AppShell::assemble_voyage_view`] — the resolved
/// voyage plus its pager framing and sourcing flags. Grouped into one struct so
/// the live and historical builders share one assembly path without a
/// 12-argument call.
struct AssembleView<'a> {
    vessel_name: Option<String>,
    ship_type: Option<String>,
    period: Option<String>,
    elapsed_secs: Option<i64>,
    voyage: &'a crate::voyage::Voyage,
    confirmed: bool,
    consumption: crate::voyage::stats::ConsumptionStats,
    read_only: bool,
    saveable: bool,
    badge: crate::voyage::ui::VoyageBadge,
    page: usize,
    page_count: usize,
}

// ---------------------------------------------------------------------------
// Free functions operating on shared data
// ---------------------------------------------------------------------------

/// The voyage's clock span for the Voyage Statistics header: the start date and
/// time, then the end time, e.g. `"2026-07-05 15:09 to 17:32"`. The date is
/// shown once on the start (a run rarely crosses midnight; if it does, only the
/// start date is labelled).
fn voyage_period(
    start: chrono::NaiveDateTime,
    end: chrono::NaiveDateTime,
) -> String {
    format!(
        "{} to {}",
        start.format("%Y-%m-%d %H:%M"),
        end.format("%H:%M")
    )
}

pub fn commod_name(commodities: &[Commodity], id: u64) -> &str {
    commodities
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.name.as_str())
        .unwrap_or("???")
}

pub fn suggest_island<'a>(
    query: &str,
    available_islands: &'a [String],
) -> Option<&'a str> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }

    // Alias lookup
    if let Some(&target) = aliases::get_islands().get(query.as_str())
        && let Some(island) = available_islands
            .iter()
            .find(|i| i.eq_ignore_ascii_case(target))
    {
        return Some(island);
    }

    // Exact match
    if let Some(island) = available_islands
        .iter()
        .find(|i| i.eq_ignore_ascii_case(&query))
    {
        return Some(island);
    }

    // Unique prefix
    let prefix_matches: Vec<_> = available_islands
        .iter()
        .filter(|i| i.to_lowercase().starts_with(&query))
        .collect();
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0]);
    }

    // Jaro-Winkler (minimum 0.75, unique winner)
    let mut best_score = f64::NEG_INFINITY;
    let mut best = None;
    let mut tie = false;
    for island in available_islands {
        let score = text_similarity(&query, &island.to_lowercase());
        if best_score < score {
            best_score = score;
            best = Some(island.as_str());
            tie = false;
        } else if score == best_score {
            tie = true;
        }
    }
    if 0.75 <= best_score && !tie {
        return best;
    }

    None
}

/// Match `query` to one of an ocean's archipelagos by exact name, unique
/// prefix, then a strict Jaro-Winkler (≥0.85 — stricter than [`suggest_island`]
/// so an archipelago never gets fuzzily "stolen" out from under an island the
/// user actually meant). No alias table: archipelago names are few and
/// distinctive.
pub fn suggest_archipelago<'a>(
    query: &str,
    ocean: &'a bare::Ocean,
) -> Option<&'a bare::Archipelago> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }

    // Exact match
    if let Some(arch) = ocean
        .archipelagos
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&query))
    {
        return Some(arch);
    }

    // Unique prefix
    let prefix_matches: Vec<_> = ocean
        .archipelagos
        .iter()
        .filter(|a| a.name.to_lowercase().starts_with(&query))
        .collect();
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0]);
    }

    // Jaro-Winkler (minimum 0.85, unique winner)
    let mut best_score = f64::NEG_INFINITY;
    let mut best = None;
    let mut tie = false;
    for arch in &ocean.archipelagos {
        let score = text_similarity(&query, &arch.name.to_lowercase());
        if best_score < score {
            best_score = score;
            best = Some(arch);
            tie = false;
        } else if score == best_score {
            tie = true;
        }
    }
    if 0.85 <= best_score && !tie {
        return best;
    }

    None
}

/// What the Restocking field resolves to. The field accepts either a single
/// island *or* a whole archipelago; a blank field prices ocean-wide.
pub enum RestockScope {
    /// Blank field — no location filter; restock at the cheapest offers
    /// ocean-wide.
    OceanWide,
    /// One island: restock offers must be on this island.
    Island(String),
    /// A whole archipelago: restock offers may be on any of its islands.
    Archipelago {
        name: String,
        islands: Vec<String>,
    },
    /// Non-blank text that matched neither an island nor an archipelago.
    Unknown,
}

impl RestockScope {
    /// The island names offers must be on, or `None` for no restriction
    /// (ocean-wide / unrecognized). A single-island scope yields one name; an
    /// archipelago yields all of its islands.
    pub fn island_filter(&self) -> Option<&[String]> {
        match self {
            RestockScope::Island(name) => Some(std::slice::from_ref(name)),
            RestockScope::Archipelago {
                islands,
                ..
            } => Some(islands),
            RestockScope::OceanWide | RestockScope::Unknown => None,
        }
    }
}

/// Resolve a Restocking-field query to a [`RestockScope`]. An archipelago name
/// (exact/prefix/strict-fuzzy) takes precedence, since archipelago names are
/// distinctive and won't collide with the island most users type; anything else
/// non-blank is handed to [`suggest_island`].
pub fn resolve_restock_scope(
    query: &str,
    available_islands: &[String],
    ocean_geo: Option<&bare::Ocean>,
) -> RestockScope {
    if query.trim().is_empty() {
        return RestockScope::OceanWide;
    }
    if let Some(arch) = ocean_geo.and_then(|o| suggest_archipelago(query, o)) {
        return RestockScope::Archipelago {
            name: arch.name.clone(),
            islands: arch.islands.iter().map(|i| i.name.clone()).collect(),
        };
    }
    if let Some(island) = suggest_island(query, available_islands) {
        return RestockScope::Island(island.to_owned());
    }
    RestockScope::Unknown
}

pub fn rebuild_island_list(
    cached_offers: &HashMap<String, CachedOffers>,
    commod_names: &[String],
) -> Vec<String> {
    let mut islands = Vec::new();
    for name in commod_names {
        let Some(cached) = cached_offers.get(name.as_str()) else {
            continue;
        };
        for offer in &cached.offers {
            if 0 < offer.sellprice
                && 0 < offer.sellqty
                && !islands.contains(&offer.islandname)
            {
                islands.push(offer.islandname.clone());
            }
        }
    }
    islands.sort();
    islands
}

/// The Exit app: a single centered prompt. Pressing Enter or Esc while it is
/// the open app quits the program.
fn render_exit(frame: &mut Frame, area: Rect) {
    let bold = |s: &'static str| Span::styled(s, Style::default().bold());

    // Footer/credits pinned to the bottom, with one blank line below it.
    let footer = vec![
        Line::from(vec![
            bold("Rusty Quartermaster"),
            Span::raw(" by "),
            bold("F5XS"),
        ]),
        Line::from(""),
        Line::from(vec![
            bold("Puzzle Pirates"),
            Span::raw(" is a trademark of "),
            bold("Grey Havens"),
            Span::raw(" and is used without permission."),
        ]),
        Line::from(vec![
            bold("Rusty Quartermaster"),
            Span::raw(" is an unofficial fan tool and is not affiliated with "),
            bold("Grey Havens"),
            Span::raw(", "),
            bold("Three Rings"),
            Span::raw(", or "),
            bold("Sega"),
            Span::raw("."),
        ]),
    ];

    // Vertically center the prompt; let the footer sit at the bottom. The
    // footer block is generously sized so the long disclaimer line can
    // wrap.
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Press Enter or Esc to Exit.",
            Style::default().bold(),
        ))
        .centered(),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(footer).centered().wrap(Wrap {
            trim: true,
        }),
        rows[3],
    );
}

// ---------------------------------------------------------------------------
// AppShell
// ---------------------------------------------------------------------------

pub struct AppShell {
    pub commodities: Vec<Commodity>,
    pub cached_offers: HashMap<String, CachedOffers>,
    pub available_islands: Vec<String>,
    pub loading: bool,
    /// Selected ocean, or `None` if the user skipped selection.
    pub ocean: Option<Ocean>,
    /// Whether Market querying is enabled at all (the `--query-market`
    /// flag). Gated together with the ocean's Market support.
    pub query_market: bool,
    // app routing
    sidebar_index: usize,
    global_focus: GlobalFocus,
    // per-app state
    pub profits: ProfitsApp,
    pub damage: DamageApp,
    pub chatlog: GameState,
    pub pirate_cache: PirateCache,
    pub jobbers_ui: JobbersUi,
    pub voyage_ui: crate::voyage::ui::VoyageStatsUi,
    pub map: MapApp,
    /// Yoweb's list of the selected ocean's colonized islands, from the
    /// cache or this run's fetch; `None` until first fetched.
    pub islands: Option<crate::islands::CachedIslands>,
    /// Whether the Map page has asked for the island list and the fetch has
    /// not come back yet (the event loop runs it).
    pub island_list_wanted: bool,
    /// What the user has done and learned, loaded from / written to
    /// `persistence_path`: the voyage history and per-pirate memorization.
    pub persistence: crate::persistence::SavedPersistence,
    /// Where that lives on disk (set by `--persistence`).
    pub persistence_path: Option<std::path::PathBuf>,
    // click regions rebuilt each render
    click_regions: Vec<ClickRegion>,
}

impl AppShell {
    pub fn new(commodities: Vec<Commodity>) -> Self {
        Self {
            commodities,
            cached_offers: HashMap::new(),
            available_islands: Vec::new(),
            loading: false,
            ocean: None,
            query_market: true,
            sidebar_index: 0,
            // Start on the top bar with Profits selected.
            global_focus: GlobalFocus::TopBar,
            profits: ProfitsApp::new(),
            damage: DamageApp::new(),
            chatlog: GameState::new(),
            pirate_cache: PirateCache::new(),
            jobbers_ui: JobbersUi::default(),
            voyage_ui: crate::voyage::ui::VoyageStatsUi::default(),
            map: MapApp::new(),
            islands: None,
            island_list_wanted: false,
            persistence: crate::persistence::SavedPersistence::default(),
            persistence_path: None,
            click_regions: Vec::new(),
        }
    }

    /// Whether profit calculation is available: Market querying is enabled
    /// *and* the selected ocean has Market data. If either is false we never
    /// hit Market.
    fn market_ok(&self) -> bool {
        self.query_market
            && self.ocean.is_some_and(Ocean::market_supported)
    }

    /// The selected ocean's geography from the baked-in [bare
    /// cache](crate::bare), if any. Feeds [`SharedState::ocean_geo`] so
    /// archipelago restock filtering can resolve names.
    pub fn ocean_geo(&self) -> Option<&'static bare::Ocean> {
        self.ocean.and_then(|o| bare::BARE.ocean(o.name()))
    }

    /// The selected ocean's compiled-in map for the Map page, if any.
    fn ocean_map(&self) -> Option<&'static Map> {
        self.ocean.and_then(|o| Map::for_ocean(o.name()))
    }

    /// Put the Map page's cursor back where the last run left it. A point
    /// that is no longer on the ocean's map is dropped, so a cursor cannot
    /// land in open water when the map data changes; the page then opens on
    /// its own default.
    pub fn restore_map_cursor(&mut self, at: Option<Point>) {
        let Some((map, p)) = self.ocean_map().zip(at) else {
            return;
        };
        if map.points().contains(&p) {
            self.map.cursor = Some(p);
        }
    }

    /// Fold the Map page's memorization into the persisted data, under the
    /// pirate and ocean it belongs to. A run with no pirate or no ocean has
    /// nowhere to put it and leaves the file's own marks alone.
    pub fn save_memorization(&mut self) {
        if let (Some(ocean), Some(pirate)) =
            (self.ocean, self.map.pirate.as_deref())
        {
            self.persistence.set_memorized(
                ocean.name(),
                pirate,
                self.map.memorized.clone(),
            );
        }
    }

    /// The Map page was opened: ask for the ocean's island list if it has
    /// never been fetched or has gone stale. Nothing is fetched at startup;
    /// opening the page is the trigger, and a failed fetch is retried the
    /// next time it is opened.
    fn open_map(&mut self) {
        if self.ocean.is_some()
            && self
                .islands
                .as_ref()
                .is_none_or(|c| c.is_stale(chrono::Utc::now()))
        {
            self.island_list_wanted = true;
        }
    }

    /// Fold a finished island-list fetch into the shell.
    pub fn apply_island_list(
        &mut self,
        result: Result<crate::islands::CachedIslands, String>,
    ) {
        self.island_list_wanted = false;
        match result {
            Ok(list) => self.islands = Some(list),
            Err(e) => crate::diag!("warning: island list: {e}"),
        }
    }

    pub fn rebuild_island_list(&mut self) {
        let commod_names: Vec<String> = self
            .profits
            .rows
            .iter()
            .map(|r| commod_name(&self.commodities, r.commod_id).to_owned())
            .collect();
        self.available_islands =
            rebuild_island_list(&self.cached_offers, &commod_names);
    }

    // -- rendering --

    pub fn render(&mut self, frame: &mut Frame) {
        self.click_regions.clear();

        let area = frame.area();

        // The shell's own floor is the bar and a row to draw a page in: short
        // or narrow of that there is no way left to reach a page at
        // all, so nothing is drawn but the notice. Whether the page
        // that is open fits the room left over is the page's own call.
        if area.width < topbar_min_width() || area.height < MIN_HEIGHT {
            self.render_too_small(frame, area);
            return;
        }

        // Layout tree: [top bar / content]. The top bar spans the full
        // terminal width; each page then centers its own fixed-width block in
        // the content area rather than stretching to fill it. Pages own
        // everything in their content area — the Jobbers page, for instance,
        // draws its own tooltip inside its centered block rather than as a
        // full-width strip here.
        let chunks = Layout::vertical([
            Constraint::Length(TOPBAR_HEIGHT),
            Constraint::Min(0),
        ])
        .split(area);

        self.render_topbar(frame, chunks[0]);

        let content_area = chunks[1];
        let content_focused = self.global_focus == GlobalFocus::Content;
        match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    ocean_geo: self.ocean_geo(),
                    loading: self.loading,
                    market_supported: self.market_ok(),
                    pillage_gross,
                    pillage_stolen,
                    pillage_chest,
                };
                crate::profits::ui::render(
                    frame,
                    content_area,
                    &mut self.profits,
                    &shared,
                    content_focused,
                    &mut self.click_regions,
                );
            }
            AppId::Damage => {
                crate::damage::ui::render(
                    frame,
                    content_area,
                    &mut self.damage,
                    content_focused,
                    &mut self.click_regions,
                );
            }
            AppId::Chatlog => {
                jobbers::render(
                    frame,
                    content_area,
                    &self.chatlog,
                    &self.pirate_cache,
                    &mut self.jobbers_ui,
                    content_focused,
                    &mut self.click_regions,
                );
            }
            AppId::Voyage => {
                let view = self.build_voyage_view();
                crate::voyage::ui::render(
                    frame,
                    content_area,
                    &view,
                    &mut self.voyage_ui,
                    content_focused,
                    &mut self.click_regions,
                );
            }
            AppId::Map => {
                let ctx = crate::map::ui::OceanContext {
                    map: self.ocean_map(),
                    geo: self.ocean_geo(),
                    ocean: self.ocean.map(Ocean::name),
                    islands: self.islands.as_ref(),
                    fetching_islands: self.island_list_wanted,
                };
                crate::map::ui::render(
                    frame,
                    content_area,
                    &mut self.map,
                    ctx,
                    content_focused,
                    &mut self.click_regions,
                );
            }
            AppId::Exit => {
                render_exit(frame, content_area);
            }
        }
    }

    /// Draw the full-width two-line top bar: a continuous shaded strip split
    /// into four equal slots. Each slot's whole box is shaded along a
    /// three-step brightness ramp — the open/selected app is brightest;
    /// while the bar is focused the other slots sit a step up from the
    /// resting shade; once focus drops into an app those others fall back
    /// to the base shade.
    /// A terminal too small for the app at all: nothing is drawn but a note
    /// asking for more room. Drawing a page here would drop widgets silently,
    /// which reads as the app being wrong rather than the window being small.
    ///
    /// The bar survives as long as it fits whole, since it still says what the
    /// app is; once a label would be clipped it goes too, a clipped bar being
    /// worse than none.
    fn render_too_small(&mut self, frame: &mut Frame, area: Rect) {
        let bar_fits =
            topbar_min_width() <= area.width && TOPBAR_HEIGHT < area.height;
        let body = if bar_fits {
            let chunks = Layout::vertical([
                Constraint::Length(TOPBAR_HEIGHT),
                Constraint::Min(0),
            ])
            .split(area);
            self.render_topbar(frame, chunks[0]);
            chunks[1]
        } else {
            area
        };

        let detail = format!(
            "Enlarge the window to at least {}x{MIN_HEIGHT} (it is {}x{}).",
            topbar_min_width(),
            area.width,
            area.height,
        );
        // Unlike a page's own refusal, this one cannot count on a bar above it.
        crate::utils::render_page_notice(
            frame,
            body,
            &[
                (
                    "Terminal too small",
                    Style::default().bold(),
                ),
                (
                    &detail,
                    Style::default().fg(Color::DarkGray),
                ),
            ],
        );
    }

    fn render_topbar(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.global_focus == GlobalFocus::TopBar;

        // Greyscale ramp: base (resting bar) → middle (bar focused) → strongest
        // (the open app). Backgrounds fill the entire slot box.
        let base = Style::default().bg(Color::DarkGray).fg(Color::White);
        let middle = Style::default().bg(Color::Gray).fg(Color::Black);
        let strongest =
            Style::default().bg(Color::White).fg(Color::Black).bold();

        // Slots side by side, contiguous (no gaps) so the shade reads as one
        // bar. Each is wide enough for its own label and padding before any
        // slack is shared out, so no label is ever clipped.
        let slots = Layout::horizontal(
            topbar_slots(area.width).into_iter().map(Constraint::Length),
        )
        .split(area);

        for (i, (&app_id, &slot)) in
            APP_LIST.iter().zip(slots.iter()).enumerate()
        {
            let style = if i == self.sidebar_index {
                strongest
            } else if focused {
                middle
            } else {
                base
            };

            let (upper, lower) = app_id.bar_lines();
            // Paragraph::style shades the whole slot box; centered text rides
            // on top of it.
            let para = Paragraph::new(vec![
                Line::from(upper),
                Line::from(lower),
            ])
            .style(style)
            .centered();
            frame.render_widget(para, slot);

            self.click_regions.push(ClickRegion {
                rect: slot,
                target: ClickTarget::SidebarItem(i),
            });
        }
    }

    // -- key handling --

    /// Returns true if the app should exit.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        tx: &tokio::sync::mpsc::UnboundedSender<
            Result<HashMap<String, CachedOffers>, String>,
        >,
    ) -> bool {
        // The Exit app lives entirely on the top bar — it has no content to
        // descend into. Selecting it shows its widget; Enter/Esc then quit.
        let on_exit = APP_LIST[self.sidebar_index] == AppId::Exit;

        // Universal Esc: from anywhere it selects the Exit app on the bar
        // (which shows its widget); pressed again while Exit is already
        // selected, it quits. A modal popup keeps first claim on Esc so
        // it stays closable.
        let popup_open = self.global_focus == GlobalFocus::Content
            && self.current_popup_open();
        if key.code == KeyCode::Esc && !popup_open {
            if on_exit {
                return true;
            }
            self.sidebar_index = exit_index();
            self.global_focus = GlobalFocus::TopBar;
            return false;
        }

        if self.global_focus == GlobalFocus::TopBar {
            return self.handle_topbar_key(key);
        }

        let result = match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    ocean_geo: self.ocean_geo(),
                    loading: self.loading,
                    market_supported: self.market_ok(),
                    pillage_gross,
                    pillage_stolen,
                    pillage_chest,
                };
                self.profits.handle_key(key, &shared)
            }
            AppId::Damage => self.damage.handle_key(key),
            AppId::Chatlog => self.handle_jobbers_key(key),
            AppId::Voyage => self.handle_voyage_key(key),
            AppId::Map => {
                let map = self.ocean_map();
                self.map.handle_key(key, map)
            }
            // Handled above (Exit-app keys quit / return to the bar).
            AppId::Exit => InputResult::Consumed,
        };

        match result {
            InputResult::Consumed => {}
            InputResult::Exit => {
                self.global_focus = GlobalFocus::TopBar;
            }
            InputResult::StartFetch(purpose) => {
                self.loading = true;
                self.spawn_fetch(purpose, tx);
            }
            InputResult::RebuildIslands => {
                self.rebuild_island_list();
            }
        }
        false
    }

    /// Whether the currently-open app has a modal popup open (which should keep
    /// first claim on Esc instead of the universal jump-to-Exit).
    fn current_popup_open(&self) -> bool {
        match APP_LIST[self.sidebar_index] {
            AppId::Profits => self.profits.popup.is_some(),
            AppId::Damage => {
                self.damage.popup.is_some()
                    || self.damage.battle_prompt.is_some()
            }
            AppId::Chatlog => {
                self.jobbers_ui.ship_popup.is_some()
                    || self.jobbers_ui.vessel_popup.is_some()
                    || self.jobbers_ui.voyage_popup.is_some()
                    || self.jobbers_ui.pirate_popup.is_some()
                    || self.jobbers_ui.trophy_popup.is_some()
                    || self.jobbers_ui.skill_dist_popup.is_some()
                    || self.jobbers_ui.per_fight_popup.is_some()
            }
            AppId::Voyage => {
                self.voyage_ui.prompt.is_some()
                    || self.voyage_ui.chart_popup.is_some()
                    || self.voyage_ui.battles_popup.is_some()
            }
            // The search prompt and the help popup own Esc while open.
            AppId::Map => self.map.search.is_some() || self.map.help,
            AppId::Exit => false,
        }
    }

    /// Key handling while the top bar is focused: ←/→ live-switch the app shown
    /// beneath, ↓/Enter drop focus into it. The Exit app is special — it has no
    /// content, so ↓ is a no-op there and Enter quits. (Esc is handled
    /// universally in `handle_key`.)
    fn handle_topbar_key(&mut self, key: KeyEvent) -> bool {
        let on_exit = APP_LIST[self.sidebar_index] == AppId::Exit;
        let last = APP_LIST.len() - 1;
        match key.code {
            // ←/→ wrap around the ends of the bar.
            KeyCode::Left => {
                self.sidebar_index = if self.sidebar_index == 0 {
                    last
                } else {
                    self.sidebar_index - 1
                };
            }
            KeyCode::Right => {
                self.sidebar_index = if self.sidebar_index == last {
                    0
                } else {
                    self.sidebar_index + 1
                };
            }
            // Enter drops into the selected app — except Exit, which has
            // nothing to enter, so Enter there quits.
            KeyCode::Enter => {
                if on_exit {
                    return true;
                }
                self.enter_app();
            }
            // ↓ descends into the app; the Exit app has nothing below it.
            KeyCode::Down if !on_exit => self.enter_app(),
            _ => {}
        }
        // Landing on Map shows the page, which counts as opening it.
        if APP_LIST[self.sidebar_index] == AppId::Map {
            self.open_map();
        }
        false
    }

    /// Drop focus from the bar into the selected app's content.
    fn enter_app(&mut self) {
        self.global_focus = GlobalFocus::Content;
        // Entering the Jobbers page lands on the Vessels button (the top
        // widget).
        self.jobbers_ui.focus = JobberFocus::Vessels;
        // Entering Profits lands on the topmost widget — the inventory table
        // (or the search box when the inventory is empty).
        self.profits.focus_table_top();
        if APP_LIST[self.sidebar_index] == AppId::Map {
            self.open_map();
        }
    }

    /// Voyage Statistics keys. With the save/discard prompt open it's modal
    /// (←/→ select, Enter confirm, S/D shortcut, Esc cancel). Otherwise: Esc
    /// returns to the bar, ↑/↓ (and PageUp/Down) move focus through the stat
    /// numbers and then the charts (the focused item's tooltip shows below the
    /// widget), Enter enlarges the focused chart, S/D open the save/discard
    /// prompt.
    fn handle_voyage_key(&mut self, key: KeyEvent) -> InputResult {
        use crate::voyage::ui::SaveChoice;

        // The Sea Battles popup is modal: it owns all keys until dismissed.
        if self.voyage_ui.battles_popup.is_some() {
            return self.handle_battles_key(key);
        }

        // Chart enlarge popup is modal: Esc/Enter close it.
        if self.voyage_ui.chart_popup.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.voyage_ui.chart_popup = None;
                self.voyage_ui.winrate_hover = None;
            }
            return InputResult::Consumed;
        }

        if let Some(choice) = self.voyage_ui.prompt {
            match key.code {
                KeyCode::Esc => self.voyage_ui.prompt = None,
                KeyCode::Left | KeyCode::Right => {
                    self.voyage_ui.prompt = Some(match choice {
                        SaveChoice::Cancel => SaveChoice::Save,
                        SaveChoice::Save => SaveChoice::Cancel,
                    });
                }
                KeyCode::Enter => {
                    if choice == SaveChoice::Save {
                        self.save_displayed_voyage();
                    }
                    self.voyage_ui.prompt = None;
                }
                KeyCode::Char('s' | 'S') => {
                    self.save_displayed_voyage();
                    self.voyage_ui.prompt = None;
                }
                _ => {}
            }
            return InputResult::Consumed;
        }

        match key.code {
            KeyCode::Esc => InputResult::Exit,
            KeyCode::Char('s' | 'S') => {
                self.open_voyage_save_prompt();
                InputResult::Consumed
            }
            // ←/→ page across selectable voyages (current login's runs + past
            // runs from the voyages file).
            KeyCode::Left => {
                self.nav_voyage(-1);
                InputResult::Consumed
            }
            KeyCode::Right => {
                self.nav_voyage(1);
                InputResult::Consumed
            }
            // ↑/↓ move the focused stat (the body auto-scrolls to follow it);
            // ↑ off the first stat returns focus to the top bar.
            KeyCode::Up => {
                if self.voyage_ui.focus == 0 {
                    InputResult::Exit
                } else {
                    self.voyage_focus(self.voyage_ui.focus - 1);
                    InputResult::Consumed
                }
            }
            KeyCode::Down => {
                self.voyage_focus(self.voyage_ui.focus.saturating_add(1));
                InputResult::Consumed
            }
            // Enter opens the focused item: the Sea Battles section (focus 0)
            // opens the per-fight log; a focused chart enlarges
            // (charts follow the stats).
            KeyCode::Enter => {
                if self.voyage_ui.focus == 0 {
                    self.open_battles_popup();
                } else if self.voyage_ui.focus >= self.voyage_ui.n_stats {
                    let idx = self.voyage_ui.focus - self.voyage_ui.n_stats;
                    if crate::voyage::ui::CHART_ENLARGEABLE.get(idx)
                        == Some(&true)
                    {
                        self.voyage_ui.chart_popup = Some(idx);
                    }
                }
                InputResult::Consumed
            }
            KeyCode::PageUp => {
                self.voyage_focus(self.voyage_ui.focus.saturating_sub(5));
                InputResult::Consumed
            }
            KeyCode::PageDown => {
                self.voyage_focus(self.voyage_ui.focus.saturating_add(5));
                InputResult::Consumed
            }
            _ => InputResult::Consumed,
        }
    }

    /// Build the computed view for the Voyage Statistics page: resolve which
    /// vessel/voyage to show, the chosen ship's cannon size, and the aggregated
    /// battle + consumption stats. Shows the current vessel's live run, or its
    /// most recent completed run.
    /// The ordered strip of selectable voyages for the pager: past runs from
    /// the voyages file (read-only) first, then the current login's in-RAM
    /// runs (read-write), oldest→newest. A run saved this session keeps its
    /// live page and its on-disk twin is hidden, so nothing is listed
    /// twice.
    fn voyage_pages(&self) -> Vec<crate::voyage::ui::VoyageSel> {
        use crate::voyage::ui::VoyageSel;
        // History indices already represented by a live (in-RAM) saved run.
        let claimed: std::collections::HashSet<usize> = self
            .chatlog
            .vessels
            .values()
            .flat_map(|v| v.voyages.iter().chain(v.current_voyage.iter()))
            .filter_map(|vy| vy.saved_to)
            .collect();
        let mut pages: Vec<VoyageSel> = (0 .. self.persistence.voyages.len())
            .filter(|i| !claimed.contains(i))
            .map(VoyageSel::Saved)
            .collect();
        // Current-login runs across all vessels, chronological (sail time, then
        // id).
        let mut live: Vec<(Option<chrono::NaiveDateTime>, u64)> = self
            .chatlog
            .vessels
            .values()
            .flat_map(|v| v.voyages.iter().chain(v.current_voyage.iter()))
            .map(|vy| (vy.sailed_at, vy.id))
            .collect();
        live.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        pages.extend(live.into_iter().map(|(_, id)| VoyageSel::Session(id)));
        pages
    }

    /// Resolve the current selection to a page index. `Live` maps to the newest
    /// (last) page; a stale pin falls back there too.
    fn current_voyage_page(
        &self,
        pages: &[crate::voyage::ui::VoyageSel],
    ) -> usize {
        use crate::voyage::ui::VoyageSel;
        let last = pages.len().saturating_sub(1);
        match self.voyage_ui.selected {
            VoyageSel::Live => last,
            sel => pages.iter().position(|p| *p == sel).unwrap_or(last),
        }
    }

    /// Find a current-login voyage (and its vessel key) by stable id.
    fn voyage_by_id(
        &self,
        id: u64,
    ) -> Option<(
        std::sync::Arc<str>,
        &crate::voyage::Voyage,
    )> {
        self.chatlog.vessels.iter().find_map(|(k, v)| {
            v.voyages
                .iter()
                .chain(v.current_voyage.iter())
                .find(|vy| vy.id == id)
                .map(|vy| (k.clone(), vy))
        })
    }

    /// The selected voyage's id **iff** it's a current-login run that's
    /// finished and not yet saved — i.e. the one the save/discard prompt
    /// would act on.
    fn selected_saveable_id(&self) -> Option<u64> {
        use crate::voyage::ui::VoyageSel;
        let pages = self.voyage_pages();
        let page = self.current_voyage_page(&pages);
        match pages.get(page).copied()? {
            VoyageSel::Session(id) => {
                let (_, vy) = self.voyage_by_id(id)?;
                (vy.ported_at.is_some() && !vy.saved).then_some(id)
            }
            _ => None,
        }
    }

    /// Step the pager by `delta` pages (clamped). Landing on the newest page
    /// returns to `Live` so the page keeps auto-following new runs; any page
    /// change closes the Sea Battles popup. Scroll and the *focused field*
    /// are preserved across the turn: we stash the outgoing field's stable
    /// key so the next render re-focuses the same field by name on the
    /// incoming voyage — robust to the conditional sections
    /// (Divvy/Enemies/Advantage/Consumption) that shift raw indices between
    /// voyages. Scroll then follows via the render's auto-scroll.
    fn nav_voyage(&mut self, delta: isize) {
        use crate::voyage::ui::VoyageSel;
        let pages = self.voyage_pages();
        if pages.is_empty() {
            return;
        }
        let cur = self.current_voyage_page(&pages) as isize;
        let next = (cur + delta).clamp(0, pages.len() as isize - 1) as usize;
        self.voyage_ui.selected = if next + 1 == pages.len() {
            VoyageSel::Live
        } else {
            pages[next]
        };
        self.voyage_ui.battles_popup = None;
        // Carry focus to the same field on the new page (resolved at render),
        // and let the body follow it there rather than holding the row a hand
        // scroll left it on — the new page is a different length.
        self.voyage_ui.pending_focus_key =
            self.voyage_ui.focus_keys.get(self.voyage_ui.focus).cloned();
        self.voyage_ui.pan = None;
    }

    /// Move the Voyage page's focus, which returns the body to following it: a
    /// body scrolled by hand stays where it was put only until the focus moves.
    fn voyage_focus(&mut self, idx: usize) {
        self.voyage_ui.focus = idx;
        self.voyage_ui.pan = None;
    }

    /// The computed view for the Voyage Statistics page, resolving the pager
    /// selection to a live (read-write) or historical (read-only) run.
    fn build_voyage_view(&self) -> crate::voyage::ui::VoyageView {
        use crate::voyage::ui::VoyageSel;
        let pages = self.voyage_pages();
        let page_count = pages.len();
        let page = self.current_voyage_page(&pages);
        match pages.get(page).copied() {
            Some(VoyageSel::Saved(idx)) => {
                self.build_saved_view(idx, page, page_count)
            }
            Some(VoyageSel::Session(id)) => {
                self.build_session_view(id, page, page_count)
                    .unwrap_or_else(|| self.empty_voyage_view())
            }
            _ => self.empty_voyage_view(),
        }
    }

    /// The "no voyage tracked yet" view — still shows the current vessel
    /// headline if we're aboard one.
    fn empty_voyage_view(&self) -> crate::voyage::ui::VoyageView {
        use crate::voyage::ui::{VoyageBadge, VoyageView};
        let key = self.displayed_vessel_key();
        let vessel_name = key.as_ref().map(|k| k.to_string());
        let ship_type = key
            .as_ref()
            .and_then(|k| self.jobbers_ui.ship_types.get(k).copied())
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.name.to_string());
        VoyageView {
            has_voyage: false,
            read_only: false,
            page: 0,
            page_count: 0,
            badge: VoyageBadge::Unsaved,
            vessel: vessel_name,
            ship_type,
            period: None,
            elapsed_secs: None,
            saveable: false,
            battle: Default::default(),
            consumption: Default::default(),
            charts: Default::default(),
            battles: Vec::new(),
            divvied: false,
            booty_chest: None,
            booty_goods: Vec::new(),
        }
    }

    /// Build a read-write view for a current-login run selected by id.
    fn build_session_view(
        &self,
        id: u64,
        page: usize,
        page_count: usize,
    ) -> Option<crate::voyage::ui::VoyageView> {
        use crate::voyage::ui::VoyageBadge;
        let (key, voyage) = self.voyage_by_id(id)?;
        let vessel_name = Some(key.to_string());
        let ship_type = self
            .jobbers_ui
            .ship_types
            .get(&key)
            .copied()
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.name.to_string());
        let is_live = self
            .chatlog
            .vessels
            .get(&key)
            .and_then(|v| v.current_voyage.as_ref())
            .map(|vy| vy.id)
            == Some(id);
        let ported = voyage.ported_at.is_some();
        // End of the run: the port time once ported, else the live log clock.
        let end_at = voyage.ported_at.or_else(|| self.chatlog.now());
        let elapsed_secs = match (voyage.sailed_at, end_at) {
            (Some(start), Some(end)) => Some((end - start).num_seconds()),
            _ => None,
        };
        let period = match (voyage.sailed_at, end_at) {
            (Some(start), Some(end)) => Some(voyage_period(start, end)),
            _ => None,
        };
        let confirmed = self.chatlog.self_confirmed;
        let consumption = crate::voyage::stats::consumption_stats(
            voyage,
            &self.profits.rows,
            &self.commodities,
        );
        let saveable = ported && !voyage.saved;
        let badge = if is_live {
            VoyageBadge::Live
        } else if voyage.saved {
            VoyageBadge::Saved
        } else {
            VoyageBadge::Unsaved
        };
        Some(self.assemble_voyage_view(AssembleView {
            vessel_name,
            ship_type,
            period,
            elapsed_secs,
            voyage,
            confirmed,
            consumption,
            read_only: false,
            saveable,
            badge,
            page,
            page_count,
        }))
    }

    /// Build a read-only view for a past run loaded from the voyages file.
    fn build_saved_view(
        &self,
        idx: usize,
        page: usize,
        page_count: usize,
    ) -> crate::voyage::ui::VoyageView {
        use crate::voyage::ui::VoyageBadge;
        let Some(saved) = self.persistence.voyages.get(idx) else {
            return self.empty_voyage_view();
        };
        let voyage = saved.to_voyage();
        let consumption = saved
            .consumption
            .as_ref()
            .map(|c| c.to_stats(&voyage))
            .unwrap_or_default();
        // The real ported time only survives as the `ended_at` string (the
        // in-RAM voyage uses synthetic epoch clocks); the start is it
        // minus the duration.
        let period = chrono::NaiveDateTime::parse_from_str(
            &saved.ended_at,
            "%Y-%m-%d %H:%M:%S",
        )
        .ok()
        .zip(saved.duration_secs)
        .map(|(end, dur)| {
            let start = end - chrono::Duration::seconds(dur);
            voyage_period(start, end)
        });
        self.assemble_voyage_view(AssembleView {
            vessel_name: saved.vessel.clone(),
            ship_type: saved.ship_type.clone(),
            period,
            elapsed_secs: voyage.duration_secs(),
            voyage: &voyage,
            // Persisted outcomes are already final — don't re-mask them by the
            // *current* identity-confirmation state (pass confirmed=true).
            confirmed: true,
            consumption,
            read_only: true,
            saveable: false,
            badge: VoyageBadge::ReadOnly,
            page,
            page_count,
        })
    }

    /// Assemble a [`crate::voyage::ui::VoyageView`] from a resolved voyage plus
    /// its pager framing. Shared by the live and historical paths — the
    /// stat/chart/ battle-row derivation is identical; only sourcing and
    /// flags differ.
    fn assemble_voyage_view(
        &self,
        a: AssembleView<'_>,
    ) -> crate::voyage::ui::VoyageView {
        use crate::voyage::ui::VoyageView;
        let AssembleView {
            vessel_name,
            ship_type,
            period,
            elapsed_secs,
            voyage,
            confirmed,
            consumption,
            read_only,
            saveable,
            badge,
            page,
            page_count,
        } = a;
        // Booty was frozen onto the voyage at its divvy (live) or restored from
        // disk (saved), so both paths read it straight off the voyage.
        let divvied = voyage.divvied;
        let booty_chest = voyage.booty_chest;
        let booty_goods = voyage.booty_goods.clone();

        // Identity confirmation gates win/loss: until our configured name is
        // seen in the log, every win/loss shows as Unknown (and flips
        // retroactively). Historical pages pass `confirmed = true`
        // (their verdicts are final).
        let eff = |raw| crate::voyage::effective_outcome(raw, confirmed);
        let battle = crate::voyage::stats::battle_stats(voyage, confirmed);

        // Per-fight rows for the Sea Battles popup: resolved fights first, then
        // the in-progress one (so it can be inspected mid-fight).
        // Mirrors the indexing in `GameState::displayed_battle_mut`.
        let battles: Vec<crate::voyage::ui::BattleRow> = voyage
            .battles
            .iter()
            .chain(voyage.current_battle.iter())
            .map(|b| {
                let outcome = eff(b.outcome);
                crate::voyage::ui::BattleRow {
                    enemy: b.enemy.clone(),
                    outcome,
                    // PvP is its own category ("Players"); all categories carry
                    // their own label.
                    category: crate::voyage::stats::category_label(&b.category),
                    // A masked (unknown) verdict carries no signed PoE.
                    poe: matches!(
                        outcome,
                        crate::voyage::BattleOutcome::Won
                            | crate::voyage::BattleOutcome::Lost
                    )
                    .then_some(b.poe)
                    .flatten(),
                    goods: b.goods,
                    boarding_secs: b.boarding_secs(),
                    pirates: b.pirates,
                    swabbies: b.swabbies,
                    snapshot: b.snapshot,
                    recorded: b.recorded,
                    their_manpower: b.their_manpower(),
                    foe_ship: b.foe_ship,
                    timeline: b.timeline.clone(),
                }
            })
            .collect();

        // Chart series: current voyage vs persisted history (won-fight PoE +
        // per-voyage value-per-share). Value is net PoE for now; goods fold in
        // later.
        let charts = {
            use crate::voyage::BattleOutcome::{Lost, Won};
            // Signed PoE of a fight, but only for a *confirmed* win/loss (an
            // unconfirmed/unknown verdict contributes nothing to the charts).
            let decisive_poe = |b: &crate::voyage::Battle| {
                matches!(eff(b.outcome), Won | Lost)
                    .then_some(b.poe)
                    .flatten()
            };
            // Signed PoE of each concluded (won or lost) fight, chronological —
            // losses are negative. Drives the per-fight bar chart.
            let cur_fight_poe: Vec<f64> = voyage
                .battles
                .iter()
                .filter_map(&decisive_poe)
                .map(|p| p as f64)
                .collect();
            // "Value per Share": total value ÷ total divvy shares. Shares are
            // summed over the same decisive fights as the numerator
            // — only real pirates hold a share; neither mercenaries
            // nor swabbies earn one. Ship- and duration-agnostic
            // (crew size and fight count both divide out).
            let (cur_total_i, cur_shares) =
                voyage
                    .battles
                    .iter()
                    .fold((0i64, 0u32), |(poe, shares), b| {
                        match decisive_poe(b) {
                            // A fight's shares = pirates aboard (neither
                            // mercenaries nor swabbies earn a share).
                            // `our_team` is always present for a decisive
                            // fight; fall back to
                            // the pirate count if somehow absent.
                            Some(p) => {
                                (
                                    poe + p,
                                    shares
                                        + b.our_team
                                            .as_ref()
                                            .map_or(b.pirates, |t| t.shares()),
                                )
                            }
                            None => (poe, shares),
                        }
                    });
            let cur_total = cur_total_i as f64;
            let cur_per_share = if cur_shares > 0 {
                cur_total / cur_shares as f64
            } else {
                0.0
            };

            // Ship Winrate: this voyage's decisive fights bucketed by the
            // *enemy* hull. Only fights whose foe hull is known
            // contribute (an unknown foe can't be attributed to a
            // ship type).
            use crate::voyage::ui::WinCount;
            let mut wr_voyage: std::collections::BTreeMap<usize, WinCount> =
                Default::default();
            let mut voyage_fights = 0usize;
            for b in &voyage.battles {
                let o = eff(b.outcome);
                if !matches!(o, Won | Lost) {
                    continue;
                }
                // A decisive fight counts toward "have we fought" regardless of
                // whether we can name the enemy hull.
                voyage_fights += 1;
                // The enemy hull: the game-announced type when known (Black
                // Ship, Monkey Boats), else whatever ship type
                // was set in the Damage calculator for the
                // fight. Most brigand fights only have the latter.
                let foe = b.foe_ship.or_else(|| b.snapshot.map(|s| s.foe_ship));
                if let Some(foe) = foe {
                    wr_voyage.entry(foe).or_default().add(matches!(o, Won));
                }
            }
            let mut hist_per_share = Vec::new();
            // Signed per-fight PoE of the *rest* of the voyages sharing this
            // voyage's hull — drives the "History" box beneath the per-fight
            // bars. Only when the hull is actually known (no
            // guessing).
            let mut hull_fight_poe = Vec::new();
            for v in &self.persistence.voyages {
                let mut total = 0i64;
                let mut shares = 0u32;
                let same_hull = ship_type.is_some() && v.ship_type == ship_type;
                for bt in &v.battles {
                    if let Some(p) = bt.poe {
                        total += p;
                        // Shares = pirates aboard. Neither mercenaries nor
                        // swabbies earn a share, so only the pirate count
                        // feeds the divvy. Paired with the numerator per fight.
                        shares += bt.pirates;
                        // Decisive (won/lost) fights on the same hull, signed.
                        // The displayed voyage is
                        // included (History no longer self-excludes),
                        // consistent with the Ship Winrate history.
                        if same_hull
                            && matches!(bt.outcome.as_str(), "won" | "lost")
                        {
                            hull_fight_poe.push(p as f64);
                        }
                    }
                }
                // Value per share for this past voyage (0 when no shares
                // recorded).
                hist_per_share.push(
                    if shares > 0 {
                        total as f64 / shares as f64
                    } else {
                        0.0
                    },
                );
            }

            // Ship Winrate history keyed by (our hull, enemy hull): the
            // *persisted* voyage set only — runs already written to
            // disk plus those saved this
            // session (`save_displayed_voyage` pushes here and flushes
            // immediately, so this is exactly "written or confirmed
            // about to be written"). Unsaved in-RAM runs are
            // deliberately NOT counted, and the displayed voyage is NOT
            // excluded: when it's a saved run it counts here too, so Historical
            // overlaps the Voyage column rather than being disjoint
            // from it. The per-fight foe hull persists
            // independently of the `recorded` flag (top-level `foe_ship`),
            // so unrecorded fights still bucket correctly.
            let mut wr_history: std::collections::BTreeMap<
                (usize, usize),
                WinCount,
            > = Default::default();
            for v in &self.persistence.voyages {
                let Some(our_idx) =
                    v.ship_type.as_deref().and_then(crate::ships::ship_index)
                else {
                    continue;
                };
                for bt in &v.battles {
                    if !matches!(bt.outcome.as_str(), "won" | "lost") {
                        continue;
                    }
                    let foe = bt
                        .foe_ship
                        .as_deref()
                        .or(bt.snapshot.as_ref().map(|s| s.foe_ship.as_str()))
                        .and_then(crate::ships::ship_index);
                    if let Some(foe_idx) = foe {
                        wr_history
                            .entry((our_idx, foe_idx))
                            .or_default()
                            .add(bt.outcome == "won");
                    }
                }
            }
            // Boxes under the bars: this voyage's fights, then the same-hull
            // rest. With no hull selected we can't say what "same
            // hull" means, so the History row prompts the user to
            // pick one instead of a box.
            use crate::voyage::ui::ChartBox;
            let history_box = if ship_type.is_none() {
                ChartBox {
                    label: "History".to_string(),
                    values: Vec::new(),
                    empty_note: Some(
                        "Select ship hull first to show historical."
                            .to_string(),
                    ),
                }
            } else {
                ChartBox {
                    label: "History".to_string(),
                    values: hull_fight_poe,
                    empty_note: None,
                }
            };
            let fight_boxes = vec![
                ChartBox {
                    label: "Voyage".to_string(),
                    values: cur_fight_poe.clone(),
                    empty_note: None,
                },
                history_box,
            ];
            let winrate = crate::voyage::ui::ShipWinrate {
                our_ship: ship_type
                    .as_deref()
                    .and_then(crate::ships::ship_index),
                voyage_fights,
                voyage: wr_voyage,
                history: wr_history,
            };
            crate::voyage::ui::ChartData {
                cur_fight_poe,
                fight_boxes,
                cur_per_share,
                hist_per_share,
                winrate,
            }
        };

        VoyageView {
            has_voyage: true,
            read_only,
            page,
            page_count,
            badge,
            vessel: vessel_name,
            ship_type,
            period,
            elapsed_secs,
            saveable,
            battle,
            consumption,
            charts,
            battles,
            divvied,
            booty_chest,
            booty_goods,
        }
    }

    /// Feed one live chat-log line. When a fight resolves and the Damage
    /// calculator has hits entered, freeze its full state + advantage onto
    /// that fight (Left = our ship, Right = the foe) and clear the counts
    /// for the next fight — so a fight we tracked live is recorded in the
    /// Sea Battles history automatically. (The popup still lets the user
    /// amend a fight or hand-add one we missed.)
    pub fn feed_chat_line(&mut self, line: &str) {
        self.chatlog.process_line(line);
        // Capture the transition flags (and any detected foe hull) before the
        // steps below consume them, so navigation and the new-battle prompt can
        // fire regardless of the calculator state.
        let detected_foe = self.chatlog.take_detected_foe_ship();
        let battle_started = self.chatlog.take_battle_started();
        let battle_resolved = self.chatlog.take_resolved();
        if battle_resolved && self.damage.has_input() {
            // Our manpower = the crew that actually fought, as recorded on the
            // just-resolved battle (grapple roster minus the disconnected).
            // Falls back to the live count if that battle didn't
            // record one.
            let crew_n = self
                .chatlog
                .last_resolved_our_strength()
                .unwrap_or_else(|| {
                    self.chatlog.current_pirates()
                        + self.chatlog.current_swabbies()
                });
            // Their manpower came from the melee at resolution; fall back to
            // the foe ship type's pirate capacity if the fight had
            // no melee count.
            let their = self
                .chatlog
                .last_resolved_their_manpower()
                .unwrap_or_else(|| {
                    crate::ships::SHIPS[self.damage.right_ship].max_pirates
                        as u32
                });
            let snap = self.damage.snapshot(crew_n);
            let dmg = self.damage.advantage_dmg();
            let crew = self.damage.crew_advantage(crew_n, their);
            self.chatlog.record_resolved_battle(snap, dmg, crew);
            self.damage.clear_counts();
        }
        // Auto-navigation. A fight beginning surfaces the live Damage
        // calculator (so it's tracked from the first hit); a fight
        // concluding surfaces its entry in the Sea Battles log. A start
        // wins if both somehow fire.
        if battle_started {
            self.jump_to_live_damage();
            // Ask before touching the calculator: Apply seeds the foe hull and
            // clears the tally, Keep leaves it as-is. The hull is applied only
            // on Apply, so an unanswered prompt changes nothing. `prev_saved`
            // reports whether the previous fight's tally was already staged;
            // the ship name/hull/note are filled in by the sync step below.
            self.damage.battle_prompt = Some(crate::damage::BattlePrompt {
                ship_name: None,
                foe_ship: None,
                note: None,
                prev_saved: self.chatlog.last_recorded_battle_saved(),
                apply: true,
            });
        }
        // Keep an open prompt synced to the current fight (ship name, hull,
        // note), so a mid-fight reveal — e.g. the Black Ship replacing the
        // target — updates it live. With no prompt open, a mid-fight hull
        // reveal applies straight to the calculator.
        if let Some(p) = self.damage.battle_prompt.as_mut() {
            if let Some(brief) = self.chatlog.current_battle_summary() {
                p.ship_name = brief.name;
                p.foe_ship = brief.foe_ship;
                p.note = brief.note;
            }
        } else if let Some(idx) = detected_foe {
            self.damage.right_ship = idx;
        }
        if !battle_started && battle_resolved {
            self.jump_to_concluded_fight();
        }
        // Entering a vampire lair: surface the Jobbers page in its Vampirates
        // layout so the wave model and skill-distribution tooling are at hand.
        if self.chatlog.take_lair_entered() {
            self.jump_to_vampirate_jobbers();
        }
        // The Cursed Isles tell (the noxious fog) does the same for the Cursed
        // Isles layout (Enthralled leaderboard + Fight Statistics).
        if self.chatlog.take_cursed_isles_detected() {
            self.jump_to_cursed_isles_jobbers();
        }
        // The first melee KO of a grappled sea battle surfaces the live
        // advantage graph mid-fight (lair / island runs already
        // surfaced their layout on the entry tell, so they don't
        // auto-jump here).
        if self.chatlog.take_battle_first_blood() {
            self.jump_to_concluded_fight();
        }
        // Boarding a vessel snaps the Jobbers/Voyage vessel selector to it, so
        // the pages follow us onto the ship we just stepped onto rather
        // than sticking to whatever was previously picked.
        if let Some(key) = self.chatlog.take_boarded_vessel() {
            self.jobbers_ui.selected = Some(key);
        }
        // A booty division freezes the just-divvied run's booty (chest PoE +
        // goods) from the live Profits state onto the voyage, so a
        // later pillage can't blank the Divvy section.
        if self.chatlog.take_booty_divided() {
            let booty = self.current_booty_snapshot();
            if let Some(voy) = self.chatlog.current_pillage_voyage_mut() {
                voy.booty_chest = booty.chest;
                voy.booty_goods = booty.goods;
            }
        }
    }

    /// Switch the shown app and drop focus into its content (used by the
    /// fight-driven auto-navigation).
    fn switch_to(&mut self, app: AppId) {
        if let Some(idx) = APP_LIST.iter().position(|a| *a == app) {
            self.sidebar_index = idx;
            self.global_focus = GlobalFocus::Content;
        }
    }

    /// A hold landed on the clipboard: queue it for the Profits page's
    /// confirmation prompt (see [`Self::surface_hold_import`]).
    pub fn queue_hold_import(&mut self, hold: &crate::hold::HoldContents) {
        self.profits.queue_hold(hold, &self.commodities);
    }

    /// Open a queued hold prompt once the Profits popup slot is free, and
    /// surface the Profits page so the prompt is actually seen - the same
    /// auto-navigation a starting fight gives the Damage calculator.
    pub fn surface_hold_import(&mut self) {
        if self.profits.raise_pending_hold() {
            self.switch_to(AppId::Profits);
        }
    }

    /// A new fight just began: close any open Sea Battles popup and surface the
    /// live Damage calculator so the fight is tracked from the first hit.
    fn jump_to_live_damage(&mut self) {
        self.voyage_ui.battles_popup = None;
        self.switch_to(AppId::Damage);
    }

    /// A fight just concluded: surface the Voyage Statistics page with the Sea
    /// Battles popup open on the fight that just ended (the last one). No-op if
    /// the displayed voyage somehow has no fights.
    fn jump_to_concluded_fight(&mut self) {
        // The fight belongs to the live run — follow it, regardless of any page
        // the user had paged back to.
        self.voyage_ui.selected = crate::voyage::ui::VoyageSel::Live;
        let n = self.build_voyage_view().battles.len();
        let Some(last) = n.checked_sub(1) else {
            return;
        };
        self.switch_to(AppId::Voyage);
        self.voyage_ui.battles_popup = Some(last);
        self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Pager;
        self.load_battle_editor(last);
    }

    /// We just entered a vampire lair: surface the Jobbers page and switch it
    /// to the Vampirates voyage layout (wave model + skill-distribution
    /// tooling).
    fn jump_to_vampirate_jobbers(&mut self) {
        self.jobbers_ui.voyage_type = VoyageType::Vampirates;
        self.switch_to(AppId::Chatlog);
    }

    /// The Cursed Isles tell fired: surface the Jobbers page and switch it to
    /// the Cursed Isles voyage layout (Enthralled leaderboard + Fight
    /// Statistics).
    fn jump_to_cursed_isles_jobbers(&mut self) {
        self.jobbers_ui.voyage_type = VoyageType::CursedIsles;
        self.switch_to(AppId::Chatlog);
    }

    /// The vessel key whose voyage the page is currently showing (mirrors the
    /// resolution in [`Self::build_voyage_view`]).
    fn displayed_vessel_key(&self) -> Option<std::sync::Arc<str>> {
        self.jobbers_ui
            .selected
            .clone()
            .filter(|k| self.chatlog.vessels.contains_key(k))
            .or_else(|| self.chatlog.vessels_by_recency().into_iter().next())
    }

    /// Open the save/discard prompt if the displayed run is finished and
    /// unsaved.
    fn open_voyage_save_prompt(&mut self) {
        if self.build_voyage_view().saveable {
            self.voyage_ui.prompt = Some(crate::voyage::ui::SaveChoice::Cancel);
        }
    }

    /// Snapshot the current pillage's booty from the live Profits page: the
    /// chest PoE (user-entered "Booty Chest" field, else auto-deduced net
    /// chest) and the Booty-column goods (resolved to commodity names).
    /// Called on the divvy signal to freeze the just-divvied run's booty
    /// onto its voyage, since the Profits state is global and a later
    /// pillage would otherwise overwrite it.
    fn current_booty_snapshot(
        &self,
    ) -> crate::voyage::persistence::BootySnapshot {
        let (pillage_gross, pillage_stolen, pillage_chest) =
            self.chatlog.current_pillage_poe();
        let shared = SharedState {
            commodities: &self.commodities,
            cached_offers: &self.cached_offers,
            available_islands: &self.available_islands,
            ocean_geo: self.ocean_geo(),
            loading: self.loading,
            market_supported: self.market_ok(),
            pillage_gross,
            pillage_stolen,
            pillage_chest,
        };
        crate::voyage::persistence::BootySnapshot {
            chest: Some(self.profits.recorded_chest(&shared)),
            goods: self
                .profits
                .booty_goods()
                .into_iter()
                .map(|(id, qty)| {
                    (
                        commod_name(&self.commodities, id).to_string(),
                        qty,
                    )
                })
                .collect(),
        }
    }

    /// Persist the displayed (finished) voyage to history + disk, and mark it
    /// saved so it isn't offered again.
    fn save_displayed_voyage(&mut self) {
        // Only the *selected* current-login run, and only if it's finished and
        // unsaved, can be persisted.
        let Some(id) = self.selected_saveable_id() else {
            return;
        };
        // Resolve the vessel key + ship type before the mutable borrow below.
        let Some((key, _)) = self.voyage_by_id(id) else {
            return;
        };
        let vessel_name = key.to_string();
        // TODO: handle the case where no ship type was specified — this is
        // `None` when the user never picked a hull in the jobbers
        // picker. We persist `None` silently, but such a voyage can't
        // be grouped into the per-ship box plots and its size-agnostic
        // cannonball count has no hull context. Decide whether to
        // prompt for the hull at save time, warn, or exclude it.
        let ship_type = self
            .jobbers_ui
            .ship_types
            .get(&key)
            .copied()
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.name.to_string());
        let confirmed = self.chatlog.self_confirmed;
        // The disk index this run will occupy — pinned on the live voyage so
        // the pager hides the on-disk twin and keeps showing the live
        // (read-write) page.
        let new_index = self.persistence.voyages.len();
        let saved = {
            let Some(voyage) = self.chatlog.voyage_by_id_mut(id) else {
                return;
            };
            if voyage.saved || voyage.ported_at.is_none() {
                return;
            }
            // Snapshot consumption now — the Profits stock delta can't be
            // reconstructed once the hold is restocked.
            // TODO: prompt the user whether to record the inventory/consumption
            // for this voyage before storing it. `SavedVoyage.consumption` is
            // already nullable for exactly this — pass `None` when they
            // decline. For now we always record it.
            let consumption = crate::voyage::stats::consumption_stats(
                voyage,
                &self.profits.rows,
                &self.commodities,
            );
            let saved = crate::voyage::persistence::from_voyage(
                voyage,
                Some(&vessel_name),
                ship_type.as_deref(),
                Some(&consumption),
                confirmed,
            );
            voyage.saved = true;
            voyage.saved_to = Some(new_index);
            saved
        };
        self.persistence.voyages.push(saved);
        // the memorization in the same file has to survive this write, so
        // the whole of it goes out, not just the history
        if let Some(path) = self.persistence_path.clone() {
            self.save_memorization();
            crate::persistence::save(&path, &self.persistence);
        }
    }

    // -- Sea Battles popup (per-fight log) --

    /// Open the Sea Battles popup on the first fight, loading its editor. No-op
    /// if the displayed voyage has no fights.
    fn open_battles_popup(&mut self) {
        if self.build_voyage_view().battles.is_empty() {
            return;
        }
        self.voyage_ui.battles_popup = Some(0);
        self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Pager;
        self.load_battle_editor(0);
    }

    /// Load fight `page` of the displayed voyage into the Sea Battles editor,
    /// seeding it from its snapshot (or a blank calculator). The calculator is
    /// always editable; `editor_recorded` only mirrors the persistence flag.
    fn load_battle_editor(&mut self, page: usize) {
        let view = self.build_voyage_view();
        let Some(row) = view.battles.get(page) else {
            return;
        };
        self.voyage_ui.editor_recorded = row.recorded;
        // "Our strength" = the full crew aboard our ship: real pirates PLUS
        // swabbies / named mercenaries (all fight in the melee). Use
        // the fight's resolution roster; for an as-yet-unresolved fight
        // fall back to the live crew.
        let crew = row.pirates + row.swabbies;
        let crew = if crew > 0 {
            crew
        } else {
            self.chatlog.current_pirates() + self.chatlog.current_swabbies()
        };
        self.voyage_ui.editor_crew = crew;
        self.voyage_ui.editor_their = row.their_manpower;
        self.voyage_ui.battle_editor = match row.snapshot {
            Some(s) => crate::damage::DamageApp::from_snapshot(&s),
            None => {
                // No captured snapshot yet — start blank, but if the encounter
                // told us the foe's hull (Black Ship, Monkey
                // Boat) seed that as the foe ship so the
                // calculator and the displayed type are right.
                let mut app = crate::damage::DamageApp::new();
                if let Some(idx) = row.foe_ship {
                    app.right_ship = idx;
                }
                app
            }
        };
        // #16: when the foe's hull is *unknown* (no special encounter announced
        // it) and the observed headcount can't fit the currently-seeded
        // hull, bump the foe ship to the smallest hull that can man
        // that crew — a sloop can't hold 18 boarders. Special
        // encounters that announce their hull (Monkey Boats,
        // and the Black Ship, which can be staffed beyond any hull's capacity)
        // carry `foe_ship = Some(..)` and are left untouched.
        if row.foe_ship.is_none()
            && let Some(their) = row.their_manpower
        {
            let cur = crate::ships::SHIPS
                [self.voyage_ui.battle_editor.right_ship]
                .max_pirates;
            if (cur as u32) < their
                && let Some(idx) = crate::ships::smallest_ship_for(their)
            {
                self.voyage_ui.battle_editor.right_ship = idx;
            }
        }
    }

    /// Step the open Sea Battles page by `delta`, **wrapping** at the ends, and
    /// reload the editor. Paging always returns focus to the pager.
    fn battles_page(&mut self, delta: isize) {
        let Some(page) = self.voyage_ui.battles_popup else {
            return;
        };
        let n = self.build_voyage_view().battles.len() as isize;
        if n == 0 {
            return;
        }
        let new = (page as isize + delta).rem_euclid(n) as usize;
        self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Pager;
        if new != page {
            self.voyage_ui.battles_popup = Some(new);
            self.load_battle_editor(new);
        }
    }

    /// Write the editor's current state back onto the open fight, recomputing
    /// its advantage. Always runs on an edit — the calculator is always
    /// live; recording only governs persistence, not the in-RAM snapshot.
    fn sync_battle_editor(&mut self) {
        // A read-only history page has no live battle to write back to — bail
        // before `displayed_vessel_key` would target the current live vessel.
        if self.build_voyage_view().read_only {
            return;
        }
        let Some(page) = self.voyage_ui.battles_popup else {
            return;
        };
        let Some(key) = self.displayed_vessel_key() else {
            return;
        };
        let ours = self.voyage_ui.editor_crew;
        let their = self.voyage_ui.editor_their.unwrap_or_else(|| {
            crate::ships::SHIPS[self.voyage_ui.battle_editor.right_ship]
                .max_pirates as u32
        });
        let snap = self.voyage_ui.battle_editor.snapshot(ours);
        let dmg = self.voyage_ui.battle_editor.advantage_dmg();
        let crew = self.voyage_ui.battle_editor.crew_advantage(ours, their);
        self.chatlog
            .set_battle_snapshot(&key, page, snap, dmg, crew);
    }

    /// Toggle whether the open fight is recorded (persisted to disk). Purely a
    /// flag — the snapshot/calculator are untouched.
    fn toggle_battle_record(&mut self) {
        if self.build_voyage_view().read_only {
            return;
        }
        let Some(page) = self.voyage_ui.battles_popup else {
            return;
        };
        let Some(key) = self.displayed_vessel_key() else {
            return;
        };
        let now = !self.voyage_ui.editor_recorded;
        self.chatlog.set_battle_recorded(&key, page, now);
        self.voyage_ui.editor_recorded = now;
    }

    /// Key handling while the Sea Battles popup is open. Three focus zones
    /// chained top→bottom — the pager (←/→ change fight, wrapping), the
    /// record toggle (Enter/Space flips it), and the always-editable
    /// calculator (arrows drive it) — moved between with ↑/↓. Esc closes
    /// the editor's ship picker / reset confirm first, otherwise the popup.
    fn handle_battles_key(&mut self, key: KeyEvent) -> InputResult {
        use crate::{
            damage::{ROW_RAMS, ROW_SHIP, Side},
            voyage::ui::BattlesFocus::{Calc, Pager, Record},
        };

        // A read-only history page: only paging between fights and closing —
        // the calculator and record toggle are inert (nothing
        // persists).
        if self.build_voyage_view().read_only {
            match key.code {
                KeyCode::Esc => self.voyage_ui.battles_popup = None,
                KeyCode::Left | KeyCode::PageUp => self.battles_page(-1),
                KeyCode::Right | KeyCode::PageDown => self.battles_page(1),
                _ => {}
            }
            return InputResult::Consumed;
        }

        // While the editor has its own modal (ship picker or reset confirm),
        // every key — including Esc, which dismisses that modal keeping
        // the ship — drives the calculator rather than the popup.
        let editor_modal = self.voyage_ui.battle_editor.popup.is_some()
            || self.voyage_ui.battle_editor.reset_prompt.is_some();
        if editor_modal {
            self.voyage_ui.battle_editor.handle_key(key);
            self.sync_battle_editor();
            return InputResult::Consumed;
        }

        if key.code == KeyCode::Esc {
            self.voyage_ui.battles_popup = None;
            return InputResult::Consumed;
        }

        match self.voyage_ui.battles_focus {
            Pager => {
                match key.code {
                    KeyCode::Left | KeyCode::PageUp => self.battles_page(-1),
                    KeyCode::Right | KeyCode::PageDown => self.battles_page(1),
                    KeyCode::Down => self.voyage_ui.battles_focus = Record,
                    _ => {}
                }
            }
            // The toggle sits between the pager and the calculator, matching
            // its on-screen position (directly under the page
            // number, above the calc).
            Record => {
                match key.code {
                    KeyCode::Up => self.voyage_ui.battles_focus = Pager,
                    KeyCode::Down => {
                        self.voyage_ui.battles_focus = Calc;
                        self.voyage_ui.battle_editor.focus_row = ROW_SHIP;
                        self.voyage_ui.battle_editor.focus_side = Side::Left;
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        self.toggle_battle_record()
                    }
                    _ => {}
                }
            }
            Calc => {
                // ↑ off the top row returns to the toggle; ↓ off the bottom row
                // has nowhere to go (the calculator is the last
                // control).
                if key.code == KeyCode::Up
                    && self.voyage_ui.battle_editor.focus_row == ROW_SHIP
                {
                    self.voyage_ui.battles_focus = Record;
                } else if key.code == KeyCode::Down
                    && self.voyage_ui.battle_editor.focus_row == ROW_RAMS
                {
                    // bottom of the chain — stay put.
                } else {
                    self.voyage_ui.battle_editor.handle_key(key);
                    self.sync_battle_editor();
                }
            }
        }
        InputResult::Consumed
    }

    // -- jobbers (chat log) handling --

    fn handle_jobbers_key(&mut self, key: KeyEvent) -> InputResult {
        use JobberFocus::*;

        // The popups are modal: each eats keys until dismissed. The trophies
        // popup is checked first since it layers over the pirate-stats
        // popup.
        if self.jobbers_ui.trophy_popup.is_some() {
            return self.handle_trophy_popup_key(key);
        }
        if self.jobbers_ui.pirate_popup.is_some() {
            return self.handle_pirate_popup_key(key);
        }
        if self.jobbers_ui.skill_dist_popup.is_some() {
            return self.handle_skill_dist_popup_key(key);
        }
        if self.jobbers_ui.per_fight_popup.is_some() {
            return self.handle_per_fight_popup_key(key);
        }
        if let Some(sel) = self.jobbers_ui.ship_popup {
            return self.handle_ship_popup_key(key, sel);
        }
        if let Some(sel) = self.jobbers_ui.vessel_popup {
            return self.handle_vessel_popup_key(key, sel);
        }
        if let Some(sel) = self.jobbers_ui.voyage_popup {
            return self.handle_voyage_popup_key(key, sel);
        }

        let panes = self.jobbers_ui.voyage_type.panes();
        let first_pane = panes.first().map(|p| Self::pane_focus(*p));
        let poisoned = self.selected_poisoned();
        let implemented = self.jobbers_ui.voyage_type.implemented();
        // Vikings sits its leaderboard *beside* the panes (a horizontal split)
        // rather than above them, so ←/→ — not ↑/↓ — cross between the two.
        let panes_beside =
            self.jobbers_ui.voyage_type.panes_beside_top_jobbers();
        // The voyage box's bottom row: Unpoison when poisoned, else Voyage
        // Type.
        let box_bottom = if poisoned { Unpoison } else { VoyageType };
        // The Skill Distribution button (Vampirates) sits between the
        // leaderboard and the panes, so it's the row just below the
        // leaderboard and above the panes.
        let has_button = self.jobbers_ui.voyage_type.has_skill_distribution();
        let after_box = if has_button {
            Some(SkillDist)
        } else {
            first_pane
        };
        // Descending out of the voyage box lands on the Skill Leaderboard first
        // (when the layout is implemented), then the button / panes below it.
        let into_content = if implemented {
            Some(Leaderboard)
        } else {
            after_box
        };

        match key.code {
            KeyCode::Esc => return InputResult::Exit,
            KeyCode::Up => {
                match self.jobbers_ui.focus {
                    // The Vessels button is the page's top widget; ↑ returns to
                    // the bar.
                    Vessels => return InputResult::Exit,
                    ShipType => self.jobbers_ui.focus = Vessels,
                    VoyageType => self.jobbers_ui.focus = ShipType,
                    Unpoison => self.jobbers_ui.focus = VoyageType,
                    // Within the leaderboard ↑ walks up the column; at the top
                    // it leaves for the box's bottom row.
                    Leaderboard => {
                        if self.leaderboard_current_len() == 0
                            || self.jobbers_ui.top_sel == 0
                        {
                            self.jobbers_ui.focus = box_bottom;
                        } else {
                            self.jobbers_ui.top_sel -= 1;
                        }
                    }
                    // The button sits below the leaderboard; ↑ returns to it.
                    SkillDist => self.jobbers_ui.focus = Leaderboard,
                    Aboard | Greedy | Planked | Enthralled => {
                        let pane =
                            Self::focus_pane(self.jobbers_ui.focus).unwrap();
                        // At the top of a pane (or an empty one), ↑ leaves for
                        // whatever's above the panes:
                        // the button if shown, else (side-by-side) the
                        // box's bottom row, else the leaderboard stacked above.
                        if self.jobbers_pane_count(pane) == 0
                            || self.jobbers_pane_sel(pane) == 0
                        {
                            self.jobbers_ui.focus = if has_button {
                                SkillDist
                            } else if panes_beside {
                                box_bottom
                            } else {
                                Leaderboard
                            };
                        } else {
                            self.jobbers_pane_select_delta(pane, -1);
                        }
                    }
                }
            }
            KeyCode::Down => {
                match self.jobbers_ui.focus {
                    Vessels => self.jobbers_ui.focus = ShipType,
                    ShipType => self.jobbers_ui.focus = VoyageType,
                    VoyageType => {
                        self.jobbers_ui.focus = if poisoned {
                            Unpoison
                        } else {
                            into_content.unwrap_or(VoyageType)
                        };
                    }
                    Unpoison => {
                        if let Some(next) = into_content {
                            self.jobbers_ui.focus = next;
                        }
                    }
                    // Within the leaderboard ↓ walks down the column; at the
                    // bottom it leaves for the button /
                    // panes below.
                    Leaderboard => {
                        let len = self.leaderboard_current_len();
                        if len == 0 || self.jobbers_ui.top_sel + 1 >= len {
                            if let Some(next) = after_box {
                                self.jobbers_ui.focus = next;
                            }
                        } else {
                            self.jobbers_ui.top_sel += 1;
                        }
                    }
                    // ↓ from the button drops into the first pane.
                    SkillDist => {
                        if let Some(pane) = first_pane {
                            self.jobbers_ui.focus = pane;
                        }
                    }
                    Aboard | Greedy | Planked | Enthralled => {
                        let pane =
                            Self::focus_pane(self.jobbers_ui.focus).unwrap();
                        self.jobbers_pane_select_delta(pane, 1);
                    }
                }
            }
            KeyCode::Left => {
                match self.jobbers_ui.focus {
                    // ← walks to the previous leaderboard column.
                    Leaderboard => {
                        if self.jobbers_ui.top_col > 0 {
                            self.jobbers_ui.top_col -= 1;
                            self.leaderboard_clamp();
                        }
                    }
                    Aboard | Greedy | Planked | Enthralled => {
                        let cur =
                            Self::focus_pane(self.jobbers_ui.focus).unwrap();
                        if let Some(i) = panes.iter().position(|p| *p == cur) {
                            if i > 0 {
                                self.jobbers_ui.focus =
                                    Self::pane_focus(panes[i - 1]);
                            } else if panes_beside {
                                // The leftmost pane sits to the right of the
                                // leaderboard.
                                self.jobbers_ui.focus = Leaderboard;
                            }
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Right => {
                match self.jobbers_ui.focus {
                    Leaderboard => {
                        if panes_beside {
                            // The leaderboard sits to the left of the pane(s).
                            if let Some(pane) = first_pane {
                                self.jobbers_ui.focus = pane;
                            }
                        } else if self.jobbers_ui.top_col + 1
                            < self.leaderboard_ncols()
                        {
                            self.jobbers_ui.top_col += 1;
                            self.leaderboard_clamp();
                        }
                    }
                    Aboard | Greedy | Planked | Enthralled => {
                        let cur =
                            Self::focus_pane(self.jobbers_ui.focus).unwrap();
                        if let Some(i) = panes.iter().position(|p| *p == cur)
                            && i + 1 < panes.len()
                        {
                            self.jobbers_ui.focus =
                                Self::pane_focus(panes[i + 1]);
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Enter => {
                match self.jobbers_ui.focus {
                    Vessels => self.open_vessel_popup(),
                    ShipType => self.open_ship_popup(),
                    VoyageType => self.open_voyage_popup(),
                    Unpoison => {
                        self.jobbers_unpoison();
                        self.jobbers_ui.focus = Vessels;
                    }
                    Leaderboard => self.open_leaderboard_popup(),
                    SkillDist => self.open_skill_dist_popup(),
                    Aboard | Greedy | Planked | Enthralled => {
                        let pane =
                            Self::focus_pane(self.jobbers_ui.focus).unwrap();
                        self.open_pirate_popup(pane);
                    }
                }
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the ship-type popup for the selected vessel, highlighting its
    /// current pick (or the first ship if none chosen yet).
    fn open_ship_popup(&mut self) {
        let cur = self
            .jobbers_ui
            .selected
            .as_ref()
            .and_then(|k| self.jobbers_ui.ship_types.get(k).copied())
            .unwrap_or(0);
        self.jobbers_ui.ship_popup = Some(cur);
        self.jobbers_ui.focus = JobberFocus::ShipType;
    }

    /// Modal key handling while the ship-type popup is open.
    fn handle_ship_popup_key(
        &mut self,
        key: KeyEvent,
        sel: usize,
    ) -> InputResult {
        let count = crate::ships::SHIPS.len();
        match key.code {
            KeyCode::Esc => self.jobbers_ui.ship_popup = None,
            KeyCode::Up => {
                if sel > 0 {
                    self.jobbers_ui.ship_popup = Some(sel - 1);
                }
            }
            KeyCode::Down => {
                if sel + 1 < count {
                    self.jobbers_ui.ship_popup = Some(sel + 1);
                }
            }
            KeyCode::Enter => {
                if let Some(vessel) = self.jobbers_ui.selected.clone() {
                    self.jobbers_ui.ship_types.insert(vessel, sel);
                }
                self.jobbers_ui.ship_popup = None;
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the vessel picker popup, highlighting the current selection.
    fn open_vessel_popup(&mut self) {
        let ordered = self.chatlog.vessels_by_recency();
        if ordered.is_empty() {
            return;
        }
        let cur = self
            .jobbers_ui
            .selected
            .as_ref()
            .and_then(|s| ordered.iter().position(|k| k == s))
            .unwrap_or(0);
        self.jobbers_ui.vessel_popup = Some(cur);
        self.jobbers_ui.focus = JobberFocus::Vessels;
    }

    /// Modal key handling while the vessel picker is open.
    fn handle_vessel_popup_key(
        &mut self,
        key: KeyEvent,
        sel: usize,
    ) -> InputResult {
        let ordered = self.chatlog.vessels_by_recency();
        match key.code {
            KeyCode::Esc => self.jobbers_ui.vessel_popup = None,
            KeyCode::Up => {
                if sel > 0 {
                    self.jobbers_ui.vessel_popup = Some(sel - 1);
                }
            }
            KeyCode::Down => {
                if sel + 1 < ordered.len() {
                    self.jobbers_ui.vessel_popup = Some(sel + 1);
                }
            }
            KeyCode::Enter => {
                if let Some(k) = ordered.get(sel) {
                    self.jobbers_ui.selected = Some(k.clone());
                }
                self.jobbers_ui.vessel_popup = None;
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the voyage-type picker popup, highlighting the current type.
    fn open_voyage_popup(&mut self) {
        let cur = VOYAGE_TYPES
            .iter()
            .position(|v| *v == self.jobbers_ui.voyage_type)
            .unwrap_or(0);
        self.jobbers_ui.voyage_popup = Some(cur);
        self.jobbers_ui.focus = JobberFocus::VoyageType;
    }

    /// Modal key handling while the voyage-type picker is open.
    fn handle_voyage_popup_key(
        &mut self,
        key: KeyEvent,
        sel: usize,
    ) -> InputResult {
        match key.code {
            KeyCode::Esc => self.jobbers_ui.voyage_popup = None,
            KeyCode::Up => {
                if sel > 0 {
                    self.jobbers_ui.voyage_popup = Some(sel - 1);
                }
            }
            KeyCode::Down => {
                if sel + 1 < VOYAGE_TYPES.len() {
                    self.jobbers_ui.voyage_popup = Some(sel + 1);
                }
            }
            KeyCode::Enter => {
                if let Some(v) = VOYAGE_TYPES.get(sel) {
                    self.jobbers_ui.voyage_type = *v;
                }
                self.jobbers_ui.voyage_popup = None;
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the pirate-stats popup for the pane's currently-selected pirate.
    fn open_pirate_popup(&mut self, pane: JobberPane) {
        let Some(key) = self.jobbers_ui.selected.clone() else {
            return;
        };
        let names = jobbers::pane_pirates(&self.chatlog, &key, pane);
        let sel = self.jobbers_pane_sel(pane);
        if let Some(name) = names.get(sel) {
            // On-demand: jump this pirate to the top of the fetch queue.
            self.pirate_cache.force_requery(name);
            self.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: name.clone(),
                button: 0,
                offset: 0,
                view_h: 0,
            });
        }
    }

    /// The Skill Leaderboard's columns of ranked pirate names for the selected
    /// vessel (column-major), or empty when no vessel is selected.
    fn leaderboard_cols(&self) -> Vec<Vec<String>> {
        let Some(key) = self.jobbers_ui.selected.as_ref() else {
            return Vec::new();
        };
        jobbers::leaderboard_columns(
            &self.chatlog,
            &self.pirate_cache,
            key,
            self.jobbers_ui.voyage_type,
        )
    }

    /// Number of leaderboard columns for the selected vessel's voyage type.
    fn leaderboard_ncols(&self) -> usize {
        self.leaderboard_cols().len()
    }

    /// Number of ranked pirates in the currently-selected leaderboard column.
    fn leaderboard_current_len(&self) -> usize {
        self.leaderboard_cols()
            .get(self.jobbers_ui.top_col)
            .map_or(0, |c| c.len())
    }

    /// Clamp the leaderboard cursor to the live column/row shape (after a
    /// column switch, or when the aboard set changes under it).
    fn leaderboard_clamp(&mut self) {
        let cols = self.leaderboard_cols();
        if cols.is_empty() {
            self.jobbers_ui.top_col = 0;
            self.jobbers_ui.top_sel = 0;
            return;
        }
        self.jobbers_ui.top_col = self.jobbers_ui.top_col.min(cols.len() - 1);
        let len = cols[self.jobbers_ui.top_col].len();
        self.jobbers_ui.top_sel = if len == 0 {
            0
        } else {
            self.jobbers_ui.top_sel.min(len - 1)
        };
    }

    /// Open the pirate-stats popup for the leaderboard's selected pirate,
    /// jumping it to the top of the fetch queue (mirrors
    /// [`Self::open_pirate_popup`]).
    fn open_leaderboard_popup(&mut self) {
        let cols = self.leaderboard_cols();
        let Some(name) = cols
            .get(self.jobbers_ui.top_col)
            .and_then(|c| c.get(self.jobbers_ui.top_sel))
        else {
            return;
        };
        self.pirate_cache.force_requery(name);
        self.jobbers_ui.pirate_popup = Some(PiratePopup {
            name: name.clone(),
            button: 0,
            offset: 0,
            view_h: 0,
        });
    }

    /// Modal key handling for the pirate-stats popup: ←/→ toggle the two
    /// buttons, ↑/↓ scroll the skill tables, Enter activates, Esc closes.
    fn handle_pirate_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(pp) = self.jobbers_ui.pirate_popup.as_mut() else {
            return InputResult::Consumed;
        };
        match key.code {
            KeyCode::Esc => self.jobbers_ui.pirate_popup = None,
            KeyCode::Left => pp.button = 0,
            KeyCode::Right => pp.button = 1,
            KeyCode::Up => pp.offset = pp.offset.saturating_sub(1),
            KeyCode::Down => pp.offset = pp.offset.saturating_add(1),
            KeyCode::PageUp => {
                let half = (pp.view_h / 2).max(1);
                pp.offset = pp.offset.saturating_sub(half);
            }
            KeyCode::PageDown => {
                let half = (pp.view_h / 2).max(1);
                pp.offset = pp.offset.saturating_add(half);
            }
            KeyCode::Enter => {
                if pp.button == 0 {
                    self.open_trophy_popup();
                } else {
                    self.jobbers_ui.pirate_popup = None;
                }
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the Vampirates skill-distribution popup, parking the cursor on the
    /// most-populated cell so the detail panel starts non-empty.
    fn open_skill_dist_popup(&mut self) {
        let cursor = self
            .jobbers_ui
            .selected
            .as_ref()
            .map(|k| {
                jobbers::default_skill_dist_cursor(
                    &self.chatlog.aboard(k),
                    &self.pirate_cache,
                )
            })
            .unwrap_or((0, 0));
        self.jobbers_ui.skill_dist_popup = Some(SkillDistPopup {
            cursor,
        });
        self.jobbers_ui.focus = JobberFocus::SkillDist;
    }

    /// Modal key handling for the skill-distribution popup: arrows move the
    /// cursor over the 9×9 standing grid (x = Treasure Haul, y =
    /// Carpentry), Esc closes.
    fn handle_skill_dist_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut() else {
            return InputResult::Consumed;
        };
        let (th, carp) = sd.cursor;
        match key.code {
            KeyCode::Esc => self.jobbers_ui.skill_dist_popup = None,
            KeyCode::Left => sd.cursor.0 = th.saturating_sub(1),
            KeyCode::Right => sd.cursor.0 = (th + 1).min(8),
            // ↑ raises Carpentry standing, ↓ lowers it (the grid runs high →
            // low).
            KeyCode::Up => sd.cursor.1 = (carp + 1).min(8),
            KeyCode::Down => sd.cursor.1 = carp.saturating_sub(1),
            _ => {}
        }
        InputResult::Consumed
    }

    /// Modal key handling for the per-fight graph popup: ←/→ change fight, `t`
    /// or Tab toggles the X-axis, Esc closes.
    fn handle_per_fight_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let count = self
            .jobbers_ui
            .selected
            .as_ref()
            .map(|k| crate::jobbers::fight_count(&self.chatlog, k))
            .unwrap_or(0);
        let Some(pf) = self.jobbers_ui.per_fight_popup.as_mut() else {
            return InputResult::Consumed;
        };
        match key.code {
            KeyCode::Esc => self.jobbers_ui.per_fight_popup = None,
            KeyCode::Left => pf.idx = pf.idx.saturating_sub(1),
            KeyCode::Right => {
                pf.idx = (pf.idx + 1).min(count.saturating_sub(1))
            }
            KeyCode::Tab | KeyCode::Char('t') => {
                pf.axis = match pf.axis {
                    crate::voyage::AxisMode::Time => {
                        crate::voyage::AxisMode::Event
                    }
                    crate::voyage::AxisMode::Event => {
                        crate::voyage::AxisMode::Time
                    }
                };
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the trophies popup for the pirate in the stats popup.
    fn open_trophy_popup(&mut self) {
        let Some(name) = self
            .jobbers_ui
            .pirate_popup
            .as_ref()
            .map(|pp| pp.name.clone())
        else {
            return;
        };
        // On-demand: ensure this pirate's trophies are (re)fetched at top
        // priority.
        self.pirate_cache.force_requery(&name);
        self.jobbers_ui.trophy_popup = Some(TrophyPopup {
            name,
            search: String::new(),
            offset: 0,
            view_h: 0,
        });
    }

    /// Modal key handling for the trophies popup: type to filter, ↑/↓ scroll,
    /// Esc returns to the stats popup.
    fn handle_trophy_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(tp) = self.jobbers_ui.trophy_popup.as_mut() else {
            return InputResult::Consumed;
        };
        match key.code {
            // Esc clears a non-empty search first; only then closes the popup.
            KeyCode::Esc => {
                if tp.search.is_empty() {
                    self.jobbers_ui.trophy_popup = None;
                } else {
                    tp.search.clear();
                    tp.offset = 0;
                }
            }
            KeyCode::Up => tp.offset = tp.offset.saturating_sub(1),
            KeyCode::Down => tp.offset = tp.offset.saturating_add(1),
            KeyCode::PageUp => {
                let half = (tp.view_h / 2).max(1);
                tp.offset = tp.offset.saturating_sub(half);
            }
            KeyCode::PageDown => {
                let half = (tp.view_h / 2).max(1);
                tp.offset = tp.offset.saturating_add(half);
            }
            KeyCode::Backspace => {
                tp.search.pop();
                tp.offset = 0;
            }
            KeyCode::Char(c) => {
                tp.search.push(c);
                tp.offset = 0;
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Map a pane focus to its [`JobberPane`], or `None` for non-pane focuses.
    fn focus_pane(focus: JobberFocus) -> Option<JobberPane> {
        match focus {
            JobberFocus::Aboard => Some(JobberPane::Aboard),
            JobberFocus::Greedy => Some(JobberPane::Greedy),
            JobberFocus::Planked => Some(JobberPane::Planked),
            JobberFocus::Enthralled => Some(JobberPane::Enthralled),
            _ => None,
        }
    }

    /// Map a [`JobberPane`] to the focus that lands on it.
    fn pane_focus(pane: JobberPane) -> JobberFocus {
        match pane {
            JobberPane::Aboard => JobberFocus::Aboard,
            JobberPane::Greedy => JobberFocus::Greedy,
            JobberPane::Planked => JobberFocus::Planked,
            JobberPane::Enthralled => JobberFocus::Enthralled,
        }
    }

    /// Number of selectable pirates in a pane for the selected vessel.
    fn jobbers_pane_count(&self, pane: JobberPane) -> usize {
        let Some(key) = self.jobbers_ui.selected.as_ref() else {
            return 0;
        };
        match pane {
            JobberPane::Aboard => self.chatlog.aboard(key).len(),
            JobberPane::Greedy => {
                self.chatlog
                    .vessels
                    .get(key)
                    .map_or(0, |v| v.greedy_by_pirate.len())
            }
            JobberPane::Planked => {
                self.chatlog
                    .vessels
                    .get(key)
                    .map_or(0, |v| v.planked_by_us.len())
            }
            JobberPane::Enthralled => {
                self.chatlog
                    .vessels
                    .get(key)
                    .map_or(0, |v| v.thralls_total.len())
            }
        }
    }

    fn jobbers_pane_sel_mut(&mut self, pane: JobberPane) -> &mut usize {
        match pane {
            JobberPane::Aboard => &mut self.jobbers_ui.aboard_sel,
            JobberPane::Greedy => &mut self.jobbers_ui.greedy_sel,
            JobberPane::Planked => &mut self.jobbers_ui.planked_sel,
            JobberPane::Enthralled => &mut self.jobbers_ui.enthralled_sel,
        }
    }

    /// The pane's current selection, clamped to its live pirate count.
    fn jobbers_pane_sel(&self, pane: JobberPane) -> usize {
        let n = self.jobbers_pane_count(pane);
        let raw = match pane {
            JobberPane::Aboard => self.jobbers_ui.aboard_sel,
            JobberPane::Greedy => self.jobbers_ui.greedy_sel,
            JobberPane::Planked => self.jobbers_ui.planked_sel,
            JobberPane::Enthralled => self.jobbers_ui.enthralled_sel,
        };
        if n == 0 { 0 } else { raw.min(n - 1) }
    }

    /// Move a pane's selection by `delta`, clamped to the pirate count.
    fn jobbers_pane_select_delta(&mut self, pane: JobberPane, delta: i32) {
        let n = self.jobbers_pane_count(pane);
        if n == 0 {
            return;
        }
        let next = (self.jobbers_pane_sel(pane) as i32 + delta)
            .clamp(0, n as i32 - 1) as usize;
        *self.jobbers_pane_sel_mut(pane) = next;
    }

    /// Whether the selected vessel is currently poisoned.
    fn selected_poisoned(&self) -> bool {
        self.jobbers_ui
            .selected
            .as_ref()
            .and_then(|k| self.chatlog.vessels.get(k))
            .is_some_and(|v| v.poisoned)
    }

    /// Clear the poisoned flag on the selected vessel.
    fn jobbers_unpoison(&mut self) {
        if let Some(key) = self.jobbers_ui.selected.clone()
            && let Some(v) = self.chatlog.vessels.get_mut(&key)
        {
            v.poisoned = false;
        }
    }

    // -- mouse handling --

    pub fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        tx: &tokio::sync::mpsc::UnboundedSender<
            Result<HashMap<String, CachedOffers>, String>,
        >,
    ) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = clickmap::hit_test(
                    &self.click_regions,
                    mouse.column,
                    mouse.row,
                ) {
                    // A scrollbar is the one target whose answer depends on
                    // where in it the click landed, so it is read here where
                    // the row is still at hand.
                    if let ClickTarget::Scrollbar {
                        view,
                        axis,
                        bar,
                        total,
                    } = target
                    {
                        self.scroll_bar(
                            view,
                            axis,
                            bar,
                            total,
                            crate::utils::scrollbar_hit(
                                bar,
                                axis,
                                mouse.column,
                                mouse.row,
                            ),
                        );
                        return;
                    }
                    self.handle_click(target, tx);
                }
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(-1, mouse.column, mouse.row);
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(1, mouse.column, mouse.row);
            }
            // Live hover: while the skill-distribution popup is open, moving
            // the mouse over a cell parks the cursor there (updates
            // the detail panel).
            MouseEventKind::Moved => {
                if self.jobbers_ui.skill_dist_popup.is_some()
                    && let Some(ClickTarget::JobberSkillDistCell {
                        th,
                        carp,
                    }) = clickmap::hit_test(
                        &self.click_regions,
                        mouse.column,
                        mouse.row,
                    )
                    && let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut()
                {
                    sd.cursor = (th, carp);
                }
                // Live hover over the Ship Winrate matrix highlights the cell
                // and its row/column headers; leaving the grid
                // clears the highlight.
                if self.voyage_ui.chart_popup == Some(0) {
                    self.voyage_ui.winrate_hover = match clickmap::hit_test(
                        &self.click_regions,
                        mouse.column,
                        mouse.row,
                    ) {
                        Some(ClickTarget::VoyageWinrateCell {
                            row,
                            col,
                        }) => Some((row, col)),
                        _ => None,
                    };
                }
                // Live hover over a Profit Breakdown row parks the tooltip
                // cursor.
                if matches!(
                    self.profits.popup,
                    Some(crate::profits::PopupKind::ProfitResult(
                        _
                    ))
                ) && let Some(ClickTarget::ProfitsBreakdownRow(i)) =
                    clickmap::hit_test(
                        &self.click_regions,
                        mouse.column,
                        mouse.row,
                    )
                {
                    self.profits.breakdown_cursor = i;
                }
            }
            _ => {}
        }
    }

    /// Answer a click (or a wheel notch) on a view's scrollbar. `axis`, `bar`
    /// and `total` are what the bar was drawn from; `hit` is what the click
    /// asked for ([`crate::utils::scrollbar_hit`]).
    ///
    /// Two kinds of view answer it. A view that keeps its own window — the two
    /// popups — takes the ask as the window's new first row. A view whose
    /// window follows a cursor takes it as the cursor's new place and lets
    /// the window come along, since there is nothing else the bar could
    /// mean where the window is not the view's to set. Either way the view
    /// is focused first: a cursor that moves out of sight has not visibly
    /// moved at all.
    fn scroll_bar(
        &mut self,
        view: clickmap::ScrollView,
        axis: clickmap::ScrollAxis,
        bar: Rect,
        total: usize,
        hit: crate::utils::ScrollHit,
    ) {
        use clickmap::{ScrollAxis, ScrollView};

        match view {
            ScrollView::JobberPirateSkills => {
                let last = total.saturating_sub(bar.height as usize);
                if let Some(pp) = self.jobbers_ui.pirate_popup.as_mut() {
                    pp.offset = hit.resolve(pp.offset, last);
                }
            }
            ScrollView::JobberTrophies => {
                let last = total.saturating_sub(bar.height as usize);
                if let Some(tp) = self.jobbers_ui.trophy_popup.as_mut() {
                    tp.offset = hit.resolve(tp.offset, last);
                }
            }
            ScrollView::JobberPane(pane) => {
                let count = self.jobbers_pane_count(pane);
                if count == 0 {
                    return;
                }
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = Self::pane_focus(pane);
                let next = hit.resolve(self.jobbers_pane_sel(pane), count - 1);
                *self.jobbers_pane_sel_mut(pane) = next;
            }
            ScrollView::JobberLeaderboard => {
                let count = self.leaderboard_current_len();
                if count == 0 {
                    return;
                }
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Leaderboard;
                self.jobbers_ui.top_sel =
                    hit.resolve(self.jobbers_ui.top_sel, count - 1);
            }
            ScrollView::ProfitsInventory => {
                let count = self.profits.rows.len();
                if count == 0 {
                    return;
                }
                self.global_focus = GlobalFocus::Content;
                self.profits.focus = crate::profits::Focus::Table;
                match axis {
                    ScrollAxis::Vertical => {
                        let row =
                            self.profits.table_state.selected().unwrap_or(0);
                        self.profits
                            .table_state
                            .select(Some(hit.resolve(row, count - 1)));
                    }
                    // The sideways window follows the cell cursor's column, and
                    // the Item column is not one it can rest on — so the bar's
                    // ends are the first and last editable columns.
                    ScrollAxis::Horizontal => {
                        let first = crate::profits::FIRST_COL;
                        let last =
                            self.profits.last_editable_col(self.market_ok());
                        let col = self
                            .profits
                            .table_state
                            .selected_column()
                            .unwrap_or(first)
                            .max(first);
                        self.profits.table_state.select_column(Some(
                            first + hit.resolve(col - first, last - first),
                        ));
                    }
                }
            }
            ScrollView::VoyageBody => {
                let last = total.saturating_sub(bar.height as usize);
                self.global_focus = GlobalFocus::Content;
                self.voyage_ui.pan = Some(
                    hit.resolve(self.voyage_ui.scroll as usize, last) as u16,
                );
            }
            // The chart's bars pan it, which parts the window from the cursor
            // until a point is selected again. The window is the bar's own to
            // set: its cells are canvas cells, so the last offset falls out of
            // the bar's length the way a popup's does.
            ScrollView::MapCanvas => {
                let last = total.saturating_sub(match axis {
                    ScrollAxis::Vertical => bar.height as usize,
                    ScrollAxis::Horizontal => bar.width as usize,
                });
                let (x, y) = self.map.window;
                self.global_focus = GlobalFocus::Content;
                // An arrow pans by a league point, not by the character cell a
                // row of the list would be: a quarter of a point is no distance
                // at all across a canvas five hundred cells wide.
                let along = match hit {
                    crate::utils::ScrollHit::Step(by) => {
                        let (at, step) = match axis {
                            ScrollAxis::Vertical => (y, crate::map::ui::CELL_H),
                            ScrollAxis::Horizontal => {
                                (x, crate::map::ui::CELL_W)
                            }
                        };
                        if by < 0 {
                            at.saturating_sub(step)
                        } else {
                            (at + step).min(last)
                        }
                    }
                    crate::utils::ScrollHit::Jump {
                        ..
                    } => {
                        match axis {
                            ScrollAxis::Vertical => hit.resolve(y, last),
                            ScrollAxis::Horizontal => hit.resolve(x, last),
                        }
                    }
                };
                self.map.pan = Some(match axis {
                    ScrollAxis::Vertical => (x, along),
                    ScrollAxis::Horizontal => (along, y),
                });
            }
            // The Island column scrolls by the lines it is written in, and
            // keeps the cursor where it is: reading further down a point costs
            // nothing on the chart.
            ScrollView::MapIslandInfo => {
                let last = total.saturating_sub(bar.height as usize);
                self.global_focus = GlobalFocus::Content;
                self.map.info_scroll = hit.resolve(self.map.info_scroll, last);
            }
        }
    }

    fn handle_click(
        &mut self,
        target: ClickTarget,
        tx: &tokio::sync::mpsc::UnboundedSender<
            Result<HashMap<String, CachedOffers>, String>,
        >,
    ) {
        // While the Sea Battles popup is open, Damage* clicks belong to its
        // embedded editor (edits go to the recorded fight; clicks on a
        // read-only widget are swallowed) and must never reach the live
        // calculator behind it.
        if self.voyage_ui.battles_popup.is_some() && is_damage_target(&target) {
            self.voyage_ui.battles_focus =
                crate::voyage::ui::BattlesFocus::Calc;
            crate::damage::apply_click(
                &mut self.voyage_ui.battle_editor,
                &target,
            );
            self.sync_battle_editor();
            return;
        }

        match target {
            ClickTarget::SidebarItem(i) => {
                if i < APP_LIST.len() {
                    self.sidebar_index = i;
                    // Exit has no content to enter — clicking it just selects
                    // it on the bar and shows its widget
                    // (Enter/Esc then quit).
                    if APP_LIST[i] == AppId::Exit {
                        self.global_focus = GlobalFocus::TopBar;
                    } else {
                        self.enter_app();
                    }
                }
            }
            ClickTarget::ProfitsInput => {
                self.global_focus = GlobalFocus::Content;
                self.profits.focus_input();
            }
            ClickTarget::MapPoint {
                x,
                y,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.map.jump_to((x, y));
            }
            ClickTarget::MapHelpClose => {
                self.map.help = false;
            }
            // The column is a region so the wheel over it scrolls it; a click
            // in it selects nothing, since what it says is the cursor's.
            ClickTarget::MapIslandInfo => {}
            // Read in `handle_mouse`, which still has the clicked row — the
            // only thing a scrollbar click says.
            ClickTarget::Scrollbar {
                ..
            } => {}
            ClickTarget::ProfitsTableCell {
                row,
                col,
            } => {
                self.global_focus = GlobalFocus::Content;
                if col == 0 {
                    // Clicking the item name column: open delete confirm
                    if row < self.profits.rows.len() {
                        let name = commod_name(
                            &self.commodities,
                            self.profits.rows[row].commod_id,
                        )
                        .to_owned();
                        self.profits.popup = Some(
                            crate::profits::PopupKind::DeleteConfirm {
                                row_idx: row,
                                // Default to Yes so a quick Enter confirms the
                                // delete.
                                name,
                                yes_focused: true,
                            },
                        );
                        self.profits.focus = crate::profits::Focus::Popup;
                    }
                } else {
                    self.profits.focus = crate::profits::Focus::Table;
                    self.profits.table_state.select(Some(row));
                    self.profits.table_state.select_column(Some(col));
                }
            }
            ClickTarget::ProfitsPanel(i) => {
                self.global_focus = GlobalFocus::Content;
                self.profits.focus_panel(i);
            }
            ClickTarget::ProfitsButton => {
                self.global_focus = GlobalFocus::Content;
                self.profits.focus = crate::profits::Focus::Button;
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    ocean_geo: self.ocean_geo(),
                    loading: self.loading,
                    market_supported: self.market_ok(),
                    pillage_gross,
                    pillage_stolen,
                    pillage_chest,
                };
                let result = self.profits.handle_button_activate(&shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsBreakdownRow(i) => {
                self.profits.breakdown_cursor = i;
            }
            ClickTarget::ProfitsPopupNo => {
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    ocean_geo: self.ocean_geo(),
                    loading: self.loading,
                    market_supported: self.market_ok(),
                    pillage_gross,
                    pillage_stolen,
                    pillage_chest,
                };
                let result = self.profits.handle_popup_click(false, &shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupYes => {
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    ocean_geo: self.ocean_geo(),
                    loading: self.loading,
                    market_supported: self.market_ok(),
                    pillage_gross,
                    pillage_stolen,
                    pillage_chest,
                };
                let result = self.profits.handle_popup_click(true, &shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupOk => {
                self.global_focus = GlobalFocus::Content;
                self.profits.dismiss_ok_popup();
            }
            ClickTarget::DamageCell {
                row,
                side,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                if row == crate::damage::ROW_SHIP {
                    let current = match side {
                        crate::damage::Side::Left => self.damage.left_ship,
                        crate::damage::Side::Right => self.damage.right_ship,
                    };
                    self.damage.popup = Some(crate::damage::ShipSelectPopup {
                        side,
                        selected: current,
                    });
                } else {
                    self.damage.focus_row = row;
                    self.damage.focus_side = side;
                }
            }
            ClickTarget::DamageIncrement {
                row,
                side,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.increment();
            }
            ClickTarget::DamageDecrement {
                row,
                side,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.decrement();
            }
            ClickTarget::DamageRam => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_RAMS;
            }
            ClickTarget::DamageRamIncrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_RAMS;
                self.damage.increment();
            }
            ClickTarget::DamageRamDecrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_RAMS;
                self.damage.decrement();
            }
            ClickTarget::DamageShipItem(i) => {
                if let Some(ref popup) = self.damage.popup {
                    let side = popup.side;
                    match side {
                        crate::damage::Side::Left => self.damage.left_ship = i,
                        crate::damage::Side::Right => {
                            self.damage.right_ship = i
                        }
                    }
                    self.damage.popup = None;
                    if !self.damage.counts_are_default() {
                        self.damage.reset_prompt = Some(true);
                    }
                }
            }
            ClickTarget::DamageResetYes => {
                self.global_focus = GlobalFocus::Content;
                self.damage.clear_counts();
                self.damage.reset_prompt = None;
            }
            ClickTarget::DamageResetNo => {
                self.global_focus = GlobalFocus::Content;
                self.damage.reset_prompt = None;
            }
            ClickTarget::DamageBattleApply => {
                self.global_focus = GlobalFocus::Content;
                self.damage.commit_battle_prompt(true);
            }
            ClickTarget::DamageBattleKeep => {
                self.global_focus = GlobalFocus::Content;
                self.damage.commit_battle_prompt(false);
            }
            ClickTarget::JobberVesselButton => {
                self.global_focus = GlobalFocus::Content;
                self.open_vessel_popup();
            }
            ClickTarget::JobberVesselItem(i) => {
                let ordered = self.chatlog.vessels_by_recency();
                if let Some(key) = ordered.get(i) {
                    self.jobbers_ui.selected = Some(key.clone());
                }
                self.jobbers_ui.vessel_popup = None;
            }
            ClickTarget::JobberShipType => {
                self.global_focus = GlobalFocus::Content;
                self.open_ship_popup();
            }
            ClickTarget::JobberShipItem(i) => {
                if i < crate::ships::SHIPS.len()
                    && let Some(vessel) = self.jobbers_ui.selected.clone()
                {
                    self.jobbers_ui.ship_types.insert(vessel, i);
                }
                self.jobbers_ui.ship_popup = None;
            }
            ClickTarget::JobberVoyageType => {
                self.global_focus = GlobalFocus::Content;
                self.open_voyage_popup();
            }
            ClickTarget::JobberVoyageItem(i) => {
                if let Some(v) = VOYAGE_TYPES.get(i) {
                    self.jobbers_ui.voyage_type = *v;
                }
                self.jobbers_ui.voyage_popup = None;
            }
            ClickTarget::JobberUnpoison => {
                self.global_focus = GlobalFocus::Content;
                // Nothing to unpoison on a clean vessel.
                if self.selected_poisoned() {
                    self.jobbers_unpoison();
                }
                self.jobbers_ui.focus = JobberFocus::Vessels;
            }
            ClickTarget::JobberLeaderboard => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Leaderboard;
            }
            ClickTarget::JobberLeaderboardPirate {
                col,
                row,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Leaderboard;
                self.jobbers_ui.top_col = col;
                self.jobbers_ui.top_sel = row;
                self.leaderboard_clamp();
            }
            ClickTarget::JobberAboardList => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Aboard;
            }
            ClickTarget::JobberGreedyList => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Greedy;
            }
            ClickTarget::JobberPlankedList => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Planked;
            }
            ClickTarget::JobberEnthralledList => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Enthralled;
            }
            ClickTarget::JobberPirate {
                pane,
                idx,
            } => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = Self::pane_focus(pane);
                *self.jobbers_pane_sel_mut(pane) = idx;
            }
            ClickTarget::JobberPirateSeeTrophies => {
                if let Some(pp) = self.jobbers_ui.pirate_popup.as_mut() {
                    pp.button = 0;
                }
                self.open_trophy_popup();
            }
            ClickTarget::JobberPirateClose => {
                self.jobbers_ui.pirate_popup = None;
            }
            // A click on the body of the trophies popup is a no-op (the region
            // exists only so the scroll wheel has a target there); its Close
            // button is the one part of it that answers to the mouse.
            ClickTarget::JobberTrophyArea => {}
            ClickTarget::JobberTrophyClose => {
                self.jobbers_ui.trophy_popup = None;
            }
            ClickTarget::JobberSkillDistButton => self.open_skill_dist_popup(),
            // Clicking a cell parks the cursor there (same as hovering it).
            ClickTarget::JobberSkillDistCell {
                th,
                carp,
            } => {
                if let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut() {
                    sd.cursor = (th, carp);
                }
            }
            // A click on the backdrop (outside the cells) dismisses the popup.
            ClickTarget::JobberSkillDistClose => {
                self.jobbers_ui.skill_dist_popup = None;
            }
            // Open the per-fight advantage graph at the latest fight (wave).
            ClickTarget::JobberPerFightButton => {
                let last = self
                    .jobbers_ui
                    .selected
                    .as_ref()
                    .map(|k| crate::jobbers::fight_count(&self.chatlog, k))
                    .unwrap_or(0);
                self.jobbers_ui.per_fight_popup =
                    Some(crate::jobbers::PerFightPopup {
                        idx: last.saturating_sub(1),
                        axis: crate::voyage::AxisMode::default(),
                    });
            }
            ClickTarget::JobberPerFightClose => {
                self.jobbers_ui.per_fight_popup = None;
            }
            ClickTarget::JobberPerFightAxisToggle => {
                if let Some(pf) = self.jobbers_ui.per_fight_popup.as_mut() {
                    pf.axis = match pf.axis {
                        crate::voyage::AxisMode::Time => {
                            crate::voyage::AxisMode::Event
                        }
                        crate::voyage::AxisMode::Event => {
                            crate::voyage::AxisMode::Time
                        }
                    };
                }
            }
            ClickTarget::JobberPerFightPrev => {
                if let Some(pf) = self.jobbers_ui.per_fight_popup.as_mut() {
                    pf.idx = pf.idx.saturating_sub(1);
                }
            }
            ClickTarget::JobberPerFightNext => {
                let count = self
                    .jobbers_ui
                    .selected
                    .as_ref()
                    .map(|k| crate::jobbers::fight_count(&self.chatlog, k))
                    .unwrap_or(0);
                if let Some(pf) = self.jobbers_ui.per_fight_popup.as_mut() {
                    pf.idx = (pf.idx + 1).min(count.saturating_sub(1));
                }
            }
            ClickTarget::VoyagePrev => self.nav_voyage(-1),
            ClickTarget::VoyageNext => self.nav_voyage(1),
            ClickTarget::VoyageSaveOpen => self.open_voyage_save_prompt(),
            ClickTarget::VoyageSaveConfirm => {
                self.save_displayed_voyage();
                self.voyage_ui.prompt = None;
            }
            ClickTarget::VoyageSaveCancel => {
                self.voyage_ui.prompt = None;
            }
            ClickTarget::VoyageStat {
                idx,
            } => {
                self.voyage_focus(idx);
                // The Sea Battles section (focus 0) opens the per-fight log.
                if idx == 0 {
                    self.open_battles_popup();
                }
            }
            ClickTarget::VoyageChart {
                idx,
            } => {
                self.voyage_focus(self.voyage_ui.n_stats + idx);
                if crate::voyage::ui::CHART_ENLARGEABLE.get(idx) == Some(&true)
                {
                    self.voyage_ui.chart_popup = Some(idx);
                }
            }
            ClickTarget::VoyageChartClose => {
                self.voyage_ui.chart_popup = None;
                self.voyage_ui.winrate_hover = None;
            }
            // Clicking a matrix cell just parks the highlight there (same as
            // hover).
            ClickTarget::VoyageWinrateCell {
                row,
                col,
            } => {
                self.voyage_ui.winrate_hover = Some((row, col));
            }
            ClickTarget::VoyageBattlesClose => {
                // Closing the editor's ship picker takes priority over the
                // popup.
                if self.voyage_ui.battle_editor.popup.is_some() {
                    self.voyage_ui.battle_editor.popup = None;
                } else {
                    self.voyage_ui.battles_popup = None;
                }
            }
            ClickTarget::VoyageBattlesPrev => self.battles_page(-1),
            ClickTarget::VoyageBattlesNext => self.battles_page(1),
            ClickTarget::VoyageBattlesRecord => {
                self.voyage_ui.battles_focus =
                    crate::voyage::ui::BattlesFocus::Record;
                self.toggle_battle_record();
            }
        }
    }

    fn handle_scroll(&mut self, delta: i32, col: u16, row: u16) {
        // The wheel over a view's scrollbar is a notch of that view, whichever
        // page the view belongs to.
        if let Some(ClickTarget::Scrollbar {
            view,
            axis,
            bar,
            total,
        }) = clickmap::hit_test(&self.click_regions, col, row)
        {
            self.scroll_bar(
                view,
                axis,
                bar,
                total,
                crate::utils::ScrollHit::Step(delta.signum()),
            );
            return;
        }

        match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                if self.profits.focus == crate::profits::Focus::Table {
                    if delta < 0 {
                        self.profits.table_up();
                    } else {
                        if let Some(row) = self.profits.table_state.selected()
                            && row + 1 < self.profits.rows.len()
                        {
                            self.profits.table_state.select(Some(row + 1));
                        }
                    }
                }
            }
            AppId::Damage => {
                if let Some(ref mut popup) = self.damage.popup {
                    if delta < 0 {
                        if 0 < popup.selected {
                            popup.selected -= 1;
                        }
                    } else {
                        if popup.selected + 1 < crate::ships::SHIPS.len() {
                            popup.selected += 1;
                        }
                    }
                }
            }
            AppId::Chatlog => {
                // While a picker popup is open, the wheel moves its highlight.
                if let Some(sel) = self.jobbers_ui.ship_popup {
                    let count = crate::ships::SHIPS.len();
                    self.jobbers_ui.ship_popup = Some(
                        if delta < 0 {
                            sel.saturating_sub(1)
                        } else {
                            (sel + 1).min(count.saturating_sub(1))
                        },
                    );
                    return;
                }
                if let Some(sel) = self.jobbers_ui.vessel_popup {
                    let count = self.chatlog.vessels_by_recency().len();
                    self.jobbers_ui.vessel_popup = Some(
                        if delta < 0 {
                            sel.saturating_sub(1)
                        } else {
                            (sel + 1).min(count.saturating_sub(1))
                        },
                    );
                    return;
                }
                if let Some(sel) = self.jobbers_ui.voyage_popup {
                    let count = VOYAGE_TYPES.len();
                    self.jobbers_ui.voyage_popup = Some(
                        if delta < 0 {
                            sel.saturating_sub(1)
                        } else {
                            (sel + 1).min(count.saturating_sub(1))
                        },
                    );
                    return;
                }
                // The trophies popup scrolls its content with the wheel.
                if let Some(tp) = self.jobbers_ui.trophy_popup.as_mut() {
                    if delta < 0 {
                        tp.offset = tp.offset.saturating_sub(1);
                    } else {
                        tp.offset = tp.offset.saturating_add(1);
                    }
                    return;
                }
                // The pirate-stats popup scrolls its skill tables.
                if let Some(pp) = self.jobbers_ui.pirate_popup.as_mut() {
                    if delta < 0 {
                        pp.offset = pp.offset.saturating_sub(1);
                    } else {
                        pp.offset = pp.offset.saturating_add(1);
                    }
                    return;
                }
                // The wheel over the Skill Leaderboard moves its selection
                // within the current column (the shared window
                // auto-scrolls to follow it).
                if let Some(
                    ClickTarget::JobberLeaderboard
                    | ClickTarget::JobberLeaderboardPirate {
                        ..
                    },
                ) = clickmap::hit_test(&self.click_regions, col, row)
                {
                    let len = self.leaderboard_current_len();
                    if len > 0 {
                        let next = (self.jobbers_ui.top_sel as i32
                            + delta.signum())
                        .clamp(0, len as i32 - 1)
                            as usize;
                        self.jobbers_ui.top_sel = next;
                    }
                    return;
                }
                // Otherwise move the selection of whichever pane the cursor is
                // over (the panes auto-scroll to follow the
                // selection).
                let pane =
                    match clickmap::hit_test(&self.click_regions, col, row) {
                        Some(ClickTarget::JobberAboardList)
                        | Some(ClickTarget::JobberPirate {
                            pane: JobberPane::Aboard,
                            ..
                        }) => JobberPane::Aboard,
                        Some(ClickTarget::JobberGreedyList)
                        | Some(ClickTarget::JobberPirate {
                            pane: JobberPane::Greedy,
                            ..
                        }) => JobberPane::Greedy,
                        Some(ClickTarget::JobberPlankedList)
                        | Some(ClickTarget::JobberPirate {
                            pane: JobberPane::Planked,
                            ..
                        }) => JobberPane::Planked,
                        Some(ClickTarget::JobberEnthralledList)
                        | Some(ClickTarget::JobberPirate {
                            pane: JobberPane::Enthralled,
                            ..
                        }) => JobberPane::Enthralled,
                        _ => return,
                    };
                self.jobbers_pane_select_delta(pane, delta.signum());
            }
            AppId::Voyage => {
                let focus = self.voyage_ui.focus;
                self.voyage_focus(
                    if delta < 0 {
                        focus.saturating_sub(1)
                    } else {
                        focus.saturating_add(1)
                    },
                );
            }
            // The wheel pans the chart and leaves the cursor where it is. The
            // far end is clamped by the render, which is what knows how large
            // the canvas is.
            AppId::Map => {
                // over the Island column it scrolls what the column says
                if let Some(ClickTarget::MapIslandInfo) =
                    clickmap::hit_test(&self.click_regions, col, row)
                {
                    self.map.info_scroll = if delta < 0 {
                        self.map.info_scroll.saturating_sub(1)
                    } else {
                        self.map.info_scroll.saturating_add(1)
                    };
                    return;
                }
                let (x, y) = self.map.window;
                self.map.pan = Some((
                    x,
                    if delta < 0 {
                        y.saturating_sub(crate::map::ui::CELL_H)
                    } else {
                        y.saturating_add(crate::map::ui::CELL_H)
                    },
                ));
            }
            AppId::Exit => {}
        }
    }

    fn process_input_result(
        &mut self,
        result: InputResult,
        tx: &tokio::sync::mpsc::UnboundedSender<
            Result<HashMap<String, CachedOffers>, String>,
        >,
    ) {
        match result {
            InputResult::Consumed => {}
            InputResult::Exit => {
                self.global_focus = GlobalFocus::TopBar;
            }
            InputResult::StartFetch(purpose) => {
                self.loading = true;
                self.spawn_fetch(purpose, tx);
            }
            InputResult::RebuildIslands => {
                self.rebuild_island_list();
            }
        }
    }

    // -- fetch handling --

    pub fn handle_fetch_result(
        &mut self,
        result: Result<HashMap<String, CachedOffers>, String>,
    ) {
        self.loading = false;
        match result {
            Ok(offers_map) => {
                match self.profits.fetch_purpose {
                    FetchPurpose::Islands => {
                        self.cached_offers.extend(offers_map);
                        self.rebuild_island_list();
                    }
                    FetchPurpose::Profits => {
                        self.cached_offers = offers_map;
                        self.rebuild_island_list();
                        let (pillage_gross, pillage_stolen, pillage_chest) =
                            self.chatlog.current_pillage_poe();
                        let shared = SharedState {
                            commodities: &self.commodities,
                            cached_offers: &self.cached_offers,
                            available_islands: &self.available_islands,
                            ocean_geo: self.ocean_geo(),
                            loading: self.loading,
                            market_supported: self.market_ok(),
                            pillage_gross,
                            pillage_stolen,
                            pillage_chest,
                        };
                        self.profits.calculate_or_warn(&shared);
                    }
                }
            }
            Err(msg) => {
                self.profits.calc_error = Some(msg);
            }
        }
    }

    fn spawn_fetch(
        &self,
        purpose: FetchPurpose,
        tx: &tokio::sync::mpsc::UnboundedSender<
            Result<HashMap<String, CachedOffers>, String>,
        >,
    ) {
        let names: Vec<String> = match purpose {
            FetchPurpose::Islands => {
                self.profits
                    .rows
                    .iter()
                    .map(|r| {
                        commod_name(&self.commodities, r.commod_id).to_owned()
                    })
                    .collect()
            }
            FetchPurpose::Profits => {
                self.profits
                    .rows
                    .iter()
                    .filter(|r| {
                        let restock = r.restock.parse::<u64>().unwrap_or(0);
                        let stock = r.stock.parse::<u64>().unwrap_or(0);
                        let booty = r.booty.parse::<u64>().unwrap_or(0);
                        restock != 0 || stock != 0 || booty != 0
                    })
                    .map(|r| {
                        commod_name(&self.commodities, r.commod_id).to_owned()
                    })
                    .collect()
            }
        };

        let tx = tx.clone();
        let ocean = self.ocean.filter(|_| self.query_market);
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = match ocean.filter(|o| o.market_supported()) {
                Some(o) => fetch_offers_for(&client, &names, o).await,
                None => {
                    Err("No market-data ocean selected for this run".to_owned())
                }
            };
            let _ = tx.send(result);
        });
    }
}

#[cfg(test)]
mod battle_prompt_tests {
    use super::*;

    fn shell() -> AppShell {
        let mut s = AppShell::new(vec![]);
        s.chatlog.player_name = Some(std::sync::Arc::from("Playerone"));
        s
    }

    fn start_a_voyage(s: &mut AppShell) {
        s.feed_chat_line("====== 2026/05/14 ======");
        s.feed_chat_line("[01:00:00] Going aboard the War Frigate...");
        s.feed_chat_line(
            "[01:00:05] Playerone issued an order to set the vessel to sail.",
        );
    }

    #[test]
    fn new_battle_arms_prompt_without_touching_the_calculator() {
        let mut s = shell();
        start_a_voyage(&mut s);
        let before = s.damage.right_ship;
        s.feed_chat_line(
            "[01:01:00] You have been intercepted by the Xebec 'Foo'!",
        );
        // The prompt is armed with the detected foe hull, defaulting to Apply,
        // but the calculator is untouched until the user answers.
        let prompt = s.damage.battle_prompt.as_ref().expect("prompt armed");
        assert_eq!(
            prompt.foe_ship,
            crate::ships::ship_index("Xebec")
        );
        assert!(prompt.apply);
        assert_eq!(s.damage.right_ship, before);
        // Apply seeds the foe hull and closes the prompt.
        s.damage.commit_battle_prompt(true);
        assert_eq!(
            s.damage.right_ship,
            crate::ships::ship_index("Xebec").unwrap()
        );
        assert!(s.damage.battle_prompt.is_none());
    }

    #[test]
    fn black_ship_reveal_updates_the_open_prompt() {
        let mut s = shell();
        start_a_voyage(&mut s);
        s.feed_chat_line(
            "[01:01:00] You have been intercepted by the Xebec 'Foo'!",
        );
        {
            let p = s.damage.battle_prompt.as_ref().unwrap();
            assert_eq!(
                p.foe_ship,
                crate::ships::ship_index("Xebec")
            );
            assert_eq!(p.ship_name.as_deref(), Some("Foo"));
            assert!(p.note.is_none()); // ordinary brigand so far
        }
        // The Black Ship replaces the target while the prompt is still open.
        s.feed_chat_line(
            "[01:01:05] Dark clouds gather as ye bear down upon yer hapless \
             victims, and from the miasma emerges the Black Ship to take the \
             place of yer target in battle! Arrrrgh! Ye be doomed fer sure!",
        );
        let p = s.damage.battle_prompt.as_ref().unwrap();
        assert_eq!(
            p.foe_ship,
            crate::ships::ship_index("Grand Frigate")
        );
        assert_eq!(p.note.as_deref(), Some("Black Ship"));
        // Apply now seeds the corrected hull.
        s.damage.commit_battle_prompt(true);
        assert_eq!(
            s.damage.right_ship,
            crate::ships::ship_index("Grand Frigate").unwrap()
        );
    }

    #[test]
    fn keep_leaves_hull_and_tally_untouched() {
        let mut s = shell();
        start_a_voyage(&mut s);
        s.damage.right_ship = 3;
        s.damage.left[0] = 5; // a tally to preserve
        s.feed_chat_line(
            "[01:01:00] You have been intercepted by the Xebec 'Foo'!",
        );
        s.damage.commit_battle_prompt(false);
        assert_eq!(s.damage.right_ship, 3);
        assert_eq!(s.damage.left[0], 5);
        assert!(s.damage.battle_prompt.is_none());
    }
}

#[cfg(test)]
mod restock_scope_tests {
    use super::*;

    /// The real Emerald geography from the embedded bare cache. Orion has seven
    /// islands (Aimuari, Chachapoya, Matariki, Pukru, Quetzal, Saiph, Toba).
    fn emerald() -> &'static bare::Ocean {
        bare::BARE.ocean("Emerald").expect("Emerald in bare cache")
    }

    #[test]
    fn blank_query_is_ocean_wide() {
        let scope = resolve_restock_scope("   ", &[], Some(emerald()));
        assert!(matches!(scope, RestockScope::OceanWide));
        assert!(scope.island_filter().is_none());
    }

    #[test]
    fn archipelago_name_fans_out_to_all_its_islands() {
        // Even with no offers yet, the archipelago resolves to its full island
        // set.
        let scope = resolve_restock_scope("Orion", &[], Some(emerald()));
        let RestockScope::Archipelago {
            name,
            islands,
        } = scope
        else {
            panic!("expected an archipelago scope");
        };
        assert_eq!(name, "Orion");
        assert_eq!(islands.len(), 7);
        assert!(islands.contains(&"Pukru Island".to_string()));
        assert!(islands.contains(&"Toba Island".to_string()));
        assert!(islands.contains(&"Saiph Island".to_string()));
    }

    #[test]
    fn island_name_resolves_to_a_single_island() {
        let islands =
            vec!["Pukru Island".to_string(), "Toba Island".to_string()];
        let scope = resolve_restock_scope(
            "Pukru Island",
            &islands,
            Some(emerald()),
        );
        let filter = scope.island_filter().expect("island scope filters");
        assert_eq!(filter, ["Pukru Island".to_string()]);
    }

    #[test]
    fn island_prefix_wins_over_archipelago_fuzz() {
        // "Pukru" is a unique island prefix and no archipelago; it must stay an
        // island.
        let islands = vec!["Pukru Island".to_string()];
        let scope = resolve_restock_scope("Pukru", &islands, Some(emerald()));
        assert!(
            matches!(scope, RestockScope::Island(ref n) if n == "Pukru Island")
        );
    }

    #[test]
    fn unrecognized_text_is_unknown() {
        let scope = resolve_restock_scope(
            "Nowhere",
            &["Pukru Island".to_string()],
            Some(emerald()),
        );
        assert!(matches!(scope, RestockScope::Unknown));
        assert!(scope.island_filter().is_none());
    }

    #[test]
    fn archipelago_needs_geography() {
        // Without ocean geography, an archipelago name can't resolve.
        let scope = resolve_restock_scope(
            "Orion",
            &["Pukru Island".to_string()],
            None,
        );
        assert!(matches!(scope, RestockScope::Unknown));
    }
}

#[cfg(test)]
mod map_cursor_tests {
    use super::*;

    fn on_emerald() -> AppShell {
        let mut s = AppShell::new(vec![]);
        s.ocean = Some(crate::ocean::Ocean::Emerald);
        s
    }

    #[test]
    fn the_cached_view_point_comes_back() {
        let mut s = on_emerald();
        let at = Map::for_ocean("Emerald")
            .expect("Emerald map")
            .islands
            .first()
            .expect("an island on the map")
            .at();
        s.restore_map_cursor(Some(at));
        assert_eq!(s.map.cursor, Some(at));
    }

    #[test]
    fn a_view_point_that_is_no_longer_on_the_map_is_dropped() {
        let mut s = on_emerald();
        // every league point has an odd x + y, so this cell is open water
        s.restore_map_cursor(Some((0, 0)));
        assert_eq!(s.map.cursor, None);
    }

    #[test]
    fn an_ocean_with_no_map_keeps_the_cursor_unplaced() {
        let mut s = AppShell::new(vec![]);
        s.restore_map_cursor(Some((1, 1)));
        assert_eq!(s.map.cursor, None);
    }
}

#[cfg(test)]
mod voyage_scroll_tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::*;
    use crate::clickmap::{ClickTarget, ScrollAxis};

    /// A shell on the Voyage page with one finished fight behind it, which is
    /// enough for a body taller than any window over it.
    fn with_a_voyage() -> AppShell {
        let mut shell = AppShell::new(Vec::new());
        shell.chatlog.attached = true;
        shell.chatlog.player_name = Some(std::sync::Arc::from("Playerone"));
        for line in [
            "====== 2026/06/16 ======",
            "[01:00:00] Going aboard the Test Vessel...",
            "[01:00:05] This vessel is now Pillaging, Average to Hard \
             Barbarians.",
            "[01:00:06] Playerone issued an order to set the vessel to sail.",
            "[01:05:00] You intercepted the War Frigate 'Modest Sild'!",
            "[01:06:00] Test Vessel has grappled Modest Sild. A melee \
             breaks out between the crews!",
            "[01:07:00] Game over.  Winners: Playerone.",
            "[01:07:05] The victors plundered 7,756 pieces of eight and 9 \
             units of goods from the defeated vessel.",
        ] {
            shell.chatlog.process_line(line);
        }
        shell.sidebar_index = APP_LIST
            .iter()
            .position(|a| *a == AppId::Voyage)
            .expect("the Voyage page");
        shell.global_focus = GlobalFocus::Content;
        shell
    }

    /// Draw, and hand back the body's scrollbar.
    fn bar(shell: &mut AppShell) -> Rect {
        let mut terminal =
            Terminal::new(TestBackend::new(157, 37)).expect("terminal");
        terminal.draw(|frame| shell.render(frame)).expect("draw");
        shell
            .click_regions
            .iter()
            .find_map(|r| {
                match r.target {
                    ClickTarget::Scrollbar {
                        axis: ScrollAxis::Vertical,
                        bar,
                        ..
                    } => Some(bar),
                    _ => None,
                }
            })
            .expect("the body scrolls, so it has a bar")
    }

    fn click(shell: &mut AppShell, at: Rect, row: u16) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        shell.handle_mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(
                    crossterm::event::MouseButton::Left,
                ),
                column: at.x,
                row,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &tx,
        );
    }

    /// Reading down the track moves the body down it in step: every cell of the
    /// track is a place the body can be, and no two neighbouring cells are more
    /// than a cell's worth of rows apart.
    ///
    /// The bar used to move the *focus* instead, and the focusable items are
    /// not spread evenly down the body — a stat is one row, a chart is nine. So
    /// two thirds of the track scrolled nothing at all (those stats were
    /// already on show) and the last two cells leapt thirty rows between them.
    #[test]
    fn the_bar_walks_the_body_evenly_down_its_track() {
        let mut shell = with_a_voyage();
        let at = bar(&mut shell);
        let track = at.height - 2;

        let mut rows = Vec::new();
        for cell in 1 ..= track {
            let mut shell = with_a_voyage();
            let at = bar(&mut shell);
            click(&mut shell, at, at.y + cell);
            let _ = bar(&mut shell);
            rows.push(shell.voyage_ui.scroll);
        }

        let last = *rows.last().expect("a track to read");
        assert_eq!(
            rows[0], 0,
            "the first cell is the top: {rows:?}"
        );
        // What the body can scroll by, and so the most one cell of track may
        // ask for: the rows off-screen, shared over the cells that can ask.
        let per_cell = last.div_ceil(track - 1) + 1;
        for pair in rows.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "the track doubles back: {rows:?}",
            );
            assert!(
                pair[1] - pair[0] <= per_cell,
                "a cell of track jumped {} rows, over {per_cell}: {rows:?}",
                pair[1] - pair[0],
            );
        }

        // And the focus takes the body back, however far it was scrolled.
        let mut shell = with_a_voyage();
        let at = bar(&mut shell);
        click(&mut shell, at, at.y + track);
        let _ = bar(&mut shell);
        assert_eq!(shell.voyage_ui.scroll, last);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        shell.handle_key(
            KeyEvent::new(
                KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        let _ = bar(&mut shell);
        assert_eq!(shell.voyage_ui.pan, None);
        assert!(
            shell.voyage_ui.scroll < last,
            "the focus did not bring the body back",
        );
    }
}

#[cfg(test)]
mod map_scroll_tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{
        clickmap::ClickTarget,
        islands::{CachedIslands, IslandInfo},
    };

    /// A shell on the Map page of the Emerald Ocean, cursor on a capital whose
    /// island list is in, which is more than a short column can hold.
    fn on_the_map() -> AppShell {
        let mut shell = AppShell::new(Vec::new());
        shell.ocean = Some(crate::ocean::Ocean::Emerald);
        shell.sidebar_index = APP_LIST
            .iter()
            .position(|a| *a == AppId::Map)
            .expect("the Map page");
        shell.global_focus = GlobalFocus::Content;
        shell.map.pirate = Some("playerone".to_owned());
        let map = crate::map::data::Map::for_ocean("Emerald")
            .expect("the Emerald map");
        let alkaid = map
            .islands
            .iter()
            .find(|i| i.name == "Alkaid Island")
            .expect("Alkaid Island on the map");
        shell.map.cursor = Some(alkaid.at());
        shell.islands = Some(CachedIslands {
            fetched_at: chrono::Utc::now(),
            islands: vec![IslandInfo {
                name: "Alkaid Island".to_owned(),
                governor: Some("Someone".to_owned()),
                flag: Some("Some Flag".to_owned()),
                property_tax: Some(15),
                exports: ["Hemp", "Iron", "Wood", "Cloth", "Stone"]
                    .map(str::to_owned)
                    .to_vec(),
            }],
        });
        shell
    }

    /// Draw, and hand back a cell inside the Island column that is none of its
    /// scrollbar.
    fn in_the_column(shell: &mut AppShell) -> (u16, u16) {
        let mut terminal =
            Terminal::new(TestBackend::new(120, 20)).expect("terminal");
        terminal.draw(|frame| shell.render(frame)).expect("draw");
        let column = shell
            .click_regions
            .iter()
            .find_map(|r| {
                matches!(r.target, ClickTarget::MapIslandInfo).then_some(r.rect)
            })
            .expect("the Island column is a region of its own");
        (column.x + 1, column.y + 1)
    }

    /// The wheel belongs to whatever it is over: the column reads on where it
    /// sits, and the chart stays where the cursor put it.
    #[test]
    fn the_wheel_over_the_island_column_scrolls_what_it_says() {
        let mut shell = on_the_map();
        let (col, row) = in_the_column(&mut shell);

        shell.handle_scroll(1, col, row);
        assert_eq!(shell.map.info_scroll, 1);
        assert_eq!(
            shell.map.pan, None,
            "the chart followed the column"
        );

        shell.handle_scroll(-1, col, row);
        assert_eq!(shell.map.info_scroll, 0);

        // over the chart the same notch pans instead
        shell.handle_scroll(1, 2, row);
        assert_eq!(shell.map.info_scroll, 0);
        assert!(
            shell.map.pan.is_some(),
            "the chart did not pan"
        );
    }
}

#[cfg(test)]
mod topbar_tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::{
        APP_LIST,
        AppShell,
        TOPBAR_PADDING,
        topbar_min_width,
        topbar_slots,
    };

    /// The widest a page's own furniture may need. Each page decides for itself
    /// whether the window is wide enough for what it is showing (see
    /// [`crate::utils::too_narrow`]), so no gate in the app holds this figure;
    /// the tests below do. Data a page is given can be longer than any width —
    /// a pirate with a long name — and is not what this bounds.
    const MIN_WIDTH: u16 = 80;

    /// Draw into a terminal of the given size and return the screen as text.
    fn screen(width: u16, height: u16) -> String {
        let mut shell = AppShell::new(Vec::new());
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| shell.render(frame)).expect("draw");
        format!("{}", terminal.backend())
    }

    #[test]
    fn slots_span_the_bar_and_never_starve_a_label() {
        for width in [topbar_min_width(), 80, 120, 200] {
            let slots = topbar_slots(width);
            assert_eq!(
                slots.iter().sum::<u16>(),
                width,
                "slots must tile the bar exactly at {width}",
            );
            for (app, slot) in APP_LIST.iter().zip(&slots) {
                assert!(
                    app.label_width() + 2 * TOPBAR_PADDING <= *slot,
                    "{:?} has no room for its label at {width}",
                    app.bar_lines(),
                );
            }
        }
    }

    /// The floor has to be the labels' own requirement: renaming a tab must
    /// move it rather than silently start clipping.
    #[test]
    fn every_label_is_drawn_whole_at_the_bar_minimum() {
        let screen = screen(topbar_min_width(), 8);
        for app in APP_LIST {
            let (upper, lower) = app.bar_lines();
            assert!(
                screen.contains(upper),
                "{upper:?} missing from the bar"
            );
            assert!(
                lower.is_empty() || screen.contains(lower),
                "{lower:?} missing from the bar",
            );
        }
    }

    #[test]
    fn a_bar_that_would_clip_a_label_is_not_drawn_at_all() {
        let screen = screen(topbar_min_width() - 1, 8);
        for app in APP_LIST {
            let (upper, lower) = app.bar_lines();
            assert!(
                !screen.contains(upper),
                "{upper:?} drawn below the floor"
            );
            assert!(
                lower.is_empty() || !screen.contains(lower),
                "{lower:?} drawn below the floor",
            );
        }
    }

    /// The shell's own floor, below which no page can be reached at all: too
    /// short for any page, or too narrow for the bar that would let the user
    /// leave the page they are on.
    #[test]
    fn a_terminal_under_the_shell_floor_says_so_instead_of_drawing_a_page() {
        assert!(screen(60, 20).contains("Terminal too small"));
        assert!(
            screen(topbar_min_width() - 1, 40).contains("Terminal too small")
        );
    }

    /// Each page decides for itself whether the window is wide enough, and
    /// [`MIN_WIDTH`] is the ceiling those answers must stay under. Looking for
    /// the refusal at exactly that width is what holds them to it.
    #[test]
    fn no_page_needs_more_width_than_the_ceiling() {
        for (index, app) in APP_LIST.iter().enumerate() {
            let mut shell = AppShell::new(Vec::new());
            shell.sidebar_index = index;
            let mut terminal = Terminal::new(TestBackend::new(MIN_WIDTH, 40))
                .expect("terminal");
            terminal.draw(|frame| shell.render(frame)).expect("draw");
            // At this size the shell's own floor is met, so the notice could
            // only have come from the page refusing the width.
            assert!(
                !format!("{}", terminal.backend())
                    .contains("Terminal too small"),
                "{} will not draw in {MIN_WIDTH} columns",
                app.bar_lines().0,
            );
        }
    }
}
