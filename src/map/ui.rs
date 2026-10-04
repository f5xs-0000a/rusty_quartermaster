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
    bare,
    clickmap::{ClickRegion, ClickTarget},
    map::{
        MOVE_KEYS,
        MapApp,
        data::{Heading, Map, Point},
    },
    utils::offset_title,
};

/// Terminal width from which the metadata column is shown beside the map.
const METADATA_MIN_WIDTH: u16 = 100;
/// Width of the metadata column, borders included.
const METADATA_WIDTH: u16 = 32;

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

    /// The canvas as text, one line per row, for looking at the drawing.
    #[cfg(test)]
    fn dump(&self) -> String {
        self.cells
            .chunks(self.w)
            .map(|row| row.iter().map(|(ch, _)| *ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether an `n`-cell label can sit at `(x, y)` with [`LABEL_GAP`]
    /// blank cells on both sides. The gap may run off the canvas edge; the
    /// label may not.
    fn fits(&self, x: usize, y: usize, n: usize) -> bool {
        if self.w < x + n || self.h <= y {
            return false;
        }
        let lo = x.saturating_sub(LABEL_GAP);
        let hi = (x + n + LABEL_GAP).min(self.w);
        (lo .. hi).all(|i| self.is_free(i, y))
    }

    /// Place `text` for the point at `(cx, cy)` in the first spot of
    /// [`label_spots`] where it fits whole, keeping clear of every drawn
    /// glyph, the point's own included. With no such spot, the longest
    /// head of the name (three characters or more) that fits anywhere is
    /// written instead so the island stays findable. Returns whether the
    /// whole name was placed.
    fn label(
        &mut self,
        cx: usize,
        cy: usize,
        text: &str,
        paint: Paint,
    ) -> bool {
        let n = text.chars().count();
        for (x, y) in label_spots(cx, cy, n) {
            if self.fits(x, y, n) {
                self.write(x, y, text, paint);
                return true;
            }
        }
        for len in (3 .. n).rev() {
            for (x, y) in label_spots(cx, cy, len) {
                if self.fits(x, y, len) {
                    let head: String = text.chars().take(len).collect();
                    self.write(x, y, &head, paint);
                    return false;
                }
            }
        }
        false
    }
}

/// Blank cells kept between a label and anything drawn beside it on its row.
const LABEL_GAP: usize = 2;

/// Candidate top-left cells for an `n`-cell label of the point at `(cx,
/// cy)`, in order of preference: beside the point on its own row (right,
/// then left, each sliding a few cells further out), then the rows just
/// below and above, then two rows out, each starting centred on the point
/// and sliding alternately left and right until the label has cleared the
/// point entirely. Spots off the top or left edge are skipped.
fn label_spots(
    cx: usize,
    cy: usize,
    n: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let (cx, cy, n) = (cx as isize, cy as isize, n as isize);
    let gap = LABEL_GAP as isize;
    let beside = (0 ..= 4).flat_map(move |slide| {
        [(cx + 1 + gap + slide, cy), (cx - gap - n - slide, cy)]
    });
    let rows = [cy + 1, cy - 1, cy + 2, cy - 2];
    let around = rows.into_iter().flat_map(move |y| {
        let centred = cx - n / 2;
        std::iter::once((centred, y)).chain((1 ..= n).flat_map(move |slide| {
            [(centred - slide, y), (centred + slide, y)]
        }))
    });
    beside
        .chain(around)
        .filter(|&(x, y)| 0 <= x && 0 <= y)
        .map(|(x, y)| (x as usize, y as usize))
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

/// What the page knows about the selected ocean: its compiled-in map, its
/// geography, and its name. Any of them may be missing.
pub struct OceanContext {
    pub map: Option<&'static Map>,
    pub geo: Option<&'static bare::Ocean>,
    pub ocean: Option<&'static str>,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    ctx: OceanContext,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let OceanContext {
        map,
        geo,
        ocean,
    } = ctx;
    let border = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    // a wide enough terminal gets a metadata column beside the map; the
    // map keeps every column otherwise
    let (map_area, side) = if METADATA_MIN_WIDTH <= area.width {
        let cols = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(METADATA_WIDTH),
        ])
        .split(area);
        (cols[0], Some(cols[1]))
    } else {
        (area, None)
    };
    if let (Some(side), Some(map)) = (side, map) {
        render_metadata(frame, side, app, map, geo, border);
    }
    let area = map_area;

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

/// The metadata column: what the geography knows about the island under
/// the cursor, with the memorized league-point tally pinned at the bottom.
fn render_metadata(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    map: &'static Map,
    geo: Option<&'static bare::Ocean>,
    border: Style,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(offset_title("Island").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);
    if let Some(p) = app.cursor_on(map) {
        frame.render_widget(
            Paragraph::new(metadata_lines(app, map, geo, p)),
            rows[0],
        );
    }
    let (known, total) = memorized_tally(app, map);
    let percent = if total == 0 {
        0.0
    } else {
        100.0 * known as f64 / total as f64
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Memorized league points",
                Style::default().bold(),
            )),
            Line::from(format!(
                "{known} / {total}  ({percent:.1}%)"
            )),
        ]),
        rows[1],
    );
}

/// Memorized league points on this map, and how many there are in all.
/// Marks that no longer match a point (a redrawn map) are not counted.
fn memorized_tally(app: &MapApp, map: &Map) -> (usize, usize) {
    let points = map.points();
    let known = points.iter().filter(|p| app.memorized.contains(p)).count();
    (known, points.len())
}

/// The metadata lines for point `p`: name, size and status, memorized
/// state, then what the island produces, what its archipelago forages,
/// and the gems it buys. Open sea only has a name and a memorized state.
fn metadata_lines(
    app: &MapApp,
    map: &'static Map,
    geo: Option<&'static bare::Ocean>,
    p: Point,
) -> Vec<Line<'static>> {
    let bold = |s: String| Span::styled(s, Style::default().bold());
    let dim =
        |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    let memorized = if app.memorized.contains(&p) {
        Span::styled(
            "Memorized",
            Style::default().fg(Color::Yellow).bold(),
        )
    } else {
        Span::styled(
            "Not memorized",
            Style::default().fg(Color::DarkGray),
        )
    };

    let Some(island) = map.island_at(p) else {
        return vec![
            Line::from(bold(format!(
                "Open sea ({},{})",
                p.0, p.1
            ))),
            Line::from(memorized),
        ];
    };
    let mut lines = vec![Line::from(bold(island.name.to_owned()))];
    let Some((arch, info)) = geo.and_then(|g| g.island(island.name)) else {
        lines.push(Line::from(memorized));
        lines.push(Line::from(""));
        lines.push(Line::from(dim(
            "No geography data for this island."
        )));
        return lines;
    };
    lines.push(Line::from(format!(
        "{}, {}",
        info.size.label(),
        info.status.label()
    )));
    lines.push(Line::from(memorized));

    let mut section =
        |title: &'static str, items: Vec<String>, none: &'static str| {
            lines.push(Line::from(""));
            lines.push(Line::from(bold(title.to_owned())));
            if items.is_empty() {
                lines.push(Line::from(dim(none)));
            }
            for item in items {
                lines.push(Line::from(format!("  {item}")));
            }
        };
    section(
        "Produces",
        info.spawns.clone(),
        "  nothing recorded",
    );
    section(
        "Forage",
        arch.forageables.clone(),
        "  nothing recorded",
    );
    // a known purchase is the only gem fact worth stating; not knowing of
    // one is not the same as knowing there is none, so nothing is shown
    // otherwise
    if !info.buys_gems.is_empty() {
        section(
            "Buys gems",
            info.buys_gems
                .iter()
                .map(|g| format!("{g} at {} PoE", bare::GEM_BUY_PRICE))
                .collect(),
            "",
        );
    }
    lines
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

    fn row(canvas: &Canvas, y: usize) -> String {
        (0 .. canvas.w)
            .map(|x| canvas.get(x, y).unwrap().0)
            .collect()
    }

    #[test]
    fn labels_keep_two_cells_from_the_point_and_prefer_the_right_side() {
        let mut canvas = Canvas::new(20, 3);
        canvas.put(10, 1, '◇', Paint::Island);
        assert!(canvas.label(10, 1, "Foo", Paint::Name));
        assert_eq!(row(&canvas, 1), "          ◇  Foo    ");
        // an east league blocks the right side, so the label goes left
        let mut canvas = Canvas::new(20, 3);
        canvas.put(10, 1, '◇', Paint::Island);
        canvas.put(11, 1, '─', Paint::Solid);
        assert!(canvas.label(10, 1, "Foo", Paint::Name));
        assert_eq!(row(&canvas, 1), "     Foo  ◇─        ");
    }

    #[test]
    fn a_boxed_in_label_slides_along_the_row_below() {
        // an island with full-length leagues east and west and a diagonal
        // leaving it south-east
        let mut canvas = Canvas::new(30, 3);
        canvas.put(12, 1, '◇', Paint::Island);
        for x in (5 .. 12).chain(13 .. 20) {
            canvas.put(x, 1, '─', Paint::Solid);
        }
        canvas.put(14, 2, '╲', Paint::Solid);
        assert!(canvas.label(12, 1, "Barbary", Paint::Name));
        // centred below would touch the diagonal; sliding left clears it
        assert_eq!(
            row(&canvas, 2),
            "     Barbary  ╲               "
        );
    }

    #[test]
    fn with_no_room_anywhere_the_label_is_cut_rather_than_dropped() {
        // the island's row is all league, the row below is blocked, and the
        // row above only has a five-cell gap between two points
        let mut canvas = Canvas::new(14, 3);
        canvas.put(4, 1, '◇', Paint::Island);
        for x in (0 .. 4).chain(5 .. 14) {
            canvas.put(x, 1, '─', Paint::Solid);
        }
        canvas.put(4, 2, '╲', Paint::Solid);
        canvas.put(10, 2, '○', Paint::Point);
        canvas.put(0, 0, '○', Paint::Point);
        canvas.put(8, 0, '○', Paint::Point);
        assert!(!canvas.label(4, 1, "Barbary", Paint::Name));
        assert_eq!(row(&canvas, 0), "○  Bar  ○     ");
    }

    /// Reads the drawing the way a player would: every island's name must
    /// appear whole somewhere on the Emerald canvas with two blank cells
    /// on either side of it.
    #[test]
    fn every_emerald_island_is_named_whole_with_room_around_it() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let canvas = build_canvas(map, &MapApp::new());
        let rows: Vec<Vec<char>> =
            canvas.dump().lines().map(|l| l.chars().collect()).collect();
        for island in map.islands {
            let label: Vec<char> = island_label(island.name).chars().collect();
            let n = label.len();
            let found = rows.iter().any(|r| {
                (0 .. r.len().saturating_sub(n)).any(|x| {
                    r[x .. x + n] == label[..]
                        && r[x.saturating_sub(LABEL_GAP) .. x]
                            .iter()
                            .all(|c| *c == ' ')
                        && r[x + n .. (x + n + LABEL_GAP).min(r.len())]
                            .iter()
                            .all(|c| *c == ' ')
                })
            });
            assert!(
                found,
                "{} is cut or crowded",
                island.name
            );
        }
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

    /// Prints the Map page as a 120x40 terminal would show it, with the
    /// cursor on Cromwell Island:
    /// `cargo test dump_map_page -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the rendered Map page for inspection"]
    fn dump_map_page() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        let cromwell = map
            .islands
            .iter()
            .find(|i| i.name == "Cromwell Island")
            .expect("Cromwell on the map");
        app.jump_to(cromwell.at());
        app.memorized.insert(cromwell.at());
        let mut terminal =
            Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        let mut regions = Vec::new();
        terminal
            .draw(|frame| {
                let ctx = OceanContext {
                    map: Some(map),
                    geo: bare::BARE.ocean("Emerald"),
                    ocean: Some("Emerald"),
                };
                render(
                    frame,
                    frame.area(),
                    &mut app,
                    ctx,
                    true,
                    &mut regions,
                );
            })
            .expect("draw");
        println!("{}", terminal.backend());
    }

    /// Prints the whole Emerald canvas, for eyeballing the drawing:
    /// `cargo test dump_emerald_canvas -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the Emerald canvas for inspection"]
    fn dump_emerald_canvas() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let canvas = build_canvas(map, &MapApp::new());
        println!("{}", canvas.dump());
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn metadata_reads_the_island_out_of_the_geography() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let geo = bare::BARE.ocean("Emerald");
        let mut app = MapApp::new();
        let cromwell = map
            .islands
            .iter()
            .find(|i| i.name == "Cromwell Island")
            .expect("Cromwell on the map");
        app.memorized.insert(cromwell.at());
        let lines = text(&metadata_lines(
            &app,
            map,
            geo,
            cromwell.at(),
        ));
        assert_eq!(lines[0], "Cromwell Island");
        assert_eq!(lines[1], "outpost, colonized");
        assert_eq!(lines[2], "Memorized");
        assert!(lines.contains(&"Produces".to_owned()));
        assert!(lines.contains(&"  Sugar cane".to_owned()));
        assert!(lines.contains(&"Forage".to_owned()));
        // no purchase is known for Cromwell, so gems go unmentioned
        assert!(!lines.contains(&"Buys gems".to_owned()));
        // Alkaid is amber's destination
        let alkaid = map
            .islands
            .iter()
            .find(|i| i.name == "Alkaid Island")
            .expect("Alkaid on the map");
        let lines = text(&metadata_lines(
            &app,
            map,
            geo,
            alkaid.at(),
        ));
        assert!(lines.contains(&"Buys gems".to_owned()));
        assert!(lines.contains(&"  Amber at 1000 PoE".to_owned()));
        // open sea has no island data, only a name and the mark
        let sea = text(&metadata_lines(&app, map, geo, (2, 9)));
        assert_eq!(sea, ["Open sea (2,9)", "Not memorized"]);
    }

    #[test]
    fn the_tally_counts_marks_that_are_on_the_map() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        let total = map.points().len();
        assert_eq!(memorized_tally(&app, map), (0, total));
        app.memorized.insert((1, 8));
        app.memorized.insert((2, 9));
        // a stale mark off the map is ignored
        app.memorized.insert((60000, 60000));
        assert_eq!(memorized_tally(&app, map), (2, total));
    }

    #[test]
    fn viewport_origin_centres_the_focus_without_overscrolling() {
        assert_eq!(origin(5, 20, 10), 0);
        assert_eq!(origin(50, 20, 100), 40);
        assert_eq!(origin(3, 20, 100), 0);
        assert_eq!(origin(99, 20, 100), 80);
    }
}
