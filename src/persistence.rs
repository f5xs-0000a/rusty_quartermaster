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
