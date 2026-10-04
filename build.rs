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

use std::{fmt::Write as _, path::Path};

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
                writeln!(
                    out,
                    "            Place {{ name: {name:?}, x: {x}, y: {y} }},"
                )
                .unwrap();
            }
            writeln!(out, "        ],").unwrap();
        }

        writeln!(out, "        leagues: &[").unwrap();
        let leagues = map["leagues"]
            .as_array()
            .unwrap_or_else(|| fail("missing array `leagues`"));
        for league in leagues {
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
            // the far end must stay on the grid: east adds two columns, the
            // north-east diagonal needs a row above
            let heading = match heading {
                "e" if x <= u16::MAX - 2 => "E",
                "se" if x < u16::MAX && y < u16::MAX => "Se",
                "ne" if x < u16::MAX && 1 <= y => "Ne",
                _ => bad(),
            };
            let solid = match kind {
                "solid" => true,
                "dotted" => false,
                _ => bad(),
            };
            writeln!(
                out,
                "            League {{ x: {x}, y: {y}, heading: \
                 Heading::{heading}, solid: {solid} }},"
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
