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

pub const PANEL_COUNT: usize = 7;
pub const FIRST_COL: usize = 1;

// Parameter-panel field indices, in display order. The first two are
// market-location "place" fields — where we buy the restock shortfall and
// where we sell the surplus; the rest are the chest / rate / stocking inputs.
pub(crate) const P_RESTOCK_PLACE: usize = 0;
pub(crate) const P_SELL_PLACE: usize = 1;
pub(crate) const P_BOOTY_CHEST: usize = 2;
pub(crate) const P_CO_RATE: usize = 3;
pub(crate) const P_DONATION: usize = 4;
pub(crate) const P_RESTOCK_RATE: usize = 5;
pub(crate) const P_STOCKING: usize = 6;

/// Whether a panel index is a market-location field (restocking / selling),
/// which is entered as free text and locked until the market has been queried.
pub(crate) const fn is_place_field(idx: usize) -> bool {
    idx == P_RESTOCK_PLACE || idx == P_SELL_PLACE
}
/// Rightmost editable column when prices come from Market (Booty).
pub const LAST_COL: usize = 3;
/// Rightmost editable column when prices are entered manually (Buy Price).
pub const LAST_COL_OFFLINE: usize = 5;

/// The player's crew rank. Governs the player's slice of a booty division
/// together with the crew's [`BootyShare`] scheme. Ordered high → low as the
/// game lists them. See <https://yppedia.puzzlepirates.com/Officer>.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    Captain,
    SeniorOfficer,
    FleetOfficer,
    Officer,
    Pirate,
    CabinPerson,
    JobbingPirate,
}

impl Rank {
    /// Every rank, in display order (highest first) — the order the selection
    /// popup lists them.
    pub const ALL: [Rank; 7] = [
        Rank::Captain,
        Rank::SeniorOfficer,
        Rank::FleetOfficer,
        Rank::Officer,
        Rank::Pirate,
        Rank::CabinPerson,
        Rank::JobbingPirate,
    ];

    /// Human-readable name; also the persisted key.
    pub fn label(self) -> &'static str {
        match self {
            Rank::Captain => "Captain",
            Rank::SeniorOfficer => "Senior officer",
            Rank::FleetOfficer => "Fleet officer",
            Rank::Officer => "Officer",
            Rank::Pirate => "Pirate",
            Rank::CabinPerson => "Cabin person",
            Rank::JobbingPirate => "Jobbing pirate",
        }
    }

    /// Column index into a [`BootyShare`] share table (0 = jobbing pirate …
    /// 6 = captain), matching the booty-share page's rank ordering.
    // Consumed by [`BootyShare::share_for`] once the divvy calc lands.
    #[allow(dead_code)]
    fn share_index(self) -> usize {
        match self {
            Rank::JobbingPirate => 0,
            Rank::CabinPerson => 1,
            Rank::Pirate => 2,
            Rank::Officer => 3,
            Rank::FleetOfficer => 4,
            Rank::SeniorOfficer => 5,
            Rank::Captain => 6,
        }
    }

    /// Parse a persisted [`label`](Self::label); `None` if unrecognised.
    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.label().eq_ignore_ascii_case(s))
    }
}

/// The crew's booty-share scheme — how the divvy is split across ranks. Each
/// scheme carries the per-rank share counts in booty-share-table column order
/// (jobbing pirate … captain). See
/// <https://yppedia.puzzlepirates.com/Booty_share>.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BootyShare {
    Even,
    RanksPrivilege,
    JobbersDelight,
    CrewLoyalty,
    PromotionPays,
    OfficerClub,
    JobbersBane,
    TraderShares,
    CruelShelf,
}

impl BootyShare {
    /// Every scheme, in the order the game (and the selection popup) lists them.
    pub const ALL: [BootyShare; 9] = [
        BootyShare::Even,
        BootyShare::RanksPrivilege,
        BootyShare::JobbersDelight,
        BootyShare::CrewLoyalty,
        BootyShare::PromotionPays,
        BootyShare::OfficerClub,
        BootyShare::JobbersBane,
        BootyShare::TraderShares,
        BootyShare::CruelShelf,
    ];

    /// Human-readable name; also the persisted key.
    pub fn label(self) -> &'static str {
        match self {
            BootyShare::Even => "Even",
            BootyShare::RanksPrivilege => "Rank's Privilege",
            BootyShare::JobbersDelight => "Jobber's Delight",
            BootyShare::CrewLoyalty => "Crew Loyalty",
            BootyShare::PromotionPays => "Promotion Pays",
            BootyShare::OfficerClub => "Officer Club",
            BootyShare::JobbersBane => "Jobber's Bane",
            BootyShare::TraderShares => "Trader Shares",
            BootyShare::CruelShelf => "The Cruel Shelf",
        }
    }

    /// Per-rank share counts, indexed by [`Rank::share_index`] (jobbing pirate,
    /// cabin person, pirate, officer, fleet officer, senior officer, captain).
    /// Kept for the eventual divvy calc even though the popup doesn't show them.
    #[allow(dead_code)]
    pub fn shares(self) -> [u32; 7] {
        match self {
            BootyShare::Even => [1, 1, 1, 1, 1, 1, 1],
            BootyShare::RanksPrivilege => [3, 2, 3, 4, 4, 4, 4],
            BootyShare::JobbersDelight => [5, 3, 4, 4, 4, 4, 4],
            BootyShare::CrewLoyalty => [4, 4, 5, 5, 5, 5, 5],
            BootyShare::PromotionPays => [5, 6, 7, 8, 8, 9, 10],
            BootyShare::OfficerClub => [7, 5, 7, 8, 8, 9, 10],
            BootyShare::JobbersBane => [1, 1, 2, 2, 2, 2, 2],
            BootyShare::TraderShares => [4, 4, 5, 2, 2, 2, 2],
            BootyShare::CruelShelf => [5, 10, 12, 15, 20, 20, 20],
        }
    }

    /// This scheme's share count for a single rank.
    #[allow(dead_code)]
    pub fn share_for(self, rank: Rank) -> u32 {
        self.shares()[rank.share_index()]
    }

    /// Parse a persisted [`label`](Self::label); `None` if unrecognised.
    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.label().eq_ignore_ascii_case(s))
    }
}

/// Booty-chest figures deduced from the battle ledger and the restocking rate,
/// kept as floats so halving and theft subtraction don't round prematurely.
struct ChestBreakdown {
    /// Skimmed off the top for restocking (`gross * restocking_rate`).
    reserve: f64,
    /// The retained half of the post-reserve plunder, before theft.
    chest_gross: f64,
    /// The other half, paid out as cuts during the run.
    immediate_cuts: f64,
    /// `chest_gross` minus what enemies stole, floored at zero.
    chest_net: f64,
}

pub struct ProfitResult {
    /// Gross PoE we plundered this pillage (ledger sum of won battles).
    pub gross_plundered: u64,
    /// Skimmed off the top for restocking (`gross * restocking_rate`).
    pub restock_reserve: u64,
    /// The crew's immediate cuts, paid out during the run (the post-reserve half).
    pub immediate_cuts: u64,
    /// The other half, retained in the booty chest (gross, before theft).
    pub chest_gross: u64,
    /// PoE enemies stole from us on lost boardings (ledger sum of losses).
    pub stolen: u64,
    /// Net booty chest actually used: deduced (`chest_gross - stolen`) unless the
    /// user overrode the Booty Chest field.
    pub chest_net: u64,
    /// PoE from selling excess goods at market.
    pub goods_value: u64,
    /// PoE needed to buy the restock shortfall at market.
    pub restock_value: u64,
    /// Restock cost beyond the reserve — funded from goods first, then the
    /// officer's pocket on a loss (`max(restock_value − reserve, 0)`).
    pub add_to_restocking: u64,
    pub stocking: u64,
    /// The whole pillage's net profit and the authoritative base for the cuts.
    /// Can be negative on a loss.
    pub total_gained: i64,
    /// Realized C.O. cut — `total_gained × rate`, capped by the goods cash on hand
    /// (proportionally with the crew donation).
    pub co_cut: u64,
    /// Realized crew donation — same basis and cap as the C.O. cut.
    pub crew_donation: u64,
    /// Leftover goods cash dropped into the booty chest for the share-divvy (≥0).
    pub add_to_booty: u64,
}

/// One line of the Profit Breakdown popup: a label, its signed value, and a
/// one-line explanation shown as the row's tooltip.
pub struct BreakdownRow {
    pub label: &'static str,
    pub desc: &'static str,
    /// The displayed amount. Only "Total gained in pillage" can go negative (a
    /// loss); every other row is a non-negative magnitude.
    pub value: i64,
    /// Insert a blank separator line after this row (section break).
    pub gap_after: bool,
    /// The bottom-line result row (rendered emphasized).
    pub headline: bool,
}

impl ProfitResult {
    /// The breakdown rows, top to bottom — the single source of truth shared by
    /// the popup renderer and the tooltip cursor. The C.O. cut and crew donation
    /// rows are shown only when their CLI flags are enabled.
    pub fn breakdown(&self, show_co: bool, show_donation: bool) -> Vec<BreakdownRow> {
        let row = |label: &'static str, desc: &'static str, value: i64| BreakdownRow {
            label,
            desc,
            value,
            gap_after: false,
            headline: false,
        };
        let mut rows = vec![
            row(
                "Gross PoE Plundered",
                "Total PoE we won across the pillage (battle ledger).",
                self.gross_plundered as i64,
            ),
            row(
                "Reserved for Restock",
                "Skimmed from the crew's cut for restocking.",
                self.restock_reserve as i64,
            ),
            row(
                "Paid to Jobbers",
                "The crew's immediate cuts, paid out during the run.",
                self.immediate_cuts as i64,
            ),
            row(
                "Booty Chest (gross)",
                "Half of each won fight, kept in the chest before theft.",
                self.chest_gross as i64,
            ),
            row(
                "Plundered Back",
                "PoE enemies plundered from us, capped by the chest.",
                self.stolen as i64,
            ),
            BreakdownRow {
                gap_after: true,
                ..row(
                    "Booty chest (net)",
                    "Retained chest minus theft — divvied to the crew by shares.",
                    self.chest_net as i64,
                )
            },
            row(
                "Goods Value in PoE",
                "PoE from selling excess goods at market.",
                self.goods_value as i64,
            ),
            row(
                "Restock Value",
                "PoE to rebuy the materials used this voyage.",
                self.restock_value as i64,
            ),
            row(
                "Pre-voyage Stocking",
                "What you spent stocking up before the voyage; recouped from goods.",
                self.stocking as i64,
            ),
            BreakdownRow {
                headline: true,
                gap_after: true,
                ..row(
                    "Total gained in pillage",
                    "The whole pillage's net profit; the base for the cuts.",
                    self.total_gained,
                )
            },
        ];

        // Cuts — each shown only when its CLI flag enabled it. The last shown one
        // carries the separator before the Add-to-X pair.
        let mut cuts = Vec::new();
        if show_co {
            cuts.push(row(
                "C. Officer Cut",
                "Your share of total gained, realized from (capped by) the goods cash.",
                self.co_cut as i64,
            ));
        }
        if show_donation {
            cuts.push(row(
                "Crew Donation",
                "The crew's share of total gained, realized from the goods cash.",
                self.crew_donation as i64,
            ));
        }
        if let Some(last) = cuts.last_mut() {
            last.gap_after = true;
        }
        rows.extend(cuts);

        // The two "how much to put where" outputs, grouped together — both bold.
        rows.push(BreakdownRow {
            headline: true,
            ..row(
                "Add to Restocking (Hold)",
                "Restock beyond the reserve; from goods, then your pocket on a loss.",
                self.add_to_restocking as i64,
            )
        });
        rows.push(BreakdownRow {
            headline: true,
            ..row(
                "Add to Booty",
                "Leftover goods cash to drop in the chest for the divvy.",
                self.add_to_booty as i64,
            )
        });
        rows
    }
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
    /// Picking the player's crew rank. `cursor` indexes [`Rank::ALL`].
    RankSelect { cursor: usize },
    /// Picking the crew's booty-share scheme. `cursor` indexes [`BootyShare::ALL`].
    ShareSelect { cursor: usize },
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
    /// The "Rank" selector row (opens the rank popup).
    RankRow,
    /// The "Booty Share" selector row (opens the scheme popup).
    ShareRow,
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
    /// Selected row in the Profit Breakdown popup, into [`ProfitResult::breakdown`]
    /// — drives the per-row tooltip. Moved by arrows or mouse hover.
    pub breakdown_cursor: usize,
    /// The player's crew rank; persisted, and (eventually) an input to the
    /// booty-from-treasure divvy.
    pub rank: Rank,
    /// The crew's booty-share scheme; persisted alongside [`rank`](Self::rank).
    pub booty_share: BootyShare,
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
                PromptField::new("Restocking Place", FieldKind::Text),
                PromptField::new("Selling Place", FieldKind::Text),
                PromptField::new("Booty Chest", FieldKind::PositiveInt),
                PromptField::new("C.O. Rate", FieldKind::Rate),
                PromptField::new("Crew Donation Share Rate", FieldKind::Rate),
                PromptField::new("Restocking Rate", FieldKind::Rate),
                PromptField::new("Pre-voyage Stocking", FieldKind::PositiveInt),
            ],
            submit_failed: None,
            popup: None,
            calc_error: None,
            fetch_purpose: FetchPurpose::Profits,
            show_co_rate: false,
            show_donation: false,
            breakdown_cursor: 0,
            rank: Rank::Officer,
            booty_share: BootyShare::Even,
        }
    }

    // -- visible parameter rows --

    /// Panel indices that are actually shown (and thus navigable), in order.
    /// The Restocking/Selling Place rows only matter with Market pricing; the
    /// C.O. Rate and Crew Donation rows are revealed by CLI flags.
    pub fn visible_panels(&self, market_supported: bool) -> Vec<usize> {
        (0..PANEL_COUNT)
            .filter(|&i| match i {
                P_RESTOCK_PLACE | P_SELL_PLACE => market_supported,
                P_CO_RATE => self.show_co_rate,
                P_DONATION => self.show_donation,
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

    /// Goods that need restocking but have no sell supply anywhere in the restock
    /// scope (`islands`) — a single island, or every island in an archipelago.
    pub fn missing_restock_in_scope(&self, islands: &[String], shared: &SharedState) -> Vec<String> {
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
                            && islands.iter().any(|i| o.islandname.eq_ignore_ascii_case(i))
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
        let scope = app::resolve_restock_scope(
            &self.panel[0].value,
            shared.available_islands,
            shared.ocean_geo,
        );
        if let Some(islands) = scope.island_filter() {
            let missing = self.missing_restock_in_scope(islands, shared);
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
        self.open_profit_result(profit);
    }

    /// Show the Profit Breakdown popup, parking the tooltip cursor at the top.
    fn open_profit_result(&mut self, profit: ProfitResult) {
        self.breakdown_cursor = 0;
        self.popup = Some(PopupKind::ProfitResult(profit));
        self.focus = Focus::Popup;
    }

    /// Number of rows in the open Profit Breakdown popup (0 if it isn't showing).
    /// Used to clamp the tooltip cursor.
    fn breakdown_row_count(&self) -> usize {
        match &self.popup {
            Some(PopupKind::ProfitResult(r)) => {
                r.breakdown(self.show_co_rate, self.show_donation).len()
            }
            _ => 0,
        }
    }

    pub fn calculate_profits(&self, shared: &SharedState) -> ProfitResult {
        let scope = app::resolve_restock_scope(
            &self.panel[0].value,
            shared.available_islands,
            shared.ocean_geo,
        );
        // The island(s) restock offers must be on: one island, an archipelago's
        // islands, or `None` for ocean-wide (blank or unrecognized).
        let restock_islands = scope.island_filter();

        // The Selling Place is the mirror of the Restocking Place: it constrains
        // where we offload the surplus (the buy offers we sell *into*). Blank =
        // ocean-wide, i.e. the market's single best price anywhere.
        let sell_scope = app::resolve_restock_scope(
            &self.panel[P_SELL_PLACE].value,
            shared.available_islands,
            shared.ocean_geo,
        );
        let sell_islands = sell_scope.island_filter();

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
                // Goods Value: sell excess at the best buy prices on the selling
                // place (ocean-wide when it's blank).
                let mut buy_offers: Vec<_> = offers
                    .iter()
                    .filter(|o| 0 < o.buyprice && 0 < o.buyqty)
                    .filter(|o| {
                        sell_islands.map_or(true, |islands| {
                            islands.iter().any(|i| o.islandname.eq_ignore_ascii_case(i))
                        })
                    })
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
                        restock_islands.map_or(true, |islands| {
                            islands.iter().any(|i| o.islandname.eq_ignore_ascii_case(i))
                        })
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

        // -- Money flow (see PROFITS_MONEY_FLOW.md) --
        // Everything below is computed in floating point and rounded only when
        // the ProfitResult is built, so intermediate halving/percentages don't
        // accumulate truncation error.
        //
        // A hidden parameter row means that deduction doesn't apply.
        let co_rate = if self.show_co_rate { parse_rate(&self.panel[P_CO_RATE]) } else { 0.0 };
        let donation_rate =
            if self.show_donation { parse_rate(&self.panel[P_DONATION]) } else { 0.0 };

        // Chest figures deduced from the battle ledger and the restocking rate.
        let cb = self.chest_components(shared);

        // The Booty Chest field overrides the deduced net; blank uses the deduced.
        let chest_net = {
            let s = self.panel[P_BOOTY_CHEST].value.trim();
            if s.is_empty() {
                cb.chest_net
            } else {
                s.parse::<f64>().unwrap_or(cb.chest_net).max(0.0)
            }
        };

        let stocking = self.panel[P_STOCKING].value.parse::<u64>().unwrap_or(0);

        // Restocking is paid from the reserve first; the overflow is funded by the
        // goods cash (and the officer's pocket if it falls short).
        let add_to_restocking = (restock_value as f64 - cb.reserve).max(0.0);

        // The whole pillage's net profit (the authoritative base for the cuts).
        // = net booty + goods + reserve + pocketed − stocking − restocking, which
        // reduces to gross − stolen + goods − stocking − restocking.
        let total_gained = chest_net + goods_value as f64 + cb.reserve + cb.immediate_cuts
            - stocking as f64
            - restock_value as f64;

        // Authoritative cuts (none on a loss).
        let base = total_gained.max(0.0);
        let co_auth = base * co_rate;
        let crew_auth = base * donation_rate;
        let cuts_auth = co_auth + crew_auth;

        // The cuts (and anything added to booty) are realized from the goods cash
        // left after restocking and recouping stocking — never from the chest.
        let available = (goods_value as f64 - add_to_restocking - stocking as f64).max(0.0);
        let (co_cut, crew_donation, add_to_booty) = if cuts_auth <= 0.0 {
            (0.0, 0.0, available)
        } else if cuts_auth <= available {
            (co_auth, crew_auth, available - cuts_auth)
        } else {
            // Not enough goods cash to fund both cuts — scale them down
            // proportionally; nothing is left to add to the booty.
            let scale = available / cuts_auth;
            (co_auth * scale, crew_auth * scale, 0.0)
        };

        // Round here, always in the commanding officer's favor: their own cut
        // rounds up; non-negative magnitudes round down (the leftover fraction
        // stays in their pocket); the signed total rounds toward zero.
        ProfitResult {
            gross_plundered: shared.pillage_gross,
            restock_reserve: cb.reserve.floor() as u64,
            immediate_cuts: cb.immediate_cuts.floor() as u64,
            chest_gross: cb.chest_gross.floor() as u64,
            stolen: shared.pillage_stolen,
            chest_net: chest_net.floor() as u64,
            goods_value,
            restock_value,
            add_to_restocking: add_to_restocking.floor() as u64,
            stocking,
            total_gained: total_gained.trunc() as i64,
            co_cut: co_cut.ceil() as u64,
            crew_donation: crew_donation.floor() as u64,
            add_to_booty: add_to_booty.floor() as u64,
        }
    }

    /// The booty-chest breakdown deduced from the battle ledger and the
    /// restocking rate. Carried as floats; rounded only when displayed.
    fn chest_components(&self, shared: &SharedState) -> ChestBreakdown {
        let restocking_rate = parse_rate(&self.panel[P_RESTOCK_RATE]);
        // The chest keeps the full retained half of every fight (the ledger
        // already sums each fight's half, rounding the odd PoE up into the chest).
        // The restocking skim comes off the *other* half — the crew's cut — not
        // the chest, so the chest is independent of the restocking rate.
        let chest_gross = shared.pillage_chest as f64;
        let immediate_half = (shared.pillage_gross as f64 - chest_gross).max(0.0);
        let reserve = immediate_half * restocking_rate;
        let immediate_cuts = immediate_half - reserve;
        let chest_net = (chest_gross - shared.pillage_stolen as f64).max(0.0);
        ChestBreakdown {
            reserve,
            chest_gross,
            immediate_cuts,
            chest_net,
        }
    }

    /// The auto-deduced net booty chest, used as the Booty Chest field's default
    /// when the user leaves it blank. Floored to favor the C.O.
    pub fn deduced_chest(&self, shared: &SharedState) -> u64 {
        self.chest_components(shared).chest_net.floor() as u64
    }

    /// The PoE in the booty chest to record with a saved voyage: the user-entered
    /// "Booty Chest" figure if given, else the auto-deduced net chest. Mirrors the
    /// `chest_net` resolution in [`Self::calculate_profits`].
    pub fn recorded_chest(&self, shared: &SharedState) -> u64 {
        let s = self.panel[P_BOOTY_CHEST].value.trim();
        if s.is_empty() {
            self.deduced_chest(shared)
        } else {
            s.parse::<u64>().unwrap_or_else(|_| self.deduced_chest(shared))
        }
    }

    /// The goods won this voyage: each commodity with a non-zero **Booty**-column
    /// quantity, as `(commodity id, quantity)`. Stock/Hold and Restock are
    /// excluded — only the Booty column. The caller resolves ids to names for
    /// persistence.
    pub fn booty_goods(&self) -> Vec<(u64, u64)> {
        self.rows
            .iter()
            .filter_map(|r| {
                let qty = r.booty.trim().parse::<u64>().ok().filter(|&q| q > 0)?;
                Some((r.commod_id, qty))
            })
            .collect()
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
            Focus::RankRow => self.handle_selector_row_key(key, false, shared),
            Focus::ShareRow => self.handle_selector_row_key(key, true, shared),
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
            Some(PopupKind::RankSelect { .. }) => {
                self.popup = None;
                self.focus = Focus::RankRow;
            }
            Some(PopupKind::ShareSelect { .. }) => {
                self.popup = None;
                self.focus = Focus::ShareRow;
            }
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

    /// Snap a place field (Restocking/Selling) to its canonical island or
    /// archipelago name, if the typed text resolves to one. No-op otherwise.
    fn canonicalize_place(&mut self, idx: usize, shared: &SharedState) {
        let canonical = match app::resolve_restock_scope(
            &self.panel[idx].value,
            shared.available_islands,
            shared.ocean_geo,
        ) {
            app::RestockScope::Island(name) | app::RestockScope::Archipelago { name, .. } => {
                Some(name)
            }
            app::RestockScope::OceanWide | app::RestockScope::Unknown => None,
        };
        if let Some(name) = canonical {
            self.panel[idx].value = name;
            self.panel[idx].cursor = self.panel[idx].value.len();
        }
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
                    // Snap the field to the canonical island or archipelago name.
                    self.canonicalize_place(P_RESTOCK_PLACE, shared);
                    self.focus = Focus::Panel(P_SELL_PLACE);
                }
            }
            KeyCode::Enter if idx == P_SELL_PLACE => {
                if !shared.cached_offers.is_empty() {
                    self.canonicalize_place(P_SELL_PLACE, shared);
                }
                self.focus = Focus::Panel(P_BOOTY_CHEST);
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
                    // Below the last parameter sit the Rank / Booty Share rows.
                    None => self.focus = Focus::RankRow,
                }
            }
            KeyCode::Left => {
                // The place fields are buttons until the market is queried; only
                // move the text cursor once they accept input.
                if !(is_place_field(idx) && shared.cached_offers.is_empty()) {
                    self.panel[idx].move_left();
                }
            }
            KeyCode::Right => {
                if !(is_place_field(idx) && shared.cached_offers.is_empty()) {
                    self.panel[idx].move_right();
                }
            }
            KeyCode::Backspace => {
                if !(is_place_field(idx) && shared.cached_offers.is_empty()) {
                    self.panel[idx].delete_char_before();
                    self.calc_error = None;
                }
            }
            KeyCode::Delete => {
                if !(is_place_field(idx) && shared.cached_offers.is_empty()) {
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
                if !(is_place_field(idx) && shared.cached_offers.is_empty()) {
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
            self.open_profit_result(profit);
            return InputResult::Consumed;
        }

        let unknown_place = [P_RESTOCK_PLACE, P_SELL_PLACE].into_iter().find(|&i| {
            matches!(
                app::resolve_restock_scope(
                    &self.panel[i].value,
                    shared.available_islands,
                    shared.ocean_geo,
                ),
                app::RestockScope::Unknown
            )
        });
        if let Some(i) = unknown_place {
            let which = if i == P_RESTOCK_PLACE { "restocking" } else { "selling" };
            self.calc_error = Some(format!("Unknown {which} island or archipelago"));
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
                    self.open_profit_result(profit);
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
            // The selection popups use per-item click targets (ProfitsRankItem /
            // ProfitsShareItem), not a yes/no button, so this is a no-op.
            Some(PopupKind::RankSelect { .. }) | Some(PopupKind::ShareSelect { .. }) => {}
            None => {}
        }
        InputResult::Consumed
    }

    fn handle_button_key(&mut self, key: KeyEvent, shared: &SharedState) -> InputResult {
        match key.code {
            KeyCode::Enter => {
                return self.handle_button_activate(shared);
            }
            // Above the Calculate button sits the Booty Share selector row.
            KeyCode::Up => self.focus = Focus::ShareRow,
            // The button is the bottom of the focus chain; ↓ goes nowhere.
            _ => {}
        }
        InputResult::Consumed
    }

    /// Keyboard handling for the Rank (`is_share == false`) and Booty Share
    /// (`is_share == true`) selector rows. Enter opens the matching popup;
    /// Up/Down walk the focus chain (… last panel → Rank → Booty Share → Button).
    fn handle_selector_row_key(
        &mut self,
        key: KeyEvent,
        is_share: bool,
        shared: &SharedState,
    ) -> InputResult {
        match key.code {
            KeyCode::Enter => self.open_selector_popup(is_share),
            KeyCode::Up => {
                if is_share {
                    self.focus = Focus::RankRow;
                } else {
                    match self.last_visible_panel(shared.market_supported) {
                        Some(i) => self.focus = Focus::Panel(i),
                        None => self.focus_input(),
                    }
                }
            }
            KeyCode::Down => {
                self.focus = if is_share { Focus::Button } else { Focus::ShareRow };
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Open the Rank or Booty Share selection popup, pre-positioning the cursor
    /// on the currently-selected entry.
    pub fn open_selector_popup(&mut self, is_share: bool) {
        self.popup = Some(if is_share {
            let cursor = BootyShare::ALL
                .iter()
                .position(|&s| s == self.booty_share)
                .unwrap_or(0);
            PopupKind::ShareSelect { cursor }
        } else {
            let cursor = Rank::ALL.iter().position(|&r| r == self.rank).unwrap_or(0);
            PopupKind::RankSelect { cursor }
        });
        self.focus = Focus::Popup;
    }

    /// Commit a rank choice (from a clicked list item) and return to its row.
    pub fn select_rank(&mut self, i: usize) {
        if let Some(&r) = Rank::ALL.get(i) {
            self.rank = r;
        }
        self.popup = None;
        self.focus = Focus::RankRow;
    }

    /// Commit a booty-share choice (from a clicked list item) and return to its row.
    pub fn select_share(&mut self, i: usize) {
        if let Some(&s) = BootyShare::ALL.get(i) {
            self.booty_share = s;
        }
        self.popup = None;
        self.focus = Focus::ShareRow;
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
                        self.open_profit_result(profit);
                    } else {
                        self.popup = None;
                        self.focus = Focus::Panel(0);
                    }
                }
                _ => {}
            },
            Some(PopupKind::ProfitResult(_)) => match key.code {
                KeyCode::Up => {
                    self.breakdown_cursor = self.breakdown_cursor.saturating_sub(1);
                }
                KeyCode::Down => {
                    let last = self.breakdown_row_count().saturating_sub(1);
                    self.breakdown_cursor = (self.breakdown_cursor + 1).min(last);
                }
                KeyCode::Enter => self.dismiss_ok_popup(),
                _ => {}
            },
            Some(PopupKind::PriceBlock { .. }) => {
                if key.code == KeyCode::Enter {
                    self.dismiss_ok_popup();
                }
            }
            Some(PopupKind::RankSelect { ref mut cursor }) => match key.code {
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => *cursor = (*cursor + 1).min(Rank::ALL.len() - 1),
                KeyCode::Enter => {
                    let i = *cursor;
                    self.select_rank(i);
                }
                _ => {}
            },
            Some(PopupKind::ShareSelect { ref mut cursor }) => match key.code {
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => *cursor = (*cursor + 1).min(BootyShare::ALL.len() - 1),
                KeyCode::Enter => {
                    let i = *cursor;
                    self.select_share(i);
                }
                _ => {}
            },
            None => {}
        }
        InputResult::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn recorded_chest_prefers_field_then_deduces() {
        let offers = HashMap::new();
        let islands: Vec<String> = Vec::new();
        let commodities: Vec<Commodity> = Vec::new();
        let shared = SharedState {
            commodities: &commodities,
            cached_offers: &offers,
            available_islands: &islands,
            ocean_geo: None,
            loading: false,
            market_supported: false,
            pillage_gross: 10_000,
            pillage_stolen: 800,
            pillage_chest: 5_000,
        };
        let mut app = ProfitsApp::new();
        // Blank Booty Chest field -> auto-deduced net chest = retained − stolen.
        assert_eq!(app.recorded_chest(&shared), 4_200);
        // A user-entered figure wins over the deduction.
        app.panel[P_BOOTY_CHEST].value = "1234".into();
        assert_eq!(app.recorded_chest(&shared), 1_234);
    }

    #[test]
    fn booty_goods_reads_booty_column_only() {
        let mut app = ProfitsApp::new();
        let mut r0 = InventoryRow::new(7);
        r0.booty = "30".into();
        r0.stock = "5".into(); // Stock/Hold is ignored — Booty column only.
        let mut r1 = InventoryRow::new(9);
        r1.restock = "100".into(); // A restock-only row contributes no goods.
        let mut r2 = InventoryRow::new(4);
        r2.booty = "0".into(); // Zero booty is skipped.
        app.rows = vec![r0, r1, r2];
        assert_eq!(app.booty_goods(), vec![(7, 30)]);
    }
}
