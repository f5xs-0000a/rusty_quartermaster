use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Row, Table};

struct App {
    rows: Vec<Vec<String>>,
}

impl App {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let header = Row::new(vec!["item", "restock", "stock", "booty"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .rows
        .iter()
        .map(|r| Row::new(r.iter().map(|c| c.as_str())))
        .collect();

    let widths = [
        Constraint::Min(10),
        Constraint::Min(10),
        Constraint::Min(10),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Inventory"));

    frame.render_widget(table, frame.area());
}

#[tokio::main]
async fn main() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let app = App::new();

    loop {
        terminal.draw(|frame| ui(frame, &app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                break;
            }
        }
    }

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
