use std::collections::HashMap;
use std::io;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState};
use serde::{Deserialize, Serialize};

mod aliases;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
struct Args {
    /// Path to save/load inventory JSON
    #[arg(long)]
    inventory: Option<String>,

    /// Path to save/load market cache JSON
    #[arg(long)]
    market_cache: Option<String>,
}

// ---------------------------------------------------------------------------
// Persistence types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct SavedInventoryRow {
    commodity: String,
    restock: String,
    stock: String,
    booty: String,
}

#[derive(Serialize, Deserialize)]
struct SavedInventory {
    rows: Vec<SavedInventoryRow>,
    panel: Vec<String>,
    #[serde(default)]
    restocking_island: String,
}

#[derive(Serialize, Deserialize)]
struct SavedCommodity {
    id: u64,
    name: String,
}

#[derive(Serialize, Deserialize)]
struct SavedMarketCache {
    commodities: Vec<SavedCommodity>,
    offers: HashMap<String, CachedOffers>,
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Commodity {
    id: u64,
    #[serde(rename = "commodname")]
    name: String,
}

#[derive(Deserialize)]
struct BuySellResponse {
    commodity: String,
    offers: Vec<RawOffer>,
}

#[derive(Deserialize)]
struct RawOfferIsland {
    islandname: String,
}

#[derive(Deserialize)]
struct RawOffer {
    stallname: String,
    island: RawOfferIsland,
    buyprice: u64,
    sellprice: u64,
    buyqty: u64,
    sellqty: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Offer {
    stallname: String,
    islandname: String,
    buyprice: u64,
    sellprice: u64,
    buyqty: u64,
    sellqty: u64,
}

impl From<RawOffer> for Offer {
    fn from(r: RawOffer) -> Self {
        Self {
            stallname: r.stallname,
            islandname: r.island.islandname,
            buyprice: r.buyprice,
            sellprice: r.sellprice,
            buyqty: r.buyqty,
            sellqty: r.sellqty,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedOffers {
    offers: Vec<Offer>,
    fetched_at: u64,
}

struct ProfitResult {
    goods_value: u64,
    restock_value: u64,
    co_cut: u64,
    crew_donation: u64,
    subtotal: u64,
    stocking: u64,
    add_to_booty: u64,
}

enum PopupKind {
    ReQueryConfirm { yes_focused: bool },
    DeleteConfirm { row_idx: usize, name: String, yes_focused: bool },
    RestockWarning { missing: Vec<String>, ocean_wide_focused: bool },
    ProfitResult(ProfitResult),
}

struct InventoryRow {
    commod_id: u64,
    restock: String,
    stock: String,
    booty: String,
}

impl InventoryRow {
    fn new(commod_id: u64) -> Self {
        Self {
            commod_id,
            restock: String::new(),
            stock: String::new(),
            booty: String::new(),
        }
    }

    fn field(&self, col: usize) -> &str {
        match col {
            1 => &self.restock,
            2 => &self.stock,
            3 => &self.booty,
            _ => "",
        }
    }

    fn field_mut(&mut self, col: usize) -> Option<&mut String> {
        match col {
            1 => Some(&mut self.restock),
            2 => Some(&mut self.stock),
            3 => Some(&mut self.booty),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Field types & validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum FieldKind {
    PositiveInt,
    Rate,
    Text,
}

struct PromptField {
    label: &'static str,
    kind: FieldKind,
    value: String,
    cursor: usize,
}

impl PromptField {
    fn new(label: &'static str, kind: FieldKind) -> Self {
        Self {
            label,
            kind,
            value: String::new(),
            cursor: 0,
        }
    }

    fn accepts(&self, c: char) -> bool {
        match self.kind {
            FieldKind::PositiveInt => c.is_ascii_digit(),
            FieldKind::Rate => {
                if c.is_ascii_digit() {
                    true
                } else if c == '.' {
                    !self.value.contains('.')
                } else if c == '%' {
                    !self.value.contains('%') && self.cursor == self.value.len()
                } else {
                    false
                }
            }
            FieldKind::Text => !c.is_control(),
        }
    }

    fn insert_char(&mut self, c: char) {
        if self.accepts(c) {
            self.value.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.remove(prev);
            self.cursor = prev;
        }
    }

    fn delete_char_at(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += self.value[self.cursor..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

const PANEL_COUNT: usize = 6;

enum FetchPurpose {
    Islands,
    Profits,
}

#[derive(PartialEq)]
enum Focus {
    Input,
    Table,
    Panel(usize),
    Button,
    Popup,
}

struct App {
    commodities: Vec<Commodity>,
    rows: Vec<InventoryRow>,
    input: String,
    cursor: usize,
    focus: Focus,
    table_state: TableState,
    panel: [PromptField; PANEL_COUNT],
    submit_failed: Option<String>,
    popup: Option<PopupKind>,
    loading: bool,
    calc_error: Option<String>,
    cached_offers: HashMap<String, CachedOffers>,
    available_islands: Vec<String>,
    fetch_purpose: FetchPurpose,
}

const FIRST_COL: usize = 1;
const LAST_COL: usize = 3;

impl App {
    fn new(commodities: Vec<Commodity>) -> Self {
        Self {
            commodities,
            rows: Vec::new(),
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            table_state: TableState::default(),
            panel: [
                PromptField::new("Restocking Island", FieldKind::Text),
                PromptField::new("Money in Booty", FieldKind::PositiveInt),
                PromptField::new("Commanding Officer Rate", FieldKind::Rate),
                PromptField::new("Crew Donation Share Rate", FieldKind::Rate),
                PromptField::new("Restocking Rate", FieldKind::Rate),
                PromptField::new("Stocking", FieldKind::PositiveInt),
            ],
            submit_failed: None,
            popup: None,
            loading: false,
            calc_error: None,
            cached_offers: HashMap::new(),
            available_islands: Vec::new(),
            fetch_purpose: FetchPurpose::Profits,
        }
    }

    fn commod_name(&self, commod_id: u64) -> &str {
        self.commodities
            .iter()
            .find(|c| c.id == commod_id)
            .map(|c| c.name.as_str())
            .unwrap_or("???")
    }

    fn rebuild_island_list(&mut self) {
        let inventory_names: Vec<String> = self
            .rows
            .iter()
            .map(|r| self.commod_name(r.commod_id).to_owned())
            .collect();

        let mut islands = Vec::new();
        for name in &inventory_names {
            if let Some(cached) = self.cached_offers.get(name.as_str()) {
                for offer in &cached.offers {
                    if offer.sellprice > 0 && offer.sellqty > 0 && !islands.contains(&offer.islandname) {
                        islands.push(offer.islandname.clone());
                    }
                }
            }
        }
        islands.sort();
        self.available_islands = islands;
    }

    fn suggest_island(&self) -> Option<&str> {
        let query = self.panel[0].value.trim().to_lowercase();
        if query.is_empty() {
            return None;
        }

        // Alias lookup
        if let Some(&target) = aliases::get_islands().get(query.as_str()) {
            if let Some(island) = self
                .available_islands
                .iter()
                .find(|i| i.eq_ignore_ascii_case(target))
            {
                return Some(island);
            }
        }

        // Exact match
        if let Some(island) = self
            .available_islands
            .iter()
            .find(|i| i.eq_ignore_ascii_case(&query))
        {
            return Some(island);
        }

        // Unique prefix
        let prefix_matches: Vec<_> = self
            .available_islands
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
        for island in &self.available_islands {
            let score = text_similarity(&query, &island.to_lowercase());
            if score > best_score {
                best_score = score;
                best = Some(island.as_str());
                tie = false;
            } else if score == best_score {
                tie = true;
            }
        }
        if best_score >= 0.75 && !tie {
            return best;
        }

        None
    }

    /// Check for missing restock supply on the selected island and either
    /// show a warning popup or proceed straight to the profit result.
    fn calculate_or_warn(&mut self) {
        if let Some(island) = self.suggest_island().map(|s| s.to_owned()) {
            let missing = self.missing_restock_on_island(&island);
            if !missing.is_empty() {
                self.popup = Some(PopupKind::RestockWarning {
                    missing,
                    ocean_wide_focused: false,
                });
                self.focus = Focus::Popup;
                return;
            }
        }
        let profit = self.calculate_profits();
        self.popup = Some(PopupKind::ProfitResult(profit));
        self.focus = Focus::Popup;
    }

    // -- focus transitions --

    fn focus_table_bottom(&mut self) {
        if self.rows.is_empty() {
            self.focus = Focus::Button;
            return;
        }
        self.focus = Focus::Table;
        self.table_state.select(Some(self.rows.len() - 1));
        self.table_state.select_column(Some(FIRST_COL));
    }

    fn focus_input(&mut self) {
        self.focus = Focus::Input;
        self.table_state.select(None);
        self.table_state.select_column(None);
    }

    fn focus_panel(&mut self, idx: usize) {
        self.focus = Focus::Panel(idx);
        self.table_state.select(None);
        self.table_state.select_column(None);
    }

    // -- suggestion & submit --

    fn suggest(&self) -> Option<u64> {
        let query = self.input.trim().to_lowercase();
        if query.is_empty() {
            return None;
        }

        // 1. Alias lookup
        if let Some(&target) = aliases::get().get(query.as_str()) {
            if let Some(c) = self
                .commodities
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(target))
            {
                return Some(c.id);
            }
        }

        // 2. Exact match
        if let Some(c) = self
            .commodities
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&query))
        {
            return Some(c.id);
        }

        // 3. Unique prefix
        let prefix_matches: Vec<_> = self
            .commodities
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&query))
            .collect();
        if prefix_matches.len() == 1 {
            return Some(prefix_matches[0].id);
        }

        // 4. Jaro-Winkler (minimum 0.75, unique winner)
        let mut best_score = f64::NEG_INFINITY;
        let mut best_id = None;
        let mut tie = false;
        for c in &self.commodities {
            let score = text_similarity(&query, &c.name.to_lowercase());
            if score > best_score {
                best_score = score;
                best_id = Some(c.id);
                tie = false;
            } else if score == best_score {
                tie = true;
            }
        }
        if best_score >= 0.75 && !tie {
            return best_id;
        }

        None
    }

    fn submit(&mut self) {
        let query = self.input.trim().to_lowercase();
        if query.is_empty() {
            self.input.clear();
            self.cursor = 0;
            return;
        }

        match self.suggest() {
            Some(id) => {
                if !self.rows.iter().any(|r| r.commod_id == id) {
                    let pos = self
                        .rows
                        .binary_search_by_key(&id, |r| r.commod_id)
                        .unwrap_err();
                    self.rows.insert(pos, InventoryRow::new(id));
                    self.rebuild_island_list();
                }
                self.submit_failed = None;
            }
            None => {
                self.submit_failed = Some(self.input.trim().to_owned());
            }
        }

        self.input.clear();
        self.cursor = 0;
    }

    fn parse_rate(field: &PromptField) -> f64 {
        let s = field.value.trim();
        if s.is_empty() {
            return 0.0;
        }
        if let Some(stripped) = s.strip_suffix('%') {
            stripped.parse::<f64>().unwrap_or(0.0) / 100.0
        } else {
            s.parse::<f64>().unwrap_or(0.0)
        }
    }

    /// Returns names of commodities that need restocking but have no sell
    /// offers on the given island.
    fn missing_restock_on_island(&self, island: &str) -> Vec<String> {
        let mut missing = Vec::new();
        for row in &self.rows {
            let name = self.commod_name(row.commod_id);
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);

            // Only care about commodities that actually need restocking
            if restock <= stock + booty {
                continue;
            }

            let has_supply = self
                .cached_offers
                .get(name)
                .map_or(false, |cached| {
                    cached.offers.iter().any(|o| {
                        o.sellprice > 0
                            && o.sellqty > 0
                            && o.islandname.eq_ignore_ascii_case(island)
                    })
                });
            if !has_supply {
                missing.push(name.to_owned());
            }
        }
        missing
    }

    fn calculate_profits(&self) -> ProfitResult {
        // Resolve restocking island filter
        let restock_island = if self.panel[0].value.trim().is_empty() {
            None
        } else {
            self.suggest_island().map(|s| s.to_owned())
        };

        let mut goods_value: u64 = 0;
        let mut restock_value: u64 = 0;

        for row in &self.rows {
            let name = self.commod_name(row.commod_id);
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);

            let offers = match self.cached_offers.get(name) {
                Some(cached) => &cached.offers,
                None => continue,
            };

            // Goods Value: sell booty at best buy prices (ocean-wide)
            if restock < booty + stock {
                let mut buy_offers: Vec<_> = offers
                    .iter()
                    .filter(|o| o.buyprice > 0 && o.buyqty > 0)
                    .collect();
                buy_offers.sort_by(|a, b| b.buyprice.cmp(&a.buyprice));

                let mut remaining = booty + stock - restock;
                for offer in &buy_offers {
                    if remaining == 0 {
                        break;
                    }
                    let qty = remaining.min(offer.buyqty);
                    goods_value += qty * offer.buyprice;
                    remaining -= qty;
                }
            }

            else {
                let need = restock - stock - booty;
                let mut sell_offers: Vec<_> = offers
                    .iter()
                    .filter(|o| o.sellprice > 0 && o.sellqty > 0)
                    .filter(|o| {
                        restock_island
                            .as_ref()
                            .map_or(true, |island| o.islandname.eq_ignore_ascii_case(island))
                    })
                    .collect();
                sell_offers.sort_by(|a, b| a.sellprice.cmp(&b.sellprice));

                let mut remaining = need;
                for offer in &sell_offers {
                    if remaining == 0 {
                        break;
                    }
                    let qty = remaining.min(offer.sellqty);
                    restock_value += qty * offer.sellprice;
                    remaining -= qty;
                }
            }
        }

        let booty_money = self.panel[1].value.parse::<f64>().unwrap_or(0.0);
        let co_rate = Self::parse_rate(&self.panel[2]);
        let donation_rate = Self::parse_rate(&self.panel[3]);
        let restocking_rate = Self::parse_rate(&self.panel[4]);

        // Back-compute total money reward: booty_money = M * (1-R) / 2
        let total_money = if restocking_rate < 1.0 {
            2.0 * booty_money / (1.0 - restocking_rate)
        } else {
            0.0
        };
        let ship_hold = total_money * restocking_rate;

        // Goods revenue available for booty after covering restocking overflow.
        // Payment chain: ship hold first, then goods revenue, then CO's pocket.
        let restock_overflow = (restock_value as f64 - ship_hold).max(0.0);
        let goods_to_booty = (goods_value as f64 - restock_overflow).max(0.0);

        // Total adventure earnings (base for CO cut and donations)
        let total_earnings = (total_money + goods_value as f64 - restock_value as f64).max(0.0);

        // Straight percentages of total earnings
        let co_cut = (total_earnings * co_rate).ceil() as u64;
        let crew_donation = (total_earnings * donation_rate).floor() as u64;

        // Subtotal before stocking deduction
        let subtotal = (goods_to_booty - co_cut as f64 - crew_donation as f64)
            .max(0.0)
            .floor() as u64;

        let stocking = self.panel[5].value.parse::<u64>().unwrap_or(0);
        let add_to_booty = subtotal.saturating_sub(stocking);

        ProfitResult {
            goods_value,
            restock_value,
            co_cut,
            crew_donation,
            subtotal,
            stocking,
            add_to_booty,
        }
    }

    fn input_insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.submit_failed = None;
        self.calc_error = None;
    }

    fn input_delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor = prev;
            self.submit_failed = None;
            self.calc_error = None;
        }
    }

    fn input_delete_char_at(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
            self.submit_failed = None;
            self.calc_error = None;
        }
    }

    fn input_move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn input_move_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += self.input[self.cursor..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }

    // -- table helpers --

    fn selected_cell(&self) -> Option<(usize, usize)> {
        Some((
            self.table_state.selected()?,
            self.table_state.selected_column()?,
        ))
    }

    fn table_up(&mut self) {
        if let Some(row) = self.table_state.selected() {
            if row > 0 {
                self.table_state.select(Some(row - 1));
            }
        }
    }

    fn table_down(&mut self) {
        if let Some(row) = self.table_state.selected() {
            if row + 1 < self.rows.len() {
                self.table_state.select(Some(row + 1));
            } else {
                self.focus_input();
            }
        }
    }

    fn table_left(&mut self) {
        if let Some(col) = self.table_state.selected_column() {
            if col > FIRST_COL {
                self.table_state.select_column(Some(col - 1));
            }
        }
    }

    fn table_right(&mut self) {
        if let Some(col) = self.table_state.selected_column() {
            if col < LAST_COL {
                self.table_state.select_column(Some(col + 1));
            } else {
                self.focus_panel(0);
            }
        }
    }

    fn table_insert_digit(&mut self, d: char) {
        if let Some((row, col)) = self.selected_cell() {
            if let Some(field) = self.rows[row].field_mut(col) {
                field.push(d);
            }
        }
    }

    fn table_delete_digit(&mut self) {
        if let Some((row, col)) = self.selected_cell() {
            if let Some(field) = self.rows[row].field_mut(col) {
                field.pop();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Text similarity
// ---------------------------------------------------------------------------

fn text_similarity(a: &str, b: &str) -> f64 {
    strsim::jaro_winkler(a, b)
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

fn ui(frame: &mut Frame, app: &mut App) {
    let vchunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4), // border(1) + input(1) + suggestion(1) + border(1)
    ])
    .split(frame.area());

    // -- Table --
    // Resolve names up front to avoid borrowing app in the row-building closure.
    let row_data: Vec<(String, String, String, String)> = app
        .rows
        .iter()
        .map(|r| {
            (
                app.commod_name(r.commod_id).to_owned(),
                r.restock.clone(),
                r.stock.clone(),
                r.booty.clone(),
            )
        })
        .collect();

    let item_width = row_data
        .iter()
        .map(|(name, ..)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max("Item".len()) as u16;

    let header = Row::new(vec!["Item", "Restock", "Stock", "Booty"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = row_data
        .iter()
        .map(|(name, restock, stock, booty)| {
            Row::new(vec![
                Cell::new(name.as_str()),
                Cell::new(Line::from(restock.as_str()).right_aligned()),
                Cell::new(Line::from(stock.as_str()).right_aligned()),
                Cell::new(Line::from(booty.as_str()).right_aligned()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(item_width),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(5),
    ];

    // 2 = left + right border, 2 = horizontal padding, 3 = spacing between 4 columns
    let table_width = item_width + 7 + 5 + 5 + 3 + 2 + 2;

    let highlight = Style::default().bg(Color::White).fg(Color::Black);
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::default())
        .cell_highlight_style(highlight)
        .block(Block::default().borders(Borders::ALL).padding(Padding::horizontal(1)).title("─── Inventory "));

    // -- Panel --
    let panel_label_width = app
        .panel
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0) as u16;
    // border (1) + padding-left (1) + label + padding-right (1) + border (1)
    let panel_inner_width = panel_label_width;
    let panel_width = panel_inner_width + 4; // +2 borders +2 padding

    // -- Horizontal layout: table + gap + panel, centered --
    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(table_width),
        Constraint::Length(1),
        Constraint::Length(panel_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[0]);

    frame.render_stateful_widget(table, hchunks[1], &mut app.table_state);

    // Vertically center the panel: 2 rows per field (label + value), + 1 spacer + 1 button + 2 border
    let panel_content_height = (PANEL_COUNT as u16) * 2 + 2;
    let panel_block_height = panel_content_height + 2;
    let panel_vchunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(panel_block_height),
        Constraint::Fill(1),
    ])
    .split(hchunks[3]);

    let panel_block = Block::default().borders(Borders::ALL).padding(Padding::horizontal(1)).title("─── Parameters ");
    let panel_inner = panel_block.inner(panel_vchunks[1]);
    frame.render_widget(panel_block, panel_vchunks[1]);

    // Each prompt takes 2 rows: label + value, then spacer + button
    let panel_constraints: Vec<Constraint> = (0..PANEL_COUNT)
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
        .chain([Constraint::Length(1), Constraint::Length(1)])
        .collect();
    let panel_rows = Layout::vertical(panel_constraints).split(panel_inner);

    for (i, field) in app.panel.iter().enumerate() {
        let label_area = panel_rows[i * 2];
        let value_area = panel_rows[i * 2 + 1];

        let label = Paragraph::new(Span::styled(field.label, Style::default().bold()));
        frame.render_widget(label, label_area);

        let is_focused = app.focus == Focus::Panel(i);
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default()
        };

        if i == 0 {
            // Island field: button when no market data, text field otherwise
            if app.cached_offers.is_empty() {
                let btn_style = if is_focused {
                    Style::default().bg(Color::White).fg(Color::Black).bold()
                } else {
                    Style::default().fg(Color::DarkGray).italic()
                };
                let value = Paragraph::new(Span::styled("Query Market first", btn_style));
                frame.render_widget(value, value_area);
            } else if field.value.is_empty() {
                let ph_style = if is_focused {
                    Style::default().fg(Color::DarkGray).bg(Color::White).italic()
                } else {
                    Style::default().fg(Color::DarkGray).italic()
                };
                let value = Paragraph::new(Line::from(Span::styled("Ocean-wide", ph_style)).right_aligned());
                frame.render_widget(value, value_area);
                if is_focused {
                    frame.set_cursor_position((value_area.x, value_area.y));
                }
            } else {
                let value = Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned())
                    .style(value_style);
                frame.render_widget(value, value_area);
                if is_focused {
                    let cx = value_area.x + value_area.width
                        - (field.value.chars().count()
                            - field.value[..field.cursor].chars().count())
                            as u16;
                    frame.set_cursor_position((cx, value_area.y));
                }
            }
        } else {
            // Numeric fields: right-aligned
            let value = Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned())
                .style(value_style);
            frame.render_widget(value, value_area);

            if is_focused {
                let cx = value_area.x + value_area.width
                    - (field.value.chars().count() - field.value[..field.cursor].chars().count())
                        as u16;
                frame.set_cursor_position((cx, value_area.y));
            }
        }
    }

    // "Calculate profits!" button
    let button_area = panel_rows[PANEL_COUNT * 2 + 1]; // skip spacer row
    let button_focused = app.focus == Focus::Button;
    let button_style = if button_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    };
    let button = Paragraph::new(Line::from(Span::styled("Calculate profits!", button_style)).centered());
    frame.render_widget(button, button_area);

    // -- Bottom box (input + suggestion), centered to match table+panel width --
    let bottom_width = table_width + 1 + panel_width;
    let bottom_hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(bottom_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[1]);

    let bottom_block = Block::default().borders(Borders::ALL).padding(Padding::horizontal(1)).title("─── Search ");
    let bottom_inner = bottom_block.inner(bottom_hchunks[1]);
    frame.render_widget(bottom_block, bottom_hchunks[1]);

    let bottom_rows = Layout::vertical([
        Constraint::Length(1), // input
        Constraint::Length(1), // suggestion / error
    ])
    .split(bottom_inner);

    // -- Input prompt --
    let label = "Add Commodity: ";
    let input_style = if app.focus == Focus::Input {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().bold()),
        Span::styled(&app.input, input_style),
    ]));
    frame.render_widget(prompt, bottom_rows[0]);

    if app.focus == Focus::Input {
        let cursor_x = bottom_rows[0].x
            + label.len() as u16
            + app.input[..app.cursor].chars().count() as u16;
        let cursor_y = bottom_rows[0].y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    // -- Suggestion / error line (priority: loading > calc_error > suggestion/submit_failed) --
    let status_line: Option<Line> = if app.loading {
        Some(Line::from(Span::styled(
            "Fetching prices from market...",
            Style::default().fg(Color::Yellow),
        )))
    } else if let Some(ref err) = app.calc_error {
        Some(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red),
        )))
    } else if app.focus == Focus::Panel(0) && !app.cached_offers.is_empty() {
        // Island suggestion
        let query = app.panel[0].value.trim();
        if query.is_empty() {
            None
        } else {
            match app.suggest_island() {
                Some(name) if name.eq_ignore_ascii_case(query) => None,
                Some(name) => Some(Line::from(vec![
                    Span::styled("Did you mean \"", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        name,
                        Style::default().bold().italic().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "\"? Press enter to accept.",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])),
                None => Some(Line::from(Span::styled(
                    "No matching island",
                    Style::default().fg(Color::Red),
                ))),
            }
        }
    } else {
        let suggestion = app.suggest();
        let query_lower = app.input.trim().to_lowercase();
        match suggestion {
            Some(id) => {
                let name = app.commod_name(id);
                if name.eq_ignore_ascii_case(&query_lower) {
                    None
                } else {
                    Some(Line::from(vec![
                        Span::styled("Did you mean \"", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            name,
                            Style::default().bold().italic().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            "\"? Press enter if yes.",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                }
            }
            None => {
                if let Some(ref query) = app.submit_failed {
                    Some(Line::from(vec![
                        Span::styled("No \"", Style::default().fg(Color::Red)),
                        Span::styled(
                            query.as_str(),
                            Style::default().bold().italic().fg(Color::Red),
                        ),
                        Span::styled("\" found", Style::default().fg(Color::Red)),
                    ]))
                } else {
                    None
                }
            }
        }
    };
    if let Some(line) = status_line {
        frame.render_widget(Paragraph::new(line), bottom_rows[1]);
    }

    // -- Popup overlay --
    if let Some(ref popup) = app.popup {
        render_popup(frame, popup);
    }
}

fn render_popup(frame: &mut Frame, popup: &PopupKind) {
    let area = frame.area();

    match popup {
        PopupKind::ReQueryConfirm { yes_focused } => {
            let w: u16 = 40;
            let h: u16 = 6;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Re-query? ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            frame.render_widget(
                Paragraph::new("Re-query Market?"),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "This may take some time.",
                    Style::default().fg(Color::DarkGray),
                )),
                rows[1],
            );

            let no_style = if !yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let yes_style = if *yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" No ", no_style),
                Span::raw("  "),
                Span::styled(" Yes ", yes_style),
            ]);
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                rows[3],
            );
        }
        PopupKind::DeleteConfirm { row_idx: _, name, yes_focused } => {
            // "Delete row " + quotes + name + "?" + border padding
            let text_len = "Delete row \"\"?".len() + name.len();
            let w: u16 = (text_len as u16 + 6).max(22); // +2 borders +2 padding +2 margin
            let h: u16 = 5;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Delete row ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            let prompt = Line::from(vec![
                Span::raw("Delete row \""),
                Span::styled(name.as_str(), Style::default().bold().italic()),
                Span::raw("\"?"),
            ]);
            frame.render_widget(Paragraph::new(prompt), rows[0]);

            let no_style = if !yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let yes_style = if *yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" No ", no_style),
                Span::raw("  "),
                Span::styled(" Yes ", yes_style),
            ]);
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                rows[2],
            );
        }
        PopupKind::RestockWarning { missing, ocean_wide_focused } => {
            // List commodities, capped to avoid overflow
            let max_shown = 5;
            let shown: Vec<&str> = missing.iter().take(max_shown).map(|s| s.as_str()).collect();
            let extra = if missing.len() > max_shown {
                missing.len() - max_shown
            } else {
                0
            };

            // Height: 1 header + shown.len() + optional extra line + 1 blank + 1 buttons + 2 border
            let list_lines = shown.len() + if extra > 0 { 1 } else { 0 };
            let h: u16 = (3 + list_lines + 2) as u16; // header + list + blank + buttons + border
            let w: u16 = 46;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Restock warning ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let mut constraints: Vec<Constraint> = Vec::new();
            constraints.push(Constraint::Length(1)); // header
            for _ in 0..list_lines {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Length(1)); // blank
            constraints.push(Constraint::Length(1)); // buttons

            let rows = Layout::vertical(constraints).split(inner);

            frame.render_widget(
                Paragraph::new("No supply on this island for:"),
                rows[0],
            );
            for (i, name) in shown.iter().enumerate() {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  \u{2022} {}", name),
                        Style::default().fg(Color::Yellow),
                    )),
                    rows[1 + i],
                );
            }
            if extra > 0 {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  ...and {} more", extra),
                        Style::default().fg(Color::DarkGray),
                    )),
                    rows[1 + shown.len()],
                );
            }

            let ocean_style = if *ocean_wide_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let change_style = if !ocean_wide_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" Change Island ", change_style),
                Span::raw("  "),
                Span::styled(" Ocean-wide ", ocean_style),
            ]);
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                rows[rows.len() - 1],
            );
        }
        PopupKind::ProfitResult(result) => {
            let labels = [
                "Goods Value",
                "Restock Value",
                "C. Officer Cut",
                "Crew Donation",
                "Subtotal",
                "Stocking",
                "Add to Booty",
            ];
            let val_strs: [String; 7] = [
                format!("{}", result.goods_value),
                format!("{}", result.restock_value),
                format!("{}", result.co_cut),
                format!("{}", result.crew_donation),
                format!("{}", result.subtotal),
                format!("{}", result.stocking),
                format!("{}", result.add_to_booty),
            ];
            let max_content = labels
                .iter()
                .zip(val_strs.iter())
                .map(|(l, v)| l.len() + 4 + v.len())
                .max()
                .unwrap_or(0);
            // +2 borders +2 padding; min 24 to fit title
            let w: u16 = (max_content as u16 + 4).max(24);
            let h: u16 = 11;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Profit Breakdown ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            let avail = inner.width as usize;

            for (i, (lbl, val_str)) in labels.iter().zip(val_strs.iter()).enumerate() {
                let pad = avail.saturating_sub(lbl.len()).saturating_sub(val_str.len());
                let line = format!("{}{:>w$}", lbl, val_str, w = pad + val_str.len());
                frame.render_widget(Paragraph::new(line), rows[i]);
            }

            let ok_style = Style::default().bg(Color::White).fg(Color::Black).bold();
            let ok_btn = Line::from(Span::styled(" Ok ", ok_style));
            frame.render_widget(Paragraph::new(ok_btn).centered(), rows[8]);
        }
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn spawn_fetch(
    app: &App,
    tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
) {
    let names: Vec<String> = app
        .rows
        .iter()
        .filter(|r| {
            let restock = r.restock.parse::<u64>().unwrap_or(0);
            let stock = r.stock.parse::<u64>().unwrap_or(0);
            let booty = r.booty.parse::<u64>().unwrap_or(0);
            restock != 0 || stock != 0 || booty != 0
        })
        .map(|r| app.commod_name(r.commod_id).to_owned())
        .collect();

    let tx = tx.clone();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let result = fetch_offers_for(&client, &names).await;
        let _ = tx.send(result);
    });
}

fn spawn_island_fetch(
    app: &App,
    tx: &tokio::sync::mpsc::UnboundedSender<Result<HashMap<String, CachedOffers>, String>>,
) {
    let names: Vec<String> = app
        .rows
        .iter()
        .map(|r| app.commod_name(r.commod_id).to_owned())
        .collect();

    let tx = tx.clone();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let result = fetch_offers_for(&client, &names).await;
        let _ = tx.send(result);
    });
}

async fn fetch_offers_for(
    client: &reqwest::Client,
    names: &[String],
) -> Result<HashMap<String, CachedOffers>, String> {
    let mut map = HashMap::new();
    for name in names {
        let mut url =
            reqwest::Url::parse("https://api.plunderly.app/buysells/by-commodity").unwrap();
        url.query_pairs_mut()
            .append_pair("ocean", "Emerald")
            .append_pair("commodity", name);

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Fetch error: {}", e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let data: BuySellResponse = resp
            .json()
            .await
            .map_err(|e| format!("Parse error: {}", e))?;

        let offers = data.offers.into_iter().map(Offer::from).collect();
        map.insert(name.clone(), CachedOffers { offers, fetched_at: now });
    }
    Ok(map)
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    // -- Load market cache or fetch commodities from API --
    let mut cached_offers: HashMap<String, CachedOffers> = HashMap::new();

    let commodities: Vec<Commodity> = if let Some(ref path) = args.market_cache {
        if let Ok(data) = std::fs::read_to_string(path) {
            match serde_json::from_str::<SavedMarketCache>(&data) {
                Ok(cache) => {
                    eprintln!("Loaded market cache from {}", path);
                    cached_offers = cache.offers;
                    cache
                        .commodities
                        .into_iter()
                        .map(|c| Commodity { id: c.id, name: c.name })
                        .collect()
                }
                Err(e) => {
                    eprintln!("warning: failed to parse market cache: {}", e);
                    eprintln!("Fetching commodities from market...");
                    let mut c: Vec<Commodity> = reqwest::get("https://api.plunderly.app/commods")
                        .await
                        .expect("failed to fetch commodities")
                        .json()
                        .await
                        .expect("failed to parse commodities");
                    c.sort_by_key(|c| c.id);
                    c
                }
            }
        } else {
            eprintln!("Fetching commodities from market...");
            let mut c: Vec<Commodity> = reqwest::get("https://api.plunderly.app/commods")
                .await
                .expect("failed to fetch commodities")
                .json()
                .await
                .expect("failed to parse commodities");
            c.sort_by_key(|c| c.id);
            c
        }
    } else {
        eprintln!("Fetching commodities from market...");
        let mut c: Vec<Commodity> = reqwest::get("https://api.plunderly.app/commods")
            .await
            .expect("failed to fetch commodities")
            .json()
            .await
            .expect("failed to parse commodities");
        c.sort_by_key(|c| c.id);
        c
    };

    // Validate alias targets against commodity list.
    for (&alias, &target) in aliases::get().iter() {
        if !commodities
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(target))
        {
            eprintln!(
                "warning: alias '{}' targets unknown commodity '{}'",
                alias, target
            );
        }
    }

    let mut app = App::new(commodities);
    app.cached_offers = cached_offers;

    // -- Load inventory --
    if let Some(ref path) = args.inventory {
        if let Ok(data) = std::fs::read_to_string(path) {
            match serde_json::from_str::<SavedInventory>(&data) {
                Ok(inv) => {
                    eprintln!("Loaded inventory from {}", path);
                    for saved_row in inv.rows {
                        if let Some(c) = app
                            .commodities
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(&saved_row.commodity))
                        {
                            let id = c.id;
                            if !app.rows.iter().any(|r| r.commod_id == id) {
                                let pos = app
                                    .rows
                                    .binary_search_by_key(&id, |r| r.commod_id)
                                    .unwrap_err();
                                app.rows.insert(
                                    pos,
                                    InventoryRow {
                                        commod_id: id,
                                        restock: saved_row.restock,
                                        stock: saved_row.stock,
                                        booty: saved_row.booty,
                                    },
                                );
                            }
                        } else {
                            eprintln!(
                                "warning: unknown commodity '{}' in inventory, skipping",
                                saved_row.commodity
                            );
                        }
                    }
                    // Restore island field separately
                    app.panel[0].value = inv.restocking_island.clone();
                    app.panel[0].cursor = inv.restocking_island.len();
                    // Restore numeric fields (shifted by 1)
                    for (i, val) in inv.panel.into_iter().enumerate() {
                        if i + 1 < PANEL_COUNT {
                            app.panel[i + 1].value = val.clone();
                            app.panel[i + 1].cursor = val.len();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("warning: failed to parse inventory: {}", e);
                }
            }
        }
    }

    // -- Auto-fetch missing market data --
    if !app.rows.is_empty() {
        let missing: Vec<String> = app
            .rows
            .iter()
            .map(|r| app.commod_name(r.commod_id).to_owned())
            .filter(|name| !app.cached_offers.contains_key(name.as_str()))
            .collect();

        if !missing.is_empty() {
            eprintln!(
                "Fetching market data for {} missing commodities...",
                missing.len()
            );
            let client = reqwest::Client::new();
            match fetch_offers_for(&client, &missing).await {
                Ok(new_offers) => {
                    app.cached_offers.extend(new_offers);
                }
                Err(e) => {
                    eprintln!("warning: failed to fetch missing market data: {}", e);
                }
            }
        }
    }

    app.rebuild_island_list();

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<HashMap<String, CachedOffers>, String>>();

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        // Check for completed API results
        if let Ok(result) = rx.try_recv() {
            app.loading = false;
            match result {
                Ok(offers_map) => {
                    match app.fetch_purpose {
                        FetchPurpose::Islands => {
                            app.cached_offers.extend(offers_map);
                            app.rebuild_island_list();
                        }
                        FetchPurpose::Profits => {
                            app.cached_offers = offers_map;
                            app.rebuild_island_list();
                            app.calculate_or_warn();
                        }
                    }
                }
                Err(msg) => {
                    app.calc_error = Some(msg);
                }
            }
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Esc: dismiss popup if open, otherwise exit
            if key.code == KeyCode::Esc {
                if app.popup.is_some() {
                    match app.popup {
                        Some(PopupKind::ReQueryConfirm { .. }) => {
                            // Esc = use cached data
                            app.calculate_or_warn();
                        }
                        Some(PopupKind::DeleteConfirm { .. }) => {
                            // Esc = cancel delete, return to table
                            app.popup = None;
                            app.focus = Focus::Table;
                        }
                        Some(PopupKind::RestockWarning { .. }) => {
                            // Esc = change island
                            app.popup = None;
                            app.focus = Focus::Panel(0);
                        }
                        _ => {
                            app.popup = None;
                            app.focus = Focus::Input;
                        }
                    }
                } else {
                    break;
                }
                continue;
            }

            match app.focus {
                Focus::Input => match key.code {
                    KeyCode::Enter => app.submit(),
                    KeyCode::Backspace => app.input_delete_char_before(),
                    KeyCode::Delete => app.input_delete_char_at(),
                    KeyCode::Left => app.input_move_left(),
                    KeyCode::Right => app.input_move_right(),
                    KeyCode::Home => app.cursor = 0,
                    KeyCode::End => app.cursor = app.input.len(),
                    KeyCode::Up => app.focus_table_bottom(),
                    KeyCode::Char(c) => app.input_insert_char(c),
                    _ => {}
                },
                Focus::Table => match key.code {
                    KeyCode::Up => app.table_up(),
                    KeyCode::Down => app.table_down(),
                    KeyCode::Left => app.table_left(),
                    KeyCode::Right => app.table_right(),
                    KeyCode::Char(d) if d.is_ascii_digit() => app.table_insert_digit(d),
                    KeyCode::Backspace => app.table_delete_digit(),
                    KeyCode::Delete => {
                        if let Some(row) = app.table_state.selected() {
                            let name = app.commod_name(app.rows[row].commod_id).to_owned();
                            app.popup = Some(PopupKind::DeleteConfirm {
                                row_idx: row,
                                name,
                                yes_focused: false,
                            });
                            app.focus = Focus::Popup;
                        }
                    }
                    _ => {}
                },
                Focus::Panel(idx) => match key.code {
                    KeyCode::Enter if idx == 0 => {
                        if app.cached_offers.is_empty() {
                            // Button mode: trigger island fetch
                            app.calc_error = None;
                            if app.rows.is_empty() {
                                app.calc_error =
                                    Some("Add commodities first".to_owned());
                            } else {
                                app.loading = true;
                                app.fetch_purpose = FetchPurpose::Islands;
                                spawn_island_fetch(&app, &tx);
                            }
                        } else {
                            // Text mode: auto-fill suggestion and advance
                            let suggestion =
                                app.suggest_island().map(|s| s.to_owned());
                            if let Some(island) = suggestion {
                                app.panel[0].value = island;
                                app.panel[0].cursor = app.panel[0].value.len();
                            }
                            app.focus = Focus::Panel(1);
                        }
                    }
                    KeyCode::Up => {
                        if idx > 0 {
                            app.focus = Focus::Panel(idx - 1);
                        }
                    }
                    KeyCode::Down => {
                        if idx + 1 < PANEL_COUNT {
                            app.focus = Focus::Panel(idx + 1);
                        } else {
                            app.focus = Focus::Button;
                        }
                    }
                    KeyCode::Left => {
                        if idx == 0 && app.panel[0].cursor > 0 {
                            app.panel[0].move_left();
                        } else if !app.rows.is_empty() {
                            app.focus = Focus::Table;
                            app.table_state
                                .select(Some(app.rows.len() - 1));
                            app.table_state.select_column(Some(LAST_COL));
                        }
                    }
                    KeyCode::Right if idx == 0 && !app.cached_offers.is_empty() => {
                        app.panel[0].move_right();
                    }
                    KeyCode::Backspace => {
                        if idx == 0 && app.cached_offers.is_empty() {
                            // Button mode: ignore
                        } else {
                            app.panel[idx].delete_char_before();
                            app.calc_error = None;
                        }
                    }
                    KeyCode::Delete => {
                        if idx == 0 && app.cached_offers.is_empty() {
                            // Button mode: ignore
                        } else {
                            app.panel[idx].delete_char_at();
                            app.calc_error = None;
                        }
                    }
                    KeyCode::Home => app.panel[idx].cursor = 0,
                    KeyCode::End => {
                        let len = app.panel[idx].value.len();
                        app.panel[idx].cursor = len;
                    }
                    KeyCode::Char(c) => {
                        if idx == 0 && app.cached_offers.is_empty() {
                            // Button mode: ignore typing
                        } else {
                            app.panel[idx].insert_char(c);
                            app.calc_error = None;
                        }
                    }
                    _ => {}
                },
                Focus::Button => match key.code {
                    KeyCode::Enter => {
                        app.calc_error = None;
                        if app.rows.is_empty() {
                            app.calc_error = Some("Add commodities first".to_owned());
                        } else if !app.panel[0].value.trim().is_empty()
                            && app.suggest_island().is_none()
                        {
                            app.calc_error =
                                Some("Unknown restocking island".to_owned());
                        } else if app.cached_offers.is_empty() {
                            // First time: fetch immediately
                            app.loading = true;
                            app.fetch_purpose = FetchPurpose::Profits;
                            spawn_fetch(&app, &tx);
                        } else {
                            // Has cached data: ask to re-query
                            app.popup = Some(PopupKind::ReQueryConfirm {
                                yes_focused: false,
                            });
                            app.focus = Focus::Popup;
                        }
                    }
                    KeyCode::Up => {
                        app.focus = Focus::Panel(PANEL_COUNT - 1);
                    }
                    KeyCode::Down => {
                        app.focus_input();
                    }
                    KeyCode::Left => {
                        if !app.rows.is_empty() {
                            app.focus = Focus::Table;
                            app.table_state
                                .select(Some(app.rows.len() - 1));
                            app.table_state.select_column(Some(LAST_COL));
                        }
                    }
                    _ => {}
                },
                Focus::Popup => match app.popup {
                    Some(PopupKind::ReQueryConfirm { ref mut yes_focused }) => {
                        match key.code {
                            KeyCode::Left | KeyCode::Right => {
                                *yes_focused = !*yes_focused;
                            }
                            KeyCode::Enter => {
                                if *yes_focused {
                                    // Re-query
                                    app.popup = None;
                                    app.focus = Focus::Button;
                                    app.loading = true;
                                    app.fetch_purpose = FetchPurpose::Profits;
                                    spawn_fetch(&app, &tx);
                                } else {
                                    // Use cached data
                                    app.calculate_or_warn();
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(PopupKind::DeleteConfirm { row_idx, name: _, ref mut yes_focused }) => {
                        match key.code {
                            KeyCode::Left | KeyCode::Right => {
                                *yes_focused = !*yes_focused;
                            }
                            KeyCode::Enter => {
                                if *yes_focused {
                                    app.rows.remove(row_idx);
                                    app.rebuild_island_list();
                                    app.popup = None;
                                    if app.rows.is_empty() {
                                        app.focus_input();
                                    } else {
                                        app.focus = Focus::Table;
                                        let new_sel = if row_idx >= app.rows.len() {
                                            app.rows.len() - 1
                                        } else {
                                            row_idx
                                        };
                                        app.table_state.select(Some(new_sel));
                                        app.table_state.select_column(Some(FIRST_COL));
                                    }
                                } else {
                                    app.popup = None;
                                    app.focus = Focus::Table;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(PopupKind::RestockWarning { missing: _, ref mut ocean_wide_focused }) => {
                        match key.code {
                            KeyCode::Left | KeyCode::Right => {
                                *ocean_wide_focused = !*ocean_wide_focused;
                            }
                            KeyCode::Enter => {
                                if *ocean_wide_focused {
                                    // Ocean-wide: clear island, calculate
                                    app.panel[0].value.clear();
                                    app.panel[0].cursor = 0;
                                    let profit = app.calculate_profits();
                                    app.popup = Some(PopupKind::ProfitResult(profit));
                                } else {
                                    // Change island: go back to island field
                                    app.popup = None;
                                    app.focus = Focus::Panel(0);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(PopupKind::ProfitResult(_)) => {
                        if key.code == KeyCode::Enter {
                            app.popup = None;
                            app.focus = Focus::Input;
                        }
                    }
                    None => {}
                },
            }
        }
    }

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;

    // -- Save inventory --
    if let Some(ref path) = args.inventory {
        let saved = SavedInventory {
            rows: app
                .rows
                .iter()
                .map(|r| SavedInventoryRow {
                    commodity: app.commod_name(r.commod_id).to_owned(),
                    restock: r.restock.clone(),
                    stock: r.stock.clone(),
                    booty: r.booty.clone(),
                })
                .collect(),
            restocking_island: app.panel[0].value.clone(),
            panel: app.panel[1..].iter().map(|f| f.value.clone()).collect(),
        };
        match serde_json::to_string_pretty(&saved) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("error: failed to write inventory to {}: {}", path, e);
                } else {
                    eprintln!("Saved inventory to {}", path);
                }
            }
            Err(e) => eprintln!("error: failed to serialize inventory: {}", e),
        }
    }

    // -- Save market cache --
    if let Some(ref path) = args.market_cache {
        let saved = SavedMarketCache {
            commodities: app
                .commodities
                .iter()
                .map(|c| SavedCommodity {
                    id: c.id,
                    name: c.name.clone(),
                })
                .collect(),
            offers: app.cached_offers,
        };
        match serde_json::to_string_pretty(&saved) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("error: failed to write market cache to {}: {}", path, e);
                } else {
                    eprintln!("Saved market cache to {}", path);
                }
            }
            Err(e) => eprintln!("error: failed to serialize market cache: {}", e),
        }
    }

    Ok(())
}
