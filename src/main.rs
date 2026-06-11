use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use serde::Deserialize;

mod aliases;

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Commodity {
    id: u64,
    #[serde(rename = "commodname")]
    name: String,
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

const PANEL_COUNT: usize = 5;

#[derive(PartialEq)]
enum Focus {
    Input,
    Table,
    Panel(usize),
}

struct App {
    commodities: Vec<Commodity>,
    rows: Vec<InventoryRow>,
    input: String,
    cursor: usize,
    focus: Focus,
    table_state: TableState,
    panel: [PromptField; PANEL_COUNT],
    submit_failed: bool,
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
                PromptField::new("Money in Booty", FieldKind::PositiveInt),
                PromptField::new("Commanding Officer Rate", FieldKind::Rate),
                PromptField::new("Crew Donation Share Rate", FieldKind::Rate),
                PromptField::new("Restocking Rate", FieldKind::Rate),
                PromptField::new("Pre-restocking", FieldKind::PositiveInt),
            ],
            submit_failed: false,
        }
    }

    fn commod_name(&self, commod_id: u64) -> &str {
        self.commodities
            .iter()
            .find(|c| c.id == commod_id)
            .map(|c| c.name.as_str())
            .unwrap_or("???")
    }

    // -- focus transitions --

    fn focus_table_bottom(&mut self) {
        if self.rows.is_empty() {
            self.focus_panel(PANEL_COUNT - 1);
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

        // 4. Levenshtein (threshold <= 4, unique winner)
        let mut best_dist = usize::MAX;
        let mut best_id = None;
        let mut tie = false;
        for c in &self.commodities {
            let dist = text_similarity(&query, &c.name.to_lowercase());
            if dist < best_dist {
                best_dist = dist;
                best_id = Some(c.id);
                tie = false;
            } else if dist == best_dist {
                tie = true;
            }
        }
        if best_dist <= 4 && !tie {
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
                }
                self.submit_failed = false;
            }
            None => {
                self.submit_failed = true;
            }
        }

        self.input.clear();
        self.cursor = 0;
    }

    fn input_insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.submit_failed = false;
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
            self.submit_failed = false;
        }
    }

    fn input_delete_char_at(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
            self.submit_failed = false;
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

fn text_similarity(a: &str, b: &str) -> usize {
    levenshtein::levenshtein(a, b)
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

fn ui(frame: &mut Frame, app: &mut App) {
    let vchunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
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
        .max("item".len()) as u16;

    let header = Row::new(vec!["item", "restock", "stock", "booty"])
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

    // 2 = left + right border, 3 = spacing between 4 columns
    let table_width = item_width + 7 + 5 + 5 + 3 + 2;

    let highlight = Style::default().bg(Color::White).fg(Color::Black);
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .cell_highlight_style(highlight)
        .block(Block::default().borders(Borders::ALL).title("Inventory"));

    // -- Panel --
    let panel_label_width = app
        .panel
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0) as u16;
    // border (1) + padding-left (1) + label + padding-right (1) + border (1)
    let panel_inner_width = panel_label_width;
    let panel_width = panel_inner_width + 2;

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

    // Vertically center the panel: 2 rows per field (label + value), + 2 for border
    let panel_content_height = (PANEL_COUNT as u16) * 2;
    let panel_block_height = panel_content_height + 2;
    let panel_vchunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(panel_block_height),
        Constraint::Fill(1),
    ])
    .split(hchunks[3]);

    let panel_block = Block::default().borders(Borders::ALL).title("Parameters");
    let panel_inner = panel_block.inner(panel_vchunks[1]);
    frame.render_widget(panel_block, panel_vchunks[1]);

    // Each prompt takes 2 rows: label + value
    let panel_constraints: Vec<Constraint> = (0..PANEL_COUNT)
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
        .collect();
    let panel_rows = Layout::vertical(panel_constraints).split(panel_inner);

    for (i, field) in app.panel.iter().enumerate() {
        let label_area = panel_rows[i * 2];
        let value_area = panel_rows[i * 2 + 1];

        let label = Paragraph::new(Span::styled(field.label, Style::default().bold()));
        frame.render_widget(label, label_area);

        let is_focused = app.focus == Focus::Panel(i);
        let value_style = if is_focused {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };
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

    // -- Input prompt --
    let label = "add item: ";
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().bold()),
        Span::raw(&app.input),
    ]));
    frame.render_widget(prompt, vchunks[1]);

    if app.focus == Focus::Input {
        let cursor_x = vchunks[1].x
            + label.len() as u16
            + app.input[..app.cursor].chars().count() as u16;
        let cursor_y = vchunks[1].y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    // -- Suggestion / error line --
    let suggestion = app.suggest();
    let query_lower = app.input.trim().to_lowercase();
    let suggestion_widget: Option<Paragraph> = match suggestion {
        Some(id) => {
            let name = app.commod_name(id);
            if name.eq_ignore_ascii_case(&query_lower) {
                // Exact match — no suggestion shown
                None
            } else {
                Some(Paragraph::new(Span::styled(
                    format!("  {}", name),
                    Style::default().fg(Color::DarkGray),
                )))
            }
        }
        None => {
            if app.submit_failed {
                Some(Paragraph::new(Span::styled(
                    "  no match",
                    Style::default().fg(Color::Red),
                )))
            } else {
                None
            }
        }
    };
    if let Some(w) = suggestion_widget {
        frame.render_widget(w, vchunks[2]);
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> io::Result<()> {
    eprintln!("Fetching commodities from market...");
    let mut commodities: Vec<Commodity> = reqwest::get("https://api.plunderly.app/commods")
        .await
        .expect("failed to fetch commodities")
        .json()
        .await
        .expect("failed to parse commodities");
    commodities.sort_by_key(|c| c.id);

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

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new(commodities);

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Esc {
                break;
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
                    _ => {}
                },
                Focus::Panel(idx) => match key.code {
                    KeyCode::Up => {
                        if idx > 0 {
                            app.focus = Focus::Panel(idx - 1);
                        }
                    }
                    KeyCode::Down => {
                        if idx + 1 < PANEL_COUNT {
                            app.focus = Focus::Panel(idx + 1);
                        } else {
                            app.focus_input();
                        }
                    }
                    KeyCode::Left => {
                        // Back to table, booty column, last row (or stay if empty)
                        if !app.rows.is_empty() {
                            app.focus = Focus::Table;
                            app.table_state
                                .select(Some(app.rows.len() - 1));
                            app.table_state.select_column(Some(LAST_COL));
                        }
                    }
                    KeyCode::Backspace => app.panel[idx].delete_char_before(),
                    KeyCode::Delete => app.panel[idx].delete_char_at(),
                    KeyCode::Home => app.panel[idx].cursor = 0,
                    KeyCode::End => {
                        let len = app.panel[idx].value.len();
                        app.panel[idx].cursor = len;
                    }
                    KeyCode::Char(c) => app.panel[idx].insert_char(c),
                    _ => {}
                },
            }
        }
    }

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
