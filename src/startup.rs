use std::{collections::HashMap, io, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{
        EnterAlternateScreen,
        LeaveAlternateScreen,
        disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};

use crate::{
    cache::OceanCache,
    ocean::Ocean,
    pirate::{FetchPlan, PirateUpdate},
    utils::{FieldKind, PromptField, offset_title},
};

// ---------------------------------------------------------------------------
// Startup setup popup
// ---------------------------------------------------------------------------

/// The ocean picker's master layout: every live ocean keyed by its grid slot
/// `(row, column)`. Column 0 is the three Market oceans (live market data);
/// column 1 is the rest. This single map is the source of truth for both the
/// set of oceans and where each sits — rendering and navigation read it by
/// explicit `(row, col)` index, so the map's iteration order never matters.
fn ocean_grid() -> HashMap<(usize, usize), Ocean> {
    use Ocean::*;
    // (row, column) → ocean
    HashMap::from([
        ((0, 0), Emerald),
        ((1, 0), Meridian),
        ((2, 0), Cerulean),
        ((0, 1), Obsidian),
        ((1, 1), Opal),
        ((2, 1), Jade),
        ((3, 1), Ice),
    ])
}

/// Locate an ocean's `(row, column)` slot in the grid. Falls back to the first
/// cell if it isn't found (shouldn't happen — every live ocean is listed).
fn ocean_pos(
    grid: &HashMap<(usize, usize), Ocean>,
    target: Ocean,
) -> (usize, usize) {
    grid.iter()
        .find(|(_, o)| **o == target)
        .map(|(&pos, _)| pos)
        .unwrap_or((0, 0))
}

/// Which field the popup currently focuses.
#[derive(PartialEq, Clone, Copy)]
enum Field {
    Ocean,
    Name,
}

/// Outcome of a verification attempt, delivered over a channel so the UI can
/// keep redrawing "Verifying…" while it runs. `Found` carries the freshly
/// fetched pirate page so the caller can fold it into the cache instead of
/// throwing the fetch away and re-querying next run.
enum Verify {
    Found(Box<PirateUpdate>),
    NotFound,
    Error(String),
}

struct Setup {
    field: Field,
    grid: HashMap<(usize, usize), Ocean>,
    ocean_col: usize,
    ocean_row: usize,
    /// The "Don't Choose" row (below the grid) is highlighted instead of a
    /// cell. `ocean_col`/`ocean_row` still hold the last grid cell to
    /// return to.
    dont_choose: bool,
    name: PromptField,
    status: Option<String>,
    verifying: bool,
    query_market: bool,
}

impl Setup {
    /// The grid cell currently under the highlight (ignores "Don't Choose").
    fn highlighted_ocean(&self) -> Ocean {
        self.grid[&(self.ocean_row, self.ocean_col)]
    }

    /// The chosen ocean: `None` when "Don't Choose" is selected.
    fn selected_ocean(&self) -> Option<Ocean> {
        (!self.dont_choose).then(|| self.highlighted_ocean())
    }

    /// Number of columns in the grid.
    fn column_count(&self) -> usize {
        self.grid.keys().map(|&(_, c)| c + 1).max().unwrap_or(0)
    }

    /// Number of rows present in `col`.
    fn column_height(&self, col: usize) -> usize {
        self.grid.keys().filter(|&&(_, c)| c == col).count()
    }

    /// Keep the selected row inside the current column's bounds (used after a
    /// column switch, since columns can differ in height).
    fn clamp_row(&mut self) {
        let max = self.column_height(self.ocean_col).saturating_sub(1);
        self.ocean_row = self.ocean_row.min(max);
    }
}

/// Run the interactive setup popup, collecting an ocean and pirate name when
/// they were not supplied on the command line. Pre-supplied values are kept
/// and their fields pre-filled.
///
/// Drawn on the alternate screen in raw mode and torn down before returning,
/// so the caller's later `eprintln!` progress and main loop are unaffected.
/// Returns `Some((ocean, name, fetched))` on confirmation (`ocean`/`name` may
/// be `None` when the user picked "Don't Choose" / left the name blank), or
/// `None` when the user pressed Esc to quit. `fetched` is the pirate page
/// pulled while verifying a not-yet-cached name, for the caller to fold into
/// the cache; it is `None` when the name was already cached or no fetch
/// happened.
pub async fn prompt(
    client: &reqwest::Client,
    ocean: Option<Ocean>,
    user: Option<String>,
    oceans: &HashMap<String, OceanCache>,
    query_market: bool,
) -> io::Result<
    Option<(
        Option<Ocean>,
        Option<String>,
        Option<PirateUpdate>,
    )>,
> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let result = run(
        client,
        ocean,
        user,
        oceans,
        query_market,
        &mut terminal,
    )
    .await;

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    result
}

/// Draw the setup screen for a given state, with no terminal of its own, no
/// input and no network.
///
/// The screen runs before the app exists and owns its own loop, so nothing
/// that inspects the interface can reach it the way it reaches a page. This is
/// the way in: the gallery and the tests both draw it through here, so the
/// first thing anyone sees is checked like everything after it.
// the app itself never previews its own setup screen; the gallery and the
// tests are what draw it this way
#[allow(dead_code)]
pub fn preview(
    frame: &mut Frame,
    ocean: Ocean,
    name: &str,
    status: Option<&str>,
) {
    let grid = ocean_grid();
    let (ocean_row, ocean_col) = ocean_pos(&grid, ocean);
    let mut field = PromptField::new("Pirate name", FieldKind::Text);
    field.value = name.to_owned();
    field.cursor = name.len();
    render(
        frame,
        &Setup {
            field: Field::Name,
            grid,
            ocean_col,
            ocean_row,
            dont_choose: false,
            name: field,
            status: status.map(str::to_owned),
            verifying: false,
            query_market: false,
        },
    );
}

async fn run(
    client: &reqwest::Client,
    ocean: Option<Ocean>,
    user: Option<String>,
    oceans: &HashMap<String, OceanCache>,
    query_market: bool,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<
    Option<(
        Option<Ocean>,
        Option<String>,
        Option<PirateUpdate>,
    )>,
> {
    let grid = ocean_grid();
    let (ocean_row, ocean_col) =
        ocean.map(|o| ocean_pos(&grid, o)).unwrap_or((0, 0));
    let mut name = PromptField::new("Pirate name", FieldKind::Text);
    if let Some(u) = &user {
        name.value = u.clone();
        name.cursor = u.len();
    }
    let mut state = Setup {
        // Start on whichever field still needs input.
        field: if ocean.is_none() {
            Field::Ocean
        } else {
            Field::Name
        },
        grid,
        ocean_col,
        ocean_row,
        dont_choose: false,
        name,
        status: None,
        verifying: false,
        query_market,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Verify>();

    loop {
        terminal.draw(|frame| render(frame, &state))?;

        // Absorb a completed verification.
        if let Ok(outcome) = rx.try_recv() {
            state.verifying = false;
            match outcome {
                Verify::Found(update) => {
                    return Ok(Some((
                        state.selected_ocean(),
                        Some(state.name.value.trim().to_owned()),
                        Some(*update),
                    )));
                }
                Verify::NotFound => {
                    let ocean = state
                        .selected_ocean()
                        .map(|o| o.to_string())
                        .unwrap_or_default();
                    state.status = Some(format!(
                        "Arr, no '{}' to be found on the {} ocean. Check yer \
                         spelling, or Esc to skip.",
                        state.name.value.trim(),
                        ocean
                    ));
                }
                Verify::Error(e) => {
                    state.status = Some(format!(
                        "Couldn't verify: {e} (Esc to skip)"
                    ));
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

        // While verifying, only Esc (quit) is accepted.
        if state.verifying {
            if key.code == KeyCode::Esc {
                return Ok(None);
            }
            continue;
        }

        // Any keypress dismisses a stale status line so the tooltip returns.
        state.status = None;

        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Tab => {
                state.field = match state.field {
                    Field::Ocean => Field::Name,
                    Field::Name => Field::Ocean,
                };
            }
            KeyCode::Up if state.field == Field::Ocean => {
                if state.dont_choose {
                    // Leave "Don't Choose", back into the grid.
                    state.dont_choose = false;
                } else if state.ocean_row > 0 {
                    state.ocean_row -= 1;
                }
            }
            KeyCode::Down if state.field == Field::Ocean => {
                if state.dont_choose {
                    // Already at the bottom.
                } else if state.ocean_row + 1
                    < state.column_height(state.ocean_col)
                {
                    state.ocean_row += 1;
                } else {
                    // Off the bottom of the column → the "Don't Choose" row.
                    state.dont_choose = true;
                }
            }
            KeyCode::Left
                if state.field == Field::Ocean && !state.dont_choose =>
            {
                if state.ocean_col > 0 {
                    state.ocean_col -= 1;
                    state.clamp_row();
                }
            }
            KeyCode::Right
                if state.field == Field::Ocean && !state.dont_choose =>
            {
                if state.ocean_col + 1 < state.column_count() {
                    state.ocean_col += 1;
                    state.clamp_row();
                }
            }
            KeyCode::Up if state.field == Field::Name => {
                state.field = Field::Ocean
            }
            KeyCode::Enter => {
                match state.field {
                    Field::Ocean => state.field = Field::Name,
                    Field::Name => {
                        let trimmed = state.name.value.trim().to_owned();
                        let ocean = state.selected_ocean();
                        if trimmed.is_empty() {
                            // Proceed without identifying a pirate.
                            return Ok(Some((ocean, None, None)));
                        }
                        match ocean {
                            // No ocean → nothing to verify against; take the
                            // name as-is.
                            None => {
                                return Ok(Some((None, Some(trimmed), None)));
                            }
                            Some(o)
                                if cached_player(oceans, o, &trimmed)
                                    .is_some() =>
                            {
                                // Verified on a previous run — skip the yoweb
                                // round-trip.
                                return Ok(Some((
                                    Some(o),
                                    Some(trimmed),
                                    None,
                                )));
                            }
                            Some(o) => {
                                state.verifying = true;
                                state.status = Some(format!(
                                    "Verifying {trimmed} on {o}…"
                                ));
                                let tx = tx.clone();
                                let client = client.clone();
                                tokio::spawn(async move {
                                    // Fetch the basic page (not just an
                                    // existence check) so
                                    // the result can be cached and never
                                    // re-queried next run.
                                    // Trophies are left for the lazy background
                                    // fetcher.
                                    let plan = FetchPlan {
                                        basic: true,
                                        trophies: false,
                                    };
                                    let update =
                                        crate::pirate::fetch_pirate_update(
                                            &client, &trimmed, o, plan,
                                        )
                                        .await;
                                    let outcome = match update {
                                        u @ PirateUpdate::Refreshed {
                                            basic: Some(_),
                                            ..
                                        } => Verify::Found(Box::new(u)),
                                        // basic was requested, so absent means
                                        // no pirate.
                                        PirateUpdate::Refreshed {
                                            ..
                                        }
                                        | PirateUpdate::NotFound => {
                                            Verify::NotFound
                                        }
                                        PirateUpdate::Error(e) => {
                                            Verify::Error(e)
                                        }
                                    };
                                    let _ = tx.send(outcome);
                                });
                            }
                        }
                    }
                }
            }
            // Text editing for the name field.
            KeyCode::Char(c) if state.field == Field::Name => {
                state.name.insert_char(c)
            }
            KeyCode::Backspace if state.field == Field::Name => {
                state.name.delete_char_before()
            }
            KeyCode::Delete if state.field == Field::Name => {
                state.name.delete_char_at()
            }
            KeyCode::Left if state.field == Field::Name => {
                state.name.move_left()
            }
            KeyCode::Right if state.field == Field::Name => {
                state.name.move_right()
            }
            _ => {}
        }
    }
}

/// Look up a cached pirate (and their stats) for `ocean`. A `Some` means we
/// already fetched them on a previous run, so no yoweb verification is needed.
/// Cache keys are normalized pirate names, so we normalize before looking up.
fn cached_player<'a>(
    oceans: &'a HashMap<String, OceanCache>,
    ocean: Ocean,
    name: &str,
) -> Option<&'a crate::pirate::CachedPirate> {
    let norm = crate::pirate::normalize_name(name).ok()?;
    oceans.get(ocean.name())?.players.get(&norm)
}

/// Context-sensitive help for the bottom region. Always ends by advertising
/// that Esc quits.
fn tooltip_lines(state: &Setup) -> Vec<String> {
    let mut lines = match state.field {
        Field::Ocean if state.dont_choose => {
            vec![
                "Select this to disable obtaining Pirate information."
                    .to_owned(),
                "Press Enter to select this.".to_owned(),
            ]
        }
        Field::Ocean => {
            let ocean = state.highlighted_ocean();
            let mut v = Vec::new();
            // The Market note only applies when it would actually take
            // effect.
            if state.query_market && ocean.market_supported() {
                v.push("Select this to enable market querying.".to_owned());
            }
            v.push(format!(
                "Press Enter to select {ocean} Ocean."
            ));
            v
        }
        Field::Name if state.name.value.trim().is_empty() => {
            vec![
                "Press Enter to not identify yourself.".to_owned(),
                "Jobber functionality will be reduced as a result.".to_owned(),
                "Voyage win/loss will also be indeterminate without a name."
                    .to_owned(),
            ]
        }
        Field::Name => {
            let name = state.name.value.trim();
            match state.selected_ocean() {
                Some(o) => {
                    vec![format!(
                        "Press Enter to identify as {name} of the {o} ocean."
                    )]
                }
                None => vec![format!("Press Enter to identify as {name}.")],
            }
        }
    };
    lines.push("Press Esc to quit.".to_owned());
    lines
}

fn render(frame: &mut Frame, state: &Setup) {
    let area = centered(frame.area(), 48, 16);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(Line::from("Choose Yer Pirate").centered());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // [ ocean box (7) | name box (3) | tooltip/status (rest) ]
    let rows = Layout::vertical([
        Constraint::Length(7),
        Constraint::Length(3),
        Constraint::Min(3),
    ])
    .split(inner);

    // Focused boxes get a bright border; idle ones stay muted.
    let border_for = |focused: bool| {
        if focused {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };
    // Highlight ramp shared by the ocean cells and the "Don't Choose" row.
    let ocean_focused = state.field == Field::Ocean;
    let cell_style = |selected: bool| {
        if selected && ocean_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else if selected {
            Style::default().bold()
        } else {
            Style::default()
        }
    };

    // -- Ocean picker: two columns + a full-width "Don't Choose" row --
    let ocean_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_for(ocean_focused))
        .title(offset_title("Ocean").0);
    let ocean_inner = ocean_block.inner(rows[0]);
    frame.render_widget(ocean_block, rows[0]);

    let oc = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
        .split(ocean_inner);
    let cols = state.column_count();
    let col_areas = Layout::horizontal(
        std::iter::repeat_n(Constraint::Ratio(1, cols as u32), cols)
            .collect::<Vec<_>>(),
    )
    .split(oc[0]);
    for c in 0 .. cols {
        let width = col_areas[c].width as usize;
        let lines: Vec<Line> = (0 .. state.column_height(c))
            .map(|r| {
                let ocean = state.grid[&(r, c)];
                let selected = !state.dont_choose
                    && c == state.ocean_col
                    && r == state.ocean_row;
                // Pad to the column width so the highlight spans the cell.
                let label = format!("{:<width$}", format!(" {ocean}"));
                Line::from(Span::styled(
                    label,
                    cell_style(selected),
                ))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), col_areas[c]);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Don't Choose",
            cell_style(state.dont_choose),
        )))
        .centered(),
        oc[1],
    );

    // -- Name field, boxed with inner horizontal padding --
    let name_focused = state.field == Field::Name;
    let name_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_for(name_focused))
        .padding(Padding::horizontal(1))
        .title(offset_title("Who Are Ye?").0);
    let name_inner = name_block.inner(rows[1]);
    frame.render_widget(name_block, rows[1]);
    let name_span = if state.name.value.is_empty() && !name_focused {
        Span::styled(
            "<pirate name>",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::styled(
            state.name.value.clone(),
            Style::default().fg(Color::White),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(name_span)),
        name_inner,
    );
    if name_focused {
        let prefix =
            state.name.value[.. state.name.cursor].chars().count() as u16;
        frame.set_cursor_position((name_inner.x + prefix, name_inner.y));
    }

    // -- Bottom: verification status takes precedence over the tooltip --
    if let Some(status) = &state.status {
        let style = if state.verifying {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Red)
        };
        frame.render_widget(
            Paragraph::new(status.as_str()).style(style).wrap(
                ratatui::widgets::Wrap {
                    trim: true,
                },
            ),
            rows[2],
        );
    } else {
        let lines: Vec<Line> = tooltip_lines(state)
            .into_iter()
            .map(|s| {
                Line::from(Span::styled(
                    s,
                    Style::default().fg(Color::DarkGray),
                ))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap {
                trim: true,
            }),
            rows[2],
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

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// The setup screen as it draws, with `status` in the bottom region.
    fn screen(name: &str, status: Option<&str>) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| preview(frame, Ocean::Emerald, name, status))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0 .. buf.area.height)
            .map(|y| {
                (0 .. buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_failed_lookup_fits_the_box_it_is_drawn_in() {
        let status = "Arr, no 'Foo' to be found on the Emerald ocean. Check \
                      yer spelling, or Esc to skip.";
        let screen = screen("Foo", Some(status));
        // Every word survives the wrap: a notice that loses its tail is worse
        // than none, since the way out is in the last sentence.
        for word in status.split_whitespace() {
            assert!(
                screen.contains(word),
                "{word:?} did not survive the wrap:\n{screen}"
            );
        }
        // And it stays inside the box it is drawn in.
        for line in screen.lines().filter(|l| l.contains("Arr,")) {
            assert!(
                line.ends_with('\u{2502}'),
                "notice overran its border:\n{line}"
            );
        }
    }
}
