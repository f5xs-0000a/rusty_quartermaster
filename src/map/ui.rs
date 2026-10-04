//! Rendering for the Map app: the map is drawn onto a character canvas
//! (points, leagues, labels), and the viewport shows the part of it around
//! the cursor.

use std::collections::BTreeSet;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};

use crate::{
    clickmap::{ClickRegion, ClickTarget},
    map::{
        MOVE_KEYS,
        MapApp,
        data::{Heading, Map, Point},
    },
    utils::offset_title,
};

// a terminal cell is about twice as tall as it is wide, so four columns by
// two rows per grid cell keeps the map's square grid square on screen: an
// east-west league is a seven-glyph run and a diagonal is one glyph in the
// row between
const CELL_W: usize = 4;
const CELL_H: usize = 2;
// blank space around the grid so labels at the edge have room
const MARGIN_X: usize = 14;
const MARGIN_Y: usize = 1;

/// What a canvas cell is part of; decides its style. Ordered weakest first so
/// a crossing keeps the more important league's paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Paint {
    Sea,
    /// A league on a route with no chart to buy.
    Dotted,
    /// A league on a route whose chart can be bought.
    Solid,
    /// A league between two memorized points: sailable from memory.
    Known,
    Point,
    PointKnown,
    Island,
    IslandKnown,
    Region,
    Name,
}

impl Paint {
    fn style(self) -> Style {
        match self {
            Paint::Sea => Style::default(),
            Paint::Dotted => Style::default().fg(Color::DarkGray),
            Paint::Solid | Paint::Point => Style::default().fg(Color::Gray),
            Paint::Known | Paint::PointKnown | Paint::IslandKnown => {
                Style::default().fg(Color::Yellow).bold()
            }
            Paint::Island => Style::default().fg(Color::Cyan).bold(),
            Paint::Region => Style::default().fg(Color::DarkGray).italic(),
            Paint::Name => Style::default().fg(Color::White),
        }
    }
}

struct Canvas {
    w: usize,
    h: usize,
    cells: Vec<(char, Paint)>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            cells: vec![(' ', Paint::Sea); w * h],
        }
    }

    fn idx(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.w && y < self.h).then_some(y * self.w + x)
    }

    fn get(&self, x: usize, y: usize) -> Option<(char, Paint)> {
        self.idx(x, y).map(|i| self.cells[i])
    }

    fn put(&mut self, x: usize, y: usize, ch: char, paint: Paint) {
        if let Some(i) = self.idx(x, y) {
            self.cells[i] = (ch, paint);
        }
    }

    fn is_free(&self, x: usize, y: usize) -> bool {
        self.get(x, y).is_some_and(|(ch, _)| ch == ' ')
    }

    /// How many of the `n` cells from `(x, y)` eastward are free before the
    /// first taken one. Zero when the run would start off the canvas.
    fn free_run(&self, x: usize, y: usize, n: usize) -> usize {
        (0 .. n).take_while(|i| self.is_free(x + i, y)).count()
    }

    /// Lay a diagonal glyph; crossing the other diagonal makes an X with the
    /// stronger paint, so a memorized route stays visible through it.
    fn put_diagonal(&mut self, x: usize, y: usize, ch: char, paint: Paint) {
        let Some((old, old_paint)) = self.get(x, y) else {
            return;
        };
        let (ch, paint) = if old == ' ' {
            (ch, paint)
        } else {
            ('╳', paint.max(old_paint))
        };
        self.put(x, y, ch, paint);
    }

    fn write(&mut self, x: usize, y: usize, text: &str, paint: Paint) {
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i, y, ch, paint);
        }
    }

    /// Place `text` next to the point at `(cx, cy)`: to its right, else to
    /// its left, else below, else above - the first spot it fits whole,
    /// otherwise the roomiest one, cut to fit. A label never touches a
    /// league glyph, so the routes stay readable.
    fn label(&mut self, cx: usize, cy: usize, text: &str, paint: Paint) {
        let n = text.chars().count();
        // (start x, y, cell that must also be free so the label doesn't abut
        // the point or a league)
        let right = (cx + 2, cy, (cx + 1, cy));
        let left = (
            cx.wrapping_sub(n + 1),
            cy,
            (cx.wrapping_sub(1), cy),
        );
        let below = (
            cx.wrapping_sub(n / 2),
            cy + 1,
            (cx, cy + 1),
        );
        let above = (
            cx.wrapping_sub(n / 2),
            cy.wrapping_sub(1),
            (cx, cy.wrapping_sub(1)),
        );
        let mut best: Option<(usize, usize, usize)> = None;
        for (x, y, (gx, gy)) in [right, left, below, above] {
            if !self.is_free(gx, gy) {
                continue;
            }
            let fit = self.free_run(x, y, n);
            if fit == n {
                self.write(x, y, text, paint);
                return;
            }
            if best.is_none_or(|(_, _, b)| b < fit) {
                best = Some((x, y, fit));
            }
        }
        if let Some((x, y, fit)) = best
            && 3 <= fit
        {
            let cut: String = text.chars().take(fit).collect();
            self.write(x, y, &cut, paint);
        }
    }
}

/// Canvas cell of a grid point.
fn cell_of((x, y): Point) -> (usize, usize) {
    (
        x as usize * CELL_W + MARGIN_X,
        y as usize * CELL_H + MARGIN_Y,
    )
}

/// An island's label: the name without its " Island" suffix, which the
/// diamond already says.
fn island_label(name: &str) -> &str {
    name.strip_suffix(" Island").unwrap_or(name)
}

fn build_canvas(map: &Map, app: &MapApp) -> Canvas {
    let (max_x, max_y) = map.extent();
    let mut canvas = Canvas::new(
        max_x as usize * CELL_W + 1 + 2 * MARGIN_X,
        max_y as usize * CELL_H + 1 + 2 * MARGIN_Y,
    );

    for league in map.leagues {
        let (a, b) = league.ends();
        let paint = if app.sailable(league) {
            Paint::Known
        } else if league.solid {
            Paint::Solid
        } else {
            Paint::Dotted
        };
        let (ax, ay) = cell_of(a);
        let (bx, by) = cell_of(b);
        match league.heading {
            Heading::E => {
                let dash = if paint == Paint::Dotted { '┄' } else { '─' };
                for x in ax + 1 .. bx {
                    canvas.put(x, ay, dash, paint);
                }
            }
            // the diagonal's single glyph sits in the row between its ends,
            // centred between their columns
            Heading::Se => {
                canvas.put_diagonal((ax + bx) / 2, ay + 1, '╲', paint)
            }
            Heading::Ne => {
                canvas.put_diagonal((ax + bx) / 2, by + 1, '╱', paint)
            }
            _ => {}
        }
    }

    let points: BTreeSet<Point> = map.points();
    for &p in &points {
        let (x, y) = cell_of(p);
        let known = app.memorized.contains(&p);
        let (ch, paint) = match (map.island_at(p).is_some(), known) {
            (true, true) => ('◆', Paint::IslandKnown),
            (true, false) => ('◇', Paint::Island),
            (false, true) => ('●', Paint::PointKnown),
            (false, false) => ('○', Paint::Point),
        };
        canvas.put(x, y, ch, paint);
    }

    for island in map.islands {
        let (x, y) = cell_of(island.at());
        canvas.label(
            x,
            y,
            island_label(island.name),
            Paint::Name,
        );
    }
    for region in map.labels {
        let (x, y) = cell_of(region.at());
        canvas.label(x, y, region.name, Paint::Region);
    }
    canvas
}

/// Where a viewport of `len` cells starts so that `focus` sits in its middle,
/// never scrolling past either end of the canvas.
fn origin(focus: usize, len: usize, total: usize) -> usize {
    if total <= len {
        0
    } else {
        focus.saturating_sub(len / 2).min(total - len)
    }
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    map: Option<&'static Map>,
    ocean: Option<&str>,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let border = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(offset_title("Map").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // two one-line rows under the map: the cursor's point (or the search
    // box) with the ocean at the right, then its leagues with the help hint
    // at the right. Each side keeps its own width so the rows fit an
    // 80-column terminal without the two halves running into each other.
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let mut first: Line = Line::from("");
    let mut second: Line = Line::from("");
    match (ocean, map) {
        (None, _) => {
            frame.render_widget(
                Paragraph::new("Select an ocean (--ocean) to see its map.")
                    .centered(),
                rows[0],
            );
        }
        (Some(ocean), None) => {
            frame.render_widget(
                Paragraph::new(format!(
                    "No map for {ocean} yet. See scripts/extract_map.py to \
                     add one."
                ))
                .centered(),
                rows[0],
            );
        }
        (Some(_), Some(map)) => {
            draw_map(frame, rows[0], app, map, regions);
            first = match &app.search {
                Some(search) => {
                    let hit =
                        app.search_hit(map).map_or("no match", |p| p.name);
                    let label = "Search: ";
                    if focused {
                        let x = rows[1].x
                            + label.len() as u16
                            + search.value[.. search.cursor].chars().count()
                                as u16;
                        frame.set_cursor_position((x, rows[1].y));
                    }
                    Line::from(vec![
                        Span::styled(label, Style::default().bold()),
                        Span::styled(
                            search.value.clone(),
                            Style::default().bg(Color::White).fg(Color::Black),
                        ),
                        Span::styled(
                            format!("  -> {hit}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                }
                None => point_line(app, map),
            };
            second = leagues_line(app, map);
        }
    }

    let ocean_tag = Span::styled(
        ocean.unwrap_or("").to_owned(),
        Style::default().fg(Color::DarkGray),
    );
    let help_tag = Span::styled(
        "Press ? for Help",
        Style::default().fg(Color::DarkGray),
    );
    for (row, left, right) in
        [(rows[1], first, ocean_tag), (rows[2], second, help_tag)]
    {
        let cols = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(right.width() as u16 + 2),
        ])
        .split(row);
        frame.render_widget(Paragraph::new(left), cols[0]);
        frame.render_widget(
            Paragraph::new(right).right_aligned(),
            cols[1],
        );
    }

    if app.help {
        render_help(frame, area, regions);
    }
}

/// The cursor's point: its name, grid cell and memorized state.
fn point_line(app: &mut MapApp, map: &'static Map) -> Line<'static> {
    let Some(p) = app.cursor_on(map) else {
        return Line::from("The map has no islands.");
    };
    let name = map.island_at(p).map_or("Open sea", |i| i.name);
    let mark = if app.memorized.contains(&p) {
        Span::styled(
            "memorized",
            Style::default().fg(Color::Yellow).bold(),
        )
    } else {
        Span::styled(
            "not memorized",
            Style::default().fg(Color::DarkGray),
        )
    };
    Line::from(vec![
        Span::styled(name, Style::default().bold()),
        Span::raw(format!(" ({},{})  ", p.0, p.1)),
        mark,
    ])
}

/// The leagues leaving the cursor's point, each as its heading and the line
/// glyph the map draws it with (so the legend in the help applies here too).
fn leagues_line(app: &mut MapApp, map: &'static Map) -> Line<'static> {
    let Some(p) = app.cursor_on(map) else {
        return Line::from("");
    };
    let mut spans = Vec::new();
    for (heading, _, league) in map.leagues_at(p) {
        let (glyph, paint) = if app.sailable(league) {
            ("━", Paint::Known)
        } else if league.solid {
            ("─", Paint::Solid)
        } else {
            ("┄", Paint::Dotted)
        };
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::raw(format!(
            "{} ",
            heading.label()
        )));
        spans.push(Span::styled(glyph, paint.style()));
    }
    Line::from(spans)
}

/// The `?` popup: the keys and the glyph legend. The backdrop is a click
/// target so a click anywhere outside the box closes it; it is pushed after
/// the page's own regions so it takes precedence over them.
fn render_help(frame: &mut Frame, area: Rect, regions: &mut Vec<ClickRegion>) {
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::MapHelpClose,
    });

    let key = |k: &str| Span::styled(k.to_owned(), Style::default().bold());
    let dim = |s: &str| {
        Span::styled(
            s.to_owned(),
            Style::default().fg(Color::DarkGray),
        )
    };
    // the movement keys as they sit on the keyboard, three per row
    let compass: Vec<Line> = MOVE_KEYS
        .chunks(3)
        .map(|row| {
            let mut spans = vec![Span::raw("  ")];
            for m in row {
                spans.push(key(&m.key.to_string()));
                spans.push(Span::raw(format!(" {:<5}", m.label)));
            }
            Line::from(spans)
        })
        .collect();
    let mut lines = vec![Line::from(Span::styled(
        "Sailing the cursor",
        Style::default().bold(),
    ))];
    lines.extend(compass);
    lines.extend([
        Line::from(dim(
            "  a/d/w/x with no league their way take the one diagonal on"
        )),
        Line::from(dim(
            "  that side, and stay put when both diagonals exist."
        )),
        Line::from(""),
        Line::from(vec![
            key("Space"),
            Span::raw("  mark the league point under the cursor as memorized"),
        ]),
        Line::from(vec![
            key("/"),
            Span::raw("      search for an island (Enter jumps, Esc cancels)"),
        ]),
        Line::from(vec![
            key("click"),
            Span::raw("  put the cursor on a league point"),
        ]),
        Line::from(vec![
            key("Up"),
            Span::raw("     back to the top bar"),
        ]),
        Line::from(vec![
            key("?"),
            Span::raw("      close this help"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Reading the map",
            Style::default().bold(),
        )),
        Line::from(vec![
            Span::styled("  ◇", Paint::Island.style()),
            Span::raw(" island    "),
            Span::styled("○", Paint::Point.style()),
            Span::raw(" open-sea league point    "),
            Span::styled("◆ ●", Paint::IslandKnown.style()),
            Span::raw(" memorized"),
        ]),
        Line::from(vec![
            Span::styled("  ───", Paint::Solid.style()),
            Span::raw(" chart can be bought    "),
            Span::styled("┄┄┄", Paint::Dotted.style()),
            Span::raw(" sail from memory"),
        ]),
        Line::from(vec![
            Span::styled("  ━━━", Paint::Known.style()),
            Span::raw(" both ends memorized: sailable from memory"),
        ]),
    ]);

    let width = 66u16.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::White))
                .title(offset_title("Help").0),
        ),
        popup,
    );
}

/// Blit the part of the canvas around the cursor into `view`, with the cursor
/// cell inverted, and register every visible point as a click target.
fn draw_map(
    frame: &mut Frame,
    view: Rect,
    app: &mut MapApp,
    map: &'static Map,
    regions: &mut Vec<ClickRegion>,
) {
    let Some(cursor) = app.cursor_on(map) else {
        return;
    };
    let canvas = build_canvas(map, app);
    let (cx, cy) = cell_of(cursor);
    let (vw, vh) = (
        view.width as usize,
        view.height as usize,
    );
    let ox = origin(cx, vw, canvas.w);
    let oy = origin(cy, vh, canvas.h);

    let buf = frame.buffer_mut();
    for vy in 0 .. vh {
        for vx in 0 .. vw {
            let (ch, paint) =
                canvas.get(ox + vx, oy + vy).unwrap_or((' ', Paint::Sea));
            let style = if (ox + vx, oy + vy) == (cx, cy) {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                paint.style()
            };
            if let Some(cell) = buf.cell_mut(Position::new(
                view.x + vx as u16,
                view.y + vy as u16,
            )) {
                cell.set_char(ch).set_style(style);
            }
        }
    }

    for p in map.points() {
        let (px, py) = cell_of(p);
        if ox <= px && px < ox + vw && oy <= py && py < oy + vh {
            regions.push(ClickRegion {
                rect: Rect::new(
                    view.x + (px - ox) as u16,
                    view.y + (py - oy) as u16,
                    1,
                    1,
                ),
                target: ClickTarget::MapPoint {
                    x: p.0,
                    y: p.1,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_avoid_taken_cells_and_prefer_the_right_side() {
        let mut canvas = Canvas::new(30, 3);
        canvas.put(10, 1, '◇', Paint::Island);
        canvas.label(10, 1, "Foo", Paint::Name);
        assert_eq!(
            canvas.get(12, 1),
            Some(('F', Paint::Name))
        );
        // an east league blocks the right side, so the label goes left
        let mut canvas = Canvas::new(30, 3);
        canvas.put(10, 1, '◇', Paint::Island);
        canvas.put(11, 1, '─', Paint::Solid);
        canvas.label(10, 1, "Foo", Paint::Name);
        assert_eq!(
            canvas.get(6, 1),
            Some(('F', Paint::Name))
        );
        assert_eq!(
            canvas.get(8, 1),
            Some(('o', Paint::Name))
        );
        assert!(canvas.is_free(9, 1));
    }

    #[test]
    fn a_cramped_label_is_cut_rather_than_overwriting_a_league() {
        let mut canvas = Canvas::new(30, 3);
        canvas.put(4, 1, '◇', Paint::Island);
        // leagues on both sides and a diagonal below: only "above" has room,
        // and only five cells of it between the two points
        canvas.put(5, 1, '─', Paint::Solid);
        canvas.put(3, 1, '─', Paint::Solid);
        canvas.put(4, 2, '╲', Paint::Solid);
        canvas.put(0, 0, '○', Paint::Point);
        canvas.put(6, 0, '○', Paint::Point);
        canvas.label(4, 1, "Barbary", Paint::Name);
        let row: String =
            (0 .. 8).map(|x| canvas.get(x, 0).unwrap().0).collect();
        assert_eq!(row, "○Barba○ ");
    }

    #[test]
    fn crossing_diagonals_become_an_x_with_the_stronger_paint() {
        let mut canvas = Canvas::new(3, 3);
        canvas.put_diagonal(1, 1, '╲', Paint::Dotted);
        canvas.put_diagonal(1, 1, '╱', Paint::Known);
        assert_eq!(
            canvas.get(1, 1),
            Some(('╳', Paint::Known))
        );
    }

    #[test]
    fn viewport_origin_centres_the_focus_without_overscrolling() {
        assert_eq!(origin(5, 20, 10), 0);
        assert_eq!(origin(50, 20, 100), 40);
        assert_eq!(origin(3, 20, 100), 0);
        assert_eq!(origin(99, 20, 100), 80);
    }
}
