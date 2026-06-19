use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::ocean::Ocean;
use crate::utils::{FieldKind, PromptField};

// ---------------------------------------------------------------------------
// Startup setup popup
// ---------------------------------------------------------------------------

/// Which field the popup currently focuses.
#[derive(PartialEq, Clone, Copy)]
enum Field {
    Ocean,
    Name,
}

/// Outcome of a verification attempt, delivered over a channel so the UI can
/// keep redrawing "Verifying…" while it runs.
enum Verify {
    Found,
    NotFound,
    Error(String),
}

struct Setup {
    field: Field,
    ocean_idx: usize,
    name: PromptField,
    status: Option<String>,
    verifying: bool,
}

/// Run the interactive setup popup, collecting an ocean and pirate name when
/// they were not supplied on the command line. Pre-supplied values are kept
/// and their fields pre-filled.
///
/// Drawn on the alternate screen in raw mode and torn down before returning,
/// so the caller's later `eprintln!` progress and main loop are unaffected.
/// Returns the resolved `(ocean, name)`; either may be `None` if the user
/// skips with Esc and never supplied one.
pub async fn prompt(
    client: &reqwest::Client,
    ocean: Option<Ocean>,
    user: Option<String>,
) -> io::Result<(Option<Ocean>, Option<String>)> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let result = run(client, ocean, user, &mut terminal).await;

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    result
}

async fn run(
    client: &reqwest::Client,
    ocean: Option<Ocean>,
    user: Option<String>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<(Option<Ocean>, Option<String>)> {
    let ocean_idx = ocean
        .and_then(|o| Ocean::LIVE.iter().position(|&l| l == o))
        .unwrap_or(0);
    let mut name = PromptField::new("Pirate name", FieldKind::Text);
    if let Some(u) = &user {
        name.value = u.clone();
        name.cursor = u.len();
    }
    let mut state = Setup {
        // Start on whichever field still needs input.
        field: if ocean.is_none() { Field::Ocean } else { Field::Name },
        ocean_idx,
        name,
        status: None,
        verifying: false,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Verify>();

    loop {
        terminal.draw(|frame| render(frame, &state))?;

        // Absorb a completed verification.
        if let Ok(outcome) = rx.try_recv() {
            state.verifying = false;
            match outcome {
                Verify::Found => {
                    let chosen = Ocean::LIVE[state.ocean_idx];
                    return Ok((Some(chosen), Some(state.name.value.trim().to_owned())));
                }
                Verify::NotFound => {
                    state.status = Some(format!(
                        "No pirate '{}' on {}. Check spelling, or Esc to skip.",
                        state.name.value.trim(),
                        Ocean::LIVE[state.ocean_idx]
                    ));
                }
                Verify::Error(e) => {
                    state.status = Some(format!("Couldn't verify: {e} (Esc to skip)"));
                }
            }
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // While verifying, only Esc (skip) is accepted.
        if state.verifying {
            if key.code == KeyCode::Esc {
                return Ok(skip(&state));
            }
            continue;
        }

        match key.code {
            KeyCode::Esc => return Ok(skip(&state)),
            KeyCode::Tab => {
                state.field = match state.field {
                    Field::Ocean => Field::Name,
                    Field::Name => Field::Ocean,
                };
            }
            KeyCode::Up if state.field == Field::Ocean => {
                if state.ocean_idx > 0 {
                    state.ocean_idx -= 1;
                }
            }
            KeyCode::Down if state.field == Field::Ocean => {
                if state.ocean_idx + 1 < Ocean::LIVE.len() {
                    state.ocean_idx += 1;
                }
            }
            KeyCode::Enter => match state.field {
                Field::Ocean => state.field = Field::Name,
                Field::Name => {
                    let trimmed = state.name.value.trim().to_owned();
                    if trimmed.is_empty() {
                        state.status =
                            Some("Enter a pirate name, or Esc to skip.".to_owned());
                    } else {
                        let ocean = Ocean::LIVE[state.ocean_idx];
                        state.verifying = true;
                        state.status = Some(format!("Verifying {trimmed} on {ocean}…"));
                        let tx = tx.clone();
                        let client = client.clone();
                        tokio::spawn(async move {
                            let outcome =
                                match crate::pirate::verify_exists(&client, ocean, &trimmed).await {
                                    Ok(true) => Verify::Found,
                                    Ok(false) => Verify::NotFound,
                                    Err(e) => Verify::Error(e),
                                };
                            let _ = tx.send(outcome);
                        });
                    }
                }
            },
            // Text editing for the name field.
            KeyCode::Char(c) if state.field == Field::Name => state.name.insert_char(c),
            KeyCode::Backspace if state.field == Field::Name => state.name.delete_char_before(),
            KeyCode::Delete if state.field == Field::Name => state.name.delete_char_at(),
            KeyCode::Left if state.field == Field::Name => state.name.move_left(),
            KeyCode::Right if state.field == Field::Name => state.name.move_right(),
            _ => {}
        }
    }
}

/// Esc was pressed: proceed with whatever is currently chosen. An ocean is
/// always selected; the name is taken only if non-empty.
fn skip(state: &Setup) -> (Option<Ocean>, Option<String>) {
    let name = state.name.value.trim();
    let name = (!name.is_empty()).then(|| name.to_owned());
    (Some(Ocean::LIVE[state.ocean_idx]), name)
}

fn render(frame: &mut Frame, state: &Setup) {
    let area = centered(frame.area(), 52, 17);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title("─── Setup ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // [ intro (2) | "Ocean:" (1) | list (7) | name (1) | gap (1) | status ]
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(Ocean::LIVE.len() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Choose your ocean and pirate name.",
                Style::default().bold(),
            )),
            Line::from(Span::styled(
                "Tab switches field · Enter confirms · Esc skips",
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        rows[0],
    );

    // Ocean list (the 3 Market oceans tagged "(prices)").
    let ocean_focused = state.field == Field::Ocean;
    frame.render_widget(
        Paragraph::new(Span::styled("Ocean:", Style::default().fg(Color::DarkGray))),
        rows[1],
    );
    let items: Vec<ListItem> = Ocean::LIVE
        .iter()
        .map(|o| {
            let tag = if o.market_supported() { "  (prices)" } else { "" };
            ListItem::new(format!(" {o}{tag}"))
        })
        .collect();
    let highlight = if ocean_focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default().bold()
    };
    let mut list_state = ListState::default().with_selected(Some(state.ocean_idx));
    frame.render_stateful_widget(
        List::new(items).highlight_style(highlight),
        rows[2],
        &mut list_state,
    );

    // Name field.
    let name_focused = state.field == Field::Name;
    let name_span = if state.name.value.is_empty() && !name_focused {
        Span::styled("<pirate name>", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(state.name.value.clone(), Style::default().fg(Color::White))
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Name: ", Style::default().fg(Color::DarkGray)),
            name_span,
        ])),
        rows[3],
    );
    if name_focused {
        let prefix = 6 + state.name.value[..state.name.cursor].chars().count() as u16;
        frame.set_cursor_position((rows[3].x + prefix, rows[3].y));
    }

    // Status / verification line.
    if let Some(status) = &state.status {
        let style = if state.verifying {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Red)
        };
        frame.render_widget(
            Paragraph::new(status.as_str())
                .style(style)
                .wrap(ratatui::widgets::Wrap { trim: true }),
            rows[5],
        );
    }
}

/// A `width`×`height` rect centered within `area` (clamped to fit).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}
