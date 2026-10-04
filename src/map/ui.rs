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
    islands::CachedIslands,
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
    /// A league on a route whose chart is not sold: it only drops as booty.
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

    /// The cheapest spot of [`label_spots`] where an `n`-cell label of the
    /// point at `(cx, cy)` fits, keeping clear of every drawn glyph, the
    /// point's own included.
    fn best_spot(
        &self,
        cx: usize,
        cy: usize,
        n: usize,
    ) -> Option<(usize, usize)> {
        label_spots(cx, cy, n)
            .filter(|&(x, y, _)| self.fits(x, y, n))
            .min_by_key(|&(_, _, cost)| cost)
            .map(|(x, y, _)| (x, y))
    }

    /// Place `text` for the point at `(cx, cy)` in the spot that keeps it
    /// nearest the point. With no spot for the whole name, the longest
    /// head of it (three characters or more) that fits anywhere is written
    /// instead so the island stays findable. Returns whether the whole
    /// name was placed.
    fn label(
        &mut self,
        cx: usize,
        cy: usize,
        text: &str,
        paint: Paint,
    ) -> bool {
        let n = text.chars().count();
        if let Some((x, y)) = self.best_spot(cx, cy, n) {
            self.write(x, y, text, paint);
            return true;
        }
        for len in (3 .. n).rev() {
            if let Some((x, y)) = self.best_spot(cx, cy, len) {
                let head: String = text.chars().take(len).collect();
                self.write(x, y, &head, paint);
                return false;
            }
        }
        false
    }
}

/// Blank cells kept between a label and anything drawn beside it on its row.
const LABEL_GAP: usize = 2;

/// Cells a point's click box extends past its glyph on each side.
const CLICK_REACH: usize = 1;

/// What a label's placement costs, in cells of drift from its point: a row
/// step counts this much sideways drift, so a label goes to the row above
/// or below only when sliding along the nearer row would carry it further
/// from the point than that.
const ROW_STEP_COST: usize = 3;

/// Candidate top-left cells for an `n`-cell label of the point at `(cx,
/// cy)`, each with its cost: beside the point on its own row (right or
/// left, sliding up to four cells further out), and on the rows one and
/// two steps above and below, centred on the point and sliding either way
/// until the label has cleared it. Sitting right beside the point is the
/// cheapest spot, then a centred spot on the next row, then the rest by
/// drift. Spots off the top or left edge are skipped; ties go to the
/// earlier candidate (right before left, below before above).
fn label_spots(
    cx: usize,
    cy: usize,
    n: usize,
) -> impl Iterator<Item = (usize, usize, usize)> {
    let (cx, cy, n) = (cx as isize, cy as isize, n as isize);
    let gap = LABEL_GAP as isize;
    let beside = (0 ..= 4).flat_map(move |slide| {
        let cost = 2 + slide as usize;
        [
            (cx + 1 + gap + slide, cy, cost),
            (cx - gap - n - slide, cy, cost),
        ]
    });
    let rows = [(cy + 1, 1), (cy - 1, 1), (cy + 2, 2), (cy - 2, 2)];
    let around = rows.into_iter().flat_map(move |(y, steps)| {
        let centred = cx - n / 2;
        let base = ROW_STEP_COST * steps;
        std::iter::once((centred, y, base)).chain((1 ..= n).flat_map(
            move |slide| {
                let cost = base + slide as usize;
                [(centred - slide, y, cost), (centred + slide, y, cost)]
            },
        ))
    });
    beside
        .chain(around)
        .filter(|&(x, y, _)| 0 <= x && 0 <= y)
        .map(|(x, y, cost)| (x as usize, y as usize, cost))
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
/// geography, its name, and yoweb's island list. Any of them may be
/// missing.
pub struct OceanContext<'a> {
    pub map: Option<&'static Map>,
    pub geo: Option<&'static bare::Ocean>,
    pub ocean: Option<&'static str>,
    pub islands: Option<&'a CachedIslands>,
    /// Whether the island list is being fetched right now.
    pub fetching_islands: bool,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    ctx: OceanContext<'_>,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let OceanContext {
        map,
        geo,
        ocean,
        islands,
        fetching_islands,
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
        let sources = Sources {
            geo,
            islands,
            fetching_islands,
        };
        render_metadata(frame, side, app, map, sources, border);
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
/// Where the metadata column's facts come from: the compiled-in geography
/// and yoweb's island list, with whether the latter is on its way.
struct Sources<'a> {
    geo: Option<&'static bare::Ocean>,
    islands: Option<&'a CachedIslands>,
    fetching_islands: bool,
}

fn render_metadata(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    map: &'static Map,
    sources: Sources<'_>,
    border: Style,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(offset_title("Island").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .split(inner);
    if let Some(p) = app.cursor_on(map) {
        frame.render_widget(
            Paragraph::new(metadata_lines(app, map, &sources, p)),
            rows[0],
        );
    }
    if sources.fetching_islands {
        frame.render_widget(
            Paragraph::new("Fetching island info...")
                .style(Style::default().fg(Color::DarkGray)),
            rows[1],
        );
    }
    // memorization belongs to a pirate: with none loaded there is no one
    // whose knowledge the tally could count, so say what it would take
    if app.pirate.is_none() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Memorizing needs a pirate:",
                    Style::default().bold(),
                )),
                Line::from("name one with --user."),
            ])
            .style(Style::default().fg(Color::DarkGray)),
            rows[2],
        );
        return;
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
        rows[2],
    );
}

/// Memorized league points on this map, and how many there are in all.
/// Marks that no longer match a point (a redrawn map) are not counted.
fn memorized_tally(app: &MapApp, map: &Map) -> (usize, usize) {
    let points = map.points();
    let known = points.iter().filter(|p| app.memorized.contains(p)).count();
    (known, points.len())
}

/// The memorized state of `p` for the loaded pirate, or nothing when no
/// pirate is loaded (the state is a pirate's, not the map's).
fn memorized_mark(app: &MapApp, p: Point) -> Option<Span<'static>> {
    app.pirate.as_ref()?;
    Some(
        if app.memorized.contains(&p) {
            Span::styled(
                "Memorized",
                Style::default().fg(Color::Yellow).bold(),
            )
        } else {
            Span::styled(
                "Not memorized",
                Style::default().fg(Color::DarkGray),
            )
        },
    )
}

/// The metadata lines for point `p`: name, size and status, memorized
/// state, then what the island produces, what its archipelago forages,
/// and the gems it buys. Open sea only has a name and a memorized state.
fn metadata_lines(
    app: &MapApp,
    map: &'static Map,
    sources: &Sources<'_>,
    p: Point,
) -> Vec<Line<'static>> {
    let bold = |s: String| Span::styled(s, Style::default().bold());
    let dim =
        |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    let memorized = memorized_mark(app, p).map(Line::from);

    let Some(island) = map.island_at(p) else {
        let mut lines = vec![Line::from(bold(format!(
            "Open sea ({},{})",
            p.0, p.1
        )))];
        lines.extend(memorized);
        return lines;
    };
    let mut lines = vec![Line::from(bold(island.name.to_owned()))];
    let Some((arch, info)) = sources.geo.and_then(|g| g.island(island.name))
    else {
        lines.extend(memorized);
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
    lines.extend(memorized);

    // every fact below is shown only once it is known: a section that has
    // nothing to say is left out rather than saying so
    let mut section = |title: &'static str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(bold(title.to_owned())));
        for item in items {
            lines.push(Line::from(format!("  {item}")));
        }
    };
    // what yoweb says about the island, once its list has been fetched
    if let Some(yoweb) = sources.islands.and_then(|c| c.get(island.name)) {
        let mut facts = Vec::new();
        if let Some(governor) = &yoweb.governor {
            facts.push(format!("Governor  {governor}"));
        }
        if let Some(flag) = &yoweb.flag {
            facts.push(format!("Ruled by  {flag}"));
        }
        if let Some(tax) = yoweb.property_tax {
            facts.push(format!("Property tax  {tax}%"));
        }
        section("Colony", facts);
        section("Exports", yoweb.exports.clone());
    }
    section("Forage", arch.forageables.clone());
    section(
        "Buys gems",
        info.buys_gems
            .iter()
            .map(|g| format!("{g} at {} PoE", bare::GEM_BUY_PRICE))
            .collect(),
    );
    lines
}

/// The cursor's point: its name, grid cell and memorized state.
fn point_line(app: &mut MapApp, map: &'static Map) -> Line<'static> {
    let Some(p) = app.cursor_on(map) else {
        return Line::from("The map has no islands.");
    };
    let name = map.island_at(p).map_or("Open sea", |i| i.name);
    let mut spans = vec![
        Span::styled(name, Style::default().bold()),
        Span::raw(format!(" ({},{})", p.0, p.1)),
    ];
    if let Some(mark) = memorized_mark(app, p) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            mark.content.to_lowercase(),
            mark.style,
        ));
    }
    Line::from(spans)
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
        Line::from(dim(
            "  what is memorized belongs to the pirate given by --user."
        )),
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
            Span::raw(" chart is sold    "),
            Span::styled("┄┄┄", Paint::Dotted.style()),
            Span::raw(" chart only drops as booty"),
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

    // each point's click box reaches one cell past its glyph on every side,
    // clipped to the viewport; points are at least four cells apart along
    // either axis, so the boxes never overlap
    let viewport = Rect::new(
        ox as u16, oy as u16, vw as u16, vh as u16,
    );
    for p in map.points() {
        let (px, py) = cell_of(p);
        let around = Rect::new(
            px.saturating_sub(CLICK_REACH) as u16,
            py.saturating_sub(CLICK_REACH) as u16,
            (2 * CLICK_REACH + 1) as u16,
            (2 * CLICK_REACH + 1) as u16,
        )
        .intersection(viewport);
        if around.is_empty() {
            continue;
        }
        regions.push(ClickRegion {
            rect: Rect::new(
                view.x + around.x - viewport.x,
                view.y + around.y - viewport.y,
                around.width,
                around.height,
            ),
            target: ClickTarget::MapPoint {
                x: p.0,
                y: p.1,
            },
        });
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
    fn a_boxed_in_label_takes_the_nearest_free_row() {
        // an island with full-length leagues east and west, diagonals
        // leaving it both ways below, and a clear row above
        let mut canvas = Canvas::new(30, 4);
        canvas.put(12, 2, '◇', Paint::Island);
        for x in (5 .. 12).chain(13 .. 20) {
            canvas.put(x, 2, '─', Paint::Solid);
        }
        canvas.put(10, 3, '╱', Paint::Solid);
        canvas.put(14, 3, '╲', Paint::Solid);
        assert!(canvas.label(12, 2, "Barbary", Paint::Name));
        // below would have to slide nine cells to clear the diagonals;
        // centred above is nearer
        assert_eq!(
            row(&canvas, 1),
            "         Barbary              "
        );
        assert_eq!(
            row(&canvas, 3),
            "          ╱   ╲               "
        );
    }

    #[test]
    fn a_small_slide_on_the_nearer_row_beats_moving_a_row_away() {
        let mut canvas = Canvas::new(30, 3);
        canvas.put(12, 1, '◇', Paint::Island);
        for x in (5 .. 12).chain(13 .. 20) {
            canvas.put(x, 1, '─', Paint::Solid);
        }
        // the row above is taken by another league, and one diagonal sits
        // just right of centre below: a two-cell slide left on that row
        // is the nearest spot left
        for x in 4 .. 21 {
            canvas.put(x, 0, '─', Paint::Solid);
        }
        canvas.put(16, 2, '╲', Paint::Solid);
        assert!(canvas.label(12, 1, "Barbary", Paint::Name));
        assert_eq!(
            row(&canvas, 2),
            "       Barbary  ╲             "
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
        app.pirate = Some("Someone".to_owned());
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
                    islands: None,
                    fetching_islands: true,
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

    #[test]
    fn click_boxes_reach_around_each_point_without_overlapping() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        app.cursor_on(map);
        let mut terminal =
            Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        let mut regions = Vec::new();
        terminal
            .draw(|frame| {
                let ctx = OceanContext {
                    map: Some(map),
                    geo: None,
                    ocean: Some("Emerald"),
                    islands: None,
                    fetching_islands: false,
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
        let boxes: Vec<Rect> = regions
            .iter()
            .filter(|r| matches!(r.target, ClickTarget::MapPoint { .. }))
            .map(|r| r.rect)
            .collect();
        assert!(!boxes.is_empty());
        // a box away from the edges is three cells each way, and no two
        // boxes share a cell
        assert!(boxes.iter().any(|b| b.width == 3 && b.height == 3));
        for (i, a) in boxes.iter().enumerate() {
            for b in &boxes[i + 1 ..] {
                assert!(
                    a.intersection(*b).is_empty(),
                    "{a:?} overlaps {b:?}"
                );
            }
        }
    }

    /// The column's bottom block is a pirate's tally once one is named, and
    /// the way to name one until then.
    #[test]
    fn the_column_asks_for_a_pirate_before_it_tallies() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        let draw = |app: &mut MapApp| {
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("test terminal");
            terminal
                .draw(|frame| {
                    let ctx = OceanContext {
                        map: Some(map),
                        geo: None,
                        ocean: Some("Emerald"),
                        islands: None,
                        fetching_islands: false,
                    };
                    render(
                        frame,
                        frame.area(),
                        app,
                        ctx,
                        true,
                        &mut Vec::new(),
                    );
                })
                .expect("draw");
            format!("{}", terminal.backend())
        };
        let screen = draw(&mut app);
        assert!(screen.contains("Memorizing needs a pirate:"));
        assert!(!screen.contains("Memorized league points"));
        app.pirate = Some("Someone".to_owned());
        let screen = draw(&mut app);
        assert!(screen.contains("Memorized league points"));
        assert!(!screen.contains("Memorizing needs a pirate:"));
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

    fn island(map: &'static Map, name: &str) -> Point {
        map.islands
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name} on the map"))
            .at()
    }

    #[test]
    fn metadata_reads_the_island_out_of_the_geography() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let sources = Sources {
            geo: bare::BARE.ocean("Emerald"),
            islands: None,
            fetching_islands: false,
        };
        let mut app = MapApp::new();
        let cromwell = island(map, "Cromwell Island");
        // the memorized state is a pirate's: with none loaded, no line
        let lines = text(&metadata_lines(
            &app,
            map,
            &sources,
            (2, 9),
        ));
        assert_eq!(lines, ["Open sea (2,9)"]);
        app.pirate = Some("Someone".to_owned());
        app.memorized.insert(cromwell);
        let lines = text(&metadata_lines(
            &app, map, &sources, cromwell,
        ));
        assert_eq!(lines[0], "Cromwell Island");
        assert_eq!(lines[1], "outpost, colonized");
        assert_eq!(lines[2], "Memorized");
        assert!(lines.contains(&"Forage".to_owned()));
        // nothing from yoweb has been fetched, and no purchase is known for
        // Cromwell: neither is mentioned
        assert!(!lines.contains(&"Colony".to_owned()));
        assert!(!lines.contains(&"Exports".to_owned()));
        assert!(!lines.contains(&"Buys gems".to_owned()));
        // Alkaid is amber's destination
        let alkaid = island(map, "Alkaid Island");
        let lines = text(&metadata_lines(
            &app, map, &sources, alkaid,
        ));
        assert!(lines.contains(&"Buys gems".to_owned()));
        assert!(lines.contains(&"  Amber at 1000 PoE".to_owned()));
        // open sea has no island data, only a name and the mark
        let sea = text(&metadata_lines(
            &app,
            map,
            &sources,
            (2, 9),
        ));
        assert_eq!(sea, ["Open sea (2,9)", "Not memorized"]);
    }

    #[test]
    fn metadata_shows_yoweb_facts_once_the_island_list_is_in() {
        use crate::islands::{CachedIslands, IslandInfo};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let list = CachedIslands {
            fetched_at: chrono::Utc::now(),
            islands: vec![IslandInfo {
                name: "Cromwell Island".to_owned(),
                governor: Some("Someone".to_owned()),
                flag: Some("Some Flag".to_owned()),
                property_tax: Some(15),
                exports: vec!["Hemp".to_owned(), "Iron".to_owned()],
            }],
        };
        let sources = Sources {
            geo: bare::BARE.ocean("Emerald"),
            islands: Some(&list),
            fetching_islands: false,
        };
        let app = MapApp::new();
        let lines = text(&metadata_lines(
            &app,
            map,
            &sources,
            island(map, "Cromwell Island"),
        ));
        assert!(lines.contains(&"Colony".to_owned()));
        assert!(lines.contains(&"  Governor  Someone".to_owned()));
        assert!(lines.contains(&"  Ruled by  Some Flag".to_owned()));
        assert!(lines.contains(&"  Property tax  15%".to_owned()));
        assert!(lines.contains(&"Exports".to_owned()));
        assert!(lines.contains(&"  Hemp".to_owned()));
        // an island yoweb does not list gets none of those lines
        let lines = text(&metadata_lines(
            &app,
            map,
            &sources,
            island(map, "Alkaid Island"),
        ));
        assert!(!lines.contains(&"Colony".to_owned()));
        assert!(!lines.contains(&"Exports".to_owned()));
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
