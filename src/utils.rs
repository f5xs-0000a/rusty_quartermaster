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
    if let Some(lock) = DIAG_LOG.get() {
        if let Ok(mut file) = lock.lock() {
            use std::io::Write;
            let _ = writeln!(file, "{msg}");
            return;
        }
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
/// `"voyage history"`): a `Saved {label} to {path}` on success, or a
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

    #[test]
    fn offset_title_is_centered_at_min_width() {
        let (s, w) = offset_title("Ocean");
        assert_eq!(s, "─── Ocean ");
        // ┌─── Ocean ───┐  → 2 corners + 10 title cols + 3 trailing dashes.
        assert_eq!(w, 15);
        assert_eq!(offset_title_width("Ocean"), 15);
    }
}
