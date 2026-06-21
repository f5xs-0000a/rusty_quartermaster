//! Rendering for the **Voyage Statistics** page (the entire app body).
//!
//! Layout: scrollable stat sections (Timing / Loot / Enemies / Advantage /
//! Consumption) on top, a strip of three selectable mini-charts below, and a
//! pinned footer. Charts (PoE box-&-whiskers, PoE-per-fight bars, total-value
//! point-vs-box — each this voyage vs historical) enlarge to a popup. The page
//! shows the current vessel's live or most-recent-completed run. All figures are
//! computed up-front (see [`crate::voyage::stats`]) and handed in via
//! [`VoyageView`]; this module is pure rendering. See the
//! `voyage-statistics-model` memory.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::clickmap::{ClickRegion, ClickTarget};
use crate::utils::offset_title;
use crate::voyage::stats::{box_plot, BattleStats, BoxPlot, ConsumptionStats};

/// The three charts, in display order.
pub const CHART_TITLES: [&str; 3] = ["PoE won", "PoE per fight", "Total value"];

/// Which button the save/discard prompt has focused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
}

/// Persistent UI state for the page (mouse/keyboard-driven).
#[derive(Default)]
pub struct VoyageStatsUi {
    /// Vertical scroll offset, in lines.
    pub scroll: u16,
    /// When `Some`, the save/discard prompt is open with this button focused.
    pub prompt: Option<SaveChoice>,
    /// Selected mini-chart index (0..3).
    pub chart_sel: usize,
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
    /// Vessel name shown in the subtitle.
    pub vessel: Option<String>,
    /// Job kind in force (e.g. "Pillaging, Average to Hard Barbarians").
    pub job: Option<String>,
    /// Whether the run has ported (finalized) vs. still at sea.
    pub ported: bool,
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
    // Shrink horizontally and center within the available area.
    let width = BODY_WIDTH.min(full.width);
    let area = Rect {
        x: full.x + full.width.saturating_sub(width) / 2,
        y: full.y,
        width,
        height: full.height,
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

    ui.chart_sel = ui.chart_sel.min(CHART_TITLES.len() - 1);

    // Split: scrollable stats (top), a charts strip, then a pinned footer.
    let charts_h = 8u16.min(inner.height.saturating_sub(2));
    let parts = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(charts_h),
        Constraint::Length(1),
    ])
    .split(inner);
    let (body, charts_area, footer) = (parts[0], parts[1], parts[2]);

    let lines = build_lines(view);
    let max_scroll = (lines.len() as u16).saturating_sub(body.height);
    ui.scroll = ui.scroll.min(max_scroll);
    frame.render_widget(Paragraph::new(lines).scroll((ui.scroll, 0)), body);

    render_mini_charts(frame, charts_area, &view.charts, ui, focused, regions);

    // Footer: save hint takes priority; otherwise the chart nav hint.
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
                "←/→ select chart · Enter enlarge",
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

/// Render the three selectable mini-charts (2 per row) and push their click
/// regions.
fn render_mini_charts(
    frame: &mut Frame,
    area: Rect,
    data: &ChartData,
    ui: &VoyageStatsUi,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    if area.height < 3 || area.width < 6 {
        return;
    }
    let half = area.height / 2;
    let vrows = Layout::vertical([Constraint::Length(half), Constraint::Min(0)]).split(area);
    let top = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(vrows[0]);
    let bottom = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(vrows[1]);
    let slots = [top[0], top[1], bottom[0]];

    let active = focused && ui.chart_popup.is_none() && ui.prompt.is_none();
    for (i, slot) in slots.iter().enumerate() {
        let selected = active && i == ui.chart_sel;
        let border = if selected {
            Style::default().fg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(format!(" {} ", CHART_TITLES[i]));
        let inner = block.inner(*slot);
        frame.render_widget(block, *slot);
        frame.render_widget(
            Paragraph::new(chart_lines(i, data, inner.width as usize, inner.height as usize)),
            inner,
        );
        regions.push(ClickRegion {
            rect: *slot,
            target: ClickTarget::VoyageChart { idx: i },
        });
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(format!(" {}  (Esc to close) ", CHART_TITLES[idx]));
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

fn build_lines(view: &VoyageView) -> Vec<Line<'static>> {
    let b = &view.battle;
    let c = &view.consumption;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: vessel (bold), job (dim), status, winrate — one per line so the
    // box can stay narrow.
    let vessel = view.vessel.clone().unwrap_or_default();
    lines.push(Line::from(Span::styled(vessel, Style::default().bold())));
    if let Some(job) = &view.job {
        lines.push(Line::from(Span::styled(
            job.clone(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let status = match (view.ported, view.elapsed_secs) {
        (true, Some(s)) => format!("ported · {}", dur(s)),
        (false, Some(s)) => format!("at sea ({})", dur(s)),
        _ => "—".to_string(),
    };
    lines.push(Line::from(format!("Status: {status}")));
    let decisive = b.wins + b.losses;
    let winpct = if decisive > 0 {
        format!("  ({}% win)", b.wins * 100 / decisive)
    } else {
        String::new()
    };
    lines.push(Line::from(Span::styled(
        format!(
            "Sea battles: {}W – {}L – {}D{winpct}",
            b.wins, b.losses, b.disengages
        ),
        Style::default().bold(),
    )));
    lines.push(Line::from(""));

    // Timing.
    lines.push(section("Timing"));
    lines.push(stat("In pillage", opt_dur(view.elapsed_secs.map(|s| s as f64))));
    lines.push(stat("Avg sea battle", opt_dur(b.avg_battle_secs)));
    lines.push(stat("  · naval (→grapple)", opt_dur(b.avg_naval_secs)));
    lines.push(stat("  · boarding", opt_dur(b.avg_boarding_secs)));
    lines.push(stat("Time in battle", dur(b.time_in_battle_secs)));
    lines.push(stat(
        "Time at sea",
        b.time_at_sea_secs.map(dur).unwrap_or_else(|| "—".to_string()),
    ));
    lines.push(Line::from(""));

    // Loot.
    lines.push(section("Loot"));
    lines.push(stat("PoE / fight (won)", opt_commas(b.poe_per_fight_won)));
    lines.push(stat("PoE / fight (net)", opt_commas(b.poe_per_fight_net)));
    lines.push(stat("Goods / fight", opt1(b.goods_per_fight)));
    lines.push(stat("PoE total (net)", commas(b.poe_net_total)));
    lines.push(stat("PoE / crew", opt_commas(b.poe_per_crew)));
    lines.push(stat(
        "PoE / crew / fight (W)",
        opt_commas(b.poe_per_crew_per_fight_won),
    ));
    lines.push(stat(
        "PoE / crew / fight (W+L)",
        opt_commas(b.poe_per_crew_per_fight_all),
    ));
    lines.push(Line::from(""));

    // Enemies by category — only categories that occurred (no zero rows).
    if !b.categories.is_empty() {
        lines.push(section("Enemies"));
        for (label, count) in &b.categories {
            lines.push(stat(label, count.to_string()));
        }
        lines.push(Line::from(""));
    }

    // Advantage (only when damage was tracked for at least one fight).
    if b.avg_advantage_dmg.is_some() || b.avg_advantage_crew.is_some() {
        lines.push(section("Advantage (avg)"));
        lines.push(stat(
            "Damage advantage",
            b.avg_advantage_dmg
                .map(|a| format!("{:+.0}%", a * 100.0))
                .unwrap_or_else(|| "—".to_string()),
        ));
        lines.push(stat(
            "Crew advantage",
            b.avg_advantage_crew
                .map(|a| format!("{a:+.1}"))
                .unwrap_or_else(|| "—".to_string()),
        ));
        lines.push(Line::from(""));
    }

    // Consumption.
    lines.push(section("Consumption"));
    let balls_label = format!(
        "Cannonballs ({})",
        view.cannon_label.clone().unwrap_or_else(|| "—".to_string())
    );
    lines.push(stat(&balls_label, opt_u64(c.balls)));
    lines.push(stat("  · per battle", opt1(c.balls_per_battle)));
    lines.push(stat("Alcohol", commas(c.alcohol as i64)));
    lines.push(stat("  · per crew", opt1(c.alcohol_per_crew)));
    lines.push(stat("  · per crew / min", opt2(c.alcohol_per_crew_per_min)));
    lines.push(stat("Rum spice", commas(c.rum_spice as i64)));
    lines.push(stat("  · per swabbie", opt1(c.rum_spice_per_swabbie)));
    lines.push(stat(
        "  · per swabbie / min",
        opt2(c.rum_spice_per_swabbie_per_min),
    ));
    lines.push(Line::from(Span::styled(
        "  ⚠ spice approx. (swabbies / ran out skew it)".to_string(),
        Style::default().fg(Color::DarkGray).italic(),
    )));

    lines
}

/// A yellow section header line.
fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} "),
        Style::default().fg(Color::Yellow).bold(),
    ))
}

/// A `  label .......... value` stat line (label left, value right-aligned).
fn stat(label: &str, value: String) -> Line<'static> {
    Line::from(format!("  {label:<26}{value:>12}"))
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
