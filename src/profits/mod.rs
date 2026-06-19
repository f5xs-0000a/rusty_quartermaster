pub mod persistence;
pub mod ui;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::widgets::TableState;

use crate::aliases;
use crate::api::Commodity;
use crate::app::{self, FetchPurpose, InputResult, SharedState};
use crate::utils::{text_similarity, parse_rate, FieldKind, PromptField};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub const PANEL_COUNT: usize = 6;
pub const FIRST_COL: usize = 1;
pub const LAST_COL: usize = 3;

pub struct ProfitResult {
    pub goods_value: u64,
    pub restock_value: u64,
    pub co_cut: u64,
    pub crew_donation: u64,
    pub subtotal: u64,
    pub stocking: u64,
    pub add_to_booty: u64,
}

pub enum PopupKind {
    ReQueryConfirm { yes_focused: bool },
    DeleteConfirm { row_idx: usize, name: String, yes_focused: bool },
    RestockWarning { missing: Vec<String>, ocean_wide_focused: bool },
    ProfitResult(ProfitResult),
}

pub struct InventoryRow {
    pub commod_id: u64,
    pub restock: String,
    pub stock: String,
    pub booty: String,
}

impl InventoryRow {
    pub fn new(commod_id: u64) -> Self {
        Self {
            commod_id,
            restock: String::new(),
            stock: String::new(),
            booty: String::new(),
        }
    }

    pub fn field_mut(&mut self, col: usize) -> Option<&mut String> {
        match col {
            1 => Some(&mut self.restock),
            2 => Some(&mut self.stock),
            3 => Some(&mut self.booty),
            _ => None,
        }
    }
}

#[derive(PartialEq)]
pub enum Focus {
    Input,
    Table,
    Panel(usize),
    Button,
    Popup,
}

// ---------------------------------------------------------------------------
// ProfitsApp
// ---------------------------------------------------------------------------

pub struct ProfitsApp {
    pub rows: Vec<InventoryRow>,
    pub input: String,
    pub cursor: usize,
    pub focus: Focus,
    pub table_state: TableState,
    pub panel: [PromptField; PANEL_COUNT],
    pub submit_failed: Option<String>,
    pub popup: Option<PopupKind>,
    pub calc_error: Option<String>,
    pub fetch_purpose: FetchPurpose,
}

impl ProfitsApp {
    pub fn new() -> Self {
        Self {
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
            calc_error: None,
            fetch_purpose: FetchPurpose::Profits,
        }
    }

    // -- commodity suggestion & submit --

    pub fn suggest(&self, commodities: &[Commodity]) -> Option<u64> {
        let query = self.input.trim().to_lowercase();
        if query.is_empty() {
            return None;
        }

        // Alias lookup
        if let Some(&target) = aliases::get().get(query.as_str()) {
            if let Some(c) = commodities
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(target))
            {
                return Some(c.id);
            }
        }

        // Exact match
        if let Some(c) = commodities
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&query))
        {
            return Some(c.id);
        }

        // Unique prefix
        let prefix_matches: Vec<_> = commodities
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&query))
            .collect();
        if prefix_matches.len() == 1 {
            return Some(prefix_matches[0].id);
        }

        // Jaro-Winkler (minimum 0.75, unique winner)
        let mut best_score = f64::NEG_INFINITY;
        let mut best_id = None;
        let mut tie = false;
        for c in commodities {
            let score = text_similarity(&query, &c.name.to_lowercase());
            if best_score < score {
                best_score = score;
                best_id = Some(c.id);
                tie = false;
            } else if score == best_score {
                tie = true;
            }
        }
        if 0.75 <= best_score && !tie {
            return best_id;
        }

        None
    }

    /// Returns true if the island list should be rebuilt (a new row was added).
    pub fn submit(&mut self, commodities: &[Commodity]) -> bool {
        let query = self.input.trim().to_lowercase();
        if query.is_empty() {
            self.input.clear();
            self.cursor = 0;
            return false;
        }

        let mut changed = false;
        match self.suggest(commodities) {
            Some(id) => {
                if !self.rows.iter().any(|r| r.commod_id == id) {
                    let pos = self
                        .rows
                        .binary_search_by_key(&id, |r| r.commod_id)
                        .unwrap_err();
                    self.rows.insert(pos, InventoryRow::new(id));
                    changed = true;
                }
                self.submit_failed = None;
            }
            None => {
                self.submit_failed = Some(self.input.trim().to_owned());
            }
        }

        self.input.clear();
        self.cursor = 0;
        changed
    }

    // -- focus transitions --

    pub fn focus_table_bottom(&mut self) {
        if self.rows.is_empty() {
            self.focus = Focus::Button;
            return;
        }
        self.focus = Focus::Table;
        self.table_state.select(Some(self.rows.len() - 1));
        self.table_state.select_column(Some(FIRST_COL));
    }

    pub fn focus_input(&mut self) {
        self.focus = Focus::Input;
        self.table_state.select(None);
        self.table_state.select_column(None);
    }

    pub fn focus_panel(&mut self, idx: usize) {
        self.focus = Focus::Panel(idx);
        self.table_state.select(None);
        self.table_state.select_column(None);
    }

    // -- input manipulation --

    pub fn input_insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.submit_failed = None;
        self.calc_error = None;
    }

    pub fn input_delete_char_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
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

    pub fn input_delete_char_at(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
            self.submit_failed = None;
            self.calc_error = None;
        }
    }

    pub fn input_move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn input_move_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += self.input[self.cursor..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }

    // -- table helpers --

    pub fn selected_cell(&self) -> Option<(usize, usize)> {
        Some((
            self.table_state.selected()?,
            self.table_state.selected_column()?,
        ))
    }

    pub fn table_up(&mut self) {
        if let Some(row) = self.table_state.selected() {
            if 0 < row {
                self.table_state.select(Some(row - 1));
            }
        }
    }

    pub fn table_down(&mut self) {
        if let Some(row) = self.table_state.selected() {
            if row + 1 < self.rows.len() {
                self.table_state.select(Some(row + 1));
            } else {
                self.focus_input();
            }
        }
    }

    pub fn table_right(&mut self) {
        if let Some(col) = self.table_state.selected_column() {
            if col < LAST_COL {
                self.table_state.select_column(Some(col + 1));
            } else {
                self.focus_panel(0);
            }
        }
    }

    pub fn table_insert_digit(&mut self, d: char) {
        if let Some((row, col)) = self.selected_cell() {
            if let Some(field) = self.rows[row].field_mut(col) {
                field.push(d);
            }
        }
    }

    pub fn table_delete_digit(&mut self) {
        if let Some((row, col)) = self.selected_cell() {
            if let Some(field) = self.rows[row].field_mut(col) {
                field.pop();
            }
        }
    }

    // -- calculation --

    pub fn missing_restock_on_island(&self, island: &str, shared: &SharedState) -> Vec<String> {
        let mut missing = Vec::new();
        for row in &self.rows {
            let name = app::commod_name(shared.commodities, row.commod_id);
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);

            if restock <= stock + booty {
                continue;
            }

            let has_supply = shared
                .cached_offers
                .get(name)
                .is_some_and(|cached| {
                    cached.offers.iter().any(|o| {
                        0 < o.sellprice
                            && 0 < o.sellqty
                            && o.islandname.eq_ignore_ascii_case(island)
                    })
                });
            if !has_supply {
                missing.push(name.to_owned());
            }
        }
        missing
    }

    pub fn calculate_or_warn(&mut self, shared: &SharedState) {
        let query = self.panel[0].value.trim();
        if let Some(island) = app::suggest_island(query, shared.available_islands)
            .map(|s| s.to_owned())
        {
            let missing = self.missing_restock_on_island(&island, shared);
            if !missing.is_empty() {
                self.popup = Some(PopupKind::RestockWarning {
                    missing,
                    ocean_wide_focused: false,
                });
                self.focus = Focus::Popup;
                return;
            }
        }
        let profit = self.calculate_profits(shared);
        self.popup = Some(PopupKind::ProfitResult(profit));
        self.focus = Focus::Popup;
    }

    pub fn calculate_profits(&self, shared: &SharedState) -> ProfitResult {
        let query = self.panel[0].value.trim();
        let restock_island = if query.is_empty() {
            None
        } else {
            app::suggest_island(query, shared.available_islands).map(|s| s.to_owned())
        };

        let mut goods_value: u64 = 0;
        let mut restock_value: u64 = 0;

        for row in &self.rows {
            let name = app::commod_name(shared.commodities, row.commod_id);
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);

            let Some(cached) = shared.cached_offers.get(name) else {
                continue;
            };
            let offers = &cached.offers;

            if restock < booty + stock {
                // Goods Value: sell excess at best buy prices (ocean-wide)
                let mut buy_offers: Vec<_> = offers
                    .iter()
                    .filter(|o| 0 < o.buyprice && 0 < o.buyqty)
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
            } else {
                // Need to buy: find cheapest sell offers (island-filtered)
                let need = restock - stock - booty;
                let mut sell_offers: Vec<_> = offers
                    .iter()
                    .filter(|o| 0 < o.sellprice && 0 < o.sellqty)
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
        let co_rate = parse_rate(&self.panel[2]);
        let donation_rate = parse_rate(&self.panel[3]);
        let restocking_rate = parse_rate(&self.panel[4]);

        // Back-compute total money reward: booty_money = M * (1-R) / 2
        let total_money = if restocking_rate < 1.0 {
            2.0 * booty_money / (1.0 - restocking_rate)
        } else {
            0.0
        };
        let ship_hold = total_money * restocking_rate;

        let restock_overflow = (restock_value as f64 - ship_hold).max(0.0);
        let goods_to_booty = (goods_value as f64 - restock_overflow).max(0.0);

        let total_earnings = (total_money + goods_value as f64 - restock_value as f64).max(0.0);

        let co_cut = (total_earnings * co_rate).ceil() as u64;
        let crew_donation = (total_earnings * donation_rate).floor() as u64;

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

    // -- key handling --

    pub fn handle_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        if key.code == KeyCode::Esc {
            return self.handle_esc(shared);
        }

        match self.focus {
            Focus::Input => self.handle_input_key(key, shared),
            Focus::Table => self.handle_table_key(key, shared),
            Focus::Panel(idx) => self.handle_panel_key(key, idx, shared),
            Focus::Button => self.handle_button_key(key, shared),
            Focus::Popup => self.handle_popup_key(key, shared),
        }
    }

    fn handle_esc(&mut self, shared: &SharedState) -> InputResult {
        if self.popup.is_none() {
            return InputResult::Exit;
        }

        match self.popup {
            Some(PopupKind::ReQueryConfirm { .. }) => {
                self.calculate_or_warn(shared);
            }
            Some(PopupKind::DeleteConfirm { .. }) => {
                self.popup = None;
                self.focus = Focus::Table;
            }
            Some(PopupKind::RestockWarning { .. }) => {
                self.popup = None;
                self.focus = Focus::Panel(0);
            }
            _ => {
                self.popup = None;
                self.focus = Focus::Input;
            }
        }
        InputResult::Consumed
    }

    fn handle_input_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match key.code {
            KeyCode::Enter => {
                if self.submit(shared.commodities) {
                    return InputResult::RebuildIslands;
                }
            }
            KeyCode::Backspace => self.input_delete_char_before(),
            KeyCode::Delete => self.input_delete_char_at(),
            KeyCode::Left => self.input_move_left(),
            KeyCode::Right => self.input_move_right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Up => self.focus_table_bottom(),
            KeyCode::Char(c) => self.input_insert_char(c),
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_table_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match key.code {
            KeyCode::Up => {
                // The table is the page's top widget; ↑ from its first row hands
                // focus back to the top bar.
                if self.table_state.selected() == Some(0) {
                    return InputResult::Exit;
                }
                self.table_up();
            }
            KeyCode::Down => self.table_down(),
            KeyCode::Left => {
                if let Some(col) = self.table_state.selected_column() {
                    if FIRST_COL < col {
                        self.table_state.select_column(Some(col - 1));
                    }
                }
            }
            KeyCode::Right => self.table_right(),
            KeyCode::Char(d) if d.is_ascii_digit() => self.table_insert_digit(d),
            KeyCode::Backspace => self.table_delete_digit(),
            KeyCode::Delete => {
                if let Some(row) = self.table_state.selected() {
                    let name =
                        app::commod_name(shared.commodities, self.rows[row].commod_id).to_owned();
                    self.popup = Some(PopupKind::DeleteConfirm {
                        row_idx: row,
                        name,
                        yes_focused: false,
                    });
                    self.focus = Focus::Popup;
                }
            }
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_panel_key(
        &mut self,
        key: KeyEvent,
        idx: usize,
        shared: &SharedState,
    ) -> InputResult {
        match key.code {
            KeyCode::Enter if idx == 0 => {
                if shared.cached_offers.is_empty() {
                    self.calc_error = None;
                    if self.rows.is_empty() {
                        self.calc_error = Some("Add commodities first".to_owned());
                    } else {
                        self.fetch_purpose = FetchPurpose::Islands;
                        return InputResult::StartFetch(FetchPurpose::Islands);
                    }
                } else {
                    let query = self.panel[0].value.trim();
                    if let Some(island) =
                        app::suggest_island(query, shared.available_islands).map(|s| s.to_owned())
                    {
                        self.panel[0].value = island;
                        self.panel[0].cursor = self.panel[0].value.len();
                    }
                    self.focus = Focus::Panel(1);
                }
            }
            KeyCode::Up => {
                if 0 < idx {
                    self.focus = Focus::Panel(idx - 1);
                } else {
                    // Top of the panel column — return focus to the top bar.
                    return InputResult::Exit;
                }
            }
            KeyCode::Down => {
                if idx + 1 < PANEL_COUNT {
                    self.focus = Focus::Panel(idx + 1);
                } else {
                    self.focus = Focus::Button;
                }
            }
            KeyCode::Left => {
                if idx == 0 && 0 < self.panel[0].cursor {
                    self.panel[0].move_left();
                } else if !self.rows.is_empty() {
                    self.focus = Focus::Table;
                    self.table_state.select(Some(self.rows.len() - 1));
                    self.table_state.select_column(Some(LAST_COL));
                }
            }
            KeyCode::Right if idx == 0 && !shared.cached_offers.is_empty() => {
                self.panel[0].move_right();
            }
            KeyCode::Backspace => {
                if !(idx == 0 && shared.cached_offers.is_empty()) {
                    self.panel[idx].delete_char_before();
                    self.calc_error = None;
                }
            }
            KeyCode::Delete => {
                if !(idx == 0 && shared.cached_offers.is_empty()) {
                    self.panel[idx].delete_char_at();
                    self.calc_error = None;
                }
            }
            KeyCode::Home => self.panel[idx].cursor = 0,
            KeyCode::End => {
                let len = self.panel[idx].value.len();
                self.panel[idx].cursor = len;
            }
            KeyCode::Char(c) => {
                if !(idx == 0 && shared.cached_offers.is_empty()) {
                    self.panel[idx].insert_char(c);
                    self.calc_error = None;
                }
            }
            _ => {}
        }
        InputResult::Consumed
    }

    pub fn handle_button_activate(&mut self, shared: &SharedState) -> InputResult {
        self.calc_error = None;
        if !shared.market_supported {
            self.calc_error =
                Some("Profit calc needs a Market ocean (Emerald, Meridian, or Cerulean).".to_owned());
        } else if self.rows.is_empty() {
            self.calc_error = Some("Add commodities first".to_owned());
        } else if !self.panel[0].value.trim().is_empty()
            && app::suggest_island(self.panel[0].value.trim(), shared.available_islands)
                .is_none()
        {
            self.calc_error = Some("Unknown restocking island".to_owned());
        } else if shared.cached_offers.is_empty() {
            self.fetch_purpose = FetchPurpose::Profits;
            return InputResult::StartFetch(FetchPurpose::Profits);
        } else {
            self.popup = Some(PopupKind::ReQueryConfirm { yes_focused: false });
            self.focus = Focus::Popup;
        }
        InputResult::Consumed
    }

    /// Handle a mouse click on a popup button. `yes_side` is true for
    /// Yes / Ocean-wide (the right button), false for No / Change Island (left).
    pub fn handle_popup_click(&mut self, yes_side: bool, shared: &SharedState) -> InputResult {
        match self.popup {
            Some(PopupKind::ReQueryConfirm { .. }) => {
                if yes_side {
                    self.popup = None;
                    self.focus = Focus::Button;
                    self.fetch_purpose = FetchPurpose::Profits;
                    return InputResult::StartFetch(FetchPurpose::Profits);
                }
                self.calculate_or_warn(shared);
            }
            Some(PopupKind::DeleteConfirm { row_idx, .. }) => {
                if yes_side {
                    self.rows.remove(row_idx);
                    self.popup = None;
                    if self.rows.is_empty() {
                        self.focus_input();
                    } else {
                        self.focus = Focus::Table;
                        let new_sel = if row_idx < self.rows.len() {
                            row_idx
                        } else {
                            self.rows.len() - 1
                        };
                        self.table_state.select(Some(new_sel));
                        self.table_state.select_column(Some(FIRST_COL));
                    }
                    return InputResult::RebuildIslands;
                }
                self.popup = None;
                self.focus = Focus::Table;
            }
            Some(PopupKind::RestockWarning { .. }) => {
                if yes_side {
                    self.panel[0].value.clear();
                    self.panel[0].cursor = 0;
                    let profit = self.calculate_profits(shared);
                    self.popup = Some(PopupKind::ProfitResult(profit));
                } else {
                    self.popup = None;
                    self.focus = Focus::Panel(0);
                }
            }
            Some(PopupKind::ProfitResult(_)) => {
                self.popup = None;
                self.focus = Focus::Input;
            }
            None => {}
        }
        InputResult::Consumed
    }

    fn handle_button_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match key.code {
            KeyCode::Enter => {
                return self.handle_button_activate(shared);
            }
            KeyCode::Up => {
                self.focus = Focus::Panel(PANEL_COUNT - 1);
            }
            KeyCode::Down => {
                self.focus_input();
            }
            KeyCode::Left if !self.rows.is_empty() => {
                self.focus = Focus::Table;
                self.table_state.select(Some(self.rows.len() - 1));
                self.table_state.select_column(Some(LAST_COL));
            }
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_popup_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match self.popup {
            Some(PopupKind::ReQueryConfirm {
                ref mut yes_focused,
            }) => match key.code {
                KeyCode::Left | KeyCode::Right => *yes_focused = !*yes_focused,
                KeyCode::Enter => {
                    if *yes_focused {
                        self.popup = None;
                        self.focus = Focus::Button;
                        self.fetch_purpose = FetchPurpose::Profits;
                        return InputResult::StartFetch(FetchPurpose::Profits);
                    }
                    self.calculate_or_warn(shared);
                }
                _ => {}
            },
            Some(PopupKind::DeleteConfirm {
                row_idx,
                name: _,
                ref mut yes_focused,
            }) => match key.code {
                KeyCode::Left | KeyCode::Right => *yes_focused = !*yes_focused,
                KeyCode::Enter => {
                    if *yes_focused {
                        self.rows.remove(row_idx);
                        self.popup = None;
                        if self.rows.is_empty() {
                            self.focus_input();
                        } else {
                            self.focus = Focus::Table;
                            let new_sel = if row_idx < self.rows.len() {
                                row_idx
                            } else {
                                self.rows.len() - 1
                            };
                            self.table_state.select(Some(new_sel));
                            self.table_state.select_column(Some(FIRST_COL));
                        }
                        return InputResult::RebuildIslands;
                    }
                    self.popup = None;
                    self.focus = Focus::Table;
                }
                _ => {}
            },
            Some(PopupKind::RestockWarning {
                missing: _,
                ref mut ocean_wide_focused,
            }) => match key.code {
                KeyCode::Left | KeyCode::Right => *ocean_wide_focused = !*ocean_wide_focused,
                KeyCode::Enter => {
                    if *ocean_wide_focused {
                        self.panel[0].value.clear();
                        self.panel[0].cursor = 0;
                        let profit = self.calculate_profits(shared);
                        self.popup = Some(PopupKind::ProfitResult(profit));
                    } else {
                        self.popup = None;
                        self.focus = Focus::Panel(0);
                    }
                }
                _ => {}
            },
            Some(PopupKind::ProfitResult(_)) => {
                if key.code == KeyCode::Enter {
                    self.popup = None;
                    self.focus = Focus::Input;
                }
            }
            None => {}
        }
        InputResult::Consumed
    }
}
