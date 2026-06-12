use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding};

use crate::aliases;
use crate::api::{CachedOffers, Commodity, fetch_offers_for};
use crate::damage::DamageApp;
use crate::profits::ProfitsApp;
use crate::utils::text_similarity;

// ---------------------------------------------------------------------------
// App routing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum AppId {
    Profits,
    Damage,
}

impl AppId {
    pub fn label(self) -> &'static str {
        match self {
            AppId::Profits => "Profits",
            AppId::Damage => "Damage",
        }
    }
}

pub const APP_LIST: &[AppId] = &[AppId::Profits, AppId::Damage];

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
        let area = frame.area();

        let chunks = Layout::horizontal([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Min(0),
        ])
        .split(area);

        self.render_sidebar(frame, chunks[0]);

        let content_area = chunks[1];
        match APP_LIST[self.sidebar_index] {
            AppId::Profits => {
                let shared = SharedState {
                    commodities: &self.commodities,
                    cached_offers: &self.cached_offers,
                    available_islands: &self.available_islands,
                    loading: self.loading,
                };
                crate::profits::ui::render(frame, content_area, &mut self.profits, &shared);
            }
            AppId::Damage => {
                crate::damage::ui::render(frame, content_area, &mut self.damage);
            }
        }
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
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
            }
            _ => {}
        }
        false
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
