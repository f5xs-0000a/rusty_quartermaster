//! Rendering for the **Voyage Statistics** page (the entire app body).
//!
//! Layout: a centered, scrolling body — header, the Sea Battles table, the stat
//! sections (Timing / Loot / Enemies / Advantage / Consumption), then the three
//! charts as full-width bordered boxes — over a pinned footer, with a focus-bound
//! tooltip strip below the whole widget. The stat numbers and the charts form one
//! focus chain (↑/↓): pressing Down off the last number focuses the first chart.
//! The body scrolls to keep the focused item visible; because the chart boxes are
//! bordered, the body is rendered into an offscreen [`Buffer`] and the visible
//! window blitted, so partially-scrolled boxes clip cleanly. Charts (PoE
//! box-&-whiskers, PoE-per-fight bars, total-value point-vs-box — each this
//! voyage vs historical) enlarge to a popup on Enter. The page shows the current
//! vessel's live or most-recent-completed run. All figures are computed up-front
//! (see [`crate::voyage::stats`]) and handed in via [`VoyageView`]. See the
//! `voyage-statistics-model` memory.

use ratatui::buffer::Buffer;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::clickmap::{ClickRegion, ClickTarget};
use crate::utils::offset_title;
use crate::voyage::stats::{box_plot, BattleStats, BoxPlot, ConsumptionStats};

/// The three charts, in display order.
pub const CHART_TITLES: [&str; 3] = ["PoE won", "PoE per fight", "Total value"];

/// One-liners shown below the widget when a chart is focused (parallel to
/// [`CHART_TITLES`]).
const CHART_TOOLTIPS: [&str; 3] = [
    "Pieces of eight per won fight — this voyage's spread vs history. Enter to enlarge.",
    "PoE of each won fight (oldest first, newest last) vs a historical box. Enter to enlarge.",
    "This voyage's net value against past voyages. Enter to enlarge.",
];

/// Height in rows of each chart's bordered box in the scrolling body.
const CHART_H: u16 = 7;

/// Which button the save/discard prompt has focused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
}

/// Persistent UI state for the page (mouse/keyboard-driven).
#[derive(Default)]
pub struct VoyageStatsUi {
    /// Vertical scroll offset, in rows. Driven by [`Self::focus`] — the body
    /// auto-scrolls to keep the focused item visible.
    pub scroll: u16,
    /// Focused item index. Indices `0..n_stats` are the in-body focusables (the
    /// Sea Battles section, then the Timing-onward stat numbers); `n_stats..
    /// n_stats+3` are the three charts. The focused item's tooltip shows below
    /// the widget.
    pub focus: usize,
    /// Number of non-chart focusables in the last render — the boundary at which
    /// [`Self::focus`] crosses into the charts. Set by `render`.
    pub n_stats: usize,
    /// When `Some`, the save/discard prompt is open with this button focused.
    pub prompt: Option<SaveChoice>,
    /// When `Some(i)`, chart `i` is enlarged in a popup.
    pub chart_popup: Option<usize>,
}

/// Per-fight series for the charts: current voyage vs persisted history.
#[derive(Default, Clone)]
pub struct ChartData {
    /// PoE of each won fight this voyage (chronological).
    pub cur_won_poe: Vec<f64>,
    /// PoE of every won fight across saved history.
    pub hist_won_poe: Vec<f64>,
    /// PoE of the most recent won fight this voyage (highlighted marker).
    pub last_win: Option<f64>,
    /// This voyage's total value (net PoE for now; goods fold in later).
    pub cur_total: f64,
    /// Total value of each past voyage (one point each).
    pub hist_totals: Vec<f64>,
}

/// Everything the page needs to draw one voyage, computed by the caller so this
/// module stays free of app-state plumbing.
pub struct VoyageView {
    pub has_voyage: bool,
    /// Vessel name — the centered headline.
    pub vessel: Option<String>,
    /// Ship type (e.g. "War Frigate"), from the vessel's chosen ship.
    pub ship_type: Option<String>,
    /// The run's clock span, e.g. `"12:34 to 13:50"` (end = current time while
    /// still at sea, or the port time once ported). `None` until we've sailed.
    pub period: Option<String>,
    /// Elapsed run time — final duration if ported, else live elapsed.
    pub elapsed_secs: Option<i64>,
    /// Cannon-size label for the cannonball row ("Small"/"Medium"/"Large").
    pub cannon_label: Option<String>,
    /// The displayed run is finished and not yet saved/dismissed — offer to save.
    pub saveable: bool,
    pub battle: BattleStats,
    pub consumption: ConsumptionStats,
    pub charts: ChartData,
}

/// Fixed widget width (the stat rows are narrow; a full-width box wastes space).
const BODY_WIDTH: u16 = 56;

pub fn render(
    frame: &mut Frame,
    full: Rect,
    view: &VoyageView,
    ui: &mut VoyageStatsUi,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    // Reserve a tooltip strip below the widget (focus-bound, full content width).
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(full);
    let (widget_area, tip_area) = (outer[0], outer[1]);

    // Shrink horizontally and center the widget within the available area.
    let width = BODY_WIDTH.min(widget_area.width);
    let area = Rect {
        x: widget_area.x + widget_area.width.saturating_sub(width) / 2,
        y: widget_area.y,
        width,
        height: widget_area.height,
    };

    let (title, _) = offset_title("Voyage Statistics");
    let border = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !view.has_voyage {
        let para = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("No voyage tracked yet.", Style::default().bold())),
            Line::from(Span::styled(
                "Set sail on a vessel to begin recording stats.",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .centered();
        frame.render_widget(para, inner);
        return;
    }

    // Pinned header (ship name / type / clock span), kept out of the scroll so
    // it never disappears. The scroll body starts at "Sea Battles".
    let iw = inner.width as usize;
    let mut header: Vec<Line<'static>> = vec![centered_line(
        view.vessel.clone().unwrap_or_default(),
        iw,
        Style::default().bold(),
    )];
    if let Some(t) = &view.ship_type {
        header.push(centered_line(t.clone(), iw, Style::default().fg(Color::Gray)));
    }
    if let Some(p) = &view.period {
        header.push(centered_line(p.clone(), iw, Style::default().fg(Color::DarkGray)));
    }
    header.push(Line::from("")); // separator from the scrolling body
    let header_h = header.len() as u16;

    // Split: pinned header, the scrollable body (Sea Battles + stats + charts),
    // then a pinned footer.
    let parts = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    let (header_area, body, footer) = (parts[0], parts[1], parts[2]);
    frame.render_widget(Paragraph::new(header), header_area);

    // Build the body lines and the parallel focusable list (Sea Battles, then
    // the stat numbers). The three charts follow them in the focus order.
    let mut built = build_lines(view, body.width as usize);
    let n_stats = built.focusable.len();
    let n_charts = CHART_TITLES.len();
    let n_focus = n_stats + n_charts;
    ui.n_stats = n_stats;
    ui.focus = ui.focus.min(n_focus.saturating_sub(1));
    let focused_chart = if ui.focus >= n_stats {
        Some(ui.focus - n_stats)
    } else {
        None
    };

    // Content geometry: stat lines, a 1-row gap, then the stacked chart boxes.
    let n_lines = built.lines.len() as u16;
    let charts_top = n_lines + 1;
    let total_h = charts_top + n_charts as u16 * CHART_H;

    // Row + height of the focused item, for auto-scroll.
    let (focus_row, focus_h) = match focused_chart {
        Some(ci) => (charts_top + ci as u16 * CHART_H, CHART_H),
        None if n_stats > 0 => (built.focusable[ui.focus].line as u16, 1),
        None => (0, 0),
    };
    let max_scroll = total_h.saturating_sub(body.height);
    if focused {
        if focus_row < ui.scroll {
            ui.scroll = focus_row;
        } else if focus_row + focus_h > ui.scroll + body.height {
            ui.scroll = (focus_row + focus_h).saturating_sub(body.height);
        }
    }
    ui.scroll = ui.scroll.min(max_scroll);

    // Highlight the focused stat row (charts highlight via their border below).
    if focused && focused_chart.is_none() && n_stats > 0 {
        let li = built.focusable[ui.focus].line;
        built.lines[li] = built.lines[li]
            .clone()
            .patch_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    }

    // Render the whole scroll content into an offscreen canvas, then blit the
    // visible window — so partially-scrolled chart boxes clip cleanly.
    let mut canvas = Buffer::empty(Rect::new(0, 0, body.width, total_h.max(1)));
    Paragraph::new(built.lines).render(Rect::new(0, 0, body.width, n_lines), &mut canvas);
    render_charts(
        &mut canvas,
        Rect::new(0, charts_top, body.width, n_charts as u16 * CHART_H),
        &view.charts,
        if focused { focused_chart } else { None },
    );
    for row in 0..body.height {
        let src_y = ui.scroll + row;
        if src_y >= total_h {
            break;
        }
        for col in 0..body.width {
            if let Some(src) = canvas.cell(Position::new(col, src_y)).cloned() {
                if let Some(dst) = frame.buffer_mut().cell_mut(Position::new(body.x + col, body.y + row)) {
                    *dst = src;
                }
            }
        }
    }

    // Click regions: stat rows and chart boxes that fall within the body window.
    for (i, f) in built.focusable.iter().enumerate() {
        let line = f.line as u16;
        if line >= ui.scroll && line < ui.scroll + body.height {
            regions.push(ClickRegion {
                rect: Rect::new(body.x, body.y + (line - ui.scroll), body.width, 1),
                target: ClickTarget::VoyageStat { idx: i },
            });
        }
    }
    for i in 0..n_charts {
        let top = charts_top + i as u16 * CHART_H;
        let vis_top = top.max(ui.scroll);
        let vis_bot = (top + CHART_H).min(ui.scroll + body.height);
        if vis_top < vis_bot {
            regions.push(ClickRegion {
                rect: Rect::new(body.x, body.y + (vis_top - ui.scroll), body.width, vis_bot - vis_top),
                target: ClickTarget::VoyageChart { idx: i },
            });
        }
    }

    // Tooltip for the focused item, below the widget.
    if focused {
        let tip = match focused_chart {
            Some(ci) => CHART_TOOLTIPS[ci].to_string(),
            None if n_stats > 0 => built.focusable[ui.focus].tooltip.clone(),
            None => String::new(),
        };
        if !tip.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(tip))
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: true })
                    .centered(),
                tip_area,
            );
        }
    }

    // Footer: save hint takes priority; otherwise the nav hint.
    if view.saveable {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "S  save voyage to history  ·  D  discard",
                Style::default().fg(Color::Cyan),
            )))
            .centered(),
            footer,
        );
        regions.push(ClickRegion {
            rect: footer,
            target: ClickTarget::VoyageSaveOpen,
        });
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↑/↓ move · Enter enlarge chart",
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            footer,
        );
    }

    // Modal popups (chart enlarge takes priority over the save prompt). The
    // chart popup uses the full content width, not the narrow body column.
    if let Some(i) = ui.chart_popup {
        render_chart_popup(frame, full, i, &view.charts, regions);
    } else if let Some(choice) = ui.prompt {
        render_save_prompt(frame, area, choice, regions);
    }
}

/// Modal: "Save this voyage to history, or discard it?" with two buttons.
fn render_save_prompt(
    frame: &mut Frame,
    area: Rect,
    choice: SaveChoice,
    regions: &mut Vec<ClickRegion>,
) {
    // Backdrop swallows clicks outside the box (acts as cancel).
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageSaveCancel,
    });

    let w = 44.min(area.width);
    let h = 7.min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(" Save voyage? ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::vertical([
        Constraint::Length(1), // prompt
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
        Constraint::Min(0),    // hint
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new("Persist this run to your history?").centered(),
        rows[0],
    );

    // Two side-by-side buttons.
    let btns = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(rows[2]);
    let button = |label: &str, focused: bool| {
        let style = if focused {
            Style::default().fg(Color::Black).bg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::Cyan)
        };
        Paragraph::new(Line::from(Span::styled(format!("[ {label} ]"), style))).centered()
    };
    frame.render_widget(button("Save", choice == SaveChoice::Save), btns[0]);
    frame.render_widget(button("Discard", choice == SaveChoice::Discard), btns[1]);
    regions.push(ClickRegion {
        rect: btns[0],
        target: ClickTarget::VoyageSaveConfirm,
    });
    regions.push(ClickRegion {
        rect: btns[1],
        target: ClickTarget::VoyageSaveDiscard,
    });

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "←/→ select · Enter confirm · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )))
        .centered(),
        rows[3],
    );
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

fn cur_style() -> Style {
    Style::default().fg(Color::Cyan)
}
fn hist_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Render the three charts as full-width bordered boxes stacked down `area`,
/// into `canvas` (an offscreen buffer the caller blits into the scroll body).
/// `focused_chart` highlights one box's border.
fn render_charts(canvas: &mut Buffer, area: Rect, data: &ChartData, focused_chart: Option<usize>) {
    for i in 0..CHART_TITLES.len() {
        let slot = Rect::new(area.x, area.y + i as u16 * CHART_H, area.width, CHART_H);
        let border = if focused_chart == Some(i) {
            Style::default().fg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let (title, _) = offset_title(CHART_TITLES[i]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title);
        let inner = block.inner(slot);
        block.render(slot, canvas);
        Paragraph::new(chart_lines(i, data, inner.width as usize, inner.height as usize))
            .render(inner, canvas);
    }
}

/// Render the enlarged chart popup over `area`.
fn render_chart_popup(
    frame: &mut Frame,
    area: Rect,
    idx: usize,
    data: &ChartData,
    regions: &mut Vec<ClickRegion>,
) {
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageChartClose,
    });
    let w = area.width.saturating_sub(2).min(78).max(24);
    let h = area.height.saturating_sub(2).min(20).max(6);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let (title, _) = offset_title(CHART_TITLES[idx]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(title)
        .title_bottom(Line::from(" Esc to close ").right_aligned());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(chart_lines(
            idx,
            data,
            inner.width as usize,
            inner.height as usize,
        )),
        inner,
    );
}

/// Build the lines for chart `idx`, fitting `width`×`height` (used at both
/// mini and enlarged sizes — it scales by the area it's given).
fn chart_lines(idx: usize, data: &ChartData, width: usize, height: usize) -> Vec<Line<'static>> {
    match idx {
        0 => poe_box_lines(data, width),
        1 => poe_bar_lines(data, width, height),
        _ => total_value_lines(data, width),
    }
}

/// Chart 0 — box & whiskers of won-fight PoE: this voyage vs historical, with a
/// marker at the most recent win.
fn poe_box_lines(data: &ChartData, width: usize) -> Vec<Line<'static>> {
    let axis = width.saturating_sub(5);
    let range = combined_range(&[data.cur_won_poe.as_slice(), data.hist_won_poe.as_slice()]);
    let mut lines = vec![
        box_or_msg("this ", box_plot(&data.cur_won_poe), range, axis, data.last_win, cur_style()),
        box_or_msg("hist ", box_plot(&data.hist_won_poe), range, axis, None, hist_style()),
    ];
    if let Some((lo, hi)) = range {
        lines.push(axis_line(lo, hi, width));
    }
    lines.push(Line::from(Span::styled(
        "◆ last win",
        Style::default().fg(Color::Cyan),
    )));
    lines
}

/// Chart 1 — bar graph of won-fight PoE (first two, then the latest that fit),
/// with a historical box & whiskers on the last row.
fn poe_bar_lines(data: &ChartData, width: usize, height: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let vals = &data.cur_won_poe;
    if vals.is_empty() {
        lines.push(Line::from(Span::styled(
            "no fights yet",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let hi = vals.iter().copied().fold(1.0_f64, f64::max);
        let bar_cols = width.saturating_sub(14); // "#NN " + value
        let cap = height.saturating_sub(2).max(1); // leave a row for the hist box
        for slot in pick_bars(vals.len(), cap) {
            match slot {
                Some(i) => {
                    let v = vals[i];
                    let filled = if hi > 0.0 {
                        ((v / hi) * bar_cols as f64).round() as usize
                    } else {
                        0
                    };
                    let bar = "█".repeat(filled.min(bar_cols));
                    lines.push(Line::from(Span::styled(
                        format!("#{:<3}{bar} {}", i + 1, commas(v.round() as i64)),
                        cur_style(),
                    )));
                }
                None => lines.push(Line::from(Span::styled(
                    "  …",
                    Style::default().fg(Color::DarkGray),
                ))),
            }
        }
    }
    // Historical reference box on the final row.
    let axis = width.saturating_sub(5);
    if let Some(range) = combined_range(&[data.hist_won_poe.as_slice()]) {
        lines.push(box_or_msg(
            "hist ",
            box_plot(&data.hist_won_poe),
            Some(range),
            axis,
            None,
            hist_style(),
        ));
    }
    lines
}

/// Chart 2 — this voyage's total value as a single point against a historical
/// box & whiskers of past voyages' totals.
fn total_value_lines(data: &ChartData, width: usize) -> Vec<Line<'static>> {
    let axis = width.saturating_sub(5);
    let mut all = data.hist_totals.clone();
    all.push(data.cur_total);
    let range = combined_range(&[all.as_slice()]);
    let mut lines = vec![box_or_msg(
        "hist ",
        box_plot(&data.hist_totals),
        range,
        axis,
        Some(data.cur_total),
        hist_style(),
    )];
    let hist_med = box_plot(&data.hist_totals).map(|b| commas(b.median.round() as i64));
    lines.push(Line::from(Span::styled(
        format!(
            "◆ this {}   ·   hist median {}",
            commas(data.cur_total.round() as i64),
            hist_med.unwrap_or_else(|| "—".to_string())
        ),
        Style::default().fg(Color::Cyan),
    )));
    lines
}

/// A 5-char-prefixed box-and-whiskers row, or a dim "no data" message.
fn box_or_msg(
    prefix: &str,
    bp: Option<BoxPlot>,
    range: Option<(f64, f64)>,
    axis: usize,
    marker: Option<f64>,
    style: Style,
) -> Line<'static> {
    match (bp, range) {
        (Some(bp), Some((lo, hi))) if axis > 0 => Line::from(Span::styled(
            format!("{prefix}{}", box_line(axis, lo, hi, &bp, marker)),
            style,
        )),
        _ => Line::from(Span::styled(
            format!("{prefix}(no data)"),
            Style::default().fg(Color::DarkGray),
        )),
    }
}

/// A "lo …… hi" axis label line.
fn axis_line(lo: f64, hi: f64, width: usize) -> Line<'static> {
    let lo_s = commas(lo.round() as i64);
    let hi_s = commas(hi.round() as i64);
    let gap = width.saturating_sub(5 + lo_s.len() + hi_s.len()).max(1);
    Line::from(Span::styled(
        format!("     {lo_s}{}{hi_s}", " ".repeat(gap)),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Column for value `v` within `[lo, hi]` mapped onto `width` cells.
fn val_col(v: f64, lo: f64, hi: f64, width: usize) -> usize {
    if width <= 1 || hi <= lo {
        return 0;
    }
    (((v - lo) / (hi - lo)) * (width - 1) as f64)
        .round()
        .clamp(0.0, (width - 1) as f64) as usize
}

/// Build a horizontal box-and-whiskers string of `width` cells over `[lo, hi]`.
/// `n == 1` renders a single point; `marker` overlays a `◆` at its value.
fn box_line(width: usize, lo: f64, hi: f64, bp: &BoxPlot, marker: Option<f64>) -> String {
    let w = width.max(1);
    let mut cells = vec![' '; w];
    if bp.n == 1 {
        cells[val_col(bp.median, lo, hi, w)] = '●';
    } else {
        let cmin = val_col(bp.min, lo, hi, w);
        let cmax = val_col(bp.max, lo, hi, w);
        let cq1 = val_col(bp.q1, lo, hi, w);
        let cq3 = val_col(bp.q3, lo, hi, w);
        for c in cmin..=cmax {
            cells[c] = '─';
        }
        for c in cq1..=cq3 {
            cells[c] = '█';
        }
        cells[cmin] = '├';
        cells[cmax] = '┤';
        cells[val_col(bp.median, lo, hi, w)] = '┃';
    }
    if let Some(m) = marker {
        cells[val_col(m, lo, hi, w)] = '◆';
    }
    cells.into_iter().collect()
}

/// Min/max across several value slices, or `None` if all empty.
fn combined_range(slices: &[&[f64]]) -> Option<(f64, f64)> {
    let (mut lo, mut hi, mut any) = (f64::INFINITY, f64::NEG_INFINITY, false);
    for s in slices {
        for &v in *s {
            any = true;
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    any.then_some((lo, hi))
}

/// Choose which bar indices to show given `cap` rows: all if they fit, else the
/// first two, an ellipsis (`None`), then the latest that fit.
fn pick_bars(n: usize, cap: usize) -> Vec<Option<usize>> {
    if n <= cap {
        return (0..n).map(Some).collect();
    }
    if cap < 4 {
        return (n - cap..n).map(Some).collect();
    }
    let mut v = vec![Some(0), Some(1), None];
    let tail = cap - 3;
    v.extend((n - tail..n).map(Some));
    v
}

/// A focusable stat: which built line it lives on, and the tooltip to show
/// below the widget when it's focused.
struct Focusable {
    line: usize,
    tooltip: String,
}

/// The page body (everything below the pinned header): the rendered lines plus
/// the parallel list of focusables in focus order — the Sea Battles section
/// first, then every Timing-onward stat number.
#[derive(Default)]
struct Built {
    lines: Vec<Line<'static>>,
    focusable: Vec<Focusable>,
    /// Content width, so section headers can center themselves.
    width: usize,
}

impl Built {
    fn line(&mut self, l: Line<'static>) {
        self.lines.push(l);
    }
    fn blank(&mut self) {
        self.lines.push(Line::from(""));
    }
    fn section(&mut self, title: &str) {
        self.lines.push(section(title, self.width));
    }
    /// Push a section header that is itself focusable (its header line carries
    /// the `tooltip`). Used for sections you can "enter", like Sea Battles.
    fn focus_section(&mut self, title: &str, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.lines.len(),
            tooltip: tooltip.to_string(),
        });
        self.lines.push(section(title, self.width));
    }
    /// Push a focusable `label .... value` stat row tied to `tooltip`.
    fn stat(&mut self, label: &str, value: String, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.lines.len(),
            tooltip: tooltip.to_string(),
        });
        self.lines.push(stat(label, value, self.width));
    }
}

fn build_lines(view: &VoyageView, width: usize) -> Built {
    let b = &view.battle;
    let c = &view.consumption;
    let mut out = Built {
        width,
        ..Default::default()
    };

    // Sea Battles — a focusable section over a full-width three-column table
    // (labels over counts). The header (ship name/type/period) is pinned above
    // the scroll, so the body starts here.
    out.focus_section(
        "Sea Battles",
        "Win / loss / disengage tally — per-battle detail coming soon.",
    );
    let head = Style::default().fg(Color::DarkGray);
    out.line(three_col(
        width,
        [
            ("Wins".to_string(), head),
            ("Losses".to_string(), head),
            ("Disengages".to_string(), head),
        ],
    ));
    out.line(three_col(
        width,
        [
            (b.wins.to_string(), Style::default().fg(Color::Green).bold()),
            (b.losses.to_string(), Style::default().fg(Color::Red).bold()),
            (
                b.disengages.to_string(),
                Style::default().fg(Color::Yellow).bold(),
            ),
        ],
    ));
    out.blank();

    // Timing — every number from here on is focusable with a tooltip.
    out.section("Timing");
    out.stat(
        "In pillage",
        opt_dur(view.elapsed_secs.map(|s| s as f64)),
        "Total time from setting sail to putting into port.",
    );
    out.stat(
        "Avg sea battle",
        opt_dur(b.avg_battle_secs),
        "Average length of a sea engagement, interception to resolution.",
    );
    out.stat(
        "  · naval (→grapple)",
        opt_dur(b.avg_naval_secs),
        "Average time from interception to grappling — the naval phase.",
    );
    out.stat(
        "  · boarding",
        opt_dur(b.avg_boarding_secs),
        "Average time from grapple to Game Over — the boarding melee.",
    );
    out.stat(
        "Time in battle",
        dur(b.time_in_battle_secs),
        "Total time spent in sea engagements this voyage.",
    );
    out.stat(
        "Time at Sea",
        b.time_at_sea_secs.map(dur).unwrap_or_else(|| "—".to_string()),
        "Time spent searching and sailing — pillage time minus battle time.",
    );
    out.blank();

    // Loot.
    out.section("Loot");
    out.stat(
        "PoE / fight (won)",
        opt_commas(b.poe_per_fight_won),
        "Average pieces of eight plundered per won fight.",
    );
    out.stat(
        "PoE / fight (net)",
        opt_commas(b.poe_per_fight_net),
        "Average PoE per fight, counting lost fights as negative.",
    );
    out.stat(
        "Goods / fight",
        opt1(b.goods_per_fight),
        "Average units of goods won per fight (a count — the log never itemizes).",
    );
    out.stat(
        "PoE total (net)",
        commas(b.poe_net_total),
        "Net pieces of eight across every fight (losses subtracted).",
    );
    out.stat(
        "PoE / crew",
        opt_commas(b.poe_per_crew),
        "Net PoE divided by the time-weighted average crew aboard.",
    );
    out.stat(
        "PoE / crew / fight (W)",
        opt_commas(b.poe_per_crew_per_fight_won),
        "Won PoE per crew member per won fight.",
    );
    out.stat(
        "PoE / crew / fight (W+L)",
        opt_commas(b.poe_per_crew_per_fight_all),
        "Net PoE per crew member per fight, wins and losses together.",
    );
    out.blank();

    // Enemies by category — only categories that occurred (no zero rows).
    if !b.categories.is_empty() {
        out.section("Enemies");
        for (label, count) in &b.categories {
            out.stat(
                label,
                count.to_string(),
                &format!("Sea battles fought against {label}."),
            );
        }
        out.blank();
    }

    // Advantage (only when damage was tracked for at least one fight).
    if b.avg_advantage_dmg.is_some() || b.avg_advantage_crew.is_some() {
        out.section("Advantage (avg)");
        out.stat(
            "Damage advantage",
            b.avg_advantage_dmg
                .map(|a| format!("{:+.0}%", a * 100.0))
                .unwrap_or_else(|| "—".to_string()),
            "Average morale-damage edge over the enemy, from the Damage calculator.",
        );
        out.stat(
            "Crew advantage",
            b.avg_advantage_crew
                .map(|a| format!("{a:+.1}"))
                .unwrap_or_else(|| "—".to_string()),
            "Average headcount edge: our crew vs the enemy's, weighted by morale.",
        );
        out.blank();
    }

    // Consumption.
    out.section("Consumption");
    let balls_label = format!(
        "Cannonballs ({})",
        view.cannon_label.clone().unwrap_or_else(|| "—".to_string())
    );
    out.stat(
        &balls_label,
        opt_u64(c.balls),
        "Cannonballs fired this voyage (Restock minus Stock for the ship's size).",
    );
    out.stat(
        "  · per battle",
        opt1(c.balls_per_battle),
        "Average cannonballs fired per sea battle.",
    );
    out.stat(
        "Alcohol",
        commas(c.alcohol as i64),
        "Alcohol consumed, weighted by potency (swill 2 / grog 3 / fine rum 6).",
    );
    out.stat(
        "  · per crew",
        opt1(c.alcohol_per_crew),
        "Alcohol per crew member aboard.",
    );
    out.stat(
        "  · per crew / min",
        opt2(c.alcohol_per_crew_per_min),
        "Alcohol per crew member per minute of the run.",
    );
    out.stat(
        "Rum spice",
        commas(c.rum_spice as i64),
        "Rum spice consumed this voyage.",
    );
    out.stat(
        "  · per swabbie",
        opt1(c.rum_spice_per_swabbie),
        "Rum spice per swabbie — spice mainly fuels swabbies.",
    );
    out.stat(
        "  · per swabbie / min",
        opt2(c.rum_spice_per_swabbie_per_min),
        "Rum spice per swabbie per minute (approximate — see the note below).",
    );
    out.line(Line::from(Span::styled(
        "⚠ spice approx. (swabbies / ran out skew it)".to_string(),
        Style::default().fg(Color::DarkGray).italic(),
    )));

    out
}

/// A single line of `text` centered within `width` cells, styled.
fn centered_line(text: String, width: usize, style: Style) -> Line<'static> {
    let w = width.max(1);
    Line::from(Span::styled(format!("{text:^w$}"), style))
}

/// A full-width row of three centered, individually-styled columns.
fn three_col(width: usize, cells: [(String, Style); 3]) -> Line<'static> {
    let col = (width / 3).max(1);
    let spans: Vec<Span<'static>> = cells
        .into_iter()
        .map(|(s, style)| Span::styled(format!("{s:^col$}"), style))
        .collect();
    Line::from(spans)
}

/// A centered yellow section header line.
fn section(title: &str, width: usize) -> Line<'static> {
    let w = width.max(1);
    Line::from(Span::styled(
        format!("{title:^w$}"),
        Style::default().fg(Color::Yellow).bold(),
    ))
}

/// A stat row spanning the whole `width`: label flush-left, value flush-right.
/// When the two can't both fit (a narrow widget in a wider terminal), they fall
/// back to a plain two-space separator (`label  value`).
fn stat(label: &str, value: String, width: usize) -> Line<'static> {
    let lw = label.chars().count();
    let vw = value.chars().count();
    let text = if width >= lw + 2 + vw {
        format!("{label}{}{value}", " ".repeat(width - lw - vw))
    } else {
        format!("{label}  {value}")
    };
    Line::from(text)
}

// -- value formatters --

/// `12,345`-style thousands grouping for a signed integer.
fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, ch) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch as char);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `1h 04m` / `3m 45s` / `45s` from a second count.
fn dur(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else {
        format!("{sec}s")
    }
}

fn opt_dur(x: Option<f64>) -> String {
    x.map(|v| dur(v.round() as i64)).unwrap_or_else(|| "—".to_string())
}

fn opt_commas(x: Option<f64>) -> String {
    x.map(|v| commas(v.round() as i64)).unwrap_or_else(|| "—".to_string())
}

fn opt_u64(x: Option<u64>) -> String {
    x.map(|v| commas(v as i64)).unwrap_or_else(|| "—".to_string())
}

fn opt1(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".to_string())
}

fn opt2(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".to_string())
}
