//! The Map app: the selected ocean's map, scrolled by a cursor that sails
//! league by league, with league points marked as memorized. What is
//! memorized belongs to one pirate on one ocean.

pub mod data;
pub mod ui;

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent};

use crate::{
    app::InputResult,
    map::data::{Heading, League, Map, Place, Point},
    utils::{FieldKind, PromptField},
};

/// A movement key, laid out like the compass on the keyboard: `q`/`e` are
/// the northern diagonals, `z`/`c` the southern ones, `a`/`d` west and
/// east, `w`/`x` north and south.
pub struct MoveKey {
    pub key: char,
    /// Compass label for the hint line.
    pub label: &'static str,
    /// The heading sailed when a league runs that way. `None` for north and
    /// south, which have no leagues of their own.
    pub exact: Option<Heading>,
    /// Headings tried when `exact` has no league: the key sails the one of
    /// them that does, and stays put if more than one does.
    pub fallback: &'static [Heading],
}

pub const MOVE_KEYS: [MoveKey; 8] = [
    MoveKey {
        key: 'q',
        label: "NW",
        exact: Some(Heading::Nw),
        fallback: &[],
    },
    MoveKey {
        key: 'w',
        label: "N",
        exact: None,
        fallback: &[Heading::Nw, Heading::Ne],
    },
    MoveKey {
        key: 'e',
        label: "NE",
        exact: Some(Heading::Ne),
        fallback: &[],
    },
    MoveKey {
        key: 'a',
        label: "W",
        exact: Some(Heading::W),
        fallback: &[Heading::Nw, Heading::Sw],
    },
    MoveKey {
        key: 'd',
        label: "E",
        exact: Some(Heading::E),
        fallback: &[Heading::Ne, Heading::Se],
    },
    MoveKey {
        key: 'z',
        label: "SW",
        exact: Some(Heading::Sw),
        fallback: &[],
    },
    MoveKey {
        key: 'x',
        label: "S",
        exact: None,
        fallback: &[Heading::Sw, Heading::Se],
    },
    MoveKey {
        key: 'c',
        label: "SE",
        exact: Some(Heading::Se),
        fallback: &[],
    },
];

/// The movement key for a typed character, either case.
fn move_key(key: char) -> Option<&'static MoveKey> {
    let key = key.to_ascii_lowercase();
    MOVE_KEYS.iter().find(|m| m.key == key)
}

pub struct MapApp {
    /// The point under the cursor. `None` until a map has been shown.
    pub cursor: Option<Point>,
    /// The pirate (normalized name) whose memorization is loaded. Memorized
    /// points belong to one pirate on one ocean, so without a pirate there
    /// is nowhere to keep a mark and Space does nothing.
    pub pirate: Option<String>,
    /// Points `pirate` has marked as memorized on the selected ocean.
    /// Persisted per pirate per ocean in the cache.
    pub memorized: BTreeSet<Point>,
    /// The open `/` island search, if any.
    pub search: Option<PromptField>,
    /// Whether the `?` help popup is open.
    pub help: bool,
}

impl Default for MapApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MapApp {
    pub fn new() -> Self {
        Self {
            cursor: None,
            pirate: None,
            memorized: BTreeSet::new(),
            search: None,
            help: false,
        }
    }

    /// The cursor's point, parking it on the map's first island when it
    /// hasn't been placed yet. `None` only for a map with no islands.
    pub fn cursor_on(&mut self, map: &Map) -> Option<Point> {
        if self.cursor.is_none() {
            self.cursor = map.islands.first().map(Place::at);
        }
        self.cursor
    }

    /// Sail the cursor one league along `heading`. Only an existing league
    /// is followed; the cursor stays put otherwise.
    pub fn sail(&mut self, heading: Heading, map: &Map) -> bool {
        let Some(from) = self.cursor_on(map) else {
            return false;
        };
        match map.neighbour(from, heading) {
            Some((to, _)) => {
                self.cursor = Some(to);
                true
            }
            None => false,
        }
    }

    /// Sail the cursor where a movement key points: its exact heading when
    /// a league runs that way, otherwise the single fallback heading that
    /// has one. Two possible fallbacks are ambiguous, so the cursor stays.
    pub fn sail_key(&mut self, key: &MoveKey, map: &Map) -> bool {
        let Some(from) = self.cursor_on(map) else {
            return false;
        };
        if let Some(exact) = key.exact
            && self.sail(exact, map)
        {
            return true;
        }
        let mut open = key
            .fallback
            .iter()
            .filter(|h| map.neighbour(from, **h).is_some());
        match (open.next(), open.next()) {
            (Some(&heading), None) => self.sail(heading, map),
            _ => false,
        }
    }

    /// Flip the memorized mark on the point under the cursor. A mark is
    /// only ever made for a known pirate, so that none is made that could
    /// not be saved.
    pub fn toggle_memorized(&mut self) {
        let (Some(p), Some(_)) = (self.cursor, &self.pirate) else {
            return;
        };
        if !self.memorized.remove(&p) {
            self.memorized.insert(p);
        }
    }

    /// Put the cursor on `p` (a click, or a search hit).
    pub fn jump_to(&mut self, p: Point) {
        self.cursor = Some(p);
        self.search = None;
    }

    /// Whether a league can be sailed from memory: both of its ends are
    /// memorized.
    pub fn sailable(&self, league: &League) -> bool {
        let (a, b) = league.ends();
        self.memorized.contains(&a) && self.memorized.contains(&b)
    }

    /// The island the open search currently resolves to.
    pub fn search_hit(&self, map: &'static Map) -> Option<&'static Place> {
        let query = self.search.as_ref()?.value.as_str();
        let names: Vec<String> =
            map.islands.iter().map(|i| i.name.to_owned()).collect();
        let name = crate::app::suggest_island(query, &names)?;
        map.islands.iter().find(|i| i.name == name)
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        map: Option<&'static Map>,
    ) -> InputResult {
        // the help popup is modal: Esc, Enter or another ? dismisses it and
        // everything else is swallowed
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')
            ) {
                self.help = false;
            }
            return InputResult::Consumed;
        }
        if self.search.is_some() {
            return self.handle_search_key(key, map);
        }
        match key.code {
            KeyCode::Up | KeyCode::Esc => return InputResult::Exit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => {
                self.search = Some(PromptField::new(
                    "Search",
                    FieldKind::Text,
                ));
            }
            KeyCode::Char(' ') => {
                if let Some(map) = map {
                    self.cursor_on(map);
                }
                self.toggle_memorized();
            }
            KeyCode::Char(c) => {
                if let (Some(key), Some(map)) = (move_key(c), map) {
                    self.sail_key(key, map);
                }
            }
            _ => {}
        }
        InputResult::Consumed
    }

    /// Keys while the search prompt is open: it owns the letters, Enter jumps
    /// to the best match, Esc/Up close it.
    fn handle_search_key(
        &mut self,
        key: KeyEvent,
        map: Option<&'static Map>,
    ) -> InputResult {
        let Some(search) = self.search.as_mut() else {
            return InputResult::Consumed;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Up => self.search = None,
            KeyCode::Enter => {
                if let Some(hit) = map.and_then(|m| self.search_hit(m)) {
                    self.jump_to(hit.at());
                }
            }
            KeyCode::Backspace => search.delete_char_before(),
            KeyCode::Delete => search.delete_char_at(),
            KeyCode::Left => search.move_left(),
            KeyCode::Right => search.move_right(),
            KeyCode::Home => search.cursor = 0,
            KeyCode::End => search.cursor = search.value.len(),
            KeyCode::Char(c) => search.insert_char(c),
            _ => {}
        }
        InputResult::Consumed
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    // a three-point triangle: Foo -e- Bar on one row, a dotted diagonal
    // south-east of Foo to an open-sea point, and a diagonal from that point
    // north-east back up to Bar
    static MAP: Map = Map {
        ocean: "Test",
        source: "",
        islands: &[
            Place {
                name: "Foo Island",
                x: 1,
                y: 1,
            },
            Place {
                name: "Bar Island",
                x: 3,
                y: 1,
            },
        ],
        labels: &[],
        leagues: &[
            League {
                x: 1,
                y: 1,
                heading: Heading::E,
                chart: crate::map::data::Chart::Sold,
            },
            League {
                x: 1,
                y: 1,
                heading: Heading::Se,
                chart: crate::map::data::Chart::Booty,
            },
            League {
                x: 2,
                y: 2,
                heading: Heading::Ne,
                chart: crate::map::data::Chart::Sold,
            },
        ],
    };

    fn press(app: &mut MapApp, code: KeyCode) {
        app.handle_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            Some(&MAP),
        );
    }

    #[test]
    fn cursor_starts_on_the_first_island_and_follows_leagues() {
        let mut app = MapApp::new();
        assert_eq!(app.cursor_on(&MAP), Some((1, 1)));
        // east along the solid league, then back west
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.cursor, Some((3, 1)));
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.cursor, Some((1, 1)));
        // south-east along the dotted league to the open-sea point
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.cursor, Some((2, 2)));
        // no league runs south-west from there: the cursor stays
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.cursor, Some((2, 2)));
        // north-west is the way back
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.cursor, Some((1, 1)));
    }

    #[test]
    fn a_key_with_no_exact_league_takes_the_only_diagonal_its_way() {
        let mut app = MapApp::new();
        app.cursor_on(&MAP);
        // south from Foo: only the south-east league exists
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.cursor, Some((2, 2)));
        // north from the open-sea point is ambiguous (NW and NE): stay put
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.cursor, Some((2, 2)));
        // east from there has no E league, but only NE leads eastward
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.cursor, Some((3, 1)));
        // west from Bar prefers the exact W league over the SW diagonal
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.cursor, Some((1, 1)));
    }

    #[test]
    fn space_toggles_memorized_and_a_league_is_sailable_between_two_marks() {
        let mut app = MapApp::new();
        // without a pirate there is no one to remember the point
        press(&mut app, KeyCode::Char(' '));
        assert!(app.memorized.is_empty());
        app.pirate = Some("Someone".to_owned());
        press(&mut app, KeyCode::Char(' '));
        assert!(app.memorized.contains(&(1, 1)));
        assert!(!app.sailable(&MAP.leagues[0]));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.sailable(&MAP.leagues[0]));
        assert!(!app.sailable(&MAP.leagues[1]));
        // a second press clears the mark again
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.memorized.contains(&(3, 1)));
        assert!(!app.sailable(&MAP.leagues[0]));
    }

    #[test]
    fn search_owns_the_letters_and_enter_jumps_to_the_match() {
        let mut app = MapApp::new();
        app.cursor_on(&MAP);
        press(&mut app, KeyCode::Char('/'));
        for c in "bar".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        // the movement letter 'a' was typed, not sailed (west is a dead end
        // here anyway, so check the search box instead)
        assert_eq!(
            app.search.as_ref().map(|s| s.value.as_str()),
            Some("bar")
        );
        assert_eq!(
            app.search_hit(&MAP).map(|p| p.name),
            Some("Bar Island")
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.cursor, Some((3, 1)));
        assert!(app.search.is_none());
    }

    #[test]
    fn up_hands_focus_back_unless_the_search_is_open() {
        let mut app = MapApp::new();
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert!(matches!(
            app.handle_key(up, Some(&MAP)),
            InputResult::Exit
        ));
        press(&mut app, KeyCode::Char('/'));
        assert!(matches!(
            app.handle_key(up, Some(&MAP)),
            InputResult::Consumed
        ));
        assert!(app.search.is_none());
    }

    #[test]
    fn help_is_modal_and_closes_on_question_mark_or_esc() {
        let mut app = MapApp::new();
        app.pirate = Some("Someone".to_owned());
        app.cursor_on(&MAP);
        press(&mut app, KeyCode::Char('?'));
        assert!(app.help);
        // movement and memorizing are swallowed while it is open
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.cursor, Some((1, 1)));
        assert!(app.memorized.is_empty());
        // Up neither closes it nor leaves the page
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert!(matches!(
            app.handle_key(up, Some(&MAP)),
            InputResult::Consumed
        ));
        assert!(app.help);
        press(&mut app, KeyCode::Char('?'));
        assert!(!app.help);
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.help);
    }

    #[test]
    fn keys_are_harmless_without_a_map() {
        let mut app = MapApp::new();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(matches!(
            app.handle_key(key, None),
            InputResult::Consumed
        ));
        assert_eq!(app.cursor, None);
    }

    #[test]
    fn emerald_map_is_compiled_in_and_connected() {
        let map = Map::for_ocean("emerald").expect("Emerald map");
        assert!(Map::for_ocean("Atlantis").is_none());
        let points = map.points();
        for island in map.islands {
            assert!(
                map.leagues_at(island.at()).next().is_some(),
                "{} has no league",
                island.name
            );
            assert!(points.contains(&island.at()));
        }
        // every league's far end lands on another point of the map
        for league in map.leagues {
            let (a, b) = league.ends();
            assert!(points.contains(&a) && points.contains(&b));
        }
        // Cryo to Auk is four leagues straight south-east
        let mut app = MapApp::new();
        let cryo = map
            .islands
            .iter()
            .find(|i| i.name == "Cryo Island")
            .expect("Cryo on the map");
        app.jump_to(cryo.at());
        for _ in 0 .. 4 {
            assert!(app.sail(Heading::Se, map));
        }
        assert_eq!(
            app.cursor.and_then(|p| map.island_at(p)).map(|i| i.name),
            Some("Auk Island")
        );
    }
}
