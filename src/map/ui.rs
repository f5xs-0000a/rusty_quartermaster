//! Rendering for the Map app: the map is drawn onto a character canvas
//! (points, leagues, labels), and the viewport shows the part of it around
//! the cursor.
//!
//! # The Island column
//!
//! Beside the chart, where the terminal is wide enough for both, is what is
//! known about the point under the cursor. How it is laid out is this page's
//! own business, not a convention any other page keeps:
//!
//! - A **head** of three centred lines says where the point is: its name, what
//!   kind of island it is, and the archipelago it lies in. It is pinned, so it
//!   is still there at the foot of a long island, and the name is red for a
//!   point the pirate has memorized.
//! - **Below the head** come the colony, what the island exports, what its
//!   archipelago forages and the gems its palace buys, each section left out
//!   when there is nothing to say. This is the part that scrolls, under the
//!   convention every scrolling view keeps (see `UI_CONVENTIONS.md`, Rule 4).
//! - Facts yoweb or the geography is the source of are **underlined**, so a
//!   value is told apart from the words that introduce it.
//! - A line too long for the column **wraps** with its continuation two columns
//!   further in, rather than being cut off.
//! - The column's **width** is [`metadata_width`]: the ocean's longest island
//!   name and kind of island, so neither ever wraps.

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
        data::{Chart, Heading, Map, Point},
    },
    utils::offset_title,
};

/// Columns the chart keeps for itself before the Island column may take any.
/// Narrower than this and the map gets the whole width.
const CHART_MIN_WIDTH: u16 = 68;

/// What the Island column says of its own accord, whichever point the cursor
/// is on: the notice while the island list is on its way. The column is never
/// narrower than it.
const FETCHING: &str = "Fetching island info...";

/// Columns the Island column takes, borders included: enough to print the
/// longest name any of the ocean's islands carries, and what kind of island any
/// of them is, without wrapping either.
///
/// The archipelago line is not measured. One long archipelago name would widen
/// the column for every point of the ocean, so that line wraps where it must,
/// as everything below the head does.
///
/// The figure is the ocean's, not the cursor's: a column measured against the
/// island under the cursor would change width as the cursor sailed.
fn metadata_width(geo: Option<&'static bare::Ocean>) -> u16 {
    let mut longest = FETCHING.chars().count();
    if let Some(ocean) = geo {
        for arch in &ocean.archipelagos {
            for isle in &arch.islands {
                longest = longest.max(isle.name.chars().count());
                longest = longest.max(
                    format!(
                        "{} {} island",
                        isle.status.label(),
                        isle.size.label()
                    )
                    .chars()
                    .count(),
                );
            }
        }
    }
    // the scrollbar's columns, the block's padding and its borders all sit
    // outside the text
    longest as u16 + crate::utils::SCROLLBAR_W + 2 + 2
}

// a terminal cell is about twice as tall as it is wide, so four columns by
// two rows per grid cell keeps the map's square grid square on screen: an
// east-west league is a seven-glyph run and a diagonal is one glyph in the
// row between
pub(crate) const CELL_W: usize = 4;
pub(crate) const CELL_H: usize = 2;
// blank space around the grid so labels at the edge have room
const MARGIN_X: usize = 14;
const MARGIN_Y: usize = 1;

/// What a canvas cell is part of; decides its style. Ordered weakest first so
/// a crossing keeps the more important league's paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Paint {
    Sea,
    /// A league whose chart is not sold: it drops as booty. A league with
    /// no chart at all is not painted, only sailed.
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
        // the map is drawn as yppedia draws it, so a league no chart covers
        // is left out: drawing every pair of adjacent points would clutter
        // the sea. Memorizing both its ends brings it out.
        let paint = match (app.sailable(league), league.chart) {
            (true, _) => Paint::Known,
            (false, Chart::Sold) => Paint::Solid,
            (false, Chart::Booty) => Paint::Dotted,
            (false, Chart::Nonexistent) => continue,
        };
        let (a, b) = league.ends();
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
    // The chart is a viewport onto a larger map: it pans rather than shrinks,
    // so what it needs is a viewport worth sailing in, the row its sideways
    // scrollbar lies along, the row a search box takes when one is open
    // (counted whether or not it is, so opening one cannot lose the page),
    // the box around them all, and the page's hint row below the box.
    if crate::utils::too_short(
        frame,
        area,
        crate::utils::SCROLL_MIN_ROWS + crate::utils::SCROLLBAR_H + 1 + 2 + 1,
    ) {
        return;
    }

    let border = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    // the page's last row is the only hint it gives; the boxes take the rest
    let page = Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Press ? for help")
            .style(Style::default().fg(Color::DarkGray))
            .centered(),
        page[1],
    );
    let body = page[0];

    // a terminal with room for the chart and the column both gets the column
    // beside the map; the map keeps every column otherwise
    let column = metadata_width(geo);
    let (map_area, side) = if CHART_MIN_WIDTH + column <= body.width {
        let cols = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(column),
        ])
        .split(body);
        (cols[0], Some(cols[1]))
    } else {
        (body, None)
    };
    if let (Some(side), Some(map)) = (side, map) {
        let sources = Sources {
            geo,
            islands,
            fetching_islands,
        };
        render_metadata(
            frame, side, app, map, sources, border, regions,
        );
    }
    let area = map_area;

    // The chart is of one ocean, so the title names it; with no ocean picked
    // there's nothing to name and the bare word has to do.
    let title = match ocean {
        Some(ocean) => format!("Map of {ocean} Ocean"),
        None => "Map".to_owned(),
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(offset_title(&title).0);
    // how much of the map the pirate knows is a figure about the whole chart,
    // so it rides the frame at the foot rather than taking a row of its own
    if let Some(tally) = map.and_then(|map| tally_label(app, map)) {
        block = block.title_bottom(
            Line::from(crate::utils::offset_footer(&tally)).right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // the row under the map is the search box's, and is laid out only while
    // one is open: with none the chart has the row, and where the cursor is
    // and which leagues leave it are read off the drawing itself.
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(app.search.is_some())),
    ])
    .split(inner);

    let mut status: Line = Line::from("");
    match (ocean, map) {
        (None, _) => {
            crate::utils::render_notice(
                frame,
                rows[0],
                &[(
                    "Pick an ocean (--ocean) to see its map.",
                    Style::default(),
                )],
            );
        }
        (Some(ocean), None) => {
            let msg = format!(
                "No map for {ocean} yet. See scripts/extract_map.py to add \
                 one."
            );
            crate::utils::render_notice(
                frame,
                rows[0],
                &[(&msg, Style::default())],
            );
        }
        (Some(_), Some(map)) => {
            draw_map(frame, rows[0], app, map, regions);
            if let Some(search) = &app.search {
                let hit = app.search_hit(map).map_or("no match", |p| p.name);
                let label = "Search: ";
                if focused {
                    let x = rows[1].x
                        + label.len() as u16
                        + search.value[.. search.cursor].chars().count() as u16;
                    frame.set_cursor_position((x, rows[1].y));
                }
                status = Line::from(vec![
                    Span::styled(label, Style::default().bold()),
                    Span::styled(
                        search.value.clone(),
                        Style::default().bg(Color::White).fg(Color::Black),
                    ),
                    Span::styled(
                        format!("  -> {hit}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
            }
        }
    }

    frame.render_widget(Paragraph::new(status), rows[1]);

    if app.help {
        render_help(frame, area, regions);
    }
}

/// Where the metadata column's facts come from: the compiled-in geography
/// and yoweb's island list, with whether the latter is on its way.
struct Sources<'a> {
    geo: Option<&'static bare::Ocean>,
    islands: Option<&'a CachedIslands>,
    fetching_islands: bool,
}

/// The Island column: what is known about the point under the cursor, with
/// the notice at its foot while yoweb's island list is still on its way.
fn render_metadata(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    map: &'static Map,
    sources: Sources<'_>,
    border: Style,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(offset_title("Island").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    // the wheel over the column scrolls what the column says rather than
    // panning the chart beside it
    regions.push(ClickRegion {
        rect: rows[0],
        target: ClickTarget::MapIslandInfo,
    });
    if let Some(p) = app.cursor_on(map) {
        // where the point is stays put at the head of the column: what is
        // being read about is as worth knowing at the foot of a long island as
        // at the top of it. Everything under it is the column's window.
        let meta = metadata(app, map, &sources, p);
        // how far the lines wrap is the width's, and the bar's columns are
        // part of the width: taking them can only ask for more rows, never
        // fewer, so asking once whether the full width scrolls settles it
        let settle = |width: u16| {
            let wrap = |lines: &[Line<'static>]| -> Vec<Line<'static>> {
                lines
                    .iter()
                    .flat_map(|line| wrap_line(line, width))
                    .collect()
            };
            (wrap(&meta.head), wrap(&meta.body))
        };
        let (mut head, mut body) = settle(rows[0].width);
        let room = |head: &[Line<'static>]| {
            rows[0].height.saturating_sub(head.len() as u16)
        };
        if crate::utils::scrolls(room(&head), body.len()) {
            (head, body) =
                settle(rows[0].width.saturating_sub(crate::utils::SCROLLBAR_W));
        }
        let split = Layout::vertical([
            Constraint::Length(head.len() as u16),
            Constraint::Min(0),
        ])
        .split(rows[0]);
        app.info_scroll = app
            .info_scroll
            .min(body.len().saturating_sub(split[1].height as usize));
        let text = crate::utils::render_scrollbar(
            frame,
            regions,
            split[1],
            crate::clickmap::ScrollView::MapIslandInfo,
            app.info_scroll,
            body.len(),
        );
        // the head is centred on the same columns as the lines under it, bar
        // or no bar, so the column does not shift when the bar appears
        frame.render_widget(
            Paragraph::new(head),
            Rect {
                x: text.x,
                width: text.width,
                ..split[0]
            },
        );
        frame.render_widget(
            Paragraph::new(body).scroll((app.info_scroll as u16, 0)),
            text,
        );
    }
    if sources.fetching_islands {
        frame.render_widget(
            Paragraph::new(FETCHING)
                .style(Style::default().fg(Color::DarkGray)),
            rows[1],
        );
    }
}

/// Memorized league points on this map, and how many there are in all.
/// Marks that no longer match a point (a redrawn map) are not counted.
fn memorized_tally(app: &MapApp, map: &Map) -> (usize, usize) {
    let points = map.points();
    let known = points.iter().filter(|p| app.memorized.contains(p)).count();
    (known, points.len())
}

/// Word-wrap one metadata line to `width` columns, keeping every character's
/// style across the break. What a line carries on with is indented two columns
/// past the line's own indent, so a continuation reads as part of what it
/// continues rather than as the next fact.
///
/// A centred line stays centred and is not indented: a heading has nothing to
/// hang under. A single word longer than the column is broken where it reaches
/// the edge, since dropping half a name says less than splitting it.
fn wrap_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |ch| (ch, style))
        })
        .collect();
    let width = width.max(1) as usize;
    if cells.len() <= width {
        return vec![line.clone()];
    }
    let indent = cells.iter().take_while(|(ch, _)| *ch == ' ').count();
    let hang = if line.alignment.is_some() {
        indent
    } else {
        indent + 2
    }
    // a column too narrow to hang anything under still has to make progress
    .min(width - 1);
    // the line as its words, the leading indent dropped: it is put back by
    // whichever row a word lands on
    let mut words: Vec<Vec<(char, Style)>> = Vec::new();
    for (ch, style) in cells.into_iter().skip(indent) {
        match (ch, words.last_mut()) {
            (' ', _) => words.push(Vec::new()),
            (_, Some(word)) => word.push((ch, style)),
            (_, None) => words.push(vec![(ch, style)]),
        }
    }

    let blanks = |n: usize| vec![(' ', Style::default()); n];
    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut row = blanks(indent);
    let mut empty = true;
    for word in words.into_iter().filter(|word| !word.is_empty()) {
        let mut rest = word.as_slice();
        loop {
            let room = width.saturating_sub(row.len() + usize::from(!empty));
            if rest.len() <= room {
                if !empty {
                    row.push((' ', Style::default()));
                }
                row.extend_from_slice(rest);
                empty = false;
                break;
            }
            // what is left of the word does not fit: fill a row of its own
            // where it is longer than any row, and wait for the next row
            // otherwise
            if empty && 0 < room {
                let (head, tail) = rest.split_at(room);
                row.extend_from_slice(head);
                rest = tail;
            }
            rows.push(std::mem::replace(
                &mut row,
                blanks(hang),
            ));
            empty = true;
        }
    }
    rows.push(row);
    rows.into_iter()
        .map(|cells| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (ch, style) in cells {
                match spans.last_mut() {
                    Some(last) if last.style == style => {
                        last.content.to_mut().push(ch)
                    }
                    _ => spans.push(Span::styled(ch.to_string(), style)),
                }
            }
            let mut wrapped = Line::from(spans);
            wrapped.alignment = line.alignment;
            wrapped
        })
        .collect()
}

/// What the Island column has to say about a point: the three centred lines
/// pinned at its head, and the lines below them.
struct Metadata {
    /// Where the point is: its name, what kind of island it is, and the
    /// archipelago it lies in. Open sea has only the name.
    head: Vec<Line<'static>>,
    /// What is known about it: its colony, what it exports, what its
    /// archipelago forages, and the gems its palace buys.
    body: Vec<Line<'static>>,
}

/// Read point `p` out of the geography and whatever has been fetched about it.
///
/// The name is red for a point the pirate has memorized. Nothing is said in
/// colour alone: the footer under the chart spells the same mark out in words.
fn metadata(
    app: &MapApp,
    map: &'static Map,
    sources: &Sources<'_>,
    p: Point,
) -> Metadata {
    let bold = |s: String| Span::styled(s, Style::default().bold());
    let dim =
        |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    // the facts a point's own page is the source of, set off from the words
    // around them
    let value = |s: String| Span::styled(s, Style::default().underlined());
    let name_style = {
        let plain = Style::default().bold();
        // a mark belongs to a pirate, so without one there is none to show
        if app.pirate.is_some() && app.memorized.contains(&p) {
            plain.fg(Color::Red)
        } else {
            plain
        }
    };

    let Some(island) = map.island_at(p) else {
        return Metadata {
            head: vec![
                Line::from(Span::styled(
                    format!("Open sea ({},{})", p.0, p.1),
                    name_style,
                ))
                .centered(),
            ],
            body: Vec::new(),
        };
    };
    let mut head = vec![
        Line::from(Span::styled(
            island.name.to_owned(),
            name_style,
        ))
        .centered(),
    ];
    let Some((arch, info)) = sources.geo.and_then(|g| g.island(island.name))
    else {
        return Metadata {
            head,
            body: vec![
                Line::from(""),
                Line::from(dim("We know naught of this island.")),
            ],
        };
    };
    // the line opens with how the island is settled, so that is what opens
    // with a capital. The labels are ASCII words, so a label's first byte is
    // its first letter.
    let mut status = info.status.label().to_owned();
    status[.. 1].make_ascii_uppercase();
    head.push(
        Line::from(vec![
            value(status),
            Span::raw(" "),
            value(info.size.label().to_owned()),
            Span::raw(" island"),
        ])
        .centered(),
    );
    head.push(
        Line::from(vec![
            Span::raw("of the "),
            value(format!("{} Archipelago", arch.name)),
        ])
        .centered(),
    );

    let mut lines = Vec::new();
    // every fact below is shown only once it is known: a section that has
    // nothing to say is left out rather than saying so
    let mut section = |title: &'static str, items: Vec<Line<'static>>| {
        if items.is_empty() {
            return;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(bold(title.to_owned())));
        lines.extend(items);
    };
    let item = |text: String| Line::from(format!("  {text}"));
    let fact = |said: &'static str, of: String| {
        Line::from(vec![
            Span::raw(format!("  {said} ")),
            value(of),
        ])
    };
    // what yoweb says about the island, once its list has been fetched
    if let Some(yoweb) = sources.islands.and_then(|c| c.get(island.name)) {
        let mut facts = Vec::new();
        if let Some(governor) = &yoweb.governor {
            facts.push(fact("Governed by", governor.clone()));
        }
        if let Some(flag) = &yoweb.flag {
            facts.push(fact("Ruled by", flag.clone()));
        }
        if let Some(tax) = yoweb.property_tax {
            facts.push(fact(
                "Taxing properties at",
                format!("{tax}%"),
            ));
        }
        section("Colony", facts);
        section(
            "Exports",
            yoweb.exports.iter().cloned().map(item).collect(),
        );
    }
    section(
        "Forage",
        arch.forageables.iter().cloned().map(item).collect(),
    );
    section(
        "Buys gems",
        info.buys_gems
            .iter()
            .map(|g| {
                item(format!(
                    "{g} at {} PoE",
                    bare::GEM_BUY_PRICE
                ))
            })
            .collect(),
    );
    Metadata {
        head,
        body: lines,
    }
}

/// How much of the map the pirate knows, for the foot of the chart's frame:
/// points memorized, points there are, and the share of them. Nothing at all
/// with no pirate named - the tally is a pirate's knowledge and not the map's,
/// and the help is where naming one is explained.
fn tally_label(app: &MapApp, map: &Map) -> Option<String> {
    app.pirate.as_ref()?;
    let (known, total) = memorized_tally(app, map);
    let percent = if total == 0 {
        0.0
    } else {
        100.0 * known as f64 / total as f64
    };
    Some(format!(
        "{known}/{total} ({percent:.1}%)"
    ))
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
    // The movement keys as a compass rose: each sits where it sails, so the
    // drawing says which way a key goes and no label has to. Every key is
    // placed by the heading it takes, so the rose cannot drift from
    // MOVE_KEYS. The glyphs between them are the map's own - a solid league
    // east and west, diagonals to the corners, and the north-south axis
    // dotted, no league on any map running that way.
    let mut cells = [[' '; 3]; 3];
    for m in &MOVE_KEYS {
        let (col, row) = match m.label {
            "NW" => (0, 0),
            "N" => (1, 0),
            "NE" => (2, 0),
            "W" => (0, 1),
            "E" => (2, 1),
            "SW" => (0, 2),
            "S" => (1, 2),
            "SE" => (2, 2),
            _ => continue,
        };
        cells[row][col] = m.key;
    }
    let rose = [
        format!(
            "   {}   {}   {}",
            cells[0][0], cells[0][1], cells[0][2]
        ),
        "     ╲ ┆ ╱".to_owned(),
        format!(
            "   {} ─ ○ ─ {}",
            cells[1][0], cells[1][2]
        ),
        "     ╱ ┆ ╲".to_owned(),
        format!(
            "   {}   {}   {}",
            cells[2][0], cells[2][1], cells[2][2]
        ),
    ];
    // What the rose cannot draw: the keys with no league of their own, read
    // beside it a row at a time. A key named in the prose is braced, so it is
    // marked like the ones in the drawing - naming them by brace rather than
    // by shape keeps the article "a" from being read as the key.
    let why = [
        "{w} and {x} have no league of their",
        "own - the map has none running",
        "north-south - so they, and {a} or {d}",
        "with no league their way, take the",
        "one diagonal on that side.",
    ];
    let note = |text: &str| -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, part) in text.split('{').enumerate() {
            let (key, rest) = match (i, part.split_once('}')) {
                (0, _) | (_, None) => ("", part),
                (_, Some((key, rest))) => (key, rest),
            };
            if !key.is_empty() {
                spans.push(Span::styled(
                    key.to_owned(),
                    Style::default().fg(Color::DarkGray).underlined(),
                ));
            }
            if !rest.is_empty() {
                spans.push(dim(rest));
            }
        }
        spans
    };
    // the column the note hangs from, clear of the widest rose row
    const NOTE_COL: usize = 16;
    let compass: Vec<Line> = rose
        .iter()
        .zip(why)
        .map(|(row, why)| {
            let mut spans: Vec<Span> = row
                .chars()
                .map(|ch| {
                    match ch {
                        ' ' => Span::raw(" "),
                        '─' | '╲' | '╱' => {
                            Span::styled(ch.to_string(), Paint::Solid.style())
                        }
                        '┆' => {
                            Span::styled(ch.to_string(), Paint::Dotted.style())
                        }
                        '○' => {
                            Span::styled(ch.to_string(), Paint::Point.style())
                        }
                        // a key, underlined so a letter among the glyphs
                        // reads as one to press and not part of the drawing
                        _ => {
                            Span::styled(
                                ch.to_string(),
                                Style::default().bold().underlined(),
                            )
                        }
                    }
                })
                .collect();
            spans.push(Span::raw(" ".repeat(
                NOTE_COL.saturating_sub(row.chars().count()),
            )));
            spans.extend(note(why));
            Line::from(spans)
        })
        .collect();
    let mut lines = vec![Line::from(Span::styled(
        "Sailing the cursor",
        Style::default().bold(),
    ))];
    lines.extend(compass);
    lines.extend([
        Line::from(""),
        Line::from(vec![
            key("Space"),
            Span::raw("  mark the league point under the cursor as memorized"),
        ]),
        Line::from(dim(
            "  memorizing needs a pirate: name one with --user, and the"
        )),
        Line::from(dim(
            "  tally in the frame below is how much they know of the map."
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
            key("wheel"),
            Span::raw("  pan the chart, or scroll the Island column"),
        ]),
        Line::from(dim(
            "  a panned chart returns to the cursor when a point is selected."
        )),
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
            Span::raw(" chart drops as booty"),
        ]),
        Line::from(vec![
            Span::styled("  ━━━", Paint::Known.style()),
            Span::raw(" both ends memorized: sailable from memory"),
        ]),
        Line::from(dim(
            "  a league no chart covers is left off the map to keep it"
        )),
        Line::from(dim(
            "  readable; the keys sail it even so."
        )),
    ]);

    // Widest help line, plus the border and its padding.
    let width = (lines.iter().map(Line::width).max().unwrap_or(0) as u16
        + crate::utils::BOX_MARGIN)
        .min(area.width);
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
                .padding(Padding::horizontal(1))
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

    // The chart is a viewport on a canvas larger than it both ways, so it
    // carries both bars, and each costs the other room: the upright one
    // takes columns, the sideways one the bottom row. Taking room away
    // never un-needs a bar, so one pass over the pair settles both.
    let mut down = crate::utils::scrolls(view.height, canvas.h);
    let across = crate::utils::scrolls(
        view.width
            .saturating_sub(if down { crate::utils::SCROLLBAR_W } else { 0 }),
        canvas.w,
    );
    if across {
        down = crate::utils::scrolls(
            view.height.saturating_sub(crate::utils::SCROLLBAR_H),
            canvas.h,
        );
    }
    let port = Rect {
        width: view
            .width
            .saturating_sub(if down { crate::utils::SCROLLBAR_W } else { 0 }),
        height: view
            .height
            .saturating_sub(if across { crate::utils::SCROLLBAR_H } else { 0 }),
        ..view
    };

    let (vw, vh) = (
        port.width as usize,
        port.height as usize,
    );
    // Panned, the window is where the user put it, held inside the canvas in
    // case the terminal has grown since; otherwise it is the one that centres
    // the cursor. Either way the window is recorded, since a scrollbar click
    // pans from wherever it is now.
    let (ox, oy) = match app.pan {
        Some((x, y)) => {
            (
                x.min(canvas.w.saturating_sub(vw)),
                y.min(canvas.h.saturating_sub(vh)),
            )
        }
        None => {
            (
                origin(cx, vw, canvas.w),
                origin(cy, vh, canvas.h),
            )
        }
    };
    app.window = (ox, oy);

    crate::utils::render_scrollbar(
        frame,
        regions,
        Rect {
            height: port.height,
            ..view
        },
        crate::clickmap::ScrollView::MapCanvas,
        oy,
        canvas.h,
    );
    crate::utils::render_hscrollbar(
        frame,
        regions,
        Rect {
            width: port.width,
            ..view
        },
        crate::clickmap::ScrollView::MapCanvas,
        ox,
        canvas.w,
    );

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

    /// Reads the drawing the way a player would: on every ocean's canvas,
    /// each island's name appears whole somewhere with two blank cells on
    /// either side of it.
    #[test]
    fn every_island_is_named_whole_with_room_around_it() {
        let mut crowded = Vec::new();
        for map in crate::map::data::MAPS {
            let canvas = build_canvas(map, &MapApp::new());
            let rows: Vec<Vec<char>> =
                canvas.dump().lines().map(|l| l.chars().collect()).collect();
            for island in map.islands {
                let label: Vec<char> =
                    island_label(island.name).chars().collect();
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
                if !found {
                    crowded.push(format!(
                        "{}: {}",
                        map.ocean, island.name
                    ));
                }
            }
        }
        assert!(
            crowded.is_empty(),
            "cut or crowded labels: {crowded:?}"
        );
    }

    /// The sea is drawn as yppedia draws it, so a league no chart covers is
    /// absent from it - until both ends are memorized, when it comes out as
    /// the sailable line. The two approaches to Ashkelon Arch, one from
    /// Morannon Island and one from Kashgar Island, are such a pair.
    #[test]
    fn an_uncharted_league_is_drawn_once_both_its_ends_are_memorized() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let (west, south_west) = ((17, 50), (18, 51));
        let (ax, ay) = cell_of(west);
        let (bx, _) = cell_of(south_west);
        // a diagonal's glyph sits in the row between its ends
        let (gx, gy) = ((ax + bx) / 2, ay + 1);

        let mut app = MapApp::new();
        app.pirate = Some("Someone".to_owned());
        assert_eq!(
            build_canvas(map, &app).get(gx, gy),
            Some((' ', Paint::Sea)),
            "an uncharted league is not drawn"
        );
        app.memorized.insert(west);
        app.memorized.insert(south_west);
        assert_eq!(
            build_canvas(map, &app).get(gx, gy),
            Some(('╲', Paint::Known)),
            "memorizing both ends brings it out"
        );
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
    /// cursor on the ocean's first island:
    /// `cargo test dump_map_page -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the rendered Map page for inspection"]
    fn dump_map_page() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = dump_ocean();
        let mut app = MapApp::new();
        let first = map.islands.first().expect("an island on the map");
        app.jump_to(first.at());
        app.pirate = Some("Someone".to_owned());
        app.memorized.insert(first.at());
        let mut terminal =
            Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        let mut regions = Vec::new();
        terminal
            .draw(|frame| {
                let ctx = OceanContext {
                    map: Some(map),
                    geo: bare::BARE.ocean(map.ocean),
                    ocean: Some(map.ocean),
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

    /// The chart's frame carries a pirate's tally once one is named; until
    /// then it carries nothing and the help is what says how to name one.
    #[test]
    fn the_frame_tallies_only_once_a_pirate_is_named() {
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
        let tally = format!("0/{} (0.0%)", map.points().len());
        assert!(!draw(&mut app).contains(&tally));
        app.help = true;
        assert!(
            draw(&mut app).contains("name one with --user"),
            "the help is what asks for a pirate"
        );
        app.help = false;
        // the figure rides the frame, closed by the house-style run
        app.pirate = Some("Someone".to_owned());
        assert!(draw(&mut app).contains(&format!(" {tally} ───")));
    }

    /// Prints the whole canvas, for eyeballing the drawing:
    /// `cargo test dump_map_canvas -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the canvas for inspection"]
    fn dump_map_canvas() {
        let canvas = build_canvas(dump_ocean(), &MapApp::new());
        println!("{}", canvas.dump());
    }

    /// The ocean the dump tests draw: `RQ_DUMP_OCEAN`, or Emerald.
    fn dump_ocean() -> &'static Map {
        let name = std::env::var("RQ_DUMP_OCEAN")
            .unwrap_or_else(|_| "Emerald".to_owned());
        Map::for_ocean(&name).unwrap_or_else(|| panic!("{name} map"))
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
        app.pirate = Some("Someone".to_owned());
        app.memorized.insert(cromwell);
        let meta = metadata(&app, map, &sources, cromwell);
        assert_eq!(
            meta.head[0].spans[0].style.fg,
            Some(Color::Red),
            "a memorized point is named in red"
        );
        // the head says where the point is, and is all the column pins
        assert_eq!(
            text(&meta.head),
            [
                "Cromwell Island",
                "Colonized outpost island",
                "of the Gull Archipelago",
            ]
        );
        let lines = text(&meta.body);
        assert!(lines.contains(&"Forage".to_owned()));
        // nothing from yoweb has been fetched, and no purchase is known for
        // Cromwell: neither is mentioned
        assert!(!lines.contains(&"Colony".to_owned()));
        assert!(!lines.contains(&"Exports".to_owned()));
        assert!(!lines.contains(&"Buys gems".to_owned()));
        // Alkaid is amber's destination
        let alkaid = island(map, "Alkaid Island");
        let lines = text(&metadata(&app, map, &sources, alkaid).body);
        assert!(lines.contains(&"Buys gems".to_owned()));
        assert!(lines.contains(&"  Amber at 1000 PoE".to_owned()));
        // open sea has no island data, only the name of the point
        let sea = metadata(&app, map, &sources, (2, 9));
        assert_eq!(text(&sea.head), ["Open sea (2,9)"]);
        assert!(sea.body.is_empty());
        // the mark is a pirate's: with none loaded, nothing is named in red
        // however the marks read
        app.pirate = None;
        let meta = metadata(&app, map, &sources, cromwell);
        assert_eq!(meta.head[0].spans[0].style.fg, None);
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
        let lines = text(
            &metadata(
                &app,
                map,
                &sources,
                island(map, "Cromwell Island"),
            )
            .body,
        );
        assert!(lines.contains(&"Colony".to_owned()));
        assert!(lines.contains(&"  Governed by Someone".to_owned()));
        assert!(lines.contains(&"  Ruled by Some Flag".to_owned()));
        assert!(lines.contains(&"  Taxing properties at 15%".to_owned()));
        assert!(lines.contains(&"Exports".to_owned()));
        assert!(lines.contains(&"  Hemp".to_owned()));
        // an island yoweb does not list gets none of those lines
        let lines = text(
            &metadata(
                &app,
                map,
                &sources,
                island(map, "Alkaid Island"),
            )
            .body,
        );
        assert!(!lines.contains(&"Colony".to_owned()));
        assert!(!lines.contains(&"Exports".to_owned()));
    }

    /// The column is as wide as the longest island name and kind its ocean can
    /// show, so neither is ever wrapped. The archipelago line is not measured,
    /// and wraps like the lines below the head.
    #[test]
    fn the_column_fits_the_longest_name_its_ocean_can_show() {
        let text_width = |geo| {
            (metadata_width(geo) - crate::utils::SCROLLBAR_W - 4) as usize
        };
        for ocean in &bare::BARE.oceans {
            let room = text_width(Some(ocean));
            let fits = |line: &str| {
                assert!(
                    line.chars().count() <= room,
                    "{line:?} does not fit the {} column",
                    ocean.name,
                );
            };
            fits(FETCHING);
            for arch in &ocean.archipelagos {
                for isle in &arch.islands {
                    fits(&isle.name);
                    fits(&format!(
                        "{} {} island",
                        isle.status.label(),
                        isle.size.label()
                    ));
                }
            }
        }
        // an ocean with no geography still fits what the column says of its
        // own accord
        assert!(FETCHING.chars().count() <= text_width(None));
    }

    /// A fact too long for the column carries on two columns further in, and
    /// the value it names keeps its styling across the break.
    #[test]
    fn a_wrapped_line_hangs_under_the_one_it_continues() {
        let flag = Style::default().underlined();
        let line = Line::from(vec![
            Span::raw("  Ruled by "),
            Span::styled("Don't Open Till Dead", flag),
        ]);
        let wrapped = wrap_line(&line, 26);
        assert_eq!(
            text(&wrapped),
            ["  Ruled by Don't Open Till", "    Dead"]
        );
        // "Dead" is the flag's name wherever it lands
        assert_eq!(wrapped[1].spans[1].style, flag);

        // a line that fits is left as it is, centring and all
        let short = Line::from("  Ruled by Example Flag").centered();
        let kept = wrap_line(&short, 26);
        assert_eq!(text(&kept), ["  Ruled by Example Flag"]);
        assert_eq!(kept[0].alignment, short.alignment);

        // a centred line has nothing to hang under, so its continuations line
        // up with it rather than stepping in
        let long = Line::from("Example Islands of the Archipelago").centered();
        assert_eq!(
            text(&wrap_line(&long, 26)),
            ["Example Islands of the", "Archipelago"]
        );

        // and a word longer than the column is broken rather than dropped
        assert_eq!(
            text(&wrap_line(
                &Line::from("  Ruled by Abcdefghijklmnopqrstuvwxyz"),
                16
            )),
            [
                "  Ruled by",
                "    Abcdefghijkl",
                "    mnopqrstuvwx",
                "    yz"
            ]
        );
    }

    /// The lines inside the Island column's box as a 120x20 terminal shows
    /// them, the scrollbar's own column included so the bar can be read off
    /// them.
    fn column_lines(
        app: &mut MapApp,
        list: &crate::islands::CachedIslands,
    ) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let (w, h) = (120, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(w, h)).expect("test terminal");
        let mut regions = Vec::new();
        terminal
            .draw(|frame| {
                let ctx = OceanContext {
                    map: Some(map),
                    geo: bare::BARE.ocean("Emerald"),
                    ocean: Some("Emerald"),
                    islands: Some(list),
                    fetching_islands: false,
                };
                render(
                    frame,
                    frame.area(),
                    app,
                    ctx,
                    true,
                    &mut regions,
                );
            })
            .expect("draw");
        // the column is where it registered itself, so the reading does not
        // have to know how wide the ocean's names made it
        let column = regions
            .iter()
            .find_map(|r| {
                matches!(r.target, ClickTarget::MapIslandInfo).then_some(r.rect)
            })
            .expect("the Island column is a region of its own");
        let buffer = terminal.backend().buffer().clone();
        (1 .. h - 1)
            .map(|y| {
                (column.x .. column.x + column.width)
                    .map(|col| buffer[(col, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// The column is not sized for the archipelago line, so that line may wrap
    /// — and a head of four rows leaves the window one row fewer, bar and all.
    #[test]
    fn a_wrapped_head_takes_a_row_from_the_window() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        // Horse Head is the longest archipelago name on the Emerald map
        let list = crate::islands::CachedIslands {
            fetched_at: chrono::Utc::now(),
            islands: vec![crate::islands::IslandInfo {
                name: "Anegada Island".to_owned(),
                governor: Some("Someone".to_owned()),
                flag: None,
                property_tax: Some(20),
                exports: [
                    "Hemp",
                    "Iron",
                    "Wood",
                    "Cloth",
                    "Stone",
                    "Sugar cane",
                ]
                .map(str::to_owned)
                .to_vec(),
            }],
        };
        let mut app = MapApp::new();
        app.pirate = Some("Someone".to_owned());
        app.jump_to(island(map, "Anegada Island"));

        let lines = column_lines(&mut app, &list);
        assert!(
            lines[2].contains("of the Horse Head")
                && lines[3].contains("Archipelago"),
            "the archipelago line wraps: {lines:?}"
        );
        assert_eq!(
            lines.iter().position(|l| l.ends_with('┬')),
            Some(4),
            "the bar starts under the wrapped head: {lines:?}"
        );
    }

    /// What the column has to say about a point can run longer than the
    /// column, so it keeps a window of its own: the bar scrolls it, and a
    /// point selected afresh is read from the top.
    #[test]
    fn the_island_column_scrolls_what_it_cannot_fit() {
        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let list = crate::islands::CachedIslands {
            fetched_at: chrono::Utc::now(),
            islands: vec![crate::islands::IslandInfo {
                name: "Alkaid Island".to_owned(),
                governor: Some("Someone".to_owned()),
                flag: Some("Some Flag".to_owned()),
                property_tax: Some(15),
                exports: [
                    "Hemp",
                    "Iron",
                    "Wood",
                    "Cloth",
                    "Stone",
                    "Sugar cane",
                ]
                .map(str::to_owned)
                .to_vec(),
            }],
        };
        let mut app = MapApp::new();
        app.pirate = Some("Someone".to_owned());
        app.jump_to(island(map, "Alkaid Island"));

        let top = column_lines(&mut app, &list);
        assert!(
            top[0].contains("Alkaid Island"),
            "the island is named at the head of the column: {top:?}"
        );
        // the three lines saying where the point is are pinned above the bar
        assert_eq!(
            top.iter().position(|l| l.ends_with('┬')),
            Some(3),
            "the bar starts under the head: {top:?}"
        );
        assert!(
            top.iter().any(|l| l.ends_with('▼')),
            "it can still be scrolled down: {top:?}"
        );
        assert!(
            !top.iter().any(|l| l.contains("Amber")),
            "the gem it buys is past the bottom: {top:?}"
        );

        // asking past the end is the end: the last line comes into view, and
        // the bar's upper arrow with it, while the head stays where it was
        app.info_scroll = usize::MAX;
        let end = column_lines(&mut app, &list);
        assert_eq!(
            end[.. 3],
            top[.. 3],
            "the head scrolled away with the body: {end:?}"
        );
        assert!(
            end.iter().any(|l| l.contains("Amber at 1000 PoE")),
            "the end of what the column says: {end:?}"
        );
        assert!(
            end[3].ends_with('▲') && end.iter().any(|l| l.ends_with('┴')),
            "the bar is flush with the bottom: {end:?}"
        );
        // twenty lines under the head, and thirteen rows beside the bar to
        // read them in
        assert_eq!(
            app.info_scroll,
            20 - 13,
            "the ask is clamped to the last line of the column"
        );

        // and another point is read from the top again
        app.jump_to(island(map, "Cromwell Island"));
        assert_eq!(app.info_scroll, 0);
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
