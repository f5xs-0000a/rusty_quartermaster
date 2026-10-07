//! Rendering for the Map app: the map is drawn onto a character canvas
//! (points, leagues, labels), and the viewport shows the part of it around
//! the cursor.
//!
//! # Reading the chart
//!
//! One glyph says what a thing is, and colour says the rest:
//!
//! - A league point at sea is `✧` and an island `○`; memorizing one fills it
//!   in, to `✦` and `●`.
//! - A league is a rule east and west, and the single `╱` or `╲` that fits in
//!   the row between its ends on the diagonal. Grey is a chart no shipyard
//!   sells, white is one that can be bought there, and red is a league with
//!   both ends memorized.
//! - A league no chart covers at all is not drawn until both its ends are
//!   memorized, which keeps the sea as readable as the wiki's own map.
//! - Every point keeps the three-by-three of cells around it to itself, the
//!   same cells its click box reaches: a rule stops a cell short of each end,
//!   and no name is written in one, blank though those cells are. A name is
//!   read as naming what it sits beside, so the sea around a mark is left to
//!   the mark.
//! - A place is drawn under the name its map file gives it to be drawn under
//!   ([`crate::map::data::Place::drawn`]): `Kent` for Isle of Kent, and most
//!   archipelagos without their `Archipelago`. A name wider than [`LABEL_WRAP`]
//!   wraps onto further rows, and the block of rows is placed and kept clear as
//!   one.
//!
//! What a chart is worth is colour rather than line style because one cell is
//! all a diagonal gets, and one cell cannot be dashed the way a seven-cell run
//! can - a dotted rule had no diagonal to pair with. So the `.txt` gallery
//! dumps show the geometry alone, and the `.svg` and `STYLES-*.txt` beside them
//! are where a league's worth is checked.
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
//!   convention every scrolling view keeps (see `.UI_CONVENTIONS.md`, Rule 4).
//! - Facts yoweb or the geography is the source of are **underlined**, so a
//!   value is told apart from the words that introduce it.
//! - A line too long for the column **wraps** with its continuation two columns
//!   further in, rather than being cut off.
//! - The column's **width** is [`metadata_width`]: the ocean's longest island
//!   name and kind of island, so neither ever wraps.

use std::collections::{BTreeMap, BTreeSet};

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};

use crate::{
    bare,
    clickmap::{ClickMap, ClickRegion, ClickTarget},
    islands::CachedIslands,
    map::{
        MOVE_KEYS,
        MapApp,
        data::{Chart, Heading, Map, Place, Point},
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

/// What a canvas cell is part of; decides its style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paint {
    Sea,
    /// A league whose chart no shipyard sells. A league with no chart at all
    /// is not painted, only sailed.
    Unsold,
    /// A league on a route whose chart can be bought.
    Sold,
    /// A league between two memorized points, so the route is memorized
    /// too.
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
            // the chart's worth is the colour: one that cannot be bought
            // recedes into the sea, one that can is plain white, and anything
            // the pirate knows by heart is red
            Paint::Unsold => Style::default().fg(Color::DarkGray),
            Paint::Sold => Style::default().fg(Color::White),
            Paint::Point => Style::default().fg(Color::Gray),
            Paint::Known | Paint::PointKnown | Paint::IslandKnown => {
                Style::default().fg(Color::Red).bold()
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
    /// The cells league points keep to themselves: no name is written in one,
    /// blank though it is.
    kept: Vec<bool>,
    /// Where each archipelago's name ended up, by name: the cell its block of
    /// rows sits around. A view that wants to show a name has no other way of
    /// knowing where the drawing put it.
    captions: Vec<(&'static str, (usize, usize))>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            cells: vec![(' ', Paint::Sea); w * h],
            kept: vec![false; w * h],
            captions: Vec::new(),
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

    /// Mark a league point: its glyph, and the three-by-three of cells around
    /// it that it keeps to itself - the same cells its click box reaches.
    fn put_point(&mut self, x: usize, y: usize, ch: char, paint: Paint) {
        self.put(x, y, ch, paint);
        for ky in y.saturating_sub(CLICK_REACH) ..= y + CLICK_REACH {
            for kx in x.saturating_sub(CLICK_REACH) ..= x + CLICK_REACH {
                if let Some(i) = self.idx(kx, ky) {
                    self.kept[i] = true;
                }
            }
        }
    }

    fn is_free(&self, x: usize, y: usize) -> bool {
        self.get(x, y).is_some_and(|(ch, _)| ch == ' ')
    }

    /// Whether a cell can carry a letter: blank, and not a cell some point
    /// keeps to itself.
    fn is_writable(&self, x: usize, y: usize) -> bool {
        self.is_free(x, y) && self.idx(x, y).is_some_and(|i| !self.kept[i])
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

    /// Whether a block of `rows`, each centred in the widest row's width, can
    /// sit with its top-left at `(x, y)` with `pad` columns and rows of blank
    /// water around it: every letter on a cell that can carry one, and nothing
    /// drawn anywhere else in the box the block and its padding make. The box
    /// may fall on cells a point keeps, or run off the canvas edge; no letter
    /// may do either.
    fn fits(
        &self,
        x: usize,
        y: usize,
        rows: &[String],
        (pad_x, pad_y): (usize, usize),
    ) -> bool {
        let w = block_width(rows);
        if self.w < x + w || self.h < y + rows.len() {
            return false;
        }
        let (lo_x, hi_x) = (
            x.saturating_sub(pad_x),
            (x + w + pad_x).min(self.w),
        );
        let (lo_y, hi_y) = (
            y.saturating_sub(pad_y),
            (y + rows.len() + pad_y).min(self.h),
        );
        for cy in lo_y .. hi_y {
            // where the letters lie, on the rows that carry any
            let letters =
                cy.checked_sub(y).and_then(|i| rows.get(i)).map(|row| {
                    let rx = x + indent(row, w);
                    rx .. rx + row.chars().count()
                });
            for cx in lo_x .. hi_x {
                let free = if letters.as_ref().is_some_and(|l| l.contains(&cx))
                {
                    self.is_writable(cx, cy)
                } else {
                    self.is_free(cx, cy)
                };
                if !free {
                    return false;
                }
            }
        }
        true
    }

    /// The cheapest spot of [`label_spots`] where a block of `rows` for the
    /// point at `(cx, cy)` fits, keeping clear of every drawn glyph, the
    /// point's own included.
    fn best_spot(
        &self,
        cx: usize,
        cy: usize,
        rows: &[String],
    ) -> Option<(usize, usize)> {
        label_spots(cx, cy, block_width(rows), rows.len())
            .filter(|&(x, y, _)| self.fits(x, y, rows, (LABEL_GAP, 0)))
            .min_by_key(|&(_, _, cost)| cost)
            .map(|(x, y, _)| (x, y))
    }

    /// Write a block of `rows` with its top-left at `(x, y)`, each row
    /// centred in the widest one's width.
    fn write_block(
        &mut self,
        x: usize,
        y: usize,
        rows: &[String],
        paint: Paint,
    ) {
        let w = block_width(rows);
        for (i, row) in rows.iter().enumerate() {
            self.write(x + indent(row, w), y + i, row, paint);
        }
    }

    /// Place `text` for the point at `(cx, cy)` in the spot that keeps it
    /// nearest the point, wrapped onto further rows where it is wider than
    /// [`LABEL_WRAP`]. With no spot for the whole name, the longest head of
    /// it (three characters or more) that fits anywhere is written instead so
    /// the island stays findable. Returns whether the whole name was placed.
    fn label(
        &mut self,
        cx: usize,
        cy: usize,
        text: &str,
        paint: Paint,
    ) -> bool {
        let rows = label_rows(text);
        if let Some((x, y)) = self.best_spot(cx, cy, &rows) {
            self.write_block(x, y, &rows, paint);
            return true;
        }
        let n = text.chars().count();
        for len in (3 .. n).rev() {
            let head: String = text.chars().take(len).collect();
            let rows = label_rows(&head);
            if let Some((x, y)) = self.best_spot(cx, cy, &rows) {
                self.write_block(x, y, &rows, paint);
                return false;
            }
        }
        false
    }

    /// Write `text` as near `(tx, ty)` as the rules allow, the middle of the
    /// block of rows brought to that cell. This is how a name that belongs to
    /// a stretch of water rather than to a mark is placed: it is not beside
    /// anything, so the nearest spot in any direction will do, counting a row
    /// as [`CELL_W`] / [`CELL_H`] columns since a cell is taller than it is
    /// wide.
    ///
    /// Of two spots as near, one whose middle `on_its_own` accepts wins: a
    /// name is read as naming the water it sits on, so it is kept on its own
    /// where it can be. It is given [`CAPTION_PAD`] of blank water around it
    /// first; with no room for that within [`CAPTION_REACH`] leagues it
    /// settles for the clearance a name beside a mark keeps, and failing even
    /// that it is placed beside the middle like any other name. Returns the
    /// cell the name ended up around.
    fn caption(
        &mut self,
        (tx, ty): (usize, usize),
        text: &str,
        paint: Paint,
        on_its_own: impl Fn((usize, usize)) -> bool,
    ) -> Option<(usize, usize)> {
        let rows = label_rows(text);
        let (w, h) = (block_width(&rows), rows.len());
        let middle = |(x, y): (usize, usize)| (x + w / 2, y + h / 2);
        // the top-left that brings the block's middle to the cell asked for
        let (ox, oy) = (
            tx as isize - (w / 2) as isize,
            ty as isize - (h / 2) as isize,
        );
        let mut elsewhere = None;
        for &(dx, dy) in caption_offsets() {
            let (Ok(x), Ok(y)) = (
                usize::try_from(ox + dx),
                usize::try_from(oy + dy),
            ) else {
                continue;
            };
            if !self.fits(x, y, &rows, CAPTION_PAD) {
                continue;
            }
            if on_its_own(middle((x, y))) {
                self.write_block(x, y, &rows, paint);
                return Some(middle((x, y)));
            }
            elsewhere.get_or_insert((x, y));
        }
        // nowhere on its own water has the room: the nearest water that does
        // is better than none
        if let Some((x, y)) = elsewhere {
            self.write_block(x, y, &rows, paint);
            return Some(middle((x, y)));
        }
        // no water within reach can hold it at all: the name is worth more
        // than the room around it, so it goes beside the middle like any
        // other name
        self.label(tx, ty, text, paint).then_some((tx, ty))
    }
}

/// How far a caption's block may be carried from where it belongs, and in
/// what order to try: nearest first within [`CAPTION_REACH`] leagues. A row
/// counts as [`CELL_W`] / [`CELL_H`] columns, a cell being that much taller
/// than it is wide, so the distance is the distance as it looks; of two spots
/// as near, the one below and to the right comes first, as it does for a name
/// beside a mark. The order is the same every time, so it is worked out once.
fn caption_offsets() -> &'static [(isize, isize)] {
    static ORDER: std::sync::OnceLock<Vec<(isize, isize)>> =
        std::sync::OnceLock::new();
    ORDER.get_or_init(|| {
        let (reach_x, reach_y) = (
            CAPTION_REACH * CELL_W,
            CAPTION_REACH * CELL_H,
        );
        let mut spots = Vec::new();
        for dy in -(reach_y as isize) ..= reach_y as isize {
            for dx in -(reach_x as isize) ..= reach_x as isize {
                let (down, across) = (dy.unsigned_abs(), dx.unsigned_abs());
                let cost = (down * CELL_W / CELL_H).pow(2) + across.pow(2);
                spots.push((
                    cost,
                    dy < 0,
                    down,
                    dx < 0,
                    across,
                    dx,
                    dy,
                ));
            }
        }
        spots.sort_unstable();
        spots.into_iter().map(|spot| (spot.5, spot.6)).collect()
    })
}

/// The widest row of a label block, which is the width the block takes.
fn block_width(rows: &[String]) -> usize {
    rows.iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0)
}

/// Where a row starts within a block `w` cells wide, centred in it.
fn indent(row: &str, w: usize) -> usize {
    (w - row.chars().count()) / 2
}

/// A name broken into the rows it is drawn on: whole while it is no wider
/// than [`LABEL_WRAP`], else broken at the spaces, each row carrying as many
/// words as it can hold. A single word wider than that keeps a row of its
/// own, since a name says less broken mid-word than drawn too wide.
fn label_rows(text: &str) -> Vec<String> {
    if text.chars().count() <= LABEL_WRAP {
        return vec![text.to_owned()];
    }
    let mut rows: Vec<String> = Vec::new();
    for word in text.split(' ') {
        match rows.last_mut() {
            Some(row)
                if row.chars().count() + 1 + word.chars().count()
                    <= LABEL_WRAP =>
            {
                row.push(' ');
                row.push_str(word);
            }
            _ => rows.push(word.to_owned()),
        }
    }
    rows
}

/// Blank cells kept between a label and anything drawn beside it on its row.
const LABEL_GAP: usize = 2;

/// The widest row a label is drawn on before it wraps onto another. The sea
/// has room for a far wider one, but the viewport onto it does not: a name
/// that runs on for a quarter of the chart hides more water than it names.
const LABEL_WRAP: usize = 24;

/// Cells a point's click box extends past its glyph on each side.
const CLICK_REACH: usize = 1;

/// What a label's placement costs, in cells of drift from its point: a row
/// step counts this much sideways drift, so a label goes to the row above
/// or below only when sliding along the nearer row would carry it further
/// from the point than that.
const ROW_STEP_COST: usize = 3;

/// Candidate top-left cells for a label block `w` cells wide and `h` rows
/// tall belonging to the point at `(cx, cy)`, each with its cost: beside the
/// point on its own row (right or left, sliding up to four cells further
/// out, the rest of the block hanging below), and clear of the point's row
/// one and two steps above and below it, centred on the point and sliding
/// either way until the block has cleared it. Sitting right beside the point
/// is the cheapest spot, then a centred spot on the next row, then the rest
/// by drift. Spots off the top or left edge are skipped; ties go to the
/// earlier candidate (right before left, below before above).
fn label_spots(
    cx: usize,
    cy: usize,
    w: usize,
    h: usize,
) -> impl Iterator<Item = (usize, usize, usize)> {
    let (cx, cy, w, h) = (
        cx as isize,
        cy as isize,
        w as isize,
        h as isize,
    );
    let gap = LABEL_GAP as isize;
    let beside = (0 ..= 4).flat_map(move |slide| {
        let cost = 2 + slide as usize;
        [
            (cx + 1 + gap + slide, cy, cost),
            (cx - gap - w - slide, cy, cost),
        ]
    });
    // above the point, the block's last row is the one that has to clear it,
    // so its top sits that much higher
    let rows = [(cy + 1, 1), (cy - h, 1), (cy + 2, 2), (cy - h - 1, 2)];
    let around = rows.into_iter().flat_map(move |(y, steps)| {
        let centred = cx - w / 2;
        let base = ROW_STEP_COST * steps;
        std::iter::once((centred, y, base)).chain((1 ..= w).flat_map(
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

/// How far a caption may be carried from the middle of its archipelago to
/// find room for itself, in leagues. The furthest any name on any ocean has
/// to go is twelve, so this leaves a few leagues of slack.
const CAPTION_REACH: usize = 16;

/// Blank water a caption keeps around itself, in columns and in rows - three
/// leagues of it either way, a cell being twice as tall as it is wide. A name
/// beside a mark says which mark by sitting next to it, and keeps
/// [`LABEL_GAP`]; a name standing for a whole archipelago has nothing to sit
/// next to, so it takes the open water instead, and is read as the water's
/// rather than as some mark's it was wedged against. Nothing drawn may stand
/// inside it - no point, and no league running to one.
const CAPTION_PAD: (usize, usize) = (12, 6);

/// The island nearest `p`, by league rather than by cell: a league is the
/// same distance east as it is south-east, however many cells the drawing
/// spends on each. Ties go to the island named first, so the same map always
/// draws the same.
fn nearest_island(map: &Map, p: Point) -> Option<&'static Place> {
    map.islands.iter().min_by_key(|island| {
        let (dx, dy) = (
            i64::from(island.x) - i64::from(p.0),
            i64::from(island.y) - i64::from(p.1),
        );
        (dx * dx + dy * dy, island.name)
    })
}

/// Which archipelago owns each stretch of a map's water, and where the middle
/// of each one is.
///
/// Every cell of the grid is given to the island nearest it - a Voronoi
/// diagram of the ocean's islands - and the cells of islands sharing an
/// archipelago read as one region, so an archipelago's region is the water its
/// own land is the nearest to. A region's middle is the mean of the league
/// points in it, which is the water that is actually sailed rather than the
/// empty corners of the grid the diagram also hands out.
struct Regions {
    /// The archipelago owning each grid cell, row by row, as wide and as tall
    /// as the map's extent.
    cells: Vec<Option<&'static str>>,
    w: usize,
    /// The canvas cell at the middle of each archipelago's region.
    middle: BTreeMap<&'static str, (usize, usize)>,
}

impl Regions {
    /// The archipelago whose water a grid cell is, if any.
    fn owner(&self, (x, y): Point) -> Option<&'static str> {
        if self.w <= x as usize {
            return None;
        }
        self.cells
            .get(y as usize * self.w + x as usize)
            .copied()
            .flatten()
    }
}

/// The regions of every compiled map, worked out on first use: they are the
/// same for a map every time it is drawn, and a diagram costs an island
/// lookup for every cell of the grid.
fn regions_of(map: &'static Map) -> Option<&'static Regions> {
    static ALL: std::sync::OnceLock<Vec<(&'static str, Regions)>> =
        std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        crate::map::data::MAPS
            .iter()
            .map(|map| (map.ocean, build_regions(map)))
            .collect()
    })
    .iter()
    .find(|(ocean, _)| *ocean == map.ocean)
    .map(|(_, regions)| regions)
}

fn build_regions(map: &'static Map) -> Regions {
    // the map says which island a cell belongs to, the geography which
    // archipelago the island belongs to
    let geo = bare::BARE.ocean(map.ocean);
    let arch_of = |island: &'static Place| {
        geo.and_then(|geo| geo.island(island.name))
            .map(|(arch, _)| arch.name.as_str())
    };
    let (max_x, max_y) = map.extent();
    let (w, h) = (max_x as usize + 1, max_y as usize + 1);
    let mut cells = vec![None; w * h];
    for y in 0 .. h {
        for x in 0 .. w {
            cells[y * w + x] =
                nearest_island(map, (x as u16, y as u16)).and_then(arch_of);
        }
    }
    let mut sums: BTreeMap<&'static str, (usize, usize, usize)> =
        BTreeMap::new();
    for point in map.points() {
        let Some(arch) = cells[point.1 as usize * w + point.0 as usize] else {
            continue;
        };
        let (x, y) = cell_of(point);
        let sum = sums.entry(arch).or_default();
        *sum = (sum.0 + x, sum.1 + y, sum.2 + 1);
    }
    Regions {
        cells,
        w,
        middle: sums
            .into_iter()
            .map(|(arch, (x, y, n))| (arch, (x / n, y / n)))
            .collect(),
    }
}

/// The grid point a canvas cell belongs to: [`cell_of`] read backwards, each
/// cell going to the nearest point's row and column.
fn point_of((x, y): (usize, usize)) -> Point {
    let round = |cell: usize, margin: usize, size: usize| {
        ((cell.saturating_sub(margin) + size / 2) / size) as u16
    };
    (
        round(x, MARGIN_X, CELL_W),
        round(y, MARGIN_Y, CELL_H),
    )
}

/// Canvas cell of a grid point.
fn cell_of((x, y): Point) -> (usize, usize) {
    (
        x as usize * CELL_W + MARGIN_X,
        y as usize * CELL_H + MARGIN_Y,
    )
}

/// Where each archipelago's name is drawn on `map`, as the grid point nearest
/// the middle of the name. Only the drawing knows: a name goes to the middle
/// of its own water if there is room for it there, and to the nearest water
/// that has room if there is not.
#[allow(dead_code)] // the gallery centres a view on each name
pub fn caption_points(map: &'static Map) -> Vec<(&'static str, Point)> {
    build_canvas(map, &MapApp::new())
        .captions
        .into_iter()
        .map(|(arch, cell)| (arch, point_of(cell)))
        .collect()
}

fn build_canvas(map: &'static Map, app: &MapApp) -> Canvas {
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
            (false, Chart::Sold) => Paint::Sold,
            (false, Chart::Unsold) => Paint::Unsold,
            (false, Chart::Nonexistent) => continue,
        };
        let (a, b) = league.ends();
        let (ax, ay) = cell_of(a);
        let (bx, by) = cell_of(b);
        match league.heading {
            // the rule stops a cell short of each end, leaving every point the
            // 3x3 of cells around it to itself - the same cells its click box
            // reaches
            Heading::E => {
                for x in ax + 1 + CLICK_REACH .. bx - CLICK_REACH {
                    canvas.put(x, ay, '─', paint);
                }
            }
            // The diagonal's single glyph sits in the row between its ends,
            // centred between their columns. No cell ever takes two: two
            // diagonals crossing would need the two points of the other
            // `x + y` parity, and a map holds one parity class only.
            Heading::Se => canvas.put((ax + bx) / 2, ay + 1, '╲', paint),
            Heading::Ne => canvas.put((ax + bx) / 2, by + 1, '╱', paint),
            _ => {}
        }
    }

    let points: BTreeSet<Point> = map.points();
    for &p in &points {
        let (x, y) = cell_of(p);
        let known = app.memorized.contains(&p);
        let (ch, paint) = match (map.island_at(p).is_some(), known) {
            (true, true) => ('●', Paint::IslandKnown),
            (true, false) => ('○', Paint::Island),
            (false, true) => ('✦', Paint::PointKnown),
            (false, false) => ('✧', Paint::Point),
        };
        canvas.put_point(x, y, ch, paint);
    }

    for island in map.islands {
        let (x, y) = cell_of(island.at());
        canvas.label(x, y, island.drawn(), Paint::Name);
    }
    // the islands are named first: a mark's own name has the better claim on
    // the water beside it than the name of the whole archipelago has
    let regions = regions_of(map);
    for label in map.labels {
        let arch = label.name.strip_suffix(" Archipelago");
        let middle = regions
            .zip(arch)
            .and_then(|(regions, arch)| regions.middle.get(arch).copied());
        let Some(middle) = middle else {
            // an archipelago the geography does not know: the wiki's own
            // placement is all there is to go on
            let (x, y) = cell_of(label.at());
            canvas.label(x, y, label.drawn(), Paint::Region);
            continue;
        };
        let at = canvas.caption(
            middle,
            label.drawn(),
            Paint::Region,
            |cell| {
                regions.and_then(|regions| regions.owner(point_of(cell)))
                    == arch
            },
        );
        if let (Some(arch), Some(at)) = (arch, at) {
            canvas.captions.push((arch, at));
        }
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
    regions: &mut ClickMap,
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
    // scrollbar lies along, the rows a search box takes when one is open
    // (counted whether or not it is, so opening one cannot lose the page),
    // the box around them all, and the page's hint row below the box.
    if crate::utils::too_short(
        frame,
        area,
        crate::utils::SCROLL_MIN_ROWS
            + crate::utils::SCROLLBAR_H
            + crate::utils::SEARCH_H
            + 2
            + 1,
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

    // the row under the map is the search's, held for it whether one is open or
    // not: unopened, it is what says a search can be had at all.
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(crate::utils::SEARCH_H),
    ])
    .split(inner);

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
                "We carry no map of {ocean} yet. One is added as a file in \
                 src/data/maps/."
            );
            crate::utils::render_notice(
                frame,
                rows[0],
                &[(&msg, Style::default())],
            );
        }
        (Some(_), Some(map)) => {
            draw_map(frame, rows[0], app, map, regions);
            match &app.search {
                Some(search) => {
                    // what Enter would jump to is what the query found
                    let hit =
                        app.search_hit(map).map_or("no match", |p| p.name);
                    crate::utils::render_search(
                        frame,
                        rows[1],
                        search,
                        (!search.value.is_empty()).then_some(
                            crate::utils::SearchAnswer::Resolved(hit),
                        ),
                        focused,
                    );
                }
                None => {
                    crate::utils::render_search_invite(
                        frame,
                        rows[1],
                        "search for an island",
                    )
                }
            }
        }
    }

    if app.help {
        regions.layer();
        render_help(frame, area, app, regions);
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
    regions: &mut ClickMap,
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
    // an archipelago whose name opens with its own article takes no second
    // one: two in a row read as neither
    let of = match arch.name.split(' ').next() {
        Some("Ye" | "The") => "of ",
        _ => "of the ",
    };
    head.push(
        Line::from(vec![
            Span::raw(of),
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
fn render_help(
    frame: &mut Frame,
    area: Rect,
    app: &mut MapApp,
    regions: &mut ClickMap,
) {
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::MapHelpClose,
    });

    // The popup reads down two edges: what to press, and what it does. A key
    // is underlined wherever it is named, and bold besides in the column of
    // its own; the mouse is named in that column too but is no key, so it is
    // left unmarked.
    const INDENT: usize = 2;
    const KEY_W: usize = 5;
    const GLYPH_W: usize = 3;
    const GAP: usize = 2;

    let act = |k: &str| Span::styled(k.to_owned(), Style::default().bold());
    let key = |k: &str| {
        Span::styled(
            k.to_owned(),
            Style::default().bold().underlined(),
        )
    };
    // A key named in prose is braced, so it is marked like the ones in a
    // column - naming them by brace rather than by shape keeps the article
    // "a" from being read as the key.
    let keyed = |text: &str, base: Style| -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, part) in text.split('{').enumerate() {
            let (named, rest) = match (i, part.split_once('}')) {
                (0, _) | (_, None) => ("", part),
                (_, Some((named, rest))) => (named, rest),
            };
            if !named.is_empty() {
                spans.push(Span::styled(
                    named.to_owned(),
                    base.underlined(),
                ));
            }
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_owned(), base));
            }
        }
        spans
    };
    let note = |text: &str| {
        keyed(
            text,
            Style::default().fg(Color::DarkGray),
        )
    };
    let heading = |text: &str| {
        Line::from(Span::styled(
            text.to_owned(),
            Style::default().bold(),
        ))
    };
    // one entry: its key or its mouse action, then what it does
    let row = |what: Span<'static>, text: &str| {
        let pad = KEY_W.saturating_sub(what.content.chars().count()) + GAP;
        let mut spans = vec![
            Span::raw(" ".repeat(INDENT)),
            what,
            Span::raw(" ".repeat(pad)),
        ];
        spans.extend(keyed(text, Style::default()));
        Line::from(spans)
    };
    // a glyph off the map, and what it is
    let mark = |glyph: &str, paint: Paint, text: &str| {
        let pad = GLYPH_W.saturating_sub(glyph.chars().count()) + GAP;
        Line::from(vec![
            Span::styled(" ".repeat(INDENT), Style::default()),
            Span::styled(glyph.to_owned(), paint.style()),
            Span::raw(" ".repeat(pad)),
            Span::raw(text.to_owned()),
        ])
    };
    // anything more about an entry hangs under its text, not under its key
    let under = |col: usize, text: &str| {
        let mut spans = vec![Span::raw(" ".repeat(col))];
        spans.extend(note(text));
        Line::from(spans)
    };
    let said = INDENT + KEY_W + GAP;
    let drawn = INDENT + GLYPH_W + GAP;

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
            "   {} ─ ✧ ─ {}",
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
                            Span::styled(ch.to_string(), Paint::Sold.style())
                        }
                        '┆' => {
                            Span::styled(ch.to_string(), Paint::Unsold.style())
                        }
                        '✧' => {
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
    let mut lines = vec![heading("Sailing the cursor")];
    lines.extend(compass);
    lines.extend([
        Line::from(""),
        heading("Keys"),
        row(
            key("Space"),
            "memorize the point under the cursor, or forget it",
        ),
        under(
            said,
            "what is memorized belongs to the pirate named by",
        ),
        under(
            said,
            "--user, and the tally in the frame counts it",
        ),
        row(
            key("/"),
            "search for an island - {Enter} jumps, {Esc} or {Up} cancels",
        ),
        row(key("Up"), "back to the top bar"),
        row(
            key("?"),
            "close this help - {Esc}, {Enter} or a click do too",
        ),
        under(
            said,
            "{Up} and {Down}, or the wheel, read on through it",
        ),
        Line::from(""),
        heading("The mouse"),
        row(
            act("click"),
            "put the cursor on a league point",
        ),
        row(
            act("wheel"),
            "pan the chart, or scroll the Island column",
        ),
        under(
            said,
            "a panned chart returns to the cursor when a point",
        ),
        under(said, "is selected"),
        row(
            act("bars"),
            "an arrow steps, the track jumps",
        ),
        Line::from(""),
        heading("Legend"),
        mark("○", Paint::Island, "an island"),
        mark(
            "✧",
            Paint::Point,
            "a league point at sea",
        ),
        mark(
            "● ✦",
            Paint::IslandKnown,
            "memorized by the pirate",
        ),
        mark(
            "───",
            Paint::Sold,
            "white: a chart sold in game",
        ),
        mark(
            "───",
            Paint::Unsold,
            "grey: a chart no shipyard sells",
        ),
        mark(
            "───",
            Paint::Known,
            "red: a memorized route, between two memorized points",
        ),
        under(
            drawn,
            "a league no chart covers is not drawn at all, to keep",
        ),
        under(
            drawn,
            "the map readable; the keys sail it even so",
        ),
    ]);

    // Widest help line, plus the border and its padding. The popup is as tall
    // as what it has to say and no taller than the room it has; the rest is
    // read by scrolling, so a short window costs the help its foot no longer.
    // A bar's columns sit outside the text, so wanting one widens the popup
    // rather than wrapping what it says.
    let height = (lines.len() as u16 + 2).min(area.height);
    let text = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let bar = u16::from(crate::utils::scrolls(
        height.saturating_sub(2),
        lines.len(),
    )) * crate::utils::SCROLLBAR_W;
    let width = (text + bar + crate::utils::BOX_MARGIN).min(area.width);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(Color::White))
        .title(offset_title("Help").0);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    app.help_view_h = inner.height as usize;
    app.help_scroll = app
        .help_scroll
        .min(lines.len().saturating_sub(app.help_view_h));
    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        inner,
        crate::clickmap::ScrollView::MapHelp,
        app.help_scroll,
        lines.len(),
    );
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(app.help_scroll)
                .take(body.height as usize)
                .collect::<Vec<Line>>(),
        ),
        body,
    );
}

/// Blit the part of the canvas around the cursor into `view`, with the cursor
/// cell inverted, and register every visible point as a click target.
fn draw_map(
    frame: &mut Frame,
    view: Rect,
    app: &mut MapApp,
    map: &'static Map,
    regions: &mut ClickMap,
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
        canvas.put_point(10, 1, '○', Paint::Island);
        assert!(canvas.label(10, 1, "Foo", Paint::Name));
        assert_eq!(row(&canvas, 1), "          ○  Foo    ");
        // an east league blocks the right side, so the label goes left
        let mut canvas = Canvas::new(20, 3);
        canvas.put_point(10, 1, '○', Paint::Island);
        canvas.put(11, 1, '─', Paint::Sold);
        assert!(canvas.label(10, 1, "Foo", Paint::Name));
        assert_eq!(row(&canvas, 1), "     Foo  ○─        ");
    }

    /// A point keeps the cells around it whether or not anything is drawn in
    /// them, so a name on the row above or below slides clear of the point's
    /// own columns rather than sitting against its mark.
    #[test]
    fn a_name_keeps_out_of_the_cells_a_point_keeps() {
        // the point's row is league from end to end, so the clear row above
        // is where the name has to go
        let mut canvas = Canvas::new(30, 3);
        canvas.put_point(12, 2, '○', Paint::Island);
        for x in (0 .. 12).chain(13 .. 30) {
            canvas.put(x, 2, '─', Paint::Sold);
        }
        assert!(canvas.label(12, 2, "Foo", Paint::Name));
        assert_eq!(
            row(&canvas, 1),
            "        Foo                   "
        );
    }

    #[test]
    fn a_boxed_in_label_takes_the_nearest_free_row() {
        // an island with full-length leagues east and west, diagonals
        // leaving it both ways below, and clear rows above
        let mut canvas = Canvas::new(30, 4);
        canvas.put_point(12, 2, '○', Paint::Island);
        for x in (5 .. 12).chain(13 .. 20) {
            canvas.put(x, 2, '─', Paint::Sold);
        }
        canvas.put(10, 3, '╱', Paint::Sold);
        canvas.put(14, 3, '╲', Paint::Sold);
        assert!(canvas.label(12, 2, "Barbary", Paint::Name));
        // the row below is barred by the diagonals, and the row above by the
        // cells the point keeps unless the name slides five cells off centre:
        // a second row up, still centred on the island, reads nearer than that
        assert_eq!(
            row(&canvas, 0),
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
        canvas.put_point(12, 1, '○', Paint::Island);
        for x in (5 .. 12).chain(13 .. 20) {
            canvas.put(x, 1, '─', Paint::Sold);
        }
        // the row above is taken by another league, and one diagonal sits
        // just right of centre below: sliding left on that row is the
        // nearest spot left
        for x in 4 .. 21 {
            canvas.put(x, 0, '─', Paint::Sold);
        }
        canvas.put(16, 2, '╲', Paint::Sold);
        assert!(canvas.label(12, 1, "Barbary", Paint::Name));
        assert_eq!(
            row(&canvas, 2),
            "    Barbary     ╲             "
        );
    }

    #[test]
    fn with_no_room_anywhere_the_label_is_cut_rather_than_dropped() {
        // the island's row is all league, the row below is blocked, and the
        // row above only has a five-cell gap between two points
        let mut canvas = Canvas::new(14, 3);
        canvas.put(4, 1, '○', Paint::Island);
        for x in (0 .. 4).chain(5 .. 14) {
            canvas.put(x, 1, '─', Paint::Sold);
        }
        canvas.put(4, 2, '╲', Paint::Sold);
        canvas.put(10, 2, '○', Paint::Point);
        canvas.put(0, 0, '○', Paint::Point);
        canvas.put(8, 0, '○', Paint::Point);
        assert!(!canvas.label(4, 1, "Barbary", Paint::Name));
        assert_eq!(row(&canvas, 0), "○  Bar  ○     ");
    }

    /// Reads the drawing the way a player would: on every ocean's canvas,
    /// each island and each archipelago is named whole, every row of the name
    /// with two blank cells on either side of it, and a name on more than one
    /// row drawn as a block of rows centred on each other.
    #[test]
    fn every_place_is_named_whole_with_room_around_it() {
        let mut crowded = Vec::new();
        for map in crate::map::data::MAPS {
            let canvas = build_canvas(map, &MapApp::new());
            let rows: Vec<Vec<char>> =
                canvas.dump().lines().map(|l| l.chars().collect()).collect();
            // whether `text` sits at `(x, y)` with its gap clear on both
            // sides, which is what the eye reads as a name of its own
            let clear_at = |x: usize, y: usize, text: &str| {
                let want: Vec<char> = text.chars().collect();
                let Some(row) = rows.get(y) else {
                    return false;
                };
                if row.len() < x + want.len() {
                    return false;
                }
                row[x .. x + want.len()] == want[..]
                    && row[x.saturating_sub(LABEL_GAP) .. x]
                        .iter()
                        .all(|c| *c == ' ')
                    && row[x + want.len()
                        .. (x + want.len() + LABEL_GAP).min(row.len())]
                        .iter()
                        .all(|c| *c == ' ')
            };
            for place in map.islands.iter().chain(map.labels) {
                let block = label_rows(place.drawn());
                let w = block_width(&block);
                let found = (0 .. rows.len()).any(|y| {
                    (0 .. canvas.w).any(|x| {
                        block.iter().enumerate().all(|(i, text)| {
                            clear_at(x + indent(text, w), y + i, text)
                        })
                    })
                });
                if !found {
                    crowded.push(format!("{}: {}", map.ocean, place.name));
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
    /// the memorized line. The two approaches to Ashkelon Arch, one from
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

    /// Nothing is ever drawn over a diagonal, on any ocean: the two diagonals
    /// that would cross in one cell leave from points of the other `x + y`
    /// parity, and a map holds one parity class only. This is what lets a
    /// diagonal be laid with no thought for what is already there.
    #[test]
    fn no_cell_holds_two_diagonals_on_any_map() {
        for ocean in &crate::bare::BARE.oceans {
            let Some(map) = Map::for_ocean(&ocean.name) else {
                continue;
            };
            let mut seen = std::collections::HashSet::new();
            for league in map.leagues {
                let (a, b) = league.ends();
                let (ax, ay) = cell_of(a);
                let (bx, by) = cell_of(b);
                let cell = match league.heading {
                    Heading::Se => ((ax + bx) / 2, ay + 1),
                    Heading::Ne => ((ax + bx) / 2, by + 1),
                    _ => continue,
                };
                assert!(
                    seen.insert(cell),
                    "{} draws two diagonals in {cell:?}",
                    map.ocean
                );
            }
        }
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
        let mut regions = ClickMap::new();
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

    /// The help is longer than a 24-row window, so in one it scrolls rather
    /// than losing its foot: the last thing it says is reachable, and the bar
    /// that says so takes its columns from the popup rather than from the
    /// text.
    #[test]
    fn the_help_scrolls_in_a_window_too_short_for_it() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        app.help = true;
        let draw = |app: &mut MapApp, rows: u16| {
            let mut terminal = Terminal::new(TestBackend::new(120, rows))
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
                        &mut ClickMap::new(),
                    );
                })
                .expect("draw");
            format!("{}", terminal.backend())
        };
        let last = "the map readable; the keys sail it even so";
        let first = "Sailing the cursor";
        // tall enough for all of it: no scrolling, and so no bar
        let whole = draw(&mut app, 40);
        assert!(whole.contains(first) && whole.contains(last));
        assert_eq!(app.help_scroll, 0);

        let top = draw(&mut app, 24);
        assert!(top.contains(first) && !top.contains(last));
        app.help_scroll = usize::MAX;
        let foot = draw(&mut app, 24);
        assert!(
            foot.contains(last) && !foot.contains(first),
            "the far end of the help, clamped to its last line"
        );
        // the render clamps what the keys only ask for, and the clamp holds
        let settled = app.help_scroll;
        assert!(settled < usize::MAX);
        assert_eq!(draw(&mut app, 24), foot);
        assert_eq!(app.help_scroll, settled);

        // the help asks for no more height than the page it is on: at the
        // page's own floor it is still read, a window of it at a time
        app.help_scroll = 0;
        let tight = draw(&mut app, 11);
        assert!(tight.contains(first) && !tight.contains(last));
        app.help_scroll = usize::MAX;
        assert!(draw(&mut app, 11).contains(last));
    }

    #[test]
    fn click_boxes_reach_around_each_point_without_overlapping() {
        use ratatui::{Terminal, backend::TestBackend};

        let map = Map::for_ocean("Emerald").expect("Emerald map");
        let mut app = MapApp::new();
        app.cursor_on(map);
        let mut terminal =
            Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        let mut regions = ClickMap::new();
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
            .top()
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
                        &mut ClickMap::new(),
                    );
                })
                .expect("draw");
            format!("{}", terminal.backend())
        };
        let tally = format!("0/{} (0.0%)", map.points().len());
        assert!(!draw(&mut app).contains(&tally));
        app.help = true;
        assert!(
            draw(&mut app).contains("--user"),
            "the help is what asks for a pirate"
        );
        app.help = false;
        // the figure rides the frame, closed by the house-style run
        app.pirate = Some("Someone".to_owned());
        assert!(draw(&mut app).contains(&format!(" {tally} ───")));
    }

    /// A name standing for a whole archipelago stands in open water:
    /// [`CAPTION_PAD`] of blank cells around it on every ocean, with nothing
    /// drawn inside that box - no point, and no league running to one.
    #[test]
    fn an_archipelago_name_stands_in_open_water() {
        let (pad_x, pad_y) = CAPTION_PAD;
        for map in crate::map::data::MAPS {
            let canvas = build_canvas(map, &MapApp::new());
            for &(arch, (mx, my)) in &canvas.captions {
                let label = map
                    .labels
                    .iter()
                    .find(|label| {
                        label.name.strip_suffix(" Archipelago") == Some(arch)
                    })
                    .unwrap_or_else(|| panic!("{arch} on the map"));
                let rows = label_rows(label.drawn());
                let (w, h) = (block_width(&rows), rows.len());
                let (x, y) = (mx - w / 2, my - h / 2);
                for cy in
                    y.saturating_sub(pad_y) .. (y + h + pad_y).min(canvas.h)
                {
                    for cx in
                        x.saturating_sub(pad_x) .. (x + w + pad_x).min(canvas.w)
                    {
                        let letter = cy
                            .checked_sub(y)
                            .and_then(|i| rows.get(i))
                            .is_some_and(|row| {
                                let rx = x + indent(row, w);
                                (rx .. rx + row.chars().count()).contains(&cx)
                            });
                        assert!(
                            letter || canvas.is_free(cx, cy),
                            "{}: {arch} has {:?} at ({cx},{cy}), inside the \
                             water its name keeps",
                            map.ocean,
                            canvas.get(cx, cy).map(|(ch, _)| ch)
                        );
                    }
                }
            }
        }
    }

    /// Every archipelago's name is drawn on water its own islands are the
    /// nearest land to, so a name names the stretch of sea it sits on. The
    /// middle of that water is where a name is wanted; how far it ends up
    /// from the middle is what `dump_caption_placement` is for.
    #[test]
    fn every_archipelago_is_named_on_its_own_water() {
        for map in crate::map::data::MAPS {
            let regions = regions_of(map).expect("the map's regions");
            for (arch, at) in caption_points(map) {
                assert_eq!(
                    regions.owner(at),
                    Some(arch),
                    "{}: {arch} is named at {at:?}, which is not its water",
                    map.ocean
                );
            }
        }
    }

    /// Prints where every archipelago's name landed and how far that is from
    /// the middle of its water, for judging the placing across the oceans:
    /// `cargo test dump_caption_placement -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the caption placement for inspection"]
    fn dump_caption_placement() {
        for map in crate::map::data::MAPS {
            let Some(regions) = regions_of(map) else {
                continue;
            };
            for (arch, at) in caption_points(map) {
                let Some(&middle) = regions.middle.get(arch) else {
                    continue;
                };
                let middle = point_of(middle);
                let (dx, dy) = (
                    at.0 as i32 - middle.0 as i32,
                    at.1 as i32 - middle.1 as i32,
                );
                let own = regions.owner(at) == Some(arch);
                println!(
                    "{:9} {arch:24} middle {middle:?} drawn {at:?} off by \
                     ({dx},{dy}){}",
                    map.ocean,
                    if own { "" } else { " - NOT ITS OWN WATER" }
                );
            }
        }
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

    /// An archipelago that carries its own article is not given a second one.
    #[test]
    fn an_archipelago_named_with_its_article_is_said_without_another() {
        let map = Map::for_ocean("Obsidian").expect("Obsidian map");
        let sources = Sources {
            geo: bare::BARE.ocean("Obsidian"),
            islands: None,
            fetching_islands: false,
        };
        let magpie = island(map, "Magpie Island");
        let meta = metadata(&MapApp::new(), map, &sources, magpie);
        assert!(
            text(&meta.head)
                .contains(&"of Ye Bloody Bounding Main Archipelago".to_owned()),
            "{:?}",
            text(&meta.head)
        );
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
        let mut regions = ClickMap::new();
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
            .top()
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
