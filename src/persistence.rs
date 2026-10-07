//! The persistence file: what the human behind the keyboard has done and
//! learned, across every pirate they play.
//!
//! Set by `--persistence` (default: `ypp_persistence.json` next to the
//! executable), it holds the completed-voyage history and, per ocean, what
//! each of the user's pirates has memorized. This is the counterpart to
//! [`crate::cache`], which holds data about the *game* (market prices, other
//! players, island lists) and can be thrown away and refetched; nothing in
//! here can be recovered from anywhere else, so every write goes through
//! [`save`], which writes the whole file.
//!
//! All fields use `#[serde(default)]` so a file written by an older version
//! keeps loading.

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::voyage::persistence::SavedVoyage;

/// Everything in the persistence file.
#[derive(Serialize, Deserialize, Default)]
pub struct SavedPersistence {
    /// Completed voyages, oldest first, across all the user's pirates.
    #[serde(default)]
    pub voyages: Vec<SavedVoyage>,
    /// Per-ocean knowledge, keyed by ocean name (e.g. `"Emerald"`).
    #[serde(default)]
    pub oceans: HashMap<String, OceanMemory>,
}

/// What the user's pirates know about one ocean, keyed by normalized pirate
/// name. Knowledge is a pirate's own: two pirates of the same player sail
/// the same ocean knowing different charts.
#[derive(Serialize, Deserialize, Default)]
pub struct OceanMemory {
    #[serde(default)]
    pub pirates: HashMap<String, PirateMemory>,
    /// What the user has written down about the pirates they have sailed with,
    /// keyed by normalized name. Unlike [`Self::pirates`], these are notes
    /// *about* other pirates and belong to the human rather than to any one of
    /// their own pirates; the ocean keys them because a name is an ocean's
    /// own.
    #[serde(default)]
    pub notes: HashMap<String, String>,
}

/// What one pirate knows about one ocean.
#[derive(Serialize, Deserialize, Default)]
pub struct PirateMemory {
    /// League points (islands included) the pirate has marked as memorized
    /// on the Map page, as `(x, y)` map cells.
    #[serde(default)]
    pub memorized: BTreeSet<(u16, u16)>,
}

impl SavedPersistence {
    /// Take `pirate`'s memorized league points on `ocean` out of the file,
    /// leaving the ocean's other pirates in place. Empty for a pirate the
    /// file has never seen.
    pub fn take_memorized(
        &mut self,
        ocean: &str,
        pirate: &str,
    ) -> BTreeSet<(u16, u16)> {
        self.oceans
            .get_mut(ocean)
            .and_then(|o| o.pirates.remove(pirate))
            .map(|p| p.memorized)
            .unwrap_or_default()
    }

    /// What the user has written down about `pirate` on `ocean`, or `None` for
    /// a pirate they have written nothing about. A note of blanks is
    /// nothing written, so it answers `None` too.
    pub fn note(&self, ocean: &str, pirate: &str) -> Option<&str> {
        self.oceans
            .get(ocean)?
            .notes
            .get(&note_key(pirate))
            .map(String::as_str)
            .filter(|note| !note.trim().is_empty())
    }

    /// Write down `note` about `pirate` on `ocean`, replacing whatever stood
    /// there. A note of blanks is an erasure: the entry is dropped rather than
    /// kept as an empty string, so the file holds only what was actually
    /// written.
    pub fn set_note(&mut self, ocean: &str, pirate: &str, note: &str) {
        let note = note.trim();
        if note.is_empty() {
            if let Some(o) = self.oceans.get_mut(ocean) {
                o.notes.remove(&note_key(pirate));
            }
            return;
        }
        self.oceans
            .entry(ocean.to_owned())
            .or_default()
            .notes
            .insert(note_key(pirate), note.to_owned());
    }

    /// Put `pirate`'s memorized league points on `ocean` back, creating the
    /// ocean's entry if this is their first mark there.
    pub fn set_memorized(
        &mut self,
        ocean: &str,
        pirate: &str,
        memorized: BTreeSet<(u16, u16)>,
    ) {
        self.oceans
            .entry(ocean.to_owned())
            .or_default()
            .pirates
            .insert(
                pirate.to_owned(),
                PirateMemory {
                    memorized,
                },
            );
    }
}

/// The key a note is filed under: the pirate's name as the rest of the program
/// normalizes names, so one pirate cannot end up with two notes for having been
/// typed two ways. A name too malformed to normalize is filed as it came, there
/// being nothing better to call it.
fn note_key(pirate: &str) -> String {
    crate::pirate::normalize_name(pirate)
        .unwrap_or_else(|_| pirate.trim().to_owned())
}

/// Load the persistence file. A missing or unparseable file yields an empty
/// one rather than an error, so a first run just starts fresh.
pub fn load(path: &Path) -> SavedPersistence {
    let Ok(data) = std::fs::read_to_string(path) else {
        return SavedPersistence::default();
    };
    match serde_json::from_str(&data) {
        Ok(saved) => {
            eprintln!(
                "Read yer records from {}",
                path.display()
            );
            saved
        }
        Err(e) => {
            eprintln!("warning: failed to parse persisted data: {e}");
            SavedPersistence::default()
        }
    }
}

/// Write the whole persistence file. Every writer goes through here, so one
/// kind of persisted data can never overwrite the file without the rest.
pub fn save(path: &Path, saved: &SavedPersistence) {
    crate::utils::write_json_atomic(path, saved, "persisted data");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two of the user's pirates on one ocean keep their own memorization:
    /// one's marks never answer for the other's, and taking one out leaves
    /// the other alone.
    #[test]
    fn memorization_is_kept_per_pirate_within_an_ocean() {
        let mut saved = SavedPersistence::default();
        saved.set_memorized(
            "Test",
            "Someone",
            [(1, 1), (3, 1)].into_iter().collect(),
        );
        saved.set_memorized(
            "Test",
            "Otherone",
            [(1, 1)].into_iter().collect(),
        );
        let json = serde_json::to_string(&saved).unwrap();
        let mut back: SavedPersistence = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.take_memorized("Test", "Someone").len(),
            2
        );
        assert_eq!(
            back.take_memorized("Test", "Otherone"),
            [(1, 1)].into_iter().collect()
        );
        // a pirate or an ocean the file has never seen has no marks
        assert!(back.take_memorized("Test", "Nobody").is_empty());
        assert!(back.take_memorized("Nowhere", "Someone").is_empty());
    }

    /// A file on disk comes back with each pirate's own marks, the way the
    /// Map page asks for them at startup.
    #[test]
    fn marks_load_from_a_file_per_pirate() {
        let path = std::env::temp_dir().join(format!(
            "ypp_persistence_test_{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"oceans":{"Test":{"pirates":{
                "Someone":{"memorized":[[11,30],[13,30]]},
                "Otherone":{"memorized":[[11,30]]}}}}}"#,
        )
        .expect("write the file");
        let mut saved = load(&path);
        std::fs::remove_file(&path).ok();
        assert!(saved.voyages.is_empty());
        assert_eq!(
            saved.take_memorized("Test", "Someone").len(),
            2
        );
        assert_eq!(
            saved.take_memorized("Test", "Otherone").len(),
            1
        );
    }

    /// A note is the human's own and belongs to the pirate it is about, on the
    /// ocean that pirate's name belongs to. It survives the file, and however
    /// the name was typed it is filed once.
    #[test]
    fn notes_are_kept_per_pirate_within_an_ocean() {
        let mut saved = SavedPersistence::default();
        saved.set_note("Test", "Playerone", "  Fine gunner  ");
        saved.set_note("Test", "Playertwo", "Jumped ship");
        saved.set_note("Other", "Playerone", "Someone else's");

        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedPersistence = serde_json::from_str(&json).unwrap();
        // Written down trimmed, and found however the name was typed.
        assert_eq!(
            back.note("Test", "playerONE"),
            Some("Fine gunner")
        );
        assert_eq!(
            back.note("Test", "Playertwo"),
            Some("Jumped ship")
        );
        assert_eq!(
            back.note("Other", "Playerone"),
            Some("Someone else's")
        );
        // Nothing written about them, nowhere.
        assert_eq!(back.note("Test", "Playerthree"), None);
        assert_eq!(back.note("Nowhere", "Playerone"), None);
    }

    /// An empty note is no note: writing blanks erases the entry rather than
    /// leaving a blank one behind.
    #[test]
    fn a_note_of_blanks_erases_it() {
        let mut saved = SavedPersistence::default();
        saved.set_note("Test", "Playerone", "Fine gunner");
        saved.set_note("Test", "Playerone", "   ");
        assert_eq!(saved.note("Test", "Playerone"), None);
        assert!(saved.oceans["Test"].notes.is_empty());
        // Erasing what was never written leaves the file alone.
        saved.set_note("Nowhere", "Playerone", "");
        assert!(!saved.oceans.contains_key("Nowhere"));
    }

    /// Notes and memorization share an ocean's entry, so writing one keeps the
    /// other.
    #[test]
    fn notes_and_memorization_share_an_ocean() {
        let mut saved = SavedPersistence::default();
        saved.set_memorized(
            "Test",
            "Someone",
            [(1, 1)].into_iter().collect(),
        );
        saved.set_note("Test", "Playerone", "Fine gunner");
        let json = serde_json::to_string(&saved).unwrap();
        let mut back: SavedPersistence = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.note("Test", "Playerone"),
            Some("Fine gunner")
        );
        assert_eq!(
            back.take_memorized("Test", "Someone").len(),
            1
        );
    }

    /// Memorization and the voyage history share the file, so a round trip
    /// of one keeps the other.
    #[test]
    fn voyages_and_memorization_share_the_file() {
        let mut saved = SavedPersistence {
            voyages: vec![SavedVoyage {
                vessel: Some("Test".to_owned()),
                ..SavedVoyage::default()
            }],
            ..SavedPersistence::default()
        };
        saved.set_memorized(
            "Test",
            "Someone",
            [(1, 1)].into_iter().collect(),
        );
        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedPersistence = serde_json::from_str(&json).unwrap();
        assert_eq!(back.voyages.len(), 1);
        assert_eq!(
            back.oceans["Test"].pirates["Someone"].memorized.len(),
            1
        );
    }
}
