//! Hold contents read from the clipboard.
//!
//! The game can put a vessel's hold on the clipboard as JSON:
//!
//! ```json
//! {"volumeCapacity":20250,"contents":[{"Swill":84},{"Rum Spice":69}],
//!  "inventoryOid":669287,"massCapacity":135000,"coffers":0}
//! ```
//!
//! Only the `contents` list is used; it feeds the Stock column of the Profits
//! page (the Booty column is the divvy's business, not the hold's). The
//! clipboard is polled on a background thread and only a parsed hold ever
//! crosses into the app, so arbitrary clipboard text stays out of the UI and
//! the diagnostics log.

use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

/// The goods in a hold, in the order the game listed them.
#[derive(Debug, PartialEq)]
pub struct HoldContents {
    pub goods: Vec<(String, u64)>,
}

#[derive(Deserialize)]
struct RawHold {
    // each entry is a single-key object: {"<commodity>": <quantity>}
    contents: Vec<std::collections::BTreeMap<String, u64>>,
}

/// Parse the hold JSON the game copies to the clipboard. `None` for anything
/// that isn't shaped like one.
pub fn parse_hold(text: &str) -> Option<HoldContents> {
    let raw: RawHold = serde_json::from_str(text.trim()).ok()?;
    let goods = raw.contents.into_iter().flatten().collect();
    Some(HoldContents {
        goods,
    })
}

/// Watch the clipboard and send every hold that appears on it. The text on
/// the clipboard at startup is the baseline: only a later change is reported,
/// so a stale hold left over from an earlier session doesn't prompt on every
/// launch. The thread ends when the receiver is dropped or when no clipboard
/// is reachable at all (a headless session).
pub fn spawn_watcher(tx: UnboundedSender<HoldContents>) {
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
            // re-copying the previous hold counts as a change again
            let Ok(text) = clipboard.get_text() else {
                last = None;
                continue;
            };
            if last.as_deref() == Some(text.as_str()) {
                continue;
            }
            if let Some(hold) = parse_hold(&text)
                && tx.send(hold).is_err()
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
    fn parses_the_hold_blob() {
        let text = r#"{"volumeCapacity":20250,"contents":[{"Foo":84},
            {"Bar baz":69},{"Small cannon balls":257}],"inventoryOid":1,
            "massCapacity":135000,"coffers":0}"#;
        let hold = parse_hold(text).expect("hold");
        assert_eq!(
            hold.goods,
            vec![
                ("Foo".to_owned(), 84),
                ("Bar baz".to_owned(), 69),
                ("Small cannon balls".to_owned(), 257),
            ]
        );
    }

    #[test]
    fn empty_hold_is_still_a_hold() {
        let hold = parse_hold(r#"{"contents":[],"coffers":0}"#).expect("hold");
        assert!(hold.goods.is_empty());
    }

    #[test]
    fn rejects_other_text() {
        assert!(parse_hold("hello").is_none());
        assert!(parse_hold(r#"{"coffers":0}"#).is_none());
        assert!(parse_hold(r#"{"contents":[{"Foo":"bar"}]}"#).is_none());
        assert!(parse_hold(r#"{"contents":[{"Foo":-1}]}"#).is_none());
    }
}
