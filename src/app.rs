use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding};

use crate::aliases;
use crate::api::{CachedOffers, Commodity, fetch_offers_for};
use crate::chatlog::GameState;
use crate::clickmap::{self, ClickRegion, ClickTarget};
use crate::damage::DamageApp;
use crate::jobbers::{self, JobberFocus, JobbersUi, PirateCache};
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
}

impl AppId {
    pub fn label(self) -> &'static str {
        match self {
            AppId::Profits => "Profits",
            AppId::Damage => "Damage",
            AppId::Chatlog => "Jobbers",
        }
    }
}

pub const APP_LIST: &[AppId] = &[AppId::Profits, AppId::Damage, AppId::Chatlog];

const SIDEBAR_WIDTH: u16 = 14;

#[derive(PartialEq)]
enum GlobalFocus {
    Sidebar,
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

// ---------------------------------------------------------------------------
// AppShell
// ---------------------------------------------------------------------------

pub struct AppShell {
    pub commodities: Vec<Commodity>,
    pub cached_offers: HashMap<String, CachedOffers>,
    pub available_islands: Vec<String>,
    pub loading: bool,
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
            sidebar_index: 0,
            global_focus: GlobalFocus::Content,
            profits: ProfitsApp::new(),
            damage: DamageApp::new(),
            chatlog: GameState::new(),
            pirate_cache: PirateCache::new(),
            jobbers_ui: JobbersUi::default(),
            click_regions: Vec::new(),
        }
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

        // Layout tree: [sidebar | content]. Pages own everything in their
        // content area — the Jobbers page, for instance, draws its own tooltip
        // inside its centered block rather than as a full-width strip here.
        let chunks = Layout::horizontal([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Min(0),
        ])
        .split(area);

        self.render_sidebar(frame, chunks[0]);

        let content_area = chunks[1];
        let content_focused = self.global_focus == GlobalFocus::Content;
        match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
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
        }
    }

    fn render_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.global_focus == GlobalFocus::Sidebar;

        let items: Vec<ListItem> = APP_LIST
            .iter()
            .map(|app_id| ListItem::new(app_id.label()))
            .collect();

        let highlight_style = if focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default().bold()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1))
                    .title("─── Apps "),
            )
            .highlight_style(highlight_style);

        let mut state = ListState::default().with_selected(Some(self.sidebar_index));
        frame.render_stateful_widget(list, area, &mut state);

        // Register sidebar item click regions
        // inner area: 1 border + 1 padding on each side
        let inner_x = area.x + 2;
        let inner_w = area.width.saturating_sub(4);
        let inner_y = area.y + 1; // top border
        for i in 0..APP_LIST.len() {
            let item_rect = Rect::new(inner_x, inner_y + i as u16, inner_w, 1);
            self.click_regions.push(ClickRegion {
                rect: item_rect,
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
        if self.global_focus == GlobalFocus::Sidebar {
            return self.handle_sidebar_key(key);
        }

        let result = match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                };
                self.profits.handle_key(key, &shared)
            }
            AppId::Damage => self.damage.handle_key(key),
            AppId::Chatlog => self.handle_jobbers_key(key),
        };

        match result {
            InputResult::Consumed => {}
            InputResult::Exit => {
                self.global_focus = GlobalFocus::Sidebar;
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

    fn handle_sidebar_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => {
                if 0 < self.sidebar_index {
                    self.sidebar_index -= 1;
                }
            }
            KeyCode::Down => {
                if self.sidebar_index + 1 < APP_LIST.len() {
                    self.sidebar_index += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right => {
                self.global_focus = GlobalFocus::Content;
                // Entering the Jobbers page lands on the Vessels list.
                self.jobbers_ui.focus = JobberFocus::Vessels;
            }
            _ => {}
        }
        false
    }

    // -- jobbers (chat log) handling --

    fn handle_jobbers_key(&mut self, key: KeyEvent) -> InputResult {
        use JobberFocus::*;

        // The ship-type popup is modal: it eats keys until dismissed.
        if let Some(sel) = self.jobbers_ui.ship_popup {
            return self.handle_ship_popup_key(key, sel);
        }

        // Shift+Up/Down scrolls the focused list (rather than navigating).
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let delta = if key.code == KeyCode::Up { -1 } else { 1 };
            self.jobbers_scroll_focused(delta);
            return InputResult::Consumed;
        }

        match key.code {
            KeyCode::Esc => return InputResult::Exit,
            // Left from the left column exits to the Apps sidebar; from the
            // lists it steps back to the vessel column.
            KeyCode::Left => match self.jobbers_ui.focus {
                Vessels | ShipType | Unpoison => return InputResult::Exit,
                Aboard | Greedy | Planked => self.jobbers_ui.focus = Vessels,
            },
            KeyCode::Right => match self.jobbers_ui.focus {
                Vessels | ShipType | Unpoison => self.jobbers_ui.focus = Aboard,
                _ => {}
            },
            KeyCode::Up => match self.jobbers_ui.focus {
                Vessels => self.jobbers_select_delta(-1),
                ShipType => self.jobbers_ui.focus = Vessels,
                Unpoison => self.jobbers_ui.focus = ShipType,
                Aboard => {}
                Greedy => self.jobbers_ui.focus = Aboard,
                Planked => self.jobbers_ui.focus = Greedy,
            },
            KeyCode::Down => match self.jobbers_ui.focus {
                Vessels => {
                    // Past the last vessel, drop onto the Ship Type widget.
                    if self.jobbers_at_last_vessel() {
                        self.jobbers_ui.focus = ShipType;
                    } else {
                        self.jobbers_select_delta(1);
                    }
                }
                // Unpoison is only reachable while the vessel is poisoned.
                ShipType => {
                    if self.selected_poisoned() {
                        self.jobbers_ui.focus = Unpoison;
                    }
                }
                Unpoison => {}
                Aboard => self.jobbers_ui.focus = Greedy,
                Greedy => self.jobbers_ui.focus = Planked,
                Planked => {}
            },
            KeyCode::Enter => match self.jobbers_ui.focus {
                Unpoison => {
                    self.jobbers_unpoison();
                    self.jobbers_ui.focus = Vessels;
                }
                ShipType => self.open_ship_popup(),
                _ => {}
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

    /// Scroll the currently-focused list by `delta` rows.
    fn jobbers_scroll_focused(&mut self, delta: i32) {
        let offset = match self.jobbers_ui.focus {
            JobberFocus::Aboard => &mut self.jobbers_ui.aboard_offset,
            JobberFocus::Greedy => &mut self.jobbers_ui.greedy_offset,
            JobberFocus::Planked => &mut self.jobbers_ui.planked_offset,
            _ => return,
        };
        *offset = if delta < 0 {
            offset.saturating_sub(1)
        } else {
            offset.saturating_add(1)
        };
    }

    /// Move the vessel selection up/down the latest-first list.
    fn jobbers_select_delta(&mut self, delta: i32) {
        let ordered = self.chatlog.vessels_by_recency();
        if ordered.is_empty() {
            return;
        }
        let cur = self
            .jobbers_ui
            .selected
            .as_ref()
            .and_then(|s| ordered.iter().position(|k| k == s))
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, ordered.len() as i32 - 1) as usize;
        self.jobbers_ui.selected = Some(ordered[next].clone());
    }

    /// Whether the selected vessel is the last (bottom) one in the list.
    fn jobbers_at_last_vessel(&self) -> bool {
        let ordered = self.chatlog.vessels_by_recency();
        match (&self.jobbers_ui.selected, ordered.last()) {
            (Some(sel), Some(last)) => sel == last,
            _ => false,
        }
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
                    self.global_focus = GlobalFocus::Content;
                    self.jobbers_ui.focus = JobberFocus::Vessels;
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
                            name,
                            yes_focused: false,
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
                };
                let result = self.profits.handle_popup_click(true, &shared);
                self.process_input_result(result, tx);
            }
            ClickTarget::ProfitsPopupOk => {
                self.profits.popup = None;
                self.profits.focus = crate::profits::Focus::Input;
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
            ClickTarget::JobberVessel(i) => {
                self.global_focus = GlobalFocus::Content;
                self.jobbers_ui.focus = JobberFocus::Vessels;
                let ordered = self.chatlog.vessels_by_recency();
                if let Some(key) = ordered.get(i) {
                    self.jobbers_ui.selected = Some(key.clone());
                }
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
                // While the ship-type popup is open, the wheel moves its highlight.
                if let Some(sel) = self.jobbers_ui.ship_popup {
                    let count = crate::ships::SHIPS.len();
                    self.jobbers_ui.ship_popup = Some(if delta < 0 {
                        sel.saturating_sub(1)
                    } else {
                        (sel + 1).min(count.saturating_sub(1))
                    });
                    return;
                }
                // Scroll whichever list the cursor is hovering.
                let target = clickmap::hit_test(&self.click_regions, col, row);
                let offset = match target {
                    Some(ClickTarget::JobberAboardList) => &mut self.jobbers_ui.aboard_offset,
                    Some(ClickTarget::JobberGreedyList) => &mut self.jobbers_ui.greedy_offset,
                    Some(ClickTarget::JobberPlankedList) => &mut self.jobbers_ui.planked_offset,
                    _ => return,
                };
                if delta < 0 {
                    *offset = offset.saturating_sub(1);
                } else {
                    *offset = offset.saturating_add(1);
                }
            }
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
                self.global_focus = GlobalFocus::Sidebar;
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
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = fetch_offers_for(&client, &names).await;
            let _ = tx.send(result);
        });
    }
}
