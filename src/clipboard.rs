//! Watching the clipboard for something the game put there.
//!
//! Two shapes are recognized, a vessel's [hold](crate::hold) and a
//! [duty report](crate::duty). Neither can be mistaken for the other, and
//! anything else on the clipboard is left alone.
//!
//! A parsed copy is all that crosses into the app, with one exception: a blob
//! shaped like a duty report that the parse refuses crosses verbatim, bound
//! for the persistence file and nothing else. Clipboard text reaches neither
//! the UI nor the diagnostics log by either path.

use std::{collections::BTreeSet, time::Duration};

use chrono::{DateTime, Utc};
use tokio::sync::mpsc::UnboundedSender;

/// Something the game copied, recognized.
#[derive(Debug, PartialEq)]
pub enum Copied {
    Hold(crate::hold::HoldContents),
    Duty(crate::duty::DutyReport),
    /// A copy shaped like a duty report that would not read as one, carried
    /// verbatim along with the pirate names it lists.
    ///
    /// This is the one copy whose own text travels, and it travels exactly as
    /// far as the persistence file, once the names have shown the crew to be
    /// ours: see `AppShell::take_unread_duty_report`. It is never logged and
    /// never drawn.
    UnreadDuty {
        text: String,
        names: BTreeSet<String>,
    },
}

/// A recognized copy and when the clipboard was seen to carry it.
///
/// Nothing the game copies says when it was copied, and the clipboard itself
/// keeps no such date either, so this is the only time a copy will ever have.
/// It is taken the moment the change is seen, which is the moment of the copy
/// to within one turn of the watcher's loop.
#[derive(Debug, PartialEq)]
pub struct Stamped {
    pub copied: Copied,
    pub at: DateTime<Utc>,
}

/// Read whatever the clipboard holds as one of the shapes we know, or `None`
/// for anything else. A report that reads is a report: only a blob that
/// defeats the parse is carried verbatim.
pub fn recognize(text: &str) -> Option<Copied> {
    if let Some(hold) = crate::hold::parse_hold(text) {
        return Some(Copied::Hold(hold));
    }
    if let Some(report) = crate::duty::parse(text) {
        return Some(Copied::Duty(report));
    }
    crate::duty::suspect(text).map(|names| {
        Copied::UnreadDuty {
            text: text.to_owned(),
            names,
        }
    })
}

/// Watch the clipboard and send every copy that is one of ours. The text on
/// the clipboard at startup is the baseline: only a later change is reported,
/// so a stale copy left over from an earlier session doesn't prompt on every
/// launch. The thread ends when the receiver is dropped or when no clipboard
/// is reachable at all (a headless session).
pub fn spawn_watcher(tx: UnboundedSender<Stamped>) {
    std::thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                crate::diag!("warning: clipboard unavailable: {e}");
                return;
            }
        };
        let mut last = clipboard.get_text().ok();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            // a non-text clipboard (an image, say) clears the baseline so
            // re-copying the previous text counts as a change again
            let Ok(text) = clipboard.get_text() else {
                last = None;
                continue;
            };
            if last.as_deref() == Some(text.as_str()) {
                continue;
            }
            // stamped before the text is read for what it is, so the time
            // stands for the copy and not for the work that follows it
            let at = Utc::now();
            if let Some(copied) = recognize(&text)
                && tx
                    .send(Stamped {
                        copied,
                        at,
                    })
                    .is_err()
            {
                return;
            }
            last = Some(text);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_two_shapes_apart() {
        let hold = r#"{"contents":[{"Foo":84}],"coffers":0}"#;
        let duty = r#"{"sail":{"Foo":{"performance":4}}}"#;
        assert!(matches!(
            recognize(hold),
            Some(Copied::Hold(_))
        ));
        assert!(matches!(
            recognize(duty),
            Some(Copied::Duty(_))
        ));
        assert_eq!(recognize("hello"), None);
    }

    /// A report the parse cannot read comes through with its own text, and
    /// only then: a report that reads is read, and nothing else carries text
    /// at all.
    #[test]
    fn only_an_unreadable_report_carries_its_own_text() {
        let unreadable = r#"{"sail":{"Foo":{"performance":9}}}"#;
        let Some(Copied::UnreadDuty {
            text,
            names,
        }) = recognize(unreadable)
        else {
            panic!("an unreadable report");
        };
        assert_eq!(text, unreadable);
        assert_eq!(
            names,
            BTreeSet::from(["Foo".to_owned()])
        );

        for readable in [
            r#"{"sail":{"Foo":{"performance":4}}}"#,
            r#"{"contents":[{"Foo":84}],"coffers":0}"#,
        ] {
            assert!(!matches!(
                recognize(readable),
                Some(Copied::UnreadDuty { .. })
            ));
        }
    }
}
