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
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap};

use crate::clickmap::{ClickRegion, ClickTarget};
use crate::damage::DamageApp;
use crate::ships::SHIPS;
use crate::utils::offset_title;
use crate::voyage::stats::{box_plot, BattleStats, BoxPlot, CategoryTally, ConsumptionStats};
use crate::voyage::{AxisMode, BattleOutcome, BattleSnapshot};

/// The three charts, in display order.
pub const CHART_TITLES: [&str; 3] = ["PoE won", "PoE per fight", "Total value"];

/// Which charts can enlarge into a popup (parallel to [`CHART_TITLES`]). The two
/// box-plot charts ("PoE won", "Total value") show everything in their mini box,
/// so they have no popup — they stay selectable for their tooltip only.
pub const CHART_ENLARGEABLE: [bool; 3] = [false, true, false];

/// One-liners shown below the widget when a chart is focused (parallel to
/// [`CHART_TITLES`]).
const CHART_TOOLTIPS: [&str; 3] = [
    "Pieces of eight per won fight — this voyage's spread vs history.",
    "PoE of each concluded fight, newest first (losses negative), with box-plots for this voyage and the rest of the same-hull voyages. Enter to enlarge.",
    "This voyage's total value (a point) against a historical box of past voyages.",
];

/// Height in rows of each chart's bordered box in the scrolling body.
const CHART_H: u16 = 7;

/// Which button the save/discard prompt has focused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
}

/// Which control the Sea Battles popup has focused. The chain runs top→bottom,
/// matching the on-screen order: the fight pager, the record toggle (rendered
/// directly under the pager), then the (always-editable) calculator below it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum BattlesFocus {
    /// "Battle n of m" — ←/→ change fight (wrapping).
    #[default]
    Pager,
    /// The "Recorded / Not Recorded" toggle.
    Record,
    /// The embedded Damage calculator grid.
    Calc,
}

/// Which voyage the Voyage Statistics page is showing. The page can page across
/// the current login's live runs (read-write) and past runs from the voyages file
/// (read-only); `Live` keeps following the newest run as new ones begin.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VoyageSel {
    /// Follow the newest live run of the current login (default). Auto-advances as
    /// new runs begin — the page never gets "stuck" on an old run unless pinned.
    #[default]
    Live,
    /// A pinned current-login run, by its stable [`crate::voyage::Voyage::id`].
    Session(u64),
    /// A pinned past run, by index into the loaded voyage history — read-only.
    Saved(usize),
}

/// Persistent UI state for the page (mouse/keyboard-driven).
#[derive(Default)]
pub struct VoyageStatsUi {
    /// Which voyage is shown (see [`VoyageSel`]). `Live` by default.
    pub selected: VoyageSel,
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
    /// When `Some(i)`, the Sea Battles popup is open on page `i` (battle index).
    pub battles_popup: Option<usize>,
    /// Interactive Damage calculator bound to the open page's fight. Edits flow
    /// back to that battle (when its recording is on). Reloaded on open/page-turn.
    pub battle_editor: DamageApp,
    /// Our full crew aboard for the open fight — real pirates + swabbies/named
    /// mercenaries (the "Our strength" figure; also feeds the crew advantage).
    pub editor_crew: u32,
    /// The open fight's foe headcount from the melee, if known (`None` falls back
    /// to the foe ship type's pirate capacity). Drives "Their strength".
    pub editor_their: Option<u32>,
    /// Mirrors the open fight's `recorded` flag (whether it persists to disk) —
    /// drives the toggle label only; the calculator is always editable.
    pub editor_recorded: bool,
    /// Which control inside the Sea Battles popup currently has focus.
    pub battles_focus: BattlesFocus,
}

/// Per-fight series for the charts: current voyage vs persisted history.
#[derive(Default, Clone)]
pub struct ChartData {
    /// PoE of each won fight this voyage (chronological).
    pub cur_won_poe: Vec<f64>,
    /// Signed PoE of each concluded (won or lost) fight this voyage, in order —
    /// losses are negative. Drives the per-fight bar chart.
    pub cur_fight_poe: Vec<f64>,
    /// PoE of every won fight across saved history.
    pub hist_won_poe: Vec<f64>,
    /// Box-and-whiskers rows drawn beneath the per-fight bars, on the bars' shared
    /// scale. The first is styled as "current", the rest as "historical".
    /// Typically `[Voyage, History]` — this voyage vs the rest of the same-hull
    /// voyages — but the renderer takes any number.
    pub fight_boxes: Vec<ChartBox>,
    /// PoE of the most recent won fight this voyage (highlighted marker).
    pub last_win: Option<f64>,
    /// This voyage's total value (net PoE for now; goods fold in later).
    pub cur_total: f64,
    /// Total value of each past voyage (one point each).
    pub hist_totals: Vec<f64>,
}

/// One box-and-whiskers row beneath the per-fight bars. `values` is the signed
/// population; when it's empty the row shows `empty_note` (dimmed) in place of a
/// box, or is skipped entirely if that's `None`.
#[derive(Default, Clone)]
pub struct ChartBox {
    pub label: String,
    pub values: Vec<f64>,
    pub empty_note: Option<String>,
}

/// One fight's metadata for the Sea Battles popup. The editable Damage-calculator
/// state lives in [`VoyageStatsUi::battle_editor`]; `snapshot` here is the stored
/// value used to (re)load that editor and to tell whether the fight is recorded.
#[derive(Clone, Default)]
pub struct BattleRow {
    /// Enemy vessel name, or `None` if the interception line was unparsed.
    pub enemy: Option<String>,
    pub outcome: BattleOutcome,
    /// Display label for what we fought ("Brigands and Barbarians", "King: …").
    pub category: String,
    pub poe: Option<i64>,
    pub goods: Option<u32>,
    pub my_cut: Option<u64>,
    pub total_secs: Option<i64>,
    pub sea_secs: Option<i64>,
    pub boarding_secs: Option<i64>,
    pub pirates: u32,
    pub swabbies: u32,
    /// The Damage-calculator snapshot for this fight (always editable; may be
    /// `None` until anything is captured/entered).
    pub snapshot: Option<BattleSnapshot>,
    /// Whether the fight is recorded (persisted to disk) — display-independent.
    pub recorded: bool,
    /// Foe headcount computed from the melee (`None` → use the ship-type estimate).
    pub their_manpower: Option<u32>,
    /// The foe's known hull type ([`crate::ships::SHIPS`] index) when the encounter
    /// announced it (Black Ship, Monkey Boats). Seeds the editor's foe ship when no
    /// snapshot has overridden it. `None` when the hull is unknown.
    pub foe_ship: Option<usize>,
    /// Side-tagged elimination timeline driving the advantage-over-time graph in
    /// the Sea Battles popup. Empty when the fight recorded no melee KOs (or a
    /// loaded historical fight that didn't persist one).
    pub timeline: crate::voyage::FightTimeline,
}

/// The pager badge for the shown voyage — its status among all selectable runs.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VoyageBadge {
    /// The in-progress run of the current login.
    Live,
    /// A completed current-login run not yet saved to history.
    #[default]
    Unsaved,
    /// A current-login run already persisted to history this session.
    Saved,
    /// A past run loaded from the voyages file — read-only.
    ReadOnly,
}

/// Everything the page needs to draw one voyage, computed by the caller so this
/// module stays free of app-state plumbing.
pub struct VoyageView {
    pub has_voyage: bool,
    /// This is a read-only history page (loaded from disk): no save/discard, and
    /// the Sea Battles calculator can't be edited.
    pub read_only: bool,
    /// 0-based index of the shown voyage among all selectable runs, and the total
    /// count — drives the `Voyage k of n` pager. `page_count <= 1` hides it.
    pub page: usize,
    pub page_count: usize,
    /// The shown voyage's status badge in the pager.
    pub badge: VoyageBadge,
    /// Vessel name — the centered headline.
    pub vessel: Option<String>,
    /// Ship type (e.g. "War Frigate"), from the vessel's chosen ship.
    pub ship_type: Option<String>,
    /// The run's clock span, e.g. `"12:34 to 13:50"` (end = current time while
    /// still at sea, or the port time once ported). `None` until we've sailed.
    pub period: Option<String>,
    /// Elapsed run time — final duration if ported, else live elapsed.
    pub elapsed_secs: Option<i64>,
    /// The displayed run is finished and not yet saved/dismissed — offer to save.
    pub saveable: bool,
    pub battle: BattleStats,
    pub consumption: ConsumptionStats,
    pub charts: ChartData,
    /// Per-fight rows for the Sea Battles popup, in chronological order (resolved
    /// fights first, then the in-progress one if any).
    pub battles: Vec<BattleRow>,
}

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

    // Build the body rows up front (width-agnostic) so the widget can size itself
    // to its content instead of a fixed guess.
    let mut built = build_lines(view);

    // Natural content width: the widest thing we must show without clipping —
    // the longest `label  value` stat / tally row, the pinned header, and the
    // footer hint. The save hint is the widest chrome; reserve room for it always
    // so the width doesn't jump when the save prompt becomes available.
    const FOOTER_W: usize = 40; // "S  save voyage to history  ·  D  discard"
    const NO_VOYAGE_TITLE: &str = "No voyage tracked yet.";
    const NO_VOYAGE_HINT: &str = "Set sail on a vessel to begin recording stats.";
    let content_w = if view.has_voyage {
        let header_w = [
            view.vessel.as_deref(),
            view.ship_type.as_deref(),
            view.period.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0);
        built.natural_width().max(header_w).max(FOOTER_W)
    } else {
        NO_VOYAGE_HINT.chars().count()
    };

    // Size to content (+2 for the borders) and center, never exceeding the area.
    let width = ((content_w + 2) as u16).min(widget_area.width);
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
            Line::from(Span::styled(NO_VOYAGE_TITLE, Style::default().bold())),
            Line::from(Span::styled(
                NO_VOYAGE_HINT,
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
    match &view.ship_type {
        Some(t) => header.push(centered_line(t.clone(), iw, Style::default().fg(Color::Gray))),
        // No hull assigned — say so, dimmed and italic so it reads as a placeholder.
        None => header.push(centered_line(
            "Unknown Ship Hull".to_string(),
            iw,
            Style::default().fg(Color::DarkGray).italic(),
        )),
    }
    if let Some(p) = &view.period {
        header.push(centered_line(p.clone(), iw, Style::default().fg(Color::DarkGray)));
    }
    header.push(Line::from("")); // separator from the scrolling body
    // With the pager shown, the status badge sits under the `Voyage k of n` row on
    // its own line, then a blank, then the ship name / type / clock headline. A
    // saved current-login run shows no badge (and no reserved line).
    if view.page_count > 1 {
        if let Some((label, style)) = voyage_badge_span(view.badge) {
            header.insert(0, Line::from("")); // blank between badge and ship name
            header.insert(0, centered_line(label.to_string(), iw, style));
        }
    }
    let header_h = header.len() as u16;

    // Split: an optional voyage pager, the pinned header, the scrollable body
    // (Sea Battles + stats + charts), then a pinned footer.
    let show_pager = view.page_count > 1;
    let pager_h = if show_pager { 1 } else { 0 };
    let parts = Layout::vertical([
        Constraint::Length(pager_h),
        Constraint::Length(header_h),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    let (pager_area, header_area, body, footer) = (parts[0], parts[1], parts[2], parts[3]);
    if show_pager {
        render_voyage_pager(frame, pager_area, view, regions);
    }
    frame.render_widget(Paragraph::new(header), header_area);

    // Render the body rows into lines now that the width is fixed. The parallel
    // focusable list (Sea Battles, then the stat numbers) is already populated;
    // the three charts follow them in the focus order.
    built.finalize(body.width as usize);
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
    } else if view.read_only {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "read-only history · ←/→ change voyage",
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            footer,
        );
    } else {
        let hint = if view.page_count > 1 {
            "↑/↓ move · ←/→ voyage · Enter open/enlarge"
        } else {
            "↑/↓ move · Enter open/enlarge"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            footer,
        );
    }

    // Modal popups. The Sea Battles and chart popups use the full content width,
    // not the narrow body column. Only one modal is ever open at a time.
    if ui.battles_popup.is_some() {
        render_battles_popup(frame, full, view, ui, regions);
    } else if let Some(i) = ui.chart_popup {
        render_chart_popup(frame, full, i, &view.charts, regions);
    } else if let Some(choice) = ui.prompt {
        render_save_prompt(frame, area, choice, regions);
    }
}

/// The voyage pager row: `‹ Prev   Voyage k of n · <badge>   Next ›`. Shown only
/// when more than one run is selectable (`page_count > 1`). Prev/Next are click
/// targets (dimmed and inert at the ends); the keyboard uses ←/→ on the page.
fn render_voyage_pager(
    frame: &mut Frame,
    area: Rect,
    view: &VoyageView,
    regions: &mut Vec<ClickRegion>,
) {
    let nav = Layout::horizontal([
        Constraint::Length(8),
        Constraint::Min(0),
        Constraint::Length(8),
    ])
    .split(area);
    let at_first = view.page == 0;
    let at_last = view.page + 1 >= view.page_count;
    let arrow = |label: &str, dim: bool| {
        let style = if dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Cyan)
        };
        Paragraph::new(Span::styled(label.to_string(), style)).centered()
    };
    frame.render_widget(arrow("‹ Prev", at_first), nav[0]);
    frame.render_widget(arrow("Next ›", at_last), nav[2]);
    if !at_first {
        regions.push(ClickRegion { rect: nav[0], target: ClickTarget::VoyagePrev });
    }
    if !at_last {
        regions.push(ClickRegion { rect: nav[2], target: ClickTarget::VoyageNext });
    }
    // Just the `Voyage k of n` count here — the status badge is rendered below, on
    // its own line in the header stack.
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("Voyage {} of {}", view.page + 1, view.page_count),
            Style::default().bold(),
        ))
        .centered(),
        nav[1],
    );
}

/// The label + colour for a voyage's pager status badge, or `None` when no badge
/// should show — an already-saved current-login run needs no marker.
fn voyage_badge_span(badge: VoyageBadge) -> Option<(&'static str, Style)> {
    match badge {
        VoyageBadge::Live => Some(("Live", Style::default().fg(Color::DarkGray))),
        VoyageBadge::Unsaved => Some(("Unsaved", Style::default().fg(Color::Red))),
        VoyageBadge::Saved => None,
        VoyageBadge::ReadOnly => Some(("Read-only", Style::default().fg(Color::DarkGray))),
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

/// Colour for a battle outcome label.
fn outcome_style(o: BattleOutcome) -> Style {
    let c = match o {
        BattleOutcome::Won => Color::Green,
        BattleOutcome::Lost => Color::Red,
        BattleOutcome::Disengaged => Color::Yellow,
        BattleOutcome::Ongoing => Color::Gray,
        BattleOutcome::Unknown => Color::DarkGray,
    };
    Style::default().fg(c).bold()
}

fn outcome_label(o: BattleOutcome) -> &'static str {
    match o {
        BattleOutcome::Won => "Won",
        BattleOutcome::Lost => "Lost",
        BattleOutcome::Disengaged => "Disengaged",
        BattleOutcome::Ongoing => "In progress",
        BattleOutcome::Unknown => "Unknown",
    }
}

/// Modal: the Sea Battles per-fight pager. Each page shows one fight's metadata,
/// an always-editable Damage calculator bound to its snapshot, and the strengths
/// + advantage derived from it. The Recorded/Not Recorded toggle only controls
/// whether the fight persists to disk — it never gates the display.
fn render_battles_popup(
    frame: &mut Frame,
    area: Rect,
    view: &VoyageView,
    ui: &VoyageStatsUi,
    regions: &mut Vec<ClickRegion>,
) {
    // Backdrop swallows outside clicks (acts as close).
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageBattlesClose,
    });

    // Height = 6 fixed single rows (pager, toggle, blank, ship, category, outcome)
    // + an optional advantage chart + the calculator box + the 7-row stats table +
    // 2 borders.
    let (_, calc_h) = crate::damage::ui::calc_box_size();
    let n = view.battles.len();
    let page = ui.battles_popup.unwrap_or(0).min(n.saturating_sub(1));
    // A compact advantage-over-time chart is shown only when the fight logged melee
    // KOs (live fights always; loaded history only if its timeline was persisted).
    const SB_CHART_PLOT_H: usize = 5;
    let want_chart = n > 0 && !view.battles[page].timeline.events.is_empty();
    let chart_h: u16 = if want_chart { SB_CHART_PLOT_H as u16 + 2 } else { 0 };
    let w = 60.min(area.width);
    let h = (calc_h + 15 + chart_h).min(area.height);
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
        .title(offset_title("Sea Battles").0);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if n == 0 {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No sea battles this voyage.",
                    Style::default().bold(),
                )),
            ])
            .centered(),
            inner,
        );
        return;
    }

    let row = &view.battles[page];
    let recorded = ui.editor_recorded;
    let focus = ui.battles_focus;
    let editor_crew = ui.editor_crew;
    let (box_w, box_h) = crate::damage::ui::calc_box_size();

    // Allocate rows explicitly so a short popup never shrinks the calculator box:
    // the 6 single header rows, the optional chart, and the calculator get their
    // height first; the stats table takes only the leftover (it clips before the
    // calc does).
    let body = inner.height.saturating_sub(6);
    let chart_rows = chart_h.min(body);
    let rest = body.saturating_sub(chart_rows);
    let calc_rows = box_h.min(rest);
    let table_h = rest.saturating_sub(calc_rows);
    let parts = Layout::vertical([
        Constraint::Length(1),          // Battle n of m (pager)
        Constraint::Length(1),          // (Not) Recorded toggle
        Constraint::Length(1),          // space
        Constraint::Length(1),          // enemy ship (type)
        Constraint::Length(1),          // type of enemy (category)
        Constraint::Length(1),          // won / lost
        Constraint::Length(chart_rows), // optional advantage-over-time chart
        Constraint::Length(calc_rows),  // embedded damage calculator
        Constraint::Length(table_h),    // per-fight stats table
    ])
    .split(inner);

    // -- Pager: ‹ Prev | Battle k of n | Next › (the center is focusable) --
    let nav = Layout::horizontal([
        Constraint::Length(8),
        Constraint::Min(0),
        Constraint::Length(8),
    ])
    .split(parts[0]);
    let arrow = |label: &str| Paragraph::new(Span::styled(
        label.to_string(),
        Style::default().fg(Color::Cyan),
    ))
    .centered();
    frame.render_widget(arrow("‹ Prev"), nav[0]);
    let pager_style = if focus == BattlesFocus::Pager {
        Style::default().fg(Color::Black).bg(Color::Cyan).bold()
    } else {
        Style::default().bold()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(format!("Battle {} of {}", page + 1, n), pager_style))
            .centered(),
        nav[1],
    );
    frame.render_widget(arrow("Next ›"), nav[2]);
    regions.push(ClickRegion { rect: nav[0], target: ClickTarget::VoyageBattlesPrev });
    regions.push(ClickRegion { rect: nav[2], target: ClickTarget::VoyageBattlesNext });

    // -- Record toggle (focusable), directly under the page number. On a read-only
    //    history page there's nothing to persist, so it's a static marker with no
    //    click target. --
    if view.read_only {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "read-only",
                Style::default().fg(Color::DarkGray),
            ))
            .centered(),
            parts[1],
        );
    } else {
        let rec_label = if recorded { "Recorded" } else { "Not Recorded" };
        let rec_style = if focus == BattlesFocus::Record {
            Style::default().fg(Color::Black).bg(Color::Cyan).bold()
        } else if recorded {
            Style::default().fg(Color::Green).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(rec_label, rec_style)).centered(),
            parts[1],
        );
        regions.push(ClickRegion {
            rect: parts[1],
            target: ClickTarget::VoyageBattlesRecord,
        });
    }

    // -- (blank parts[2]) then enemy ship "(type)", type of enemy, outcome. The
    //    foe ship type always mirrors the calculator's Right column. --
    let enemy = row.enemy.clone().unwrap_or_else(|| "Unknown vessel".to_string());
    let ship_line = format!("{enemy} ({})", SHIPS[ui.battle_editor.right_ship].name);
    frame.render_widget(Paragraph::new(ship_line).centered(), parts[3]);
    // PvP is its own category, labelled "Players" and flagged in magenta.
    let is_pvp = row.category == "Players";
    let cat_style = if is_pvp {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Gray)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(row.category.clone(), cat_style)).centered(),
        parts[4],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(outcome_label(row.outcome), outcome_style(row.outcome)))
            .centered(),
        parts[5],
    );

    // -- Optional advantage-over-time chart (parts[6]); only when the fight logged
    //    melee KOs. Wall-clock X-axis (the Sea Battles popup keeps no axis toggle). --
    if chart_rows > 0 {
        let series = row.timeline.advantage_series(AxisMode::Time);
        let chart = fight_chart_lines(
            &series,
            parts[6].width as usize,
            SB_CHART_PLOT_H,
            AxisMode::Time,
        );
        frame.render_widget(Paragraph::new(chart), parts[6]);
    }

    // -- Embedded Damage calculator (the shared widget; highlighted only while the
    //    calculator zone holds focus). Always editable. --
    let calc_box = Rect {
        x: parts[7].x + parts[7].width.saturating_sub(box_w) / 2,
        y: parts[7].y,
        width: box_w.min(parts[7].width),
        height: box_h.min(parts[7].height),
    };
    crate::damage::ui::render_calculator(
        frame,
        calc_box,
        &ui.battle_editor,
        focus == BattlesFocus::Calc,
        regions,
    );

    // -- Per-fight stats table (label-left / value-right). PoE/Goods/Melee come
    //    from the battle log; "Our strength" is our crew aboard; "Their strength"
    //    is the foe headcount from the melee (falling back to the foe ship type's
    //    pirate capacity); advantages come from the calculator. --
    let their = row
        .their_manpower
        .unwrap_or_else(|| SHIPS[ui.battle_editor.right_ship].max_pirates as u32);
    let table_rows: [(&str, String); 7] = [
        ("PoE won", row.poe.map(commas).unwrap_or_else(dash)),
        ("Goods", row.goods.map(|g| commas(g as i64)).unwrap_or_else(dash)),
        ("Melee", row.boarding_secs.map(dur).unwrap_or_else(dash)),
        ("Our strength", editor_crew.to_string()),
        ("Their strength", their.to_string()),
        (
            "Advantage",
            format!("{:+.0}%", ui.battle_editor.advantage_dmg() * 100.0),
        ),
        (
            "Manpower Advantage",
            format!("{:+.1}", ui.battle_editor.crew_advantage(editor_crew, their)),
        ),
    ];
    let label_w = table_rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let val_w = (parts[8].width as usize).saturating_sub(label_w + 2);
    let stat_lines: Vec<Line> = table_rows
        .iter()
        .map(|(label, value)| Line::from(format!("{label:<label_w$}  {value:>val_w$}")))
        .collect();
    frame.render_widget(
        Paragraph::new(stat_lines).style(Style::default().fg(Color::Gray)),
        parts[8],
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
        Paragraph::new(chart_lines(i, data, inner.width as usize, inner.height as usize, false))
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
        .title_bottom(Line::from(" Esc to close ").right_aligned())
        .padding(Padding::uniform(1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(chart_lines(
            idx,
            data,
            inner.width as usize,
            inner.height as usize,
            true,
        )),
        inner,
    );
}

/// Build the lines for chart `idx`, fitting `width`×`height` (used at both
/// mini and enlarged sizes — it scales by the area it's given). `enlarged` is
/// set for the popup, which shows more rows and the full legend.
fn chart_lines(
    idx: usize,
    data: &ChartData,
    width: usize,
    height: usize,
    enlarged: bool,
) -> Vec<Line<'static>> {
    match idx {
        0 => poe_box_lines(data, width),
        1 => poe_bar_lines(data, width, height, enlarged),
        _ => total_value_lines(data, width),
    }
}

/// Width of the row-label column on the PoE-won chart (fits "Historical" + gap).
const PBOX_LABEL_W: usize = 12;

/// Chart 0 — box & whiskers of won-fight PoE: this voyage vs historical, with a
/// marker at the most recent win and a legend below. It has no popup (it shows
/// everything in its mini box), so all of it must fit the box's rows.
fn poe_box_lines(data: &ChartData, width: usize) -> Vec<Line<'static>> {
    let axis = width.saturating_sub(PBOX_LABEL_W);
    let range = combined_range(&[data.cur_won_poe.as_slice(), data.hist_won_poe.as_slice()]);
    let mut lines = vec![
        box_or_msg(
            "Current",
            PBOX_LABEL_W,
            box_plot(&data.cur_won_poe),
            range,
            axis,
            data.last_win.map(|v| (v, '✦')),
            cur_style(),
        ),
        box_or_msg(
            "Historical",
            PBOX_LABEL_W,
            box_plot(&data.hist_won_poe),
            range,
            axis,
            None,
            hist_style(),
        ),
    ];
    if let Some((lo, hi)) = range {
        lines.push(axis_line(lo, hi, width, PBOX_LABEL_W));
    }
    lines.push(Line::from(Span::styled(
        "Legend:",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "  ✦ - Last Win",
        Style::default().fg(Color::Cyan),
    )));
    lines
}

/// Chart 1 — signed PoE bars for this voyage's concluded fights (newest at top),
/// with one box-and-whiskers row per [`ChartData::fight_boxes`] population below
/// them, all on one shared scale. See [`signed_bars_with_boxes`].
fn poe_bar_lines(data: &ChartData, width: usize, height: usize, enlarged: bool) -> Vec<Line<'static>> {
    signed_bars_with_boxes(&data.cur_fight_poe, &data.fight_boxes, width, height, enlarged)
}

/// Signed horizontal bars — one per value in `bars` (chronological; index `i` is
/// fight `#(i+1)`), newest at the top, with the `#N` label on the left and the
/// value on the right — followed by any number of box-and-whiskers rows from
/// `boxes` (`(label, population)`), drawn beneath on the **same** zero-bracketing
/// scale and the **same** column band so bars and boxes line up glyph-for-glyph.
///
/// General-purpose: a lost fight (negative value) extends left in red; the first
/// box is drawn in the "current" (cyan) style and the rest in the "historical"
/// (gray) style. The mini widget shows at most the latest 5 bars; the enlarged
/// popup shows as many as fit. Empty box populations are skipped.
fn signed_bars_with_boxes(
    bars: &[f64],
    boxes: &[ChartBox],
    width: usize,
    height: usize,
    enlarged: bool,
) -> Vec<Line<'static>> {
    if bars.is_empty() {
        // Centered horizontally and vertically in the plot area.
        let mut lines = vec![Line::from(""); height.saturating_sub(1) / 2];
        lines.push(centered_line(
            "Engage in a Sea Battle first".to_string(),
            width,
            Style::default().fg(Color::DarkGray).italic(),
        ));
        return lines;
    }
    // A box row renders if it has data, or a note to show in place of data.
    let renders = |b: &ChartBox| box_plot(&b.values).is_some() || b.empty_note.is_some();
    // Reserve one row per rendering box, then fit bars in the rest.
    let box_rows = boxes.iter().filter(|b| renders(b)).count();
    let fits = height.saturating_sub(box_rows).max(1);
    let cap = if enlarged { fits } else { fits.min(5) };
    let start = bars.len().saturating_sub(cap);
    // Newest first, keeping each value's 1-based index.
    let shown: Vec<(usize, f64)> = bars
        .iter()
        .enumerate()
        .skip(start)
        .map(|(i, &v)| (i + 1, v))
        .rev()
        .collect();

    // The left gutter fits the widest of the "#N" bar labels and the box labels,
    // so the bar band and every box band begin at the same column.
    let idx_w = shown
        .iter()
        .map(|(i, _)| format!("#{i}").chars().count())
        .max()
        .unwrap_or(2);
    let label_w = boxes
        .iter()
        .map(|b| b.label.chars().count())
        .chain(std::iter::once(idx_w))
        .max()
        .unwrap_or(idx_w);
    let vals_s: Vec<String> = shown.iter().map(|(_, v)| commas(v.round() as i64)).collect();
    let val_w = vals_s.iter().map(|s| s.chars().count()).max().unwrap_or(1);
    let bar_cols = width.saturating_sub(label_w + 2 + 1 + val_w).max(1);

    // One shared scale over the shown bars *and* every box population, always
    // bracketing zero so `signed_bar`'s zero boundary lines up across all rows.
    let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
    for v in shown
        .iter()
        .map(|(_, v)| *v)
        .chain(boxes.iter().flat_map(|b| b.values.iter().copied()))
    {
        lo = lo.min(v);
        hi = hi.max(v);
    }

    let mut lines = Vec::new();
    for ((i, v), vs) in shown.iter().zip(&vals_s) {
        let label = format!("{:>label_w$}", format!("#{i}"));
        let bar = signed_bar(*v, lo, hi, bar_cols);
        let style = if *v < 0.0 {
            Style::default().fg(Color::Red)
        } else {
            cur_style()
        };
        lines.push(Line::from(Span::styled(
            format!("{label}  {bar} {vs:>val_w$}"),
            style,
        )));
    }
    // Box rows — same band (`label_w + 2` prefix, then `bar_cols` cells) and scale
    // as the bars above. First box "current", rest "historical". A box with no
    // data shows its `empty_note` (dimmed) instead, or is skipped.
    for (n, b) in boxes.iter().enumerate() {
        if let Some(bp) = box_plot(&b.values) {
            let style = if n == 0 { cur_style() } else { hist_style() };
            lines.push(Line::from(Span::styled(
                format!(
                    "{:<label_w$}  {}",
                    b.label,
                    signed_box_line(bar_cols, lo, hi, &bp)
                ),
                style,
            )));
        } else if let Some(note) = &b.empty_note {
            lines.push(centered_line(
                note.clone(),
                width,
                Style::default().fg(Color::DarkGray).italic(),
            ));
        }
    }
    lines
}

/// A horizontal bar of `cols` cells with zero on the *boundary* between the
/// negative and positive halves — not on a shared cell. The field is split into
/// `neg_w` cells left of zero and `w - neg_w` right of it, proportional to how
/// far `lo`/`hi` reach so both sides share one unit-per-cell. A positive `v`
/// fills rightward from the boundary; a negative `v` fills leftward up to it.
/// Because the two halves are disjoint, a win bar and a loss bar can never
/// collide on the zero column (the bug a single shared zero cell caused).
fn signed_bar(v: f64, lo: f64, hi: f64, cols: usize) -> String {
    let w = cols.max(1);
    let mut cells = vec![' '; w];
    let span = hi - lo; // lo ≤ 0 ≤ hi by construction, so span ≥ 0
    if span <= 0.0 {
        return cells.into_iter().collect();
    }
    // Too narrow to place a boundary — show a single cell for any nonzero fight.
    if w < 2 {
        if v != 0.0 {
            cells[0] = '█';
        }
        return cells.into_iter().collect();
    }
    // Cells left of the zero boundary (index `neg_w` is the first positive cell).
    let neg_w = neg_width(lo, hi, w);
    if v > 0.0 {
        let pos_w = w - neg_w;
        let len = (((v / hi) * pos_w as f64).round() as usize)
            .max(1)
            .min(pos_w);
        for cell in cells.iter_mut().skip(neg_w).take(len) {
            *cell = '█';
        }
    } else if v < 0.0 {
        // v/lo is positive (both negative); fills the cells just left of `neg_w`.
        let len = (((v / lo) * neg_w as f64).round() as usize)
            .max(1)
            .min(neg_w);
        for cell in cells.iter_mut().take(neg_w).skip(neg_w - len) {
            *cell = '█';
        }
    }
    cells.into_iter().collect()
}

/// Cells left of the zero boundary in a `signed_bar`/`signed_box_line` of `w`
/// cells over `[lo, hi]` (index `neg_w` is the first positive cell). When both
/// signs are present, force at least one cell on each side so a small minority
/// value isn't rounded into invisibility. Shared so bars and the reference box
/// place their zero boundary on the exact same column.
fn neg_width(lo: f64, hi: f64, w: usize) -> usize {
    let w = w.max(1);
    let span = hi - lo;
    if span <= 0.0 || w < 2 {
        return 0;
    }
    if lo < 0.0 && hi > 0.0 {
        (((-lo / span) * w as f64).round() as usize).clamp(1, w - 1)
    } else if lo < 0.0 {
        w
    } else {
        0
    }
}

/// Column at which a value `v` lands in a `signed_bar` band — i.e. the cell a bar
/// of that value would reach — so a reference box drawn with these columns lines
/// up with the bars glyph-for-glyph. Positive `v` measures rightward from the
/// zero boundary over `hi`, negative leftward over `lo`, matching `signed_bar`.
fn signed_col(v: f64, lo: f64, hi: f64, w: usize) -> usize {
    let w = w.max(1);
    let neg_w = neg_width(lo, hi, w);
    if v > 0.0 && hi > 0.0 {
        let pos_w = (w - neg_w).max(1);
        let len = (((v / hi) * pos_w as f64).round() as usize).max(1).min(pos_w);
        (neg_w + len).saturating_sub(1).min(w - 1)
    } else if v < 0.0 && lo < 0.0 && neg_w > 0 {
        let len = (((v / lo) * neg_w as f64).round() as usize).max(1).min(neg_w);
        neg_w.saturating_sub(len).min(w - 1)
    } else {
        neg_w.min(w - 1)
    }
}

/// A box-and-whiskers of `w` cells drawn on the *same* scale and column band as
/// [`signed_bar`] (via [`signed_col`]), so it aligns under the PoE-per-fight bars.
fn signed_box_line(w: usize, lo: f64, hi: f64, bp: &BoxPlot) -> String {
    let w = w.max(1);
    let mut cells = vec![' '; w];
    if bp.n == 1 {
        cells[signed_col(bp.median, lo, hi, w)] = '●';
    } else {
        let cmin = signed_col(bp.min, lo, hi, w);
        let cmax = signed_col(bp.max, lo, hi, w);
        let (cmin, cmax) = (cmin.min(cmax), cmin.max(cmax));
        let cq1 = signed_col(bp.q1, lo, hi, w);
        let cq3 = signed_col(bp.q3, lo, hi, w);
        let (cq1, cq3) = (cq1.min(cq3), cq1.max(cq3));
        for c in cmin..=cmax {
            cells[c] = '─';
        }
        for c in cq1..=cq3 {
            cells[c] = '█';
        }
        cells[cmin] = '├';
        cells[cmax] = '┤';
        cells[signed_col(bp.median, lo, hi, w)] = '┃';
    }
    cells.into_iter().collect()
}

/// Chart 2 — total value, drawn like chart 0: this voyage's total as a single
/// point (the Current row) against a historical box & whiskers of past voyages'
/// totals, sharing one axis, with a legend below. No popup.
fn total_value_lines(data: &ChartData, width: usize) -> Vec<Line<'static>> {
    let axis = width.saturating_sub(PBOX_LABEL_W);
    let mut all = data.hist_totals.clone();
    all.push(data.cur_total);
    let range = combined_range(&[all.as_slice()]);
    let mut lines = vec![
        box_or_msg(
            "Current",
            PBOX_LABEL_W,
            box_plot(&[data.cur_total]),
            range,
            axis,
            None,
            cur_style(),
        ),
        box_or_msg(
            "Historical",
            PBOX_LABEL_W,
            box_plot(&data.hist_totals),
            range,
            axis,
            None,
            hist_style(),
        ),
    ];
    if let Some((lo, hi)) = range {
        lines.push(axis_line(lo, hi, width, PBOX_LABEL_W));
    }
    lines.push(Line::from(Span::styled(
        "Legend:",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "  ● - This Voyage",
        Style::default().fg(Color::Cyan),
    )));
    lines
}

/// A box-and-whiskers row prefixed by `label` (padded to `label_w` columns), or
/// a dim "no data" message.
fn box_or_msg(
    label: &str,
    label_w: usize,
    bp: Option<BoxPlot>,
    range: Option<(f64, f64)>,
    axis: usize,
    marker: Option<(f64, char)>,
    style: Style,
) -> Line<'static> {
    match (bp, range) {
        (Some(bp), Some((lo, hi))) if axis > 0 => Line::from(Span::styled(
            format!("{label:<label_w$}{}", box_line(axis, lo, hi, &bp, marker)),
            style,
        )),
        _ => Line::from(Span::styled(
            format!("{label:<label_w$}(no data)"),
            Style::default().fg(Color::DarkGray),
        )),
    }
}

/// A "lo …… hi" axis label line, its numbers aligned under a `label_w`-indented
/// box row.
fn axis_line(lo: f64, hi: f64, width: usize, label_w: usize) -> Line<'static> {
    let lo_s = commas(lo.round() as i64);
    let hi_s = commas(hi.round() as i64);
    let gap = width.saturating_sub(label_w + lo_s.len() + hi_s.len()).max(1);
    Line::from(Span::styled(
        format!("{}{lo_s}{}{hi_s}", " ".repeat(label_w), " ".repeat(gap)),
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
/// `n == 1` renders a single point; `marker` overlays its char at its value.
fn box_line(width: usize, lo: f64, hi: f64, bp: &BoxPlot, marker: Option<(f64, char)>) -> String {
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
    if let Some((m, ch)) = marker {
        cells[val_col(m, lo, hi, w)] = ch;
    }
    cells.into_iter().collect()
}

/// Format a signed advantage value for the Y-axis gutter (`+6`, `0`, `-3`).
fn fmt_adv(v: i32) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

/// Format a duration in seconds as `M:SS` for the time axis.
fn fmt_mmss(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Render a per-fight **advantage-over-time** line graph: a single signed line
/// (`our_alive − their_alive`) over a zero baseline, `height` plot rows tall plus
/// two axis rows. `series` is `(x, advantage)` from
/// [`crate::voyage::FightTimeline::advantage_series`] (`x` is seconds under
/// [`AxisMode::Time`], else the event index). Drawn in the same text/braille style
/// as the other charts (reuses the per-cell sign colouring: cyan when we're ahead,
/// red when behind). Shared by the jobbers per-fight popup and the Sea Battles
/// popup.
pub fn fight_chart_lines(
    series: &[(f64, i32)],
    width: usize,
    height: usize,
    axis: AxisMode,
) -> Vec<Line<'static>> {
    const GUTTER: usize = 4; // 3-wide signed label + a space
    let rows = height.max(3);
    let plot_w = width.saturating_sub(GUTTER + 1).max(2); // +1 for the axis column
    if series.is_empty() {
        return vec![Line::from(Span::styled(
            "(no fight data)",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    // Y-range, always spanning zero (the baseline), with a 1-unit minimum span.
    let (mut ymin, mut ymax) = series
        .iter()
        .fold((0i32, 0i32), |(lo, hi), &(_, v)| (lo.min(v), hi.max(v)));
    if ymin == ymax {
        ymin -= 1;
        ymax += 1;
    }
    let span = (ymax - ymin) as f64;
    let row_of = |v: f64| -> usize {
        (((ymax as f64 - v) / span) * (rows - 1) as f64)
            .round()
            .clamp(0.0, (rows - 1) as f64) as usize
    };
    let zero_row = row_of(0.0);
    let xmax = series.last().map(|&(x, _)| x).unwrap_or(0.0).max(1.0);

    // Rasterize the step function into a (char, colour) grid.
    let mut cells = vec![vec![(' ', Color::Reset); plot_w]; rows];
    for cell in cells[zero_row].iter_mut() {
        *cell = ('┄', Color::DarkGray);
    }
    let mut idx = 0usize;
    let mut prev_row: Option<usize> = None;
    let denom = (plot_w - 1).max(1) as f64;
    for c in 0..plot_w {
        let x = (c as f64 / denom) * xmax;
        while idx + 1 < series.len() && series[idx + 1].0 <= x {
            idx += 1;
        }
        let v = series[idx].1;
        let r = row_of(v as f64);
        let color = match v.cmp(&0) {
            std::cmp::Ordering::Greater => Color::Cyan,
            std::cmp::Ordering::Less => Color::Red,
            std::cmp::Ordering::Equal => Color::Gray,
        };
        match prev_row {
            // A level change: draw the riser in this column with rounded corners.
            // The top of the screen is the *higher* advantage, so a rising value
            // (r < pr) turns up out of the old level and into the new; a falling
            // value (r > pr) turns down. The old level keeps its incoming `─` (from
            // column c-1) and gets the elbow here; the new level's `─` continues at
            // c+1.
            Some(pr) if pr != r => {
                for rr in (pr.min(r) + 1)..pr.max(r) {
                    cells[rr][c] = ('│', color);
                }
                let (old_corner, new_corner) = if r < pr {
                    ('╯', '╭') // rising: ─╯ leaves the old level upward, ╭─ joins the new
                } else {
                    ('╮', '╰') // falling: ─╮ leaves downward, ╰─ joins below
                };
                cells[pr][c] = (old_corner, color);
                cells[r][c] = (new_corner, color);
            }
            // Same level (or the first column): a flat horizontal run.
            _ => {
                cells[r][c] = ('─', color);
            }
        }
        prev_row = Some(r);
    }

    // Plot rows, each prefixed by the gutter label + axis tick.
    let mut lines = Vec::with_capacity(rows + 2);
    for (r, row_cells) in cells.iter().enumerate() {
        let label = if r == 0 {
            fmt_adv(ymax)
        } else if r == zero_row {
            "0".to_string()
        } else if r == rows - 1 {
            fmt_adv(ymin)
        } else {
            String::new()
        };
        let axis_char = if r == zero_row { '┼' } else { '┤' };
        let mut spans = vec![
            Span::styled(format!("{label:>3} "), Style::default().fg(Color::DarkGray)),
            Span::styled(axis_char.to_string(), Style::default().fg(Color::DarkGray)),
        ];
        let mut i = 0;
        while i < row_cells.len() {
            let col = row_cells[i].1;
            let mut s = String::new();
            while i < row_cells.len() && row_cells[i].1 == col {
                s.push(row_cells[i].0);
                i += 1;
            }
            spans.push(Span::styled(s, Style::default().fg(col)));
        }
        lines.push(Line::from(spans));
    }

    // X-axis: a baseline tick row, then start/end labels.
    lines.push(Line::from(Span::styled(
        format!("{}└{}", " ".repeat(GUTTER), "─".repeat(plot_w)),
        Style::default().fg(Color::DarkGray),
    )));
    let (start_lbl, end_lbl) = match axis {
        AxisMode::Time => ("0:00".to_string(), fmt_mmss(xmax)),
        AxisMode::Event => ("#0".to_string(), format!("#{}", series.len() - 1)),
    };
    let gap = plot_w
        .saturating_sub(start_lbl.len() + end_lbl.len())
        .max(1);
    lines.push(Line::from(Span::styled(
        format!(
            "{}{start_lbl}{}{end_lbl}",
            " ".repeat(GUTTER + 1),
            " ".repeat(gap)
        ),
        Style::default().fg(Color::DarkGray),
    )));
    lines
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

/// A focusable stat: which built line it lives on, and the tooltip to show
/// below the widget when it's focused.
struct Focusable {
    line: usize,
    tooltip: String,
}

/// One body row kept in raw form until the widget's width is known. The width is
/// derived from these rows ([`Built::natural_width`]); each then renders to a
/// `Line` at that width ([`Built::finalize`]).
enum Row {
    /// An empty spacer line.
    Blank,
    /// A centered yellow section header.
    Section(String),
    /// A `label … value` row: label flush-left, value flush-right.
    Stat { label: String, value: String },
    /// Three centered, individually-styled columns spanning the width.
    ThreeCol([(String, Style); 3]),
    /// A pre-rendered, width-independent line (its own width is fixed).
    Raw(Line<'static>),
}

/// The page body (everything below the pinned header): the rows in focus order —
/// the Sea Battles section first, then every Timing-onward stat number — plus the
/// parallel list of focusables. Rows are width-agnostic until [`Self::finalize`]
/// renders them into `lines` once the dynamic widget width is known.
#[derive(Default)]
struct Built {
    rows: Vec<Row>,
    /// Filled by [`Self::finalize`]; empty until then.
    lines: Vec<Line<'static>>,
    focusable: Vec<Focusable>,
}

impl Built {
    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }
    fn line(&mut self, l: Line<'static>) {
        self.push(Row::Raw(l));
    }
    fn blank(&mut self) {
        self.push(Row::Blank);
    }
    fn section(&mut self, title: &str) {
        self.push(Row::Section(title.to_string()));
    }
    fn three_col(&mut self, cells: [(String, Style); 3]) {
        self.push(Row::ThreeCol(cells));
    }
    /// Push a section header that is itself focusable (its header line carries
    /// the `tooltip`). Used for sections you can "enter", like Sea Battles.
    fn focus_section(&mut self, title: &str, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.rows.len(),
            tooltip: tooltip.to_string(),
        });
        self.push(Row::Section(title.to_string()));
    }
    /// Push a focusable `label .... value` stat row tied to `tooltip`.
    fn stat(&mut self, label: &str, value: String, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.rows.len(),
            tooltip: tooltip.to_string(),
        });
        self.push(Row::Stat {
            label: label.to_string(),
            value,
        });
    }

    /// The narrowest content width that shows every row without clipping: the
    /// widest `label + 2 spaces + value` stat and the three-column tally (each of
    /// whose cells must fit a third of the width), plus any pre-rendered line.
    /// Section headers just center, so they only need their own text width.
    fn natural_width(&self) -> usize {
        self.rows
            .iter()
            .map(|r| match r {
                Row::Blank => 0,
                Row::Section(t) => t.chars().count(),
                Row::Stat { label, value } => label.chars().count() + 2 + value.chars().count(),
                Row::ThreeCol(cells) => {
                    cells.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) * 3
                }
                Row::Raw(l) => l.width(),
            })
            .max()
            .unwrap_or(0)
    }

    /// Render every row into `self.lines` at the resolved `width`.
    fn finalize(&mut self, width: usize) {
        self.lines = self
            .rows
            .iter()
            .map(|r| match r {
                Row::Blank => Line::from(""),
                Row::Section(t) => section(t, width),
                Row::Stat { label, value } => stat(label, value.clone(), width),
                Row::ThreeCol(cells) => three_col(width, cells.clone()),
                Row::Raw(l) => l.clone(),
            })
            .collect();
    }
}

fn build_lines(view: &VoyageView) -> Built {
    let b = &view.battle;
    let c = &view.consumption;
    let mut out = Built::default();

    // Sea Battles — a focusable section over a full-width three-column table
    // (labels over counts). The header (ship name/type/period) is pinned above
    // the scroll, so the body starts here.
    out.focus_section(
        "Sea Battles",
        "Win / loss / disengage tally. Enter to open the per-fight log.",
    );
    let head = Style::default().fg(Color::DarkGray);
    out.three_col([
        ("Wins".to_string(), head),
        ("Losses".to_string(), head),
        ("Disengages".to_string(), head),
    ]);
    out.three_col([
        (b.wins.to_string(), Style::default().fg(Color::Green).bold()),
        (b.losses.to_string(), Style::default().fg(Color::Red).bold()),
        (
            b.disengages.to_string(),
            Style::default().fg(Color::Yellow).bold(),
        ),
    ]);
    out.blank();

    // Timing — every number from here on is focusable with a tooltip.
    out.section("Timing");
    out.stat(
        "At Sea",
        opt_dur(view.elapsed_secs.map(|s| s as f64)),
        "Total time from setting sail to putting into port.",
    );
    out.stat(
        "Time Sailing",
        b.time_at_sea_secs.map(dur).unwrap_or_else(dash),
        "Time searching and sailing, not fighting — total time minus time in battle.",
    );
    out.stat(
        "Time in Battle",
        dur(b.time_in_battle_secs),
        "Total fighting time this voyage — every sea engagement's length summed.",
    );
    out.stat(
        "Avg. Sea Battle",
        with_sd(opt_dur(b.avg_battle_secs), b.avg_battle_sd, sd_secs),
        "Average length of a sea engagement, interception to resolution.",
    );
    out.stat(
        "  Battle Navigation",
        with_sd(
            b.avg_naval_secs.map(turns).unwrap_or_else(dash),
            b.avg_naval_sd,
            sd_turns,
        ),
        "Average turns spent navigating to grapple (35s per turn) — the naval phase.",
    );
    out.stat(
        "  Swordfight/Rumble",
        with_sd(opt_dur(b.avg_boarding_secs), b.avg_boarding_sd, sd_secs),
        "Average time from grapple to Game Over — the boarding melee.",
    );
    out.blank();

    // Loot.
    out.section("Loot");
    out.stat(
        "PoE per win",
        with_sd(opt_commas(b.poe_per_fight_won), b.poe_per_fight_won_sd, sd_commas),
        "Average pieces of eight plundered per won fight.",
    );
    out.stat(
        "PoE per engagement",
        with_sd(opt_commas(b.poe_per_fight_net), b.poe_per_fight_net_sd, sd_commas),
        "Average net PoE per decisive fight, counting losses as negative.",
    );
    out.stat(
        "Net PoE gained",
        commas(b.poe_net_total),
        "Net pieces of eight across every fight (losses subtracted).",
    );
    out.stat(
        "PoE per crew",
        opt_commas(b.poe_per_crew),
        "Net PoE divided by the time-weighted average crew aboard.",
    );
    out.stat(
        "PoE per crew per win",
        opt_commas(b.poe_per_crew_per_fight_won),
        "Won PoE per crew member per won fight.",
    );
    out.stat(
        "PoE per crew per engagement",
        opt_commas(b.poe_per_crew_per_fight_all),
        "Net PoE per crew member per decisive fight, wins and losses together.",
    );
    out.stat(
        "Goods per win",
        with_sd(opt1(b.goods_per_fight), b.goods_per_fight_sd, sd_one),
        "Average units of goods won per won fight (a count — the log never itemizes).",
    );
    out.stat(
        "Goods per engagement",
        with_sd(opt1(b.goods_per_engagement), b.goods_per_engagement_sd, sd_one),
        "Average net goods per decisive fight — goods lost in defeats subtract.",
    );
    out.blank();

    // Enemies by category — only categories that occurred (no zero rows). The
    // value is `total (wins+losses+disengages)`. Named brigand kings are pulled
    // out of the flat list and grouped, indented, under a "Brigand King" header.
    if !b.categories.is_empty() {
        out.section("Enemies");
        let mut kings: Vec<(&str, &CategoryTally)> = Vec::new();
        for (label, t) in &b.categories {
            match label.strip_prefix("King: ") {
                Some(name) => kings.push((name, t)),
                None => out.stat(label, enemy_value(t), &enemy_tip(label, t)),
            }
        }
        if !kings.is_empty() {
            out.line(Line::from(Span::styled(
                "Brigand King".to_string(),
                Style::default().fg(Color::Gray),
            )));
            for (name, t) in kings {
                out.stat(
                    &format!("  {name}"),
                    enemy_value(t),
                    &enemy_tip(&format!("King {name}"), t),
                );
            }
        }
        out.blank();
    }

    // Advantage (only when damage was tracked for at least one fight).
    if b.avg_advantage_dmg.is_some() || b.avg_advantage_crew.is_some() {
        out.section("Advantage (avg)");
        out.stat(
            "Damage advantage",
            with_sd(
                b.avg_advantage_dmg
                    .map(|a| format!("{:+.0}%", a * 100.0))
                    .unwrap_or_else(dash),
                b.avg_advantage_dmg_sd,
                sd_pct,
            ),
            "Advantage is the difference of the working melee space between one \
             member of our crew against the opposing member of their crew.",
        );
        out.stat(
            "Crew advantage",
            with_sd(
                b.avg_advantage_crew
                    .map(|a| format!("{a:+.1}"))
                    .unwrap_or_else(dash),
                b.avg_advantage_crew_sd,
                sd_one,
            ),
            "Manpower advantage is the total sum of working melee space for our \
             crew, against the total working melee space of the opposing crew.",
        );
        out.blank();
    }

    // Consumption.
    out.section("Consumption");
    out.stat(
        "Cannon Balls",
        commas(c.balls as i64),
        "Cannon balls fired this voyage (Restock minus Stock, summed across all \
         sizes — a ship burns only its own).",
    );
    out.stat(
        "  per battle",
        opt1(c.balls_per_battle),
        "Average cannonballs fired per sea battle.",
    );
    out.stat(
        "Alcohol",
        commas(c.alcohol.weighted() as i64),
        "Alcohol consumed, weighted by potency (swill 2 / grog 3 / fine rum 6).",
    );
    out.stat(
        "  swill",
        commas(c.alcohol.swill as i64),
        "Swill drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  grog",
        commas(c.alcohol.grog as i64),
        "Grog drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  fine rum",
        commas(c.alcohol.fine_rum as i64),
        "Fine rum drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  per crew",
        opt1(c.alcohol_per_crew),
        "Alcohol per crew member aboard.",
    );
    out.stat(
        "  per crew / min",
        opt2(c.alcohol_per_crew_per_min),
        "Alcohol per crew member per minute of the run.",
    );
    out.stat(
        "Rum spice",
        commas(c.rum_spice as i64),
        "Rum spice consumed this voyage.",
    );
    out.stat(
        "  per swabbie",
        opt1(c.rum_spice_per_swabbie),
        "Rum spice per swabbie — spice mainly fuels swabbies.",
    );
    out.stat(
        "  per swabbie / min",
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

/// The em-dash placeholder for a missing value.
fn dash() -> String {
    "—".to_string()
}

/// Append a `", σ = …"` suffix to `value` when a standard deviation is present
/// (the stats layer leaves it `None` for fewer than three data points, so this
/// naturally shows nothing then). `fmt` renders the σ in the stat's own units.
fn with_sd(value: String, sd: Option<f64>, fmt: fn(f64) -> String) -> String {
    match sd {
        Some(s) => format!("{value}, σ = {}", fmt(s)),
        None => value,
    }
}

/// A duration in seconds → `"4.2 turns"` (35 seconds per battle-navigation turn).
fn turns(secs: f64) -> String {
    format!("{:.1} turns", secs / 35.0)
}

/// An enemy row's value: `total (wins+losses+disengages)`.
fn enemy_value(t: &CategoryTally) -> String {
    let total = t.wins + t.losses + t.disengages;
    format!("{total} ({}+{}+{})", t.wins, t.losses, t.disengages)
}

/// Tooltip for an enemy row, spelling out the W/L/D breakdown.
fn enemy_tip(label: &str, t: &CategoryTally) -> String {
    format!(
        "Vs {label}: {} won, {} lost, {} disengaged.",
        t.wins, t.losses, t.disengages
    )
}

/// σ formatters, one per stat unit. For time, σ is shown plainly in seconds.
fn sd_secs(s: f64) -> String {
    format!("{}s", s.round() as i64)
}
fn sd_turns(s: f64) -> String {
    format!("{:.1}", s / 35.0)
}
fn sd_commas(s: f64) -> String {
    commas(s.round() as i64)
}
fn sd_one(s: f64) -> String {
    format!("{s:.1}")
}
fn sd_pct(s: f64) -> String {
    format!("{:.0}%", s * 100.0)
}

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

fn opt1(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".to_string())
}

fn opt2(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".to_string())
}
