// ---------------------------------------------------------------------------
// Field types & validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum FieldKind {
    PositiveInt,
    Rate,
    Text,
}

pub struct PromptField {
    pub label: &'static str,
    pub kind: FieldKind,
    pub value: String,
    pub cursor: usize,
}

impl PromptField {
    pub fn new(label: &'static str, kind: FieldKind) -> Self {
        Self {
            label,
            kind,
            value: String::new(),
            cursor: 0,
        }
    }

    pub fn accepts(&self, c: char) -> bool {
        match self.kind {
            FieldKind::PositiveInt => c.is_ascii_digit(),
            FieldKind::Rate => {
                if c.is_ascii_digit() {
                    true
                } else if c == '.' {
                    !self.value.contains('.')
                } else if c == '%' {
                    !self.value.contains('%') && self.cursor == self.value.len()
                } else {
                    false
                }
            }
            FieldKind::Text => !c.is_control(),
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.accepts(c) {
            self.value.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[.. self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.remove(prev);
            self.cursor = prev;
        }
    }

    pub fn delete_char_at(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.value[.. self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += self.value[self.cursor ..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }
}

// ---------------------------------------------------------------------------
// General-purpose utilities
// ---------------------------------------------------------------------------

pub fn text_similarity(a: &str, b: &str) -> f64 {
    strsim::jaro_winkler(a, b)
}

/// Sink for best-effort diagnostic lines (save results, load warnings). While
/// the TUI owns the terminal it draws to stdout's alternate screen, but stderr
/// still points at the same terminal — so an `eprintln!` mid-run paints raw
/// bytes over the frame and garbles the render until the next full redraw. Once
/// [`init_diag_log`] points this at a file, [`diag`] appends there instead;
/// before the TUI starts (or if no log file could be opened) it falls back to
/// stderr, so startup progress output is unaffected.
static DIAG_LOG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> =
    std::sync::OnceLock::new();

/// Redirect [`diag`] output to `path` (append, created if absent) for the rest
/// of the process. Call once, just before entering the alternate screen. On
/// failure the sink stays on stderr rather than aborting — diagnostics are
/// best-effort.
pub fn init_diag_log(path: &std::path::Path) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = DIAG_LOG.set(std::sync::Mutex::new(file));
    }
}

/// Emit a diagnostic line to the log file if one is configured (TUI is up),
/// else to stderr. Prefer the [`diag!`] macro for `eprintln!`-style formatting.
pub fn diag(msg: &str) {
    if let Some(lock) = DIAG_LOG.get()
        && let Ok(mut file) = lock.lock()
    {
        use std::io::Write;
        let _ = writeln!(file, "{msg}");
        return;
    }
    eprintln!("{msg}");
}

/// `eprintln!`-style wrapper over [`diag`]: formats its arguments and routes
/// the line through the TUI-safe sink instead of straight to stderr.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => { $crate::utils::diag(&format!($($arg)*)) };
}

/// Atomically write `value` to `path` as compact JSON.
///
/// The single write path for every JSON file we persist. Serialization is
/// streamed (no intermediate `String`) into a sibling temp file, which is then
/// `rename`d over `path`. A crash or serialization error mid-write leaves the
/// original file untouched rather than truncated, and `rename` on the same
/// filesystem is atomic, so a reader never sees a half-written file.
///
/// `label` names the payload for the log lines (e.g. `"cache"`,
/// `"persisted data"`): a `Saved {label} to {path}` on success, or a
/// `failed to … {label}` on error. Messages go through [`diag`] (the TUI-safe
/// sink) rather than straight to stderr — this runs mid-render from the
/// save/discard prompt, and a raw `eprintln!` would garble the alternate
/// screen. Errors are swallowed beyond that line (save is best-effort).
pub fn write_json_atomic<T: serde::Serialize>(
    path: &std::path::Path,
    value: &T,
    label: &str,
) {
    use std::io::Write;

    // Temp file alongside the target so the final rename stays on one
    // filesystem (a cross-device rename would fail). Tie the name to the
    // target's so concurrent saves of *different* files don't collide.
    let file_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let mut tmp_name = file_name;
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);

    let file = match std::fs::File::create(&tmp) {
        Ok(file) => file,
        Err(e) => {
            crate::diag!(
                "error: failed to open {} for writing: {e}",
                tmp.display()
            );
            return;
        }
    };
    let mut writer = std::io::BufWriter::new(file);
    if let Err(e) = serde_json::to_writer(&mut writer, value) {
        crate::diag!("error: failed to serialize {label}: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Flush the BufWriter before the rename, or buffered bytes could be lost.
    if let Err(e) = writer.flush() {
        crate::diag!(
            "error: failed to flush {}: {e}",
            tmp.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    drop(writer);
    if let Err(e) = std::fs::rename(&tmp, path) {
        crate::diag!(
            "error: failed to write {label} to {} (rename from temp failed: \
             {e})",
            path.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    crate::diag!("Saved {label} to {}", path.display());
}

pub fn parse_rate(field: &PromptField) -> f64 {
    let s = field.value.trim();
    if s.is_empty() {
        return 0.0;
    }
    if let Some(stripped) = s.strip_suffix('%') {
        stripped.parse::<f64>().unwrap_or(0.0) / 100.0
    } else {
        s.parse::<f64>().unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Widget titles
// ---------------------------------------------------------------------------

/// Length of the `─` run that leads an offset title. The block's border
/// supplies the matching trailing run.
const TITLE_DASHES: usize = 3;

/// Build a block title in the app's house style (`─── Title `) together with
/// the minimum widget width that keeps it readable. The width comes from
/// [`offset_title_width`] so the two never drift.
///
/// The title is left-aligned on the block; at the returned width it reads
/// `┌─── Title ───┐`, and any extra width simply lengthens the trailing run.
pub fn offset_title(title: &'static str) -> (String, u16) {
    // The `─── …` frame, kept as a tiny macro local to this fn since nothing
    // else needs it. (`concat!` can't be used — `title` isn't a literal.)
    macro_rules! framed {
        ($t:expr) => {
            format!("{} {} ", "─".repeat(TITLE_DASHES), $t)
        };
    }
    (
        framed!(title),
        offset_title_width(title),
    )
}

/// Minimum widget width at which an [`offset_title`] for `title` sits centered
/// — equal `───` runs flank the text. `const` so widgets can derive a `const`
/// minimum width and use it directly as a layout floor. Assumes an ASCII title
/// (byte length == column count), which all of ours are.
pub const fn offset_title_width(title: &'static str) -> u16 {
    // `─── {title} ` spans `title.len() + TITLE_DASHES + 2` columns; the box
    // adds a matching trailing dash run + 2 corners.
    (title.len() + 2 * TITLE_DASHES + 4) as u16
}

/// Columns a boxed widget spends on its frame: a border and a blank column on
/// each side, the blanks holding the contents off the border.
pub const BOX_MARGIN: u16 = 2 * (1 + PADDING);

/// Blank columns between a widget's border and its contents, on each side. A
/// widget with no border keeps the same blank columns at the edges of the room
/// it was given.
pub const PADDING: u16 = 1;

/// A bordered, padded block titled in the house style, together with the width
/// the widget must not go below.
///
/// `content_width` is what the contents alone occupy, counting neither border
/// nor padding. The width answers both of the things that can force a widget
/// wider: the contents inside their margins, and the title needing a trailing
/// rule.
///
/// Taking both from one call is the point: [`offset_title`] hands back the
/// same floor, but a caller that wants only the title tends to drop it.
pub fn titled_block(
    title: &'static str,
    content_width: u16,
) -> (ratatui::widgets::Block<'static>, u16) {
    use ratatui::widgets::{Block, Borders, Padding};

    (
        Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(PADDING))
            .title(offset_title(title).0),
        (content_width + BOX_MARGIN).max(offset_title_width(title)),
    )
}

/// Draw the notice that stands in for something unusable until a prerequisite
/// is met — a missing argument, a window too small — centered on both axes of
/// `area` and word-wrapped to its width. Each entry is wrapped on its own, so a
/// heading and the detail beneath it can carry different styles.
///
/// `area` is the room the text may fill, already holding the blank column per
/// side that the notice keeps at its edges: a boxed widget has it from
/// [`titled_block`]'s padding and passes `block.inner(area)`, while a notice
/// standing in for a whole page has no border to take it from and insets by
/// [`PADDING`] itself.
///
/// Folding the text here rather than leaving it to `Wrap` is what allows the
/// vertical centering: the height follows from the text once it is folded to
/// the width, and is not known before.
pub fn render_notice(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    paragraphs: &[(&str, ratatui::style::Style)],
) {
    use ratatui::{
        text::{Line, Span},
        widgets::Paragraph,
    };

    let lines = paragraphs
        .iter()
        .flat_map(|(text, style)| {
            wrap_words(text, area.width as usize)
                .into_iter()
                .map(|line| Line::from(Span::styled(line, *style)))
        })
        .collect::<Vec<_>>();

    let height = (lines.len() as u16).min(area.height);
    let rect = ratatui::layout::Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    };
    frame.render_widget(Paragraph::new(lines).centered(), rect);
}

/// Draw a [`render_notice`] that stands in for a whole page rather than for one
/// widget. With no border to take them from, the blank columns it keeps at its
/// edges come from [`PADDING`].
pub fn render_page_notice(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    paragraphs: &[(&str, ratatui::style::Style)],
) {
    render_notice(
        frame,
        ratatui::layout::Rect {
            x: area.x + PADDING.min(area.width),
            width: area.width.saturating_sub(2 * PADDING),
            ..area
        },
        paragraphs,
    );
}

/// Refuse to draw a page in less width than `needed`, drawing the notice saying
/// so in its place. Returns whether it did, so a page that cannot narrow any
/// further returns on `true`.
///
/// How much width is needed is the page's own business and moves with what it
/// has to show, so each page weighs its own content here rather than being
/// held to one figure for the whole app. A page that fits the window is drawn
/// even when another page would not have fit, and the top bar stays above the
/// notice so the pages that do fit are still reachable.
pub fn too_narrow(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    needed: u16,
) -> bool {
    use ratatui::style::{Color, Style};

    if needed <= area.width {
        return false;
    }

    let detail = format!(
        "Enlarge the window to at least {needed} columns (it is {}).",
        area.width,
    );
    // The same heading the app uses when the window is too small for anything
    // at all: to the user it is one condition, and what is lacking this
    // time is the detail line's business.
    render_page_notice(
        frame,
        area,
        &[
            (
                "Terminal too small",
                Style::default().bold(),
            ),
            (
                &detail,
                Style::default().fg(Color::DarkGray),
            ),
        ],
    );
    true
}

/// Word-wrap `text` to `width` columns, hard-breaking any single word longer
/// than the line so a narrow column never overflows. Returns one `String` per
/// line.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for mut word in text.split_whitespace() {
        // A word that can't fit on its own line is chopped to width.
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            let consumed = head.len();
            lines.push(head);
            word = &word[consumed ..];
        }
        if word.is_empty() {
            continue;
        }
        let need = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if need > width {
            lines.push(std::mem::take(&mut cur));
        } else if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // The width is const-evaluable, so widgets can build `const` floors from
    // it.
    const _: () = assert!(offset_title_width("Ocean") == 15);

    #[test]
    fn wrap_words_breaks_on_spaces() {
        assert_eq!(
            wrap_words("Understaffed. Hire jobbers.", 20),
            vec!["Understaffed. Hire", "jobbers."],
        );
    }

    #[test]
    fn wrap_words_hard_breaks_overlong_words() {
        assert_eq!(
            wrap_words("Understaffed.", 5),
            vec!["Under", "staff", "ed."]
        );
    }

    // A notice sits in the middle of the room it was given on both axes, folded
    // to that room's width.
    #[test]
    fn notice_is_centered_on_both_axes() {
        use ratatui::{
            Terminal,
            backend::TestBackend,
            layout::Rect,
            style::Style,
        };

        let mut terminal =
            Terminal::new(TestBackend::new(14, 5)).expect("terminal");
        terminal
            .draw(|frame| {
                // One blank column each side of a 14-column screen.
                render_notice(
                    frame,
                    Rect::new(PADDING, 0, 14 - 2 * PADDING, 5),
                    &[("one two three four", Style::default())],
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let rows = (0 .. buffer.area.height)
            .map(|y| {
                (0 .. buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            rows,
            [
                "              ",
                // An odd remainder goes to the left of the line.
                "    one two   ",
                "  three four  ",
                "              ",
                "              ",
            ],
        );
    }

    #[test]
    fn offset_title_is_centered_at_min_width() {
        let (s, w) = offset_title("Ocean");
        assert_eq!(s, "─── Ocean ");
        // ┌─── Ocean ───┐  → 2 corners + 10 title cols + 3 trailing dashes.
        assert_eq!(w, 15);
        assert_eq!(offset_title_width("Ocean"), 15);
    }
}
