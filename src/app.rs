use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::aliases;
use crate::api::{CachedOffers, Commodity, fetch_offers_for};
use crate::ocean::Ocean;
use crate::chatlog::GameState;
use crate::clickmap::{self, ClickRegion, ClickTarget};
use crate::damage::DamageApp;
use crate::jobbers::{
    self, JobberFocus, JobberPane, JobbersUi, PirateCache, PiratePopup, TrophyPopup, VOYAGE_TYPES,
};
use crate::profits::ProfitsApp;
use crate::utils::{offset_title, text_similarity};

// ---------------------------------------------------------------------------
// App routing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum AppId {
    Profits,
    Damage,
    Chatlog,
    Voyage,
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
            AppId::Exit => ("Exit", ""),
        }
    }
}

pub const APP_LIST: &[AppId] =
    &[AppId::Profits, AppId::Damage, AppId::Chatlog, AppId::Voyage, AppId::Exit];

/// Index of the Exit app in `APP_LIST` (where the universal Esc lands).
fn exit_index() -> usize {
    APP_LIST.iter().position(|a| *a == AppId::Exit).unwrap()
}

/// Top bar: two label lines, no border (a shaded strip).
const TOPBAR_HEIGHT: u16 = 2;

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
    pub loading: bool,
    /// Whether the selected ocean has Market market data (profit calc works).
    pub market_supported: bool,
}

// ---------------------------------------------------------------------------
// Free functions operating on shared data
// ---------------------------------------------------------------------------

pub fn commod_name<'a>(commodities: &'a [Commodity], id: u64) -> &'a str {
    commodities
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.name.as_str())
        .unwrap_or("???")
}

pub fn suggest_island<'a>(query: &str, available_islands: &'a [String]) -> Option<&'a str> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }

    // Alias lookup
    if let Some(&target) = aliases::get_islands().get(query.as_str()) {
        if let Some(island) = available_islands
            .iter()
            .find(|i| i.eq_ignore_ascii_case(target))
        {
            return Some(island);
        }
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

/// Inert "coming soon" page for the Voyage Statistics app. Navigable (the top
/// bar can land on it) but draws nothing interactive yet.
fn render_voyage_placeholder(frame: &mut Frame, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(offset_title("Voyage Statistics").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "Voyage Statistics — coming soon.",
            Style::default().bold(),
        )),
        Line::from(Span::styled(
            "Per-voyage stats (Atlantis, Cursed Isles, Vampirates, …) will live here.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .centered();
    frame.render_widget(para, inner);
}

/// The Exit app: a single centered prompt. Pressing Enter or Esc while it is the
/// open app quits the program.
fn render_exit(frame: &mut Frame, area: Rect) {
    let bold = |s: &'static str| Span::styled(s, Style::default().bold());

    // Footer/credits pinned to the bottom, with one blank line below it.
    let footer = vec![
        Line::from(vec![bold("Rusty Quartermaster"), Span::raw(" by "), bold("F5XS")]),
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

    // Vertically center the prompt; let the footer sit at the bottom. The footer
    // block is generously sized so the long disclaimer line can wrap.
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
        Paragraph::new(footer).centered().wrap(Wrap { trim: true }),
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
            click_regions: Vec::new(),
        }
    }

    /// Whether profit calculation is available: Market querying is enabled
    /// *and* the selected ocean has Market data. If either is false we never
    /// hit Market.
    fn market_ok(&self) -> bool {
        self.query_market && self.ocean.is_some_and(Ocean::market_supported)
    }

    pub fn rebuild_island_list(&mut self) {
        let commod_names: Vec<String> = self
            .profits
            .rows
            .iter()
            .map(|r| commod_name(&self.commodities, r.commod_id).to_owned())
            .collect();
        self.available_islands = rebuild_island_list(&self.cached_offers, &commod_names);
    }

    // -- rendering --

    pub fn render(&mut self, frame: &mut Frame) {
        self.click_regions.clear();

        let area = frame.area();

        // Layout tree: [top bar / content]. The full-width top bar reclaims the
        // columns the old sidebar spent, so every page now gets all 80 columns.
        // Pages own everything in their content area — the Jobbers page, for
        // instance, draws its own tooltip inside its centered block rather than
        // as a full-width strip here.
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
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                    market_supported: self.market_ok(),
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
                render_voyage_placeholder(frame, content_area);
            }
            AppId::Exit => {
                render_exit(frame, content_area);
            }
        }
    }

    /// Draw the full-width two-line top bar: a continuous shaded strip split
    /// into four equal slots. Each slot's whole box is shaded along a three-step
    /// brightness ramp — the open/selected app is brightest; while the bar is
    /// focused the other slots sit a step up from the resting shade; once focus
    /// drops into an app those others fall back to the base shade.
    fn render_topbar(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.global_focus == GlobalFocus::TopBar;

        // Greyscale ramp: base (resting bar) → middle (bar focused) → strongest
        // (the open app). Backgrounds fill the entire slot box.
        let base = Style::default().bg(Color::DarkGray).fg(Color::White);
        let middle = Style::default().bg(Color::Gray).fg(Color::Black);
        let strongest = Style::default().bg(Color::White).fg(Color::Black).bold();

        // Four equal slots side by side, contiguous (no gaps) so the shade reads
        // as one bar.
        let slots = Layout::horizontal(
            APP_LIST.iter().map(|_| Constraint::Ratio(1, APP_LIST.len() as u32)),
        )
        .split(area);

        for (i, (&app_id, &slot)) in APP_LIST.iter().zip(slots.iter()).enumerate() {
            let style = if i == self.sidebar_index {
                strongest
            } else if focused {
                middle
            } else {
                base
            };

            let (upper, lower) = app_id.bar_lines();
            // Paragraph::style shades the whole slot box; centered text rides on
            // top of it.
            let para = Paragraph::new(vec![Line::from(upper), Line::from(lower)])
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
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
    ) -> bool {
        // The Exit app lives entirely on the top bar — it has no content to
        // descend into. Selecting it shows its widget; Enter/Esc then quit.
        let on_exit = APP_LIST[self.sidebar_index] == AppId::Exit;

        // Universal Esc: from anywhere it selects the Exit app on the bar (which
        // shows its widget); pressed again while Exit is already selected, it
        // quits. A modal popup keeps first claim on Esc so it stays closable.
        let popup_open =
            self.global_focus == GlobalFocus::Content && self.current_popup_open();
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
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                    market_supported: self.market_ok(),
                };
                self.profits.handle_key(key, &shared)
            }
            AppId::Damage => self.damage.handle_key(key),
            AppId::Chatlog => self.handle_jobbers_key(key),
            AppId::Voyage => Self::handle_voyage_key(key),
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
            AppId::Damage => self.damage.popup.is_some(),
            AppId::Chatlog => {
                self.jobbers_ui.ship_popup.is_some()
                    || self.jobbers_ui.vessel_popup.is_some()
                    || self.jobbers_ui.voyage_popup.is_some()
                    || self.jobbers_ui.pirate_popup.is_some()
                    || self.jobbers_ui.trophy_popup.is_some()
            }
            AppId::Voyage | AppId::Exit => false,
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
                self.sidebar_index =
                    if self.sidebar_index == 0 { last } else { self.sidebar_index - 1 };
            }
            KeyCode::Right => {
                self.sidebar_index =
                    if self.sidebar_index == last { 0 } else { self.sidebar_index + 1 };
            }
            // Enter drops into the selected app — except Exit, which has nothing
            // to enter, so Enter there quits.
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
        false
    }

    /// Drop focus from the bar into the selected app's content.
    fn enter_app(&mut self) {
        self.global_focus = GlobalFocus::Content;
        // Entering the Jobbers page lands on the Vessels button (the top widget).
        self.jobbers_ui.focus = JobberFocus::Vessels;
        // Entering Profits lands on the topmost widget — the inventory table
        // (or the search box when the inventory is empty).
        self.profits.focus_table_top();
    }

    /// The Voyage Statistics page is inert: ↑ or Esc returns focus to the bar,
    /// everything else is ignored.
    fn handle_voyage_key(key: KeyEvent) -> InputResult {
        match key.code {
            KeyCode::Up | KeyCode::Esc => InputResult::Exit,
            _ => InputResult::Consumed,
        }
    }

    // -- jobbers (chat log) handling --

    fn handle_jobbers_key(&mut self, key: KeyEvent) -> InputResult {
        use JobberFocus::*;

        // The popups are modal: each eats keys until dismissed. The trophies popup
        // is checked first since it layers over the pirate-stats popup.
        if self.jobbers_ui.trophy_popup.is_some() {
            return self.handle_trophy_popup_key(key);
        }
        if self.jobbers_ui.pirate_popup.is_some() {
            return self.handle_pirate_popup_key(key);
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
        // The voyage box's bottom row: Unpoison when poisoned, else Voyage Type.
        let box_bottom = if poisoned { Unpoison } else { VoyageType };

        match key.code {
            KeyCode::Esc => return InputResult::Exit,
            KeyCode::Up => match self.jobbers_ui.focus {
                // The Vessels button is the page's top widget; ↑ returns to the bar.
                Vessels => return InputResult::Exit,
                ShipType => self.jobbers_ui.focus = Vessels,
                VoyageType => self.jobbers_ui.focus = ShipType,
                Unpoison => self.jobbers_ui.focus = VoyageType,
                Aboard | Greedy | Planked => {
                    let pane = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    // At the top of a pane (or an empty one), ↑ leaves for the box.
                    if self.jobbers_pane_count(pane) == 0 || self.jobbers_pane_sel(pane) == 0 {
                        self.jobbers_ui.focus = box_bottom;
                    } else {
                        self.jobbers_pane_select_delta(pane, -1);
                    }
                }
            },
            KeyCode::Down => match self.jobbers_ui.focus {
                Vessels => self.jobbers_ui.focus = ShipType,
                ShipType => self.jobbers_ui.focus = VoyageType,
                VoyageType => {
                    self.jobbers_ui.focus = if poisoned {
                        Unpoison
                    } else {
                        first_pane.unwrap_or(VoyageType)
                    };
                }
                Unpoison => {
                    if let Some(pane) = first_pane {
                        self.jobbers_ui.focus = pane;
                    }
                }
                Aboard | Greedy | Planked => {
                    let pane = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    self.jobbers_pane_select_delta(pane, 1);
                }
            },
            KeyCode::Left => {
                if let Some(cur) = Self::focus_pane(self.jobbers_ui.focus) {
                    if let Some(i) = panes.iter().position(|p| *p == cur) {
                        if i > 0 {
                            self.jobbers_ui.focus = Self::pane_focus(panes[i - 1]);
                        }
                    }
                }
            }
            KeyCode::Right => {
                if let Some(cur) = Self::focus_pane(self.jobbers_ui.focus) {
                    if let Some(i) = panes.iter().position(|p| *p == cur) {
                        if i + 1 < panes.len() {
                            self.jobbers_ui.focus = Self::pane_focus(panes[i + 1]);
                        }
                    }
                }
            }
            KeyCode::Enter => match self.jobbers_ui.focus {
                Vessels => self.open_vessel_popup(),
                ShipType => self.open_ship_popup(),
                VoyageType => self.open_voyage_popup(),
                Unpoison => {
                    self.jobbers_unpoison();
                    self.jobbers_ui.focus = Vessels;
                }
                Aboard | Greedy | Planked => {
                    let pane = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    self.open_pirate_popup(pane);
                }
            },
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
    fn handle_ship_popup_key(&mut self, key: KeyEvent, sel: usize) -> InputResult {
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
    fn handle_vessel_popup_key(&mut self, key: KeyEvent, sel: usize) -> InputResult {
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
    fn handle_voyage_popup_key(&mut self, key: KeyEvent, sel: usize) -> InputResult {
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
            self.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: name.clone(),
                button: 0,
            });
        }
    }

    /// Modal key handling for the pirate-stats popup: ←/→ toggle the two buttons,
    /// Enter activates, Esc closes.
    fn handle_pirate_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(pp) = self.jobbers_ui.pirate_popup.as_mut() else {
            return InputResult::Consumed;
        };
        match key.code {
            KeyCode::Esc => self.jobbers_ui.pirate_popup = None,
            KeyCode::Left => pp.button = 0,
            KeyCode::Right => pp.button = 1,
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

    /// Open the trophies popup for the pirate in the stats popup.
    fn open_trophy_popup(&mut self) {
        if let Some(pp) = &self.jobbers_ui.pirate_popup {
            self.jobbers_ui.trophy_popup = Some(TrophyPopup {
                name: pp.name.clone(),
                search: String::new(),
                offset: 0,
                view_h: 0,
            });
        }
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
            _ => None,
        }
    }

    /// Map a [`JobberPane`] to the focus that lands on it.
    fn pane_focus(pane: JobberPane) -> JobberFocus {
        match pane {
            JobberPane::Aboard => JobberFocus::Aboard,
            JobberPane::Greedy => JobberFocus::Greedy,
            JobberPane::Planked => JobberFocus::Planked,
        }
    }

    /// Number of selectable pirates in a pane for the selected vessel.
    fn jobbers_pane_count(&self, pane: JobberPane) -> usize {
        let Some(key) = self.jobbers_ui.selected.as_ref() else {
            return 0;
        };
        match pane {
            JobberPane::Aboard => self.chatlog.aboard(key).len(),
            JobberPane::Greedy => self
                .chatlog
                .vessels
                .get(key)
                .map_or(0, |v| v.greedy_by_pirate.len()),
            JobberPane::Planked => self
                .chatlog
                .vessels
                .get(key)
                .map_or(0, |v| v.planked_by_us.len()),
        }
    }

    fn jobbers_pane_sel_mut(&mut self, pane: JobberPane) -> &mut usize {
        match pane {
            JobberPane::Aboard => &mut self.jobbers_ui.aboard_sel,
            JobberPane::Greedy => &mut self.jobbers_ui.greedy_sel,
            JobberPane::Planked => &mut self.jobbers_ui.planked_sel,
        }
    }

    /// The pane's current selection, clamped to its live pirate count.
    fn jobbers_pane_sel(&self, pane: JobberPane) -> usize {
        let n = self.jobbers_pane_count(pane);
        let raw = match pane {
            JobberPane::Aboard => self.jobbers_ui.aboard_sel,
            JobberPane::Greedy => self.jobbers_ui.greedy_sel,
            JobberPane::Planked => self.jobbers_ui.planked_sel,
        };
        if n == 0 {
            0
        } else {
            raw.min(n - 1)
        }
    }

    /// Move a pane's selection by `delta`, clamped to the pirate count.
    fn jobbers_pane_select_delta(&mut self, pane: JobberPane, delta: i32) {
        let n = self.jobbers_pane_count(pane);
        if n == 0 {
            return;
        }
        let next = (self.jobbers_pane_sel(pane) as i32 + delta).clamp(0, n as i32 - 1) as usize;
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
        if let Some(key) = self.jobbers_ui.selected.clone() {
            if let Some(v) = self.chatlog.vessels.get_mut(&key) {
                v.poisoned = false;
            }
        }
    }

    // -- mouse handling --

    pub fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
    ) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = clickmap::hit_test(&self.click_regions, mouse.column, mouse.row) {
                    self.handle_click(target, tx);
                }
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(-1, mouse.column, mouse.row);
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(1, mouse.column, mouse.row);
            }
            _ => {}
        }
    }

    fn handle_click(
        &mut self,
        target: ClickTarget,
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
    ) {
        match target {
            ClickTarget::SidebarItem(i) => {
                if i < APP_LIST.len() {
                    self.sidebar_index = i;
                    // Exit has no content to enter — clicking it just selects it
                    // on the bar and shows its widget (Enter/Esc then quit).
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
            ClickTarget::ProfitsTableCell { row, col } => {
                self.global_focus = GlobalFocus::Content;
                if col == 0 {
                    // Clicking the item name column: open delete confirm
                    if row < self.profits.rows.len() {
                        let name = commod_name(
                            &self.commodities,
                            self.profits.rows[row].commod_id,
                        )
                        .to_owned();
                        self.profits.popup = Some(crate::profits::PopupKind::DeleteConfirm {
                            row_idx: row,
                            // Default to Yes so a quick Enter confirms the delete.
                            name,
                            yes_focused: true,
                        });
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
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                    market_supported: self.market_ok(),
                };
                let result = self.profits.handle_button_activate(&shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupNo => {
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                    market_supported: self.market_ok(),
                };
                let result = self.profits.handle_popup_click(false, &shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupYes => {
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                    market_supported: self.market_ok(),
                };
                let result = self.profits.handle_popup_click(true, &shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupOk => {
                self.global_focus = GlobalFocus::Content;
                self.profits.dismiss_ok_popup();
            }
            ClickTarget::DamageCell { row, side } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
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
            ClickTarget::DamageIncrement { row, side } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.increment();
            }
            ClickTarget::DamageDecrement { row, side } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.decrement();
            }
            ClickTarget::DamageHeadon => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
                self.damage.focus_row = crate::damage::ROW_HEADON;
            }
            ClickTarget::DamageHeadonIncrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
                self.damage.focus_row = crate::damage::ROW_HEADON;
                self.damage.increment();
            }
            ClickTarget::DamageHeadonDecrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.button_focused = false;
                self.damage.focus_row = crate::damage::ROW_HEADON;
                self.damage.decrement();
            }
            ClickTarget::DamageButton(i) => {
                self.global_focus = GlobalFocus::Content;
                self.damage.button_focused = true;
                self.damage.button_index = i;
                self.damage.activate_button();
            }
            ClickTarget::DamageShipItem(i) => {
                if let Some(ref popup) = self.damage.popup {
                    let side = popup.side;
                    match side {
                        crate::damage::Side::Left => self.damage.left_ship = i,
                        crate::damage::Side::Right => self.damage.right_ship = i,
                    }
                    self.damage.popup = None;
                }
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
                if i < crate::ships::SHIPS.len() {
                    if let Some(vessel) = self.jobbers_ui.selected.clone() {
                        self.jobbers_ui.ship_types.insert(vessel, i);
                    }
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
            ClickTarget::JobberPirate { pane, idx } => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = match pane {
                    JobberPane::Aboard => JobberFocus::Aboard,
                    JobberPane::Greedy => JobberFocus::Greedy,
                    JobberPane::Planked => JobberFocus::Planked,
                };
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
            // The trophies popup is keyboard-driven; a click on it is a no-op (it
            // exists only so the scroll wheel has a target there).
            ClickTarget::JobberTrophyArea => {}
        }
    }

    fn handle_scroll(&mut self, delta: i32, col: u16, row: u16) {
        match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                if self.profits.focus == crate::profits::Focus::Table {
                    if delta < 0 {
                        self.profits.table_up();
                    } else {
                        if let Some(row) = self.profits.table_state.selected() {
                            if row + 1 < self.profits.rows.len() {
                                self.profits.table_state.select(Some(row + 1));
                            }
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
                    self.jobbers_ui.ship_popup = Some(if delta < 0 {
                        sel.saturating_sub(1)
                    } else {
                        (sel + 1).min(count.saturating_sub(1))
                    });
                    return;
                }
                if let Some(sel) = self.jobbers_ui.vessel_popup {
                    let count = self.chatlog.vessels_by_recency().len();
                    self.jobbers_ui.vessel_popup = Some(if delta < 0 {
                        sel.saturating_sub(1)
                    } else {
                        (sel + 1).min(count.saturating_sub(1))
                    });
                    return;
                }
                if let Some(sel) = self.jobbers_ui.voyage_popup {
                    let count = VOYAGE_TYPES.len();
                    self.jobbers_ui.voyage_popup = Some(if delta < 0 {
                        sel.saturating_sub(1)
                    } else {
                        (sel + 1).min(count.saturating_sub(1))
                    });
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
                // The pirate-stats popup has nothing to scroll.
                if self.jobbers_ui.pirate_popup.is_some() {
                    return;
                }
                // Otherwise move the selection of whichever pane the cursor is over
                // (the panes auto-scroll to follow the selection).
                let pane = match clickmap::hit_test(&self.click_regions, col, row) {
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
                    _ => return,
                };
                self.jobbers_pane_select_delta(pane, delta.signum());
            }
            AppId::Voyage | AppId::Exit => {}
        }
    }

    fn process_input_result(
        &mut self,
        result: InputResult,
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
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
            Ok(offers_map) => match self.profits.fetch_purpose {
                FetchPurpose::Islands => {
                    self.cached_offers.extend(offers_map);
                    self.rebuild_island_list();
                }
                FetchPurpose::Profits => {
                    self.cached_offers = offers_map;
                    self.rebuild_island_list();
                    let shared = SharedState {
                        commodities: &self.commodities,
                        cached_offers: &self.cached_offers,
                        available_islands: &self.available_islands,
                        loading: self.loading,
                        market_supported: self.market_ok(),
                    };
                    self.profits.calculate_or_warn(&shared);
                }
            },
            Err(msg) => {
                self.profits.calc_error = Some(msg);
            }
        }
    }

    fn spawn_fetch(
        &self,
        purpose: FetchPurpose,
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
    ) {
        let names: Vec<String> = match purpose {
            FetchPurpose::Islands => self
                .profits
                .rows
                .iter()
                .map(|r| commod_name(&self.commodities, r.commod_id).to_owned())
                .collect(),
            FetchPurpose::Profits => self
                .profits
                .rows
                .iter()
                .filter(|r| {
                    let restock = r.restock.parse::<u64>().unwrap_or(0);
                    let stock = r.stock.parse::<u64>().unwrap_or(0);
                    let booty = r.booty.parse::<u64>().unwrap_or(0);
                    restock != 0 || stock != 0 || booty != 0
                })
                .map(|r| commod_name(&self.commodities, r.commod_id).to_owned())
                .collect(),
        };

        let tx = tx.clone();
        let ocean = self.ocean.filter(|_| self.query_market);
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = match ocean.filter(|o| o.market_supported()) {
                Some(o) => fetch_offers_for(&client, &names, o).await,
                None => Err("No Market ocean selected for this run".to_owned()),
            };
            let _ = tx.send(result);
        });
    }
}
