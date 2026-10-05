//! Watching the clipboard for something the game put there.
//!
//! Two shapes are recognized, a vessel's [hold](crate::hold) and a
//! [duty report](crate::duty). Neither can be mistaken for the other, and
//! anything else on the clipboard is left alone: only a parsed copy ever
//! crosses into the app, so arbitrary clipboard text stays out of the UI and
//! the diagnostics log.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

/// Something the game copied, recognized.
#[derive(Debug, PartialEq)]
pub enum Copied {
    Hold(crate::hold::HoldContents),
    Duty(crate::duty::DutyReport),
}

/// Read whatever the clipboard holds as one of the shapes we know, or `None`
/// for anything else.
pub fn recognize(text: &str) -> Option<Copied> {
    if let Some(hold) = crate::hold::parse_hold(text) {
        return Some(Copied::Hold(hold));
    }
    crate::duty::parse(text).map(Copied::Duty)
}

/// Watch the clipboard and send every copy that is one of ours. The text on
/// the clipboard at startup is the baseline: only a later change is reported,
/// so a stale copy left over from an earlier session doesn't prompt on every
/// launch. The thread ends when the receiver is dropped or when no clipboard
/// is reachable at all (a headless session).
pub fn spawn_watcher(tx: UnboundedSender<Copied>) {
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
            if let Some(copied) = recognize(&text)
                && tx.send(copied).is_err()
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
}
