//! Library root for the quartermaster toolkit.
//!
//! The binary (`src/main.rs`) is a thin shim over this crate; keeping the
//! modules here lets auxiliary binaries (e.g. `src/bin/gallery.rs`) and
//! integration tests drive the same state and rendering code the app runs.

pub mod aliases;
pub mod api;
pub mod app;
pub mod bare;
pub mod cache;
pub mod chatlog;
pub mod clickmap;
pub mod commodities;
pub mod damage;
pub mod hold;
pub mod islands;
pub mod jobbers;
pub mod map;
pub mod ocean;
pub mod persistence;
pub mod pirate;
pub mod profits;
pub mod ratelimit;
pub mod ships;
pub mod startup;
pub mod utils;
pub mod voyage;
