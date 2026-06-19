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
            let prev = self.value[..self.cursor]
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
            self.cursor = self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += self.value[self.cursor..]
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

/// Length of the `─` run that leads an offset title. The block's border supplies
/// the matching trailing run.
const TITLE_DASHES: usize = 3;

/// Build a block title in the app's house style (`─── Title `) together with the
/// minimum widget width that keeps it readable. The width comes from
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
    (framed!(title), offset_title_width(title))
}

/// Minimum widget width at which an [`offset_title`] for `title` sits centered —
/// equal `───` runs flank the text. `const` so widgets can derive a `const`
/// minimum width and use it directly as a layout floor. Assumes an ASCII title
/// (byte length == column count), which all of ours are.
pub const fn offset_title_width(title: &'static str) -> u16 {
    // `─── {title} ` spans `title.len() + TITLE_DASHES + 2` columns; the box adds
    // a matching trailing dash run + 2 corners.
    (title.len() + 2 * TITLE_DASHES + 4) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    // The width is const-evaluable, so widgets can build `const` floors from it.
    const _: () = assert!(offset_title_width("Ocean") == 15);

    #[test]
    fn offset_title_is_centered_at_min_width() {
        let (s, w) = offset_title("Ocean");
        assert_eq!(s, "─── Ocean ");
        // ┌─── Ocean ───┐  → 2 corners + 10 title cols + 3 trailing dashes.
        assert_eq!(w, 15);
        assert_eq!(offset_title_width("Ocean"), 15);
    }
}
