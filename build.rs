//! Build-time asset compaction and code generation.
//!
//! The bare cache (`src/data/bare_cache.json`) is kept **prettified** in the
//! source tree so it diffs and edits cleanly. There's no point shipping all
//! that indentation and newlines in the binary, though — so at build time we
//! parse it (which also validates it: a malformed JSON fails the build) and
//! re-emit a minified copy into `OUT_DIR`. `src/bare.rs` embeds *that* compact
//! copy via `include_str!`, not the pretty source.
//!
//! The ocean maps (`src/data/maps/*.json`, written by
//! `scripts/extract_map.py`) go one step further: each is turned into Rust
//! statics (`maps.rs` in `OUT_DIR`, pulled in by `src/map/data.rs`), so the
//! Map app carries every map as compile-time data and parses nothing at run
//! time. A malformed map file fails the build.
//!
//! A map file lists only the leagues the wiki's map draws, which are the ones
//! some chart follows. The leagues between points a chart never joins are
//! derived here from the geometry, so the map files stay a transcript of the
//! wiki while the statics hold the whole league graph.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};

/// The headings a league can be listed under: as a map file spells it, as
/// `Heading` spells it, and the step from the western point to the far end.
const HEADINGS: [(&str, &str, (i16, i16)); 3] = [
    ("e", "E", (2, 0)),
    ("se", "Se", (1, 1)),
    ("ne", "Ne", (1, -1)),
];

/// The far end of a league leaving `at` along `HEADINGS[heading]`, if it stays
/// on the grid.
fn step((x, y): (u16, u16), heading: usize) -> Option<(u16, u16)> {
    let (dx, dy) = HEADINGS[heading].2;
    Some((
        x.checked_add_signed(dx)?,
        y.checked_add_signed(dy)?,
    ))
}

fn main() {
    minify_bare_cache();
    generate_maps();
}

fn minify_bare_cache() {
    let src = Path::new("src/data/bare_cache.json");
    println!(
        "cargo:rerun-if-changed={}",
        src.display()
    );

    let pretty = std::fs::read_to_string(src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", src.display()));

    // Parse then re-serialize compact. Parsing doubles as validation, and going
    // through `Value` (rather than stripping whitespace textually) keeps string
    // contents — island names with spaces, apostrophes, accents — intact.
    let value: serde_json::Value = serde_json::from_str(&pretty)
        .unwrap_or_else(|e| {
            panic!(
                "{} is not valid JSON: {e}",
                src.display()
            )
        });
    let compact = serde_json::to_string(&value)
        .expect("re-serializing bare cache to compact JSON");

    write_out("bare_cache.min.json", &compact);
}

/// Emit `maps.rs`: a `&[Map]` slice literal, one entry per map file in
/// `src/data/maps/`, in file-name order.
fn generate_maps() {
    let dir = Path::new("src/data/maps");
    // a directory path makes cargo rescan the whole tree, so adding a map
    // file triggers a rebuild too
    println!(
        "cargo:rerun-if-changed={}",
        dir.display()
    );

    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|entry| entry.expect("map directory entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    let mut out = String::from("&[\n");
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", file.display())
        });
        let map: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| {
                panic!(
                    "{} is not valid JSON: {e}",
                    file.display()
                )
            });
        let fail = |what: &str| -> ! { panic!("{}: {what}", file.display()) };
        let str_field = |key: &str| -> &str {
            map[key]
                .as_str()
                .unwrap_or_else(|| fail(&format!("missing string `{key}`")))
        };

        writeln!(out, "    Map {{").unwrap();
        writeln!(
            out,
            "        ocean: {:?},",
            str_field("ocean")
        )
        .unwrap();
        writeln!(
            out,
            "        source: {:?},",
            str_field("source")
        )
        .unwrap();
        // every island sits on a league point, so the islands feed the point
        // set the leagues are derived from below
        let mut points: BTreeSet<(u16, u16)> = BTreeSet::new();
        for key in ["islands", "labels"] {
            writeln!(out, "        {key}: &[").unwrap();
            let places = map[key]
                .as_array()
                .unwrap_or_else(|| fail(&format!("missing array `{key}`")));
            for place in places {
                let name = place["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| {
                        fail(&format!("{key} entry without a name"))
                    });
                let (x, y) = (coord(&place["x"]), coord(&place["y"]));
                let (Some(x), Some(y)) = (x, y) else {
                    fail(&format!(
                        "{name}: coordinates must be whole numbers below 65535"
                    ))
                };
                if key == "islands" {
                    points.insert((x, y));
                }
                writeln!(
                    out,
                    "            Place {{ name: {name:?}, x: {x}, y: {y} }},"
                )
                .unwrap();
            }
            writeln!(out, "        ],").unwrap();
        }

        // the wiki's map draws only the leagues a chart follows, so the drawn
        // ones are collected first and the rest of the graph derived from the
        // geometry afterwards
        let mut leagues: BTreeMap<(u16, u16, usize), &str> = BTreeMap::new();
        let drawn = map["leagues"]
            .as_array()
            .unwrap_or_else(|| fail("missing array `leagues`"));
        for league in drawn {
            let spec = league
                .as_str()
                .unwrap_or_else(|| fail("league entries are strings"));
            let bad = || -> ! {
                fail(&format!(
                    "league {spec:?} is not `x,y e|se|ne solid|dotted`"
                ))
            };
            let mut words = spec.split_whitespace();
            let (Some(at), Some(heading), Some(kind), None) = (
                words.next(),
                words.next(),
                words.next(),
                words.next(),
            ) else {
                bad()
            };
            let Some((x, y)) = at.split_once(',') else {
                bad()
            };
            let (Ok(x), Ok(y)) = (x.parse::<u16>(), y.parse::<u16>()) else {
                bad()
            };
            let Some(heading) = HEADINGS
                .iter()
                .position(|(spelling, ..)| *spelling == heading)
            else {
                bad()
            };
            // east adds two columns and the north-east diagonal needs a row
            // above, so not every heading leaves every cell on the grid
            let Some(to) = step((x, y), heading) else {
                bad()
            };
            let chart = match kind {
                "solid" => "Sold",
                "dotted" => "Unsold",
                _ => bad(),
            };
            if leagues.insert((x, y, heading), chart).is_some() {
                fail(&format!(
                    "league {spec:?} is listed twice"
                ));
            }
            points.insert((x, y));
            points.insert(to);
        }

        // two league points a single league apart can be sailed between
        // whether or not a chart covers that route, so every such pair is a
        // league; the wiki simply never draws the ones no chart follows, and
        // neither does the Map page until both ends are memorized.
        let mut derived = Vec::new();
        for &at in &points {
            for (heading, &(spelling, ..)) in HEADINGS.iter().enumerate() {
                let Some(to) = step(at, heading) else {
                    continue;
                };
                if !points.contains(&to)
                    || leagues.contains_key(&(at.0, at.1, heading))
                {
                    continue;
                }
                // an east-west league spans two columns, so deriving one over
                // a point in between would let a cursor skip that point
                if spelling == "e" && points.contains(&(at.0 + 1, at.1)) {
                    continue;
                }
                derived.push((at.0, at.1, heading));
            }
        }
        let charted = leagues.len();
        for key in derived {
            leagues.insert(key, "Nonexistent");
        }

        writeln!(
            out,
            "        // {charted} leagues drawn on the wiki's map, {} derived \
             from the geometry",
            leagues.len() - charted
        )
        .unwrap();
        writeln!(out, "        leagues: &[").unwrap();
        let mut by_row: Vec<_> = leagues.into_iter().collect();
        by_row.sort_by_key(|&((x, y, heading), _)| (y, x, heading));
        for ((x, y, heading), chart) in by_row {
            writeln!(
                out,
                "            League {{ x: {x}, y: {y}, heading: Heading::{}, \
                 chart: Chart::{chart} }},",
                HEADINGS[heading].1
            )
            .unwrap();
        }
        writeln!(out, "        ],").unwrap();
        writeln!(out, "    }},").unwrap();
    }
    out.push_str("]\n");

    write_out("maps.rs", &out);
}

/// A JSON number as a grid coordinate, if it is a whole number that fits.
fn coord(value: &serde_json::Value) -> Option<u16> {
    value.as_u64().and_then(|n| u16::try_from(n).ok())
}

fn write_out(name: &str, contents: &str) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join(name);
    std::fs::write(&dest, contents).unwrap_or_else(|e| {
        panic!(
            "failed to write {}: {e}",
            dest.display()
        )
    });
}
