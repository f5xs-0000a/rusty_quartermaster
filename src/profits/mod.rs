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
/// Rightmost editable column when prices come from Market (Booty).
pub const LAST_COL: usize = 3;
/// Rightmost editable column when prices are entered manually (Buy Price).
pub const LAST_COL_OFFLINE: usize = 5;

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
    /// Offline calc is blocked: these goods are missing a manually-entered
    /// price. `need_buy` need a Buy Price (they require restocking); `need_sell`
    /// need a Sell Price (they have excess to sell).
    PriceBlock { need_buy: Vec<String>, need_sell: Vec<String> },
    ProfitResult(ProfitResult),
}

pub struct InventoryRow {
    pub commod_id: u64,
    pub restock: String,
    pub stock: String,
    pub booty: String,
    /// Manual prices used when Market is unavailable. `sell` is what you sell
    /// excess goods for; `buy` is what you pay to restock.
    pub sell: String,
    pub buy: String,
}

impl InventoryRow {
    pub fn new(commod_id: u64) -> Self {
        Self {
            commod_id,
            restock: String::new(),
            stock: String::new(),
            booty: String::new(),
            sell: String::new(),
            buy: String::new(),
        }
    }

    pub fn field_mut(&mut self, col: usize) -> Option<&mut String> {
        match col {
            1 => Some(&mut self.restock),
            2 => Some(&mut self.stock),
            3 => Some(&mut self.booty),
            4 => Some(&mut self.sell),
            5 => Some(&mut self.buy),
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
    /// CLI-revealed parameter rows (otherwise hidden from the panel). When
    /// hidden, the corresponding deduction is treated as zero in the breakdown.
    pub show_co_rate: bool,
    pub show_donation: bool,
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
                PromptField::new("PoE in Booty Chest", FieldKind::PositiveInt),
                PromptField::new("C.O. Rate", FieldKind::Rate),
                PromptField::new("Crew Donation Share Rate", FieldKind::Rate),
                PromptField::new("Restocking Rate", FieldKind::Rate),
                PromptField::new("Stocking", FieldKind::PositiveInt),
            ],
            submit_failed: None,
            popup: None,
            calc_error: None,
            fetch_purpose: FetchPurpose::Profits,
            show_co_rate: false,
            show_donation: false,
        }
    }

    // -- visible parameter rows --

    /// Panel indices that are actually shown (and thus navigable), in order.
    /// The Restocking Island row only matters with Market pricing; the C.O.
    /// Rate and Crew Donation rows are revealed by CLI flags.
    pub fn visible_panels(&self, market_supported: bool) -> Vec<usize> {
        (0..PANEL_COUNT)
            .filter(|&i| match i {
                0 => market_supported,
                2 => self.show_co_rate,
                3 => self.show_donation,
                _ => true,
            })
            .collect()
    }

    fn first_visible_panel(&self, market: bool) -> Option<usize> {
        self.visible_panels(market).first().copied()
    }

    fn last_visible_panel(&self, market: bool) -> Option<usize> {
        self.visible_panels(market).last().copied()
    }

    /// Step to the previous/next visible panel from `idx`, or `None` at the end.
    fn step_visible_panel(&self, idx: usize, forward: bool, market: bool) -> Option<usize> {
        let vis = self.visible_panels(market);
        let pos = vis.iter().position(|&i| i == idx)?;
        if forward {
            vis.get(pos + 1).copied()
        } else {
            pos.checked_sub(1).map(|p| vis[p])
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
                    // Keep rows in canonical (in-game) commodity order.
                    let key = |cid: u64| {
                        crate::commodities::sort_key(app::commod_name(commodities, cid))
                    };
                    let new_key = key(id);
                    let pos = self.rows.partition_point(|r| key(r.commod_id) < new_key);
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

    pub fn focus_table_top(&mut self) {
        if self.rows.is_empty() {
            self.focus_input();
            return;
        }
        self.focus = Focus::Table;
        self.table_state.select(Some(0));
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

    /// The rightmost editable column: Booty when prices come from Market, or
    /// Buy Price when prices are entered manually (offline).
    pub fn last_editable_col(&self, market_supported: bool) -> usize {
        if market_supported {
            LAST_COL
        } else {
            LAST_COL_OFFLINE
        }
    }

    pub fn table_right(&mut self, last_col: usize) {
        if let Some(col) = self.table_state.selected_column() {
            if col < last_col {
                self.table_state.select_column(Some(col + 1));
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

    /// Offline price check: every row that needs to buy (restock) must have a
    /// Buy Price, and every row with excess to sell must have a Sell Price.
    /// Returns the goods missing each, so the calc can be blocked until filled.
    pub fn missing_prices(&self, shared: &SharedState) -> (Vec<String>, Vec<String>) {
        let mut need_buy = Vec::new();
        let mut need_sell = Vec::new();
        for row in &self.rows {
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);
            let name = app::commod_name(shared.commodities, row.commod_id);

            if restock > stock + booty {
                if row.buy.trim().is_empty() {
                    need_buy.push(name.to_owned());
                }
            } else if stock + booty > restock && row.sell.trim().is_empty() {
                need_sell.push(name.to_owned());
            }
        }
        (need_buy, need_sell)
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
            let restock = row.restock.parse::<u64>().unwrap_or(0);
            let stock = row.stock.parse::<u64>().unwrap_or(0);
            let booty = row.booty.parse::<u64>().unwrap_or(0);

            if !shared.market_supported {
                // Offline: value the excess/shortfall at the manually-entered
                // Sell/Buy prices instead of Market offers.
                if restock < booty + stock {
                    let sell_price = row.sell.parse::<u64>().unwrap_or(0);
                    goods_value += (booty + stock - restock) * sell_price;
                } else {
                    let buy_price = row.buy.parse::<u64>().unwrap_or(0);
                    restock_value += (restock - stock - booty) * buy_price;
                }
                continue;
            }

            let name = app::commod_name(shared.commodities, row.commod_id);
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
        // A hidden parameter row means that deduction doesn't apply.
        let co_rate = if self.show_co_rate { parse_rate(&self.panel[2]) } else { 0.0 };
        let donation_rate = if self.show_donation { parse_rate(&self.panel[3]) } else { 0.0 };
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
            Some(PopupKind::PriceBlock { .. }) => self.dismiss_ok_popup(),
            _ => {
                self.popup = None;
                self.focus = Focus::Input;
            }
        }
        InputResult::Consumed
    }

    /// Dismiss an informational "Ok" popup (PriceBlock / ProfitResult). After a
    /// PriceBlock the user needs to fill prices, so land back in the inventory
    /// table; otherwise return to the search box.
    pub fn dismiss_ok_popup(&mut self) {
        let to_table =
            matches!(self.popup, Some(PopupKind::PriceBlock { .. })) && !self.rows.is_empty();
        self.popup = None;
        if to_table {
            self.focus_table_top();
        } else {
            self.focus_input();
        }
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
            KeyCode::Up => {
                // The inventory table sits directly above the search box; ↑ from
                // here climbs into it (or to the top bar when empty).
                if self.rows.is_empty() {
                    return InputResult::Exit;
                }
                self.focus_table_bottom();
            }
            KeyCode::Down => {
                if let Some(i) = self.first_visible_panel(shared.market_supported) {
                    self.focus_panel(i);
                }
            }
            KeyCode::Char(c) => self.input_insert_char(c),
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_table_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match key.code {
            KeyCode::Up => {
                // The inventory table is the topmost widget; ↑ from the first row
                // returns to the top bar.
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
            KeyCode::Right => {
                let last = self.last_editable_col(shared.market_supported);
                self.table_right(last);
            }
            KeyCode::Char(d) if d.is_ascii_digit() => self.table_insert_digit(d),
            KeyCode::Backspace => self.table_delete_digit(),
            KeyCode::Delete => {
                if let Some(row) = self.table_state.selected() {
                    let name =
                        app::commod_name(shared.commodities, self.rows[row].commod_id).to_owned();
                    self.popup = Some(PopupKind::DeleteConfirm {
                        row_idx: row,
                        name,
                        // Default to Yes so a quick Enter confirms the delete.
                        yes_focused: true,
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
                match self.step_visible_panel(idx, false, shared.market_supported) {
                    Some(prev) => self.focus = Focus::Panel(prev),
                    // Above the first parameter sits the Search box.
                    None => self.focus_input(),
                }
            }
            KeyCode::Down => {
                match self.step_visible_panel(idx, true, shared.market_supported) {
                    Some(next) => self.focus = Focus::Panel(next),
                    None => self.focus = Focus::Button,
                }
            }
            KeyCode::Left => {
                // The island field is a button until the market is queried; only
                // move the text cursor once it accepts input.
                if !(idx == 0 && shared.cached_offers.is_empty()) {
                    self.panel[idx].move_left();
                }
            }
            KeyCode::Right => {
                if !(idx == 0 && shared.cached_offers.is_empty()) {
                    self.panel[idx].move_right();
                }
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
        if self.rows.is_empty() {
            self.calc_error = Some("Add commodities first".to_owned());
            return InputResult::Consumed;
        }

        if !shared.market_supported {
            // Offline: prices come from the manually-entered Buy/Sell columns.
            // Block the calc if any required price is missing.
            let (need_buy, need_sell) = self.missing_prices(shared);
            if !need_buy.is_empty() || !need_sell.is_empty() {
                self.popup = Some(PopupKind::PriceBlock { need_buy, need_sell });
                self.focus = Focus::Popup;
                return InputResult::Consumed;
            }
            // Everything priced — compute directly (no fetch / re-query).
            let profit = self.calculate_profits(shared);
            self.popup = Some(PopupKind::ProfitResult(profit));
            self.focus = Focus::Popup;
            return InputResult::Consumed;
        }

        if !self.panel[0].value.trim().is_empty()
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
            // PriceBlock has a single "Ok" button handled via ProfitsPopupOk.
            Some(PopupKind::PriceBlock { .. }) => {}
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
                if let Some(i) = self.last_visible_panel(shared.market_supported) {
                    self.focus = Focus::Panel(i);
                }
            }
            // The button is the bottom of the focus chain; ↓ goes nowhere.
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
            Some(PopupKind::ProfitResult(_)) | Some(PopupKind::PriceBlock { .. }) => {
                if key.code == KeyCode::Enter {
                    self.dismiss_ok_popup();
                }
            }
            None => {}
        }
        InputResult::Consumed
    }
}
