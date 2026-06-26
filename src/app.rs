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
    self, JobberFocus, JobberPane, JobbersUi, PirateCache, PiratePopup, SkillDistPopup, TrophyPopup,
    VoyageType, VOYAGE_TYPES,
};
use crate::profits::ProfitsApp;
use crate::utils::text_similarity;

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

/// Whether `target` is one of the Damage-calculator click variants (so the Sea
/// Battles popup can claim them for its embedded editor).
fn is_damage_target(target: &ClickTarget) -> bool {
    matches!(
        target,
        ClickTarget::DamageCell { .. }
            | ClickTarget::DamageIncrement { .. }
            | ClickTarget::DamageDecrement { .. }
            | ClickTarget::DamageHeadon
            | ClickTarget::DamageHeadonIncrement
            | ClickTarget::DamageHeadonDecrement
            | ClickTarget::DamageShipItem(_)
            | ClickTarget::DamageResetYes
            | ClickTarget::DamageResetNo
    )
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
    /// Gross PoE plundered, PoE stolen from us, and the retained booty chest
    /// (per-fight halves) over the current pillage, from the battle ledger. Drive
    /// the auto-deduced booty-chest figure on the Profits page. See
    /// [`crate::chatlog::GameState::current_pillage_poe`].
    pub pillage_gross: u64,
    pub pillage_stolen: u64,
    pub pillage_chest: u64,
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
    pub voyage_ui: crate::voyage::ui::VoyageStatsUi,
    /// Persisted voyage history (loaded from / written to `voyages_path`).
    pub voyage_history: crate::voyage::persistence::SavedVoyages,
    /// Where voyage history lives on disk (set by `--voyages`).
    pub voyages_path: Option<std::path::PathBuf>,
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
            voyage_history: crate::voyage::persistence::SavedVoyages::default(),
            voyages_path: None,
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
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
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
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
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
                    || self.jobbers_ui.skill_dist_popup.is_some()
            }
            AppId::Voyage => {
                self.voyage_ui.prompt.is_some()
                    || self.voyage_ui.chart_popup.is_some()
                    || self.voyage_ui.battles_popup.is_some()
            }
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
            }
            return InputResult::Consumed;
        }

        if let Some(choice) = self.voyage_ui.prompt {
            match key.code {
                KeyCode::Esc => self.voyage_ui.prompt = None,
                KeyCode::Left | KeyCode::Right => {
                    self.voyage_ui.prompt = Some(match choice {
                        SaveChoice::Save => SaveChoice::Discard,
                        SaveChoice::Discard => SaveChoice::Save,
                    });
                }
                KeyCode::Enter => {
                    match choice {
                        SaveChoice::Save => self.save_displayed_voyage(),
                        SaveChoice::Discard => self.discard_displayed_voyage(),
                    }
                    self.voyage_ui.prompt = None;
                }
                KeyCode::Char('s' | 'S') => {
                    self.save_displayed_voyage();
                    self.voyage_ui.prompt = None;
                }
                KeyCode::Char('d' | 'D') => {
                    self.discard_displayed_voyage();
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
            KeyCode::Char('d' | 'D') => {
                if self.build_voyage_view().saveable {
                    self.voyage_ui.prompt = Some(SaveChoice::Discard);
                }
                InputResult::Consumed
            }
            // ↑/↓ move the focused stat (the body auto-scrolls to follow it);
            // ↑ off the first stat returns focus to the top bar.
            KeyCode::Up => {
                if self.voyage_ui.focus == 0 {
                    InputResult::Exit
                } else {
                    self.voyage_ui.focus -= 1;
                    InputResult::Consumed
                }
            }
            KeyCode::Down => {
                self.voyage_ui.focus = self.voyage_ui.focus.saturating_add(1);
                InputResult::Consumed
            }
            // Enter opens the focused item: the Sea Battles section (focus 0) opens
            // the per-fight log; a focused chart enlarges (charts follow the stats).
            KeyCode::Enter => {
                if self.voyage_ui.focus == 0 {
                    self.open_battles_popup();
                } else if self.voyage_ui.focus >= self.voyage_ui.n_stats {
                    let idx = self.voyage_ui.focus - self.voyage_ui.n_stats;
                    if crate::voyage::ui::CHART_ENLARGEABLE.get(idx) == Some(&true) {
                        self.voyage_ui.chart_popup = Some(idx);
                    }
                }
                InputResult::Consumed
            }
            KeyCode::PageUp => {
                self.voyage_ui.focus = self.voyage_ui.focus.saturating_sub(5);
                InputResult::Consumed
            }
            KeyCode::PageDown => {
                self.voyage_ui.focus = self.voyage_ui.focus.saturating_add(5);
                InputResult::Consumed
            }
            _ => InputResult::Consumed,
        }
    }

    /// Build the computed view for the Voyage Statistics page: resolve which
    /// vessel/voyage to show, the chosen ship's cannon size, and the aggregated
    /// battle + consumption stats. Shows the current vessel's live run, or its
    /// most recent completed run.
    fn build_voyage_view(&self) -> crate::voyage::ui::VoyageView {
        use crate::voyage::ui::VoyageView;

        // Vessel: the jobbers selection if still live, else the latest boarded.
        let key = self
            .jobbers_ui
            .selected
            .clone()
            .filter(|k| self.chatlog.vessels.contains_key(k))
            .or_else(|| self.chatlog.vessels_by_recency().into_iter().next());
        let vessel = key.as_ref().and_then(|k| self.chatlog.vessels.get(k));
        let voyage = vessel.and_then(|v| v.current_voyage.as_ref().or_else(|| v.voyages.last()));

        // Chosen ship -> cannon size + display label.
        let cannon_size = key
            .as_ref()
            .and_then(|k| self.jobbers_ui.ship_types.get(k).copied())
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.cannon_size);
        let cannon_label = cannon_size.map(|sz| {
            match sz {
                crate::ships::CannonSize::Small => "Small",
                crate::ships::CannonSize::Medium => "Medium",
                crate::ships::CannonSize::Large => "Large",
            }
            .to_string()
        });
        let vessel_name = key.as_ref().map(|k| k.to_string());
        // Ship type label from the vessel's chosen ship (the jobbers picker).
        let ship_type = key
            .as_ref()
            .and_then(|k| self.jobbers_ui.ship_types.get(k).copied())
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.name.to_string());

        let Some(voyage) = voyage else {
            return VoyageView {
                has_voyage: false,
                vessel: vessel_name,
                ship_type,
                period: None,
                elapsed_secs: None,
                cannon_label,
                saveable: false,
                battle: Default::default(),
                consumption: Default::default(),
                charts: Default::default(),
                battles: Vec::new(),
            };
        };

        let ported = voyage.ported_at.is_some();
        // End of the run: the port time once ported, else the live log clock.
        let end_at = voyage.ported_at.or_else(|| self.chatlog.now());
        // Final duration if ported, else live elapsed against the log clock.
        let elapsed_secs = match (voyage.sailed_at, end_at) {
            (Some(start), Some(end)) => Some((end - start).num_seconds()),
            _ => None,
        };
        // Clock span "HH:MM to HH:MM" (end is the current time while still out).
        let period = match (voyage.sailed_at, end_at) {
            (Some(start), Some(end)) => Some(format!(
                "{} to {}",
                start.format("%H:%M"),
                end.format("%H:%M")
            )),
            _ => None,
        };

        // Identity confirmation gates win/loss: until our configured name is seen
        // in the log, every win/loss shows as Unknown (and flips retroactively).
        let confirmed = self.chatlog.self_confirmed;
        let eff = |raw| crate::voyage::effective_outcome(raw, confirmed);

        let consumption = crate::voyage::stats::consumption_stats(
            voyage,
            &self.profits.rows,
            &self.commodities,
            cannon_size,
        );
        let battle = crate::voyage::stats::battle_stats(voyage, confirmed);

        // Per-fight rows for the Sea Battles popup: resolved fights first, then the
        // in-progress one (so it can be inspected mid-fight). Mirrors the indexing
        // in `GameState::displayed_battle_mut`.
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
                    // their own label now.
                    category: crate::voyage::stats::category_label(&b.category),
                    // A masked (unknown) verdict carries no signed PoE.
                    poe: matches!(
                        outcome,
                        crate::voyage::BattleOutcome::Won | crate::voyage::BattleOutcome::Lost
                    )
                    .then_some(b.poe)
                    .flatten(),
                    goods: b.goods,
                    my_cut: b.my_cut,
                    total_secs: b.total_secs(),
                    sea_secs: b.sea_secs(),
                    boarding_secs: b.boarding_secs(),
                    pirates: b.pirates,
                    swabbies: b.swabbies,
                    snapshot: b.snapshot,
                    recorded: b.recorded,
                    their_manpower: b.their_manpower(),
                    foe_ship: b.foe_ship,
                }
            })
            .collect();

        // Chart series: current voyage vs persisted history (won-fight PoE +
        // per-voyage totals). Total value is net PoE for now; goods fold in later.
        let charts = {
            use crate::voyage::BattleOutcome::{Lost, Won};
            // Signed PoE of a fight, but only for a *confirmed* win/loss (an
            // unconfirmed/unknown verdict contributes nothing to the charts).
            let decisive_poe = |b: &crate::voyage::Battle| {
                matches!(eff(b.outcome), Won | Lost)
                    .then_some(b.poe)
                    .flatten()
            };
            let cur_won_poe: Vec<f64> = voyage
                .battles
                .iter()
                .filter(|b| eff(b.outcome) == Won)
                .filter_map(&decisive_poe)
                .filter(|p| *p > 0)
                .map(|p| p as f64)
                .collect();
            // Signed PoE of each concluded (won or lost) fight, chronological —
            // losses are negative. Drives the per-fight bar chart.
            let cur_fight_poe: Vec<f64> = voyage
                .battles
                .iter()
                .filter_map(&decisive_poe)
                .map(|p| p as f64)
                .collect();
            let last_win = voyage
                .battles
                .iter()
                .rev()
                .find(|b| eff(b.outcome) == Won)
                .and_then(&decisive_poe)
                .map(|p| p as f64);
            let cur_total = voyage
                .battles
                .iter()
                .filter_map(&decisive_poe)
                .sum::<i64>() as f64;

            let mut hist_won_poe = Vec::new();
            let mut hist_totals = Vec::new();
            for v in &self.voyage_history.voyages {
                let mut total = 0i64;
                for bt in &v.battles {
                    if let Some(p) = bt.poe {
                        total += p;
                        if p > 0 && bt.outcome == "won" {
                            hist_won_poe.push(p as f64);
                        }
                    }
                }
                hist_totals.push(total as f64);
            }
            crate::voyage::ui::ChartData {
                cur_won_poe,
                cur_fight_poe,
                hist_won_poe,
                last_win,
                cur_total,
                hist_totals,
            }
        };

        VoyageView {
            has_voyage: true,
            vessel: vessel_name,
            ship_type,
            period,
            elapsed_secs,
            cannon_label,
            saveable: ported && !voyage.saved,
            battle,
            consumption,
            charts,
            battles,
        }
    }

    /// Feed one live chat-log line. When a fight resolves and the Damage calculator
    /// has hits entered, freeze its full state + advantage onto that fight (Left =
    /// our ship, Right = the foe) and clear the counts for the next fight — so a
    /// fight we tracked live is recorded in the Sea Battles history automatically.
    /// (The popup still lets the user amend a fight or hand-add one we missed.)
    pub fn feed_chat_line(&mut self, line: &str) {
        self.chatlog.process_line(line);
        // A special encounter (e.g. the Black Ship) just told us the foe's hull —
        // point the live Damage calculator at it so live tracking and the captured
        // snapshot use the right ship. The user can still override it by hand.
        if let Some(idx) = self.chatlog.take_detected_foe_ship() {
            self.damage.right_ship = idx;
        }
        // Capture both transition flags before the freeze step consumes them, so
        // the auto-navigation below can fire regardless of the calculator state.
        let battle_started = self.chatlog.take_battle_started();
        let battle_resolved = self.chatlog.take_resolved();
        if battle_resolved && self.damage.has_input() {
            // Our manpower = the crew that actually fought, as recorded on the
            // just-resolved battle (grapple roster minus the disconnected). Falls
            // back to the live count if that battle didn't record one.
            let crew_n = self.chatlog.last_resolved_our_strength().unwrap_or_else(|| {
                self.chatlog.current_pirates() + self.chatlog.current_swabbies()
            });
            // Their manpower came from the melee at resolution; fall back to the
            // foe ship type's pirate capacity if the fight had no melee count.
            let their = self.chatlog.last_resolved_their_manpower().unwrap_or_else(|| {
                crate::ships::SHIPS[self.damage.right_ship].max_pirates as u32
            });
            let snap = self.damage.snapshot(crew_n);
            let dmg = self.damage.advantage_dmg();
            let crew = self.damage.crew_advantage(crew_n, their);
            self.chatlog.record_resolved_battle(snap, dmg, crew);
            self.damage.clear_counts();
        }
        // Auto-navigation. A fight beginning surfaces the live Damage calculator
        // (so it's tracked from the first hit); a fight concluding surfaces its
        // entry in the Sea Battles log. A start wins if both somehow fire.
        if battle_started {
            self.jump_to_live_damage();
        } else if battle_resolved {
            self.jump_to_concluded_fight();
        }
        // Entering a vampire lair: surface the Jobbers page in its Vampirates
        // layout so the wave model and skill-distribution tooling are at hand.
        if self.chatlog.take_lair_entered() {
            self.jump_to_vampirate_jobbers();
        }
        // Boarding a vessel snaps the Jobbers/Voyage vessel selector to it, so the
        // pages follow us onto the ship we just stepped onto rather than sticking
        // to whatever was previously picked.
        if let Some(key) = self.chatlog.take_boarded_vessel() {
            self.jobbers_ui.selected = Some(key);
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
        let n = self.build_voyage_view().battles.len();
        let Some(last) = n.checked_sub(1) else {
            return;
        };
        self.switch_to(AppId::Voyage);
        self.voyage_ui.battles_popup = Some(last);
        self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Pager;
        self.load_battle_editor(last);
    }

    /// We just entered a vampire lair: surface the Jobbers page and switch it to
    /// the Vampirates voyage layout (wave model + skill-distribution tooling).
    fn jump_to_vampirate_jobbers(&mut self) {
        self.jobbers_ui.voyage_type = VoyageType::Vampirates;
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

    /// Open the save/discard prompt if the displayed run is finished and unsaved.
    fn open_voyage_save_prompt(&mut self) {
        if self.build_voyage_view().saveable {
            self.voyage_ui.prompt = Some(crate::voyage::ui::SaveChoice::Save);
        }
    }

    /// Persist the displayed (finished) voyage to history + disk, and mark it
    /// saved so it isn't offered again.
    fn save_displayed_voyage(&mut self) {
        let Some(key) = self.displayed_vessel_key() else {
            return;
        };
        let vessel_name = key.to_string();
        // The vessel's chosen ship type (hull) from the jobbers picker, persisted
        // so history can be grouped by ship type. Resolved before the mutable
        // borrow of `chatlog` below.
        let ship_type = self
            .jobbers_ui
            .ship_types
            .get(&key)
            .copied()
            .and_then(|i| crate::ships::SHIPS.get(i))
            .map(|s| s.name.to_string());
        let confirmed = self.chatlog.self_confirmed;
        let saved = {
            let Some(v) = self.chatlog.vessels.get_mut(&key) else {
                return;
            };
            // The saveable run is always the latest completed (ported) one.
            let Some(voyage) = v.voyages.last_mut().filter(|vy| vy.ported_at.is_some()) else {
                return;
            };
            if voyage.saved {
                return;
            }
            let saved = crate::voyage::persistence::from_voyage(
                voyage,
                Some(&vessel_name),
                ship_type.as_deref(),
                confirmed,
            );
            voyage.saved = true;
            saved
        };
        self.voyage_history.voyages.push(saved);
        if let Some(path) = &self.voyages_path {
            crate::voyage::persistence::save(path, &self.voyage_history);
        }
    }

    /// Dismiss the displayed (finished) voyage without persisting it.
    fn discard_displayed_voyage(&mut self) {
        let Some(key) = self.displayed_vessel_key() else {
            return;
        };
        if let Some(v) = self.chatlog.vessels.get_mut(&key) {
            if let Some(voyage) = v.voyages.last_mut().filter(|vy| vy.ported_at.is_some()) {
                voyage.saved = true;
            }
        }
    }

    // -- Sea Battles popup (per-fight log) --

    /// Open the Sea Battles popup on the first fight, loading its editor. No-op if
    /// the displayed voyage has no fights.
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
        // "Our strength" = the full crew aboard our ship: real pirates PLUS swabbies
        // / named mercenaries (all fight in the melee). Use the fight's resolution
        // roster; for an as-yet-unresolved fight fall back to the live crew.
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
                // No captured snapshot yet — start blank, but if the encounter told
                // us the foe's hull (Black Ship, Monkey Boat) seed that as the foe
                // ship so the calculator and the displayed type are right.
                let mut app = crate::damage::DamageApp::new();
                if let Some(idx) = row.foe_ship {
                    app.right_ship = idx;
                }
                app
            }
        };
        // #16: when the foe's hull is *unknown* (no special encounter announced it)
        // and the observed headcount can't fit the currently-seeded hull, bump the
        // foe ship to the smallest hull that can man that crew — a sloop can't hold
        // 18 boarders. Special encounters that announce their hull (Monkey Boats,
        // and the Black Ship, which can be staffed beyond any hull's capacity) carry
        // `foe_ship = Some(..)` and are left untouched.
        if row.foe_ship.is_none() {
            if let Some(their) = row.their_manpower {
                let cur = crate::ships::SHIPS[self.voyage_ui.battle_editor.right_ship].max_pirates;
                if (cur as u32) < their {
                    if let Some(idx) = crate::ships::smallest_ship_for(their) {
                        self.voyage_ui.battle_editor.right_ship = idx;
                    }
                }
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

    /// Write the editor's current state back onto the open fight, recomputing its
    /// advantage. Always runs on an edit — the calculator is always live; recording
    /// only governs persistence, not the in-RAM snapshot.
    fn sync_battle_editor(&mut self) {
        let Some(page) = self.voyage_ui.battles_popup else {
            return;
        };
        let Some(key) = self.displayed_vessel_key() else {
            return;
        };
        let ours = self.voyage_ui.editor_crew;
        let their = self.voyage_ui.editor_their.unwrap_or_else(|| {
            crate::ships::SHIPS[self.voyage_ui.battle_editor.right_ship].max_pirates as u32
        });
        let snap = self.voyage_ui.battle_editor.snapshot(ours);
        let dmg = self.voyage_ui.battle_editor.advantage_dmg();
        let crew = self.voyage_ui.battle_editor.crew_advantage(ours, their);
        self.chatlog.set_battle_snapshot(&key, page, snap, dmg, crew);
    }

    /// Toggle whether the open fight is recorded (persisted to disk). Purely a
    /// flag — the snapshot/calculator are untouched.
    fn toggle_battle_record(&mut self) {
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

    /// Key handling while the Sea Battles popup is open. Three focus zones chained
    /// top→bottom — the pager (←/→ change fight, wrapping), the record toggle
    /// (Enter/Space flips it), and the always-editable calculator (arrows drive
    /// it) — moved between with ↑/↓. Esc closes the editor's ship picker / reset
    /// confirm first, otherwise the popup.
    fn handle_battles_key(&mut self, key: KeyEvent) -> InputResult {
        use crate::damage::{ROW_HEADON, ROW_SHIP, Side};
        use crate::voyage::ui::BattlesFocus::{Calc, Pager, Record};

        // While the editor has its own modal (ship picker or reset confirm), every
        // key — including Esc, which dismisses that modal keeping the ship — drives
        // the calculator rather than the popup.
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
            Pager => match key.code {
                KeyCode::Left | KeyCode::PageUp => self.battles_page(-1),
                KeyCode::Right | KeyCode::PageDown => self.battles_page(1),
                KeyCode::Down => self.voyage_ui.battles_focus = Record,
                _ => {}
            },
            // The toggle sits between the pager and the calculator, matching its
            // on-screen position (directly under the page number, above the calc).
            Record => match key.code {
                KeyCode::Up => self.voyage_ui.battles_focus = Pager,
                KeyCode::Down => {
                    self.voyage_ui.battles_focus = Calc;
                    self.voyage_ui.battle_editor.focus_row = ROW_SHIP;
                    self.voyage_ui.battle_editor.focus_side = Side::Left;
                }
                KeyCode::Enter | KeyCode::Char(' ') => self.toggle_battle_record(),
                _ => {}
            },
            Calc => {
                // ↑ off the top row returns to the toggle; ↓ off the bottom row has
                // nowhere to go (the calculator is the last control).
                if key.code == KeyCode::Up && self.voyage_ui.battle_editor.focus_row == ROW_SHIP {
                    self.voyage_ui.battles_focus = Record;
                } else if key.code == KeyCode::Down
                    && self.voyage_ui.battle_editor.focus_row == ROW_HEADON
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

        // The popups are modal: each eats keys until dismissed. The trophies popup
        // is checked first since it layers over the pirate-stats popup.
        if self.jobbers_ui.trophy_popup.is_some() {
            return self.handle_trophy_popup_key(key);
        }
        if self.jobbers_ui.pirate_popup.is_some() {
            return self.handle_pirate_popup_key(key);
        }
        if self.jobbers_ui.skill_dist_popup.is_some() {
            return self.handle_skill_dist_popup_key(key);
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
        let panes_beside = self.jobbers_ui.voyage_type.panes_beside_top_jobbers();
        // The voyage box's bottom row: Unpoison when poisoned, else Voyage Type.
        let box_bottom = if poisoned { Unpoison } else { VoyageType };
        // The Skill Distribution button (Vampirates) sits between the leaderboard and
        // the panes, so it's the row just below the leaderboard and above the panes.
        let has_button = self.jobbers_ui.voyage_type.has_skill_distribution();
        let after_box = if has_button { Some(SkillDist) } else { first_pane };
        // Descending out of the voyage box lands on the Skill Leaderboard first
        // (when the layout is implemented), then the button / panes below it.
        let into_content = if implemented { Some(Leaderboard) } else { after_box };

        match key.code {
            KeyCode::Esc => return InputResult::Exit,
            KeyCode::Up => match self.jobbers_ui.focus {
                // The Vessels button is the page's top widget; ↑ returns to the bar.
                Vessels => return InputResult::Exit,
                ShipType => self.jobbers_ui.focus = Vessels,
                VoyageType => self.jobbers_ui.focus = ShipType,
                Unpoison => self.jobbers_ui.focus = VoyageType,
                // Within the leaderboard ↑ walks up the column; at the top it leaves
                // for the box's bottom row.
                Leaderboard => {
                    if self.leaderboard_current_len() == 0 || self.jobbers_ui.top_sel == 0 {
                        self.jobbers_ui.focus = box_bottom;
                    } else {
                        self.jobbers_ui.top_sel -= 1;
                    }
                }
                // The button sits below the leaderboard; ↑ returns to it.
                SkillDist => self.jobbers_ui.focus = Leaderboard,
                Aboard | Greedy | Planked => {
                    let pane = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    // At the top of a pane (or an empty one), ↑ leaves for whatever's
                    // above the panes: the button if shown, else (side-by-side) the
                    // box's bottom row, else the leaderboard stacked above.
                    if self.jobbers_pane_count(pane) == 0 || self.jobbers_pane_sel(pane) == 0 {
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
            },
            KeyCode::Down => match self.jobbers_ui.focus {
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
                // Within the leaderboard ↓ walks down the column; at the bottom it
                // leaves for the button / panes below.
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
                Aboard | Greedy | Planked => {
                    let pane = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    self.jobbers_pane_select_delta(pane, 1);
                }
            },
            KeyCode::Left => match self.jobbers_ui.focus {
                // ← walks to the previous leaderboard column.
                Leaderboard => {
                    if self.jobbers_ui.top_col > 0 {
                        self.jobbers_ui.top_col -= 1;
                        self.leaderboard_clamp();
                    }
                }
                Aboard | Greedy | Planked => {
                    let cur = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    if let Some(i) = panes.iter().position(|p| *p == cur) {
                        if i > 0 {
                            self.jobbers_ui.focus = Self::pane_focus(panes[i - 1]);
                        } else if panes_beside {
                            // The leftmost pane sits to the right of the leaderboard.
                            self.jobbers_ui.focus = Leaderboard;
                        }
                    }
                }
                _ => {}
            },
            KeyCode::Right => match self.jobbers_ui.focus {
                Leaderboard => {
                    if panes_beside {
                        // The leaderboard sits to the left of the pane(s).
                        if let Some(pane) = first_pane {
                            self.jobbers_ui.focus = pane;
                        }
                    } else if self.jobbers_ui.top_col + 1 < self.leaderboard_ncols() {
                        self.jobbers_ui.top_col += 1;
                        self.leaderboard_clamp();
                    }
                }
                Aboard | Greedy | Planked => {
                    let cur = Self::focus_pane(self.jobbers_ui.focus).unwrap();
                    if let Some(i) = panes.iter().position(|p| *p == cur) {
                        if i + 1 < panes.len() {
                            self.jobbers_ui.focus = Self::pane_focus(panes[i + 1]);
                        }
                    }
                }
                _ => {}
            },
            KeyCode::Enter => match self.jobbers_ui.focus {
                Vessels => self.open_vessel_popup(),
                ShipType => self.open_ship_popup(),
                VoyageType => self.open_voyage_popup(),
                Unpoison => {
                    self.jobbers_unpoison();
                    self.jobbers_ui.focus = Vessels;
                }
                Leaderboard => self.open_leaderboard_popup(),
                SkillDist => self.open_skill_dist_popup(),
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
            // On-demand: jump this pirate to the top of the fetch queue.
            self.pirate_cache.force_requery(name);
            self.jobbers_ui.pirate_popup = Some(PiratePopup {
                name: name.clone(),
                button: 0,
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

    /// Clamp the leaderboard cursor to the live column/row shape (after a column
    /// switch, or when the aboard set changes under it).
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

    /// Open the pirate-stats popup for the leaderboard's selected pirate, jumping it
    /// to the top of the fetch queue (mirrors [`Self::open_pirate_popup`]).
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
        });
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

    /// Open the Vampirates skill-distribution popup, parking the cursor on the
    /// most-populated cell so the detail panel starts non-empty.
    fn open_skill_dist_popup(&mut self) {
        let cursor = self
            .jobbers_ui
            .selected
            .as_ref()
            .map(|k| jobbers::default_skill_dist_cursor(&self.chatlog.aboard(k), &self.pirate_cache))
            .unwrap_or((0, 0));
        self.jobbers_ui.skill_dist_popup = Some(SkillDistPopup { cursor });
        self.jobbers_ui.focus = JobberFocus::SkillDist;
    }

    /// Modal key handling for the skill-distribution popup: arrows move the cursor
    /// over the 9×9 standing grid (x = Treasure Haul, y = Carpentry), Esc closes.
    fn handle_skill_dist_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut() else {
            return InputResult::Consumed;
        };
        let (th, carp) = sd.cursor;
        match key.code {
            KeyCode::Esc => self.jobbers_ui.skill_dist_popup = None,
            KeyCode::Left => sd.cursor.0 = th.saturating_sub(1),
            KeyCode::Right => sd.cursor.0 = (th + 1).min(8),
            // ↑ raises Carpentry standing, ↓ lowers it (the grid runs high → low).
            KeyCode::Up => sd.cursor.1 = (carp + 1).min(8),
            KeyCode::Down => sd.cursor.1 = carp.saturating_sub(1),
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the trophies popup for the pirate in the stats popup.
    fn open_trophy_popup(&mut self) {
        let Some(name) = self.jobbers_ui.pirate_popup.as_ref().map(|pp| pp.name.clone()) else {
            return;
        };
        // On-demand: ensure this pirate's trophies are (re)fetched at top priority.
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
            // Live hover: while the skill-distribution popup is open, moving the
            // mouse over a cell parks the cursor there (updates the detail panel).
            MouseEventKind::Moved => {
                if self.jobbers_ui.skill_dist_popup.is_some() {
                    if let Some(ClickTarget::JobberSkillDistCell { th, carp }) =
                        clickmap::hit_test(&self.click_regions, mouse.column, mouse.row)
                    {
                        if let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut() {
                            sd.cursor = (th, carp);
                        }
                    }
                }
                // Live hover over a Profit Breakdown row parks the tooltip cursor.
                if matches!(
                    self.profits.popup,
                    Some(crate::profits::PopupKind::ProfitResult(_))
                ) {
                    if let Some(ClickTarget::ProfitsBreakdownRow(i)) =
                        clickmap::hit_test(&self.click_regions, mouse.column, mouse.row)
                    {
                        self.profits.breakdown_cursor = i;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_click(
        &mut self,
        target: ClickTarget,
        tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
    ) {
        // While the Sea Battles popup is open, Damage* clicks belong to its
        // embedded editor (edits go to the recorded fight; clicks on a read-only
        // widget are swallowed) and must never reach the live calculator behind it.
        if self.voyage_ui.battles_popup.is_some() && is_damage_target(&target) {
            self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Calc;
            crate::damage::apply_click(&mut self.voyage_ui.battle_editor, &target);
            self.sync_battle_editor();
            return;
        }

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
                let (pillage_gross, pillage_stolen, pillage_chest) =
                    self.chatlog.current_pillage_poe();
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
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
            ClickTarget::DamageCell { row, side } => {
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
            ClickTarget::DamageIncrement { row, side } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.increment();
            }
            ClickTarget::DamageDecrement { row, side } => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = row;
                self.damage.focus_side = side;
                self.damage.decrement();
            }
            ClickTarget::DamageHeadon => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_HEADON;
            }
            ClickTarget::DamageHeadonIncrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_HEADON;
                self.damage.increment();
            }
            ClickTarget::DamageHeadonDecrement => {
                self.global_focus = GlobalFocus::Content;
                self.damage.popup = None;
                self.damage.focus_row = crate::damage::ROW_HEADON;
                self.damage.decrement();
            }
            ClickTarget::DamageShipItem(i) => {
                if let Some(ref popup) = self.damage.popup {
                    let side = popup.side;
                    match side {
                        crate::damage::Side::Left => self.damage.left_ship = i,
                        crate::damage::Side::Right => self.damage.right_ship = i,
                    }
                    self.damage.popup = None;
                    self.damage.reset_prompt = Some(true);
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
            ClickTarget::JobberLeaderboard => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Leaderboard;
            }
            ClickTarget::JobberLeaderboardPirate { col, row } => {
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
            ClickTarget::JobberSkillDistButton => self.open_skill_dist_popup(),
            // Clicking a cell parks the cursor there (same as hovering it).
            ClickTarget::JobberSkillDistCell { th, carp } => {
                if let Some(sd) = self.jobbers_ui.skill_dist_popup.as_mut() {
                    sd.cursor = (th, carp);
                }
            }
            // A click on the backdrop (outside the cells) dismisses the popup.
            ClickTarget::JobberSkillDistClose => {
                self.jobbers_ui.skill_dist_popup = None;
            }
            ClickTarget::VoyageSaveOpen => self.open_voyage_save_prompt(),
            ClickTarget::VoyageSaveConfirm => {
                self.save_displayed_voyage();
                self.voyage_ui.prompt = None;
            }
            ClickTarget::VoyageSaveDiscard => {
                self.discard_displayed_voyage();
                self.voyage_ui.prompt = None;
            }
            ClickTarget::VoyageSaveCancel => {
                self.voyage_ui.prompt = None;
            }
            ClickTarget::VoyageStat { idx } => {
                self.voyage_ui.focus = idx;
                // The Sea Battles section (focus 0) opens the per-fight log.
                if idx == 0 {
                    self.open_battles_popup();
                }
            }
            ClickTarget::VoyageChart { idx } => {
                self.voyage_ui.focus = self.voyage_ui.n_stats + idx;
                if crate::voyage::ui::CHART_ENLARGEABLE.get(idx) == Some(&true) {
                    self.voyage_ui.chart_popup = Some(idx);
                }
            }
            ClickTarget::VoyageChartClose => {
                self.voyage_ui.chart_popup = None;
            }
            ClickTarget::VoyageBattlesClose => {
                // Closing the editor's ship picker takes priority over the popup.
                if self.voyage_ui.battle_editor.popup.is_some() {
                    self.voyage_ui.battle_editor.popup = None;
                } else {
                    self.voyage_ui.battles_popup = None;
                }
            }
            ClickTarget::VoyageBattlesPrev => self.battles_page(-1),
            ClickTarget::VoyageBattlesNext => self.battles_page(1),
            ClickTarget::VoyageBattlesRecord => {
                self.voyage_ui.battles_focus = crate::voyage::ui::BattlesFocus::Record;
                self.toggle_battle_record();
            }
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
                // The wheel over the Skill Leaderboard moves its selection within the
                // current column (the shared window auto-scrolls to follow it).
                if let Some(
                    ClickTarget::JobberLeaderboard | ClickTarget::JobberLeaderboardPirate { .. },
                ) = clickmap::hit_test(&self.click_regions, col, row)
                {
                    let len = self.leaderboard_current_len();
                    if len > 0 {
                        let next = (self.jobbers_ui.top_sel as i32 + delta.signum())
                            .clamp(0, len as i32 - 1) as usize;
                        self.jobbers_ui.top_sel = next;
                    }
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
            AppId::Voyage => {
                if delta < 0 {
                    self.voyage_ui.focus = self.voyage_ui.focus.saturating_sub(1);
                } else {
                    self.voyage_ui.focus = self.voyage_ui.focus.saturating_add(1);
                }
            }
            AppId::Exit => {}
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
                    let (pillage_gross, pillage_stolen, pillage_chest) =
                        self.chatlog.current_pillage_poe();
                    let shared = SharedState {
                        commodities: &self.commodities,
                        cached_offers: &self.cached_offers,
                        available_islands: &self.available_islands,
                        loading: self.loading,
                        market_supported: self.market_ok(),
                        pillage_gross,
                        pillage_stolen,
                        pillage_chest,
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
