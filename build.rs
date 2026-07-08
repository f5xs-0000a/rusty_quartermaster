//! Build-time asset compaction.
//!
//! The bare cache (`src/data/bare_cache.json`) is kept **prettified** in the
//! source tree so it diffs and edits cleanly. There's no point shipping all that
//! indentation and newlines in the binary, though — so at build time we parse it
//! (which also validates it: a malformed JSON fails the build) and re-emit a
//! minified copy into `OUT_DIR`. `src/bare.rs` embeds *that* compact copy via
//! `include_str!`, not the pretty source.

use std::path::Path;

fn main() {
    let src = Path::new("src/data/bare_cache.json");
    println!("cargo:rerun-if-changed={}", src.display());

    let pretty = std::fs::read_to_string(src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", src.display()));

    // Parse then re-serialize compact. Parsing doubles as validation, and going
    // through `Value` (rather than stripping whitespace textually) keeps string
    // contents — island names with spaces, apostrophes, accents — intact.
    let value: serde_json::Value = serde_json::from_str(&pretty)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", src.display()));
    let compact = serde_json::to_string(&value).expect("re-serializing bare cache to compact JSON");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("bare_cache.min.json");
    std::fs::write(&dest, compact)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", dest.display()));
}
