use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

struct App {
    rows: Vec<Vec<String>>,
    input: String,
    cursor: usize,
}

impl App {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            input: String::new(),
            cursor: 0,
        }
    }

    fn submit(&mut self) {
        let name = self.input.trim().to_string();
        if !name.is_empty() {
            self.rows.push(vec![
                name,
                String::new(),
                String::new(),
                String::new(),
            ]);
        }
        self.input.clear();
        self.cursor = 0;
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor = prev;
        }
    }

    fn delete_char_at(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += self.input[self.cursor..].chars().next().map_or(0, |c| c.len_utf8());
        }
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let vchunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());

    // -- Table --
    let item_width = app
        .rows
        .iter()
        .map(|r| r[0].chars().count())
        .max()
        .unwrap_or(0)
        .max("item".len()) as u16;

    let header = Row::new(vec!["item", "restock", "stock", "booty"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .rows
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::new(r[0].as_str()),
                Cell::new(Line::from(r[1].as_str()).right_aligned()),
                Cell::new(Line::from(r[2].as_str()).right_aligned()),
                Cell::new(Line::from(r[3].as_str()).right_aligned()),
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

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .block(Block::default().borders(Borders::ALL).title("Inventory"));

    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(table_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[0]);

    frame.render_widget(table, hchunks[1]);

    // -- Input prompt --
    let label = "add item: ";
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().bold()),
        Span::raw(&app.input),
    ]));
    frame.render_widget(prompt, vchunks[1]);

    let cursor_x =
        vchunks[1].x + label.len() as u16 + app.input[..app.cursor].chars().count() as u16;
    let cursor_y = vchunks[1].y;
    frame.set_cursor_position((cursor_x, cursor_y));
}

#[tokio::main]
async fn main() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new();

    loop {
        terminal.draw(|frame| ui(frame, &app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc => break,
                KeyCode::Enter => app.submit(),
                KeyCode::Backspace => app.delete_char_before(),
                KeyCode::Delete => app.delete_char_at(),
                KeyCode::Left => app.move_left(),
                KeyCode::Right => app.move_right(),
                KeyCode::Home => app.cursor = 0,
                KeyCode::End => app.cursor = app.input.len(),
                KeyCode::Char(c) => app.insert_char(c),
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
