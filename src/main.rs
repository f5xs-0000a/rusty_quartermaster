use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use crossterm::{
    event::{
        self,
        DisableMouseCapture,
        EnableMouseCapture,
        Event,
        KeyEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen,
        LeaveAlternateScreen,
        disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::prelude::*;

mod aliases;
mod api;
mod app;
mod bare;
mod cache;
mod chatlog;
mod clickmap;
mod commodities;
mod damage;
mod hold;
mod jobbers;
mod map;
mod ocean;
mod pirate;
mod profits;
mod ratelimit;
mod ships;
mod startup;
mod utils;
mod voyage;

use api::{CachedOffers, Commodity, SavedCommodity};
use app::AppShell;
use cache::{OceanCache, SavedCache};
use ocean::Ocean;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    about = "A terminal toolkit for Yohoho! Puzzle Pirates players.",
    long_about = "A terminal toolkit for Yohoho! Puzzle Pirates \
                  players.\n\nUse --cache to save your data between runs."
)]
struct Args {
    /// Path to save/load the unified cache JSON.
    ///
    /// One file holds the inventory and commodity list (global) plus, per
    /// ocean, fetched pirate stats. Loaded on startup and saved on exit.
    /// Defaults to `ypp_cache.json` next to the executable.
    #[arg(long, value_name = "PATH")]
    cache: Option<PathBuf>,

    /// Path to save/load the voyage-history JSON.
    ///
    /// Holds completed voyages (per-human-behind-keyboard, across all their
    /// pirates). Loaded on startup, appended on save. Defaults to
    /// `ypp_voyages.json` next to the executable.
    #[arg(long, value_name = "PATH")]
    voyages: Option<PathBuf>,

    /// Path to the Puzzle Pirates client chat log to monitor (optional).
    ///
    /// When given, the existing log is read in full, then tailed live for new
    /// lines. When omitted, no chat-log monitoring happens.
    #[arg(value_name = "CHAT_LOG")]
    chat_log: Option<PathBuf>,

    /// Your pirate name, used to attribute planks to you in the chat log.
    #[arg(long, value_name = "NAME")]
    user: Option<String>,

    /// Ocean (server) to use. Case-insensitive. One of the seven live oceans:
    /// Emerald, Meridian, Cerulean, Obsidian, Opal, Jade, Ice.
    #[arg(long, value_name = "OCEAN", value_parser = parse_ocean)]
    ocean: Option<Ocean>,

    /// Days before a cached pirate's basic profile is treated as stale and
    /// re-queried in the background.
    #[arg(long, value_name = "DAYS", default_value_t = 3)]
    pirate_ttl_days: i64,

    /// Days before a cached pirate's trophies are treated as stale and
    /// re-queried in the background.
    #[arg(long, value_name = "DAYS", default_value_t = 7)]
    trophy_ttl_days: i64,

    /// Whether to query the Market API at all. Combined with the ocean's
    /// Market support: if either is false, we never hit Market. Hidden;
    /// off by default — pass `--query-market` to enable Market traffic
    /// (market prices and commodity list).
    #[arg(long, hide = true)]
    query_market: bool,

    /// Reveal the "Crew Donation Share Rate" row in Profits and apply it.
    ///
    /// When set, a parameter row appears for the share of voyage earnings
    /// donated to your crew, and that donation is deducted in the breakdown.
    /// Off by default (no donation row, no donation deducted).
    #[arg(long)]
    donate_to_crew: bool,

    /// Reveal the "C.O. Rate" (commanding officer cut) row in Profits and
    /// apply it. Hidden; off by default (no C.O. row, no C.O. cut
    /// deducted).
    #[arg(long, hide = true)]
    pay_commanding_officer: bool,

    /// Minimum seconds between requests to the Market API. Hidden.
    #[arg(long, value_name = "SECONDS", default_value_t = 1, hide = true)]
    market_query_rate: u64,

    /// Minimum seconds between requests to puzzlepirates (yoweb pirate stats
    /// and trophies). Higher is gentler on yoweb.
    #[arg(long, value_name = "SECONDS", default_value_t = 60)]
    ypp_query_rate: u64,

    /// Watch the clipboard for a copied hold and offer it to Profits.
    ///
    /// When set, the clipboard is checked about once a second; when the
    /// game's hold JSON appears on it, Profits asks before filling the Stock
    /// column from it. Off by default: the clipboard is never read.
    #[arg(long)]
    clipboard: bool,
}

fn parse_ocean(s: &str) -> Result<Ocean, String> {
    s.parse()
}

/// Pull the pirate name and ocean out of a Puzzle Pirates chat-log filename.
///
/// Client logs are named `<PirateName>_<ocean>_ypp…`, e.g. `Playerone_emerald…` or
/// `Mateone-East_emerald…`; the pirate name is a single underscore-delimited
/// field, so a hyphen inside it stays intact. Only the first two fields are
/// consulted, and a value is returned solely when the second field names a live
/// ocean — the parse fails closed for paths that don't follow the convention.
fn chat_log_identity(path: Option<&Path>) -> (Option<Ocean>, Option<String>) {
    let Some(stem) = path.and_then(|p| p.file_name()).and_then(|n| n.to_str())
    else {
        return (None, None);
    };
    let mut fields = stem.split('_');
    let (Some(name), Some(ocean_field)) = (fields.next(), fields.next()) else {
        return (None, None);
    };
    match ocean_field.parse::<Ocean>() {
        Ok(ocean) if !name.is_empty() => (Some(ocean), Some(name.to_owned())),
        _ => (None, None),
    }
}

/// A path sitting next to the running executable (e.g. `cache.json` beside the
/// binary). The default location for the cache and voyage-history files when no
/// explicit `--cache` / `--voyages` path is given. `None` only if the
/// executable's location can't be determined.
fn exe_adjacent(name: &str) -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    // Resolve persistence paths: an explicit flag wins, otherwise default to a
    // file sitting next to the executable.
    let cache_path = args
        .cache
        .clone()
        .or_else(|| exe_adjacent("ypp_cache.json"));
    let voyages_path = args
        .voyages
        .clone()
        .or_else(|| exe_adjacent("ypp_voyages.json"));

    // Set per-service request spacing before any network call goes out.
    ratelimit::configure(
        args.market_query_rate,
        args.ypp_query_rate,
    );

    // -- Load the unified cache (inventory + commodities global; market +
    //    players per-ocean). Loaded before the setup popup so the popup can
    //    confirm an already-cached pirate without a yoweb round-trip. --
    let SavedCache {
        inventory: saved_inventory,
        commodities: saved_commodities,
        mut oceans,
        name_segments: saved_name_segments,
    } = cache_path
        .as_deref()
        .map(cache::load)
        .unwrap_or_else(cache::SavedCache::seeded);

    // -- Resolve ocean + pirate name (interactive popup if either is missing)
    // --
    // The chat-log filename encodes the pirate name and ocean. Explicit
    // --ocean / --user always win; the filename only fills in whichever the
    // flags leave unset, and its values pre-fill the startup fields rather than
    // skipping the popup.
    let (log_ocean, log_user) = chat_log_identity(args.chat_log.as_deref());
    let start_ocean = args.ocean.or(log_ocean);
    let start_user = args.user.clone().or(log_user);

    let http = reqwest::Client::new();
    let resolved: Option<(
        Option<Ocean>,
        Option<String>,
        Option<pirate::PirateUpdate>,
    )> = if args.ocean.is_none() || args.user.is_none() {
        startup::prompt(
            &http,
            start_ocean,
            start_user,
            &oceans,
            args.query_market,
        )
        .await?
    } else {
        Some((args.ocean, args.user.clone(), None))
    };
    // `None` means the user pressed Esc at setup to quit.
    let Some((ocean, user, self_update)) = resolved else {
        return Ok(());
    };

    // Market fetching only happens on the hidden --query-market path, so the
    // market-related notices are gated behind it; a default run stays quiet
    // about market data entirely.
    if ocean.is_none() {
        if args.query_market {
            eprintln!(
                "warning: no ocean selected — market prices and pirate stats \
                 are unavailable."
            );
        } else {
            eprintln!(
                "warning: no ocean selected — pirate stats are unavailable."
            );
        }
    } else if args.query_market
        && let Some(o) = ocean.filter(|o| !o.market_supported())
    {
        eprintln!(
            "note: {o} has no market data — profit calculation is disabled \
             (inventory still works)."
        );
    }
    if user.is_none() {
        eprintln!(
            "warning: no pirate name set — Jobbers pirate-stat lookups are \
             limited."
        );
    }

    // The selected ocean's bucket. Other oceans' data stays in `oceans` and is
    // written back untouched on save.
    let this_ocean = ocean
        .and_then(|o| oceans.remove(o.name()))
        .unwrap_or_default();

    // Commodities are ocean-independent: reuse the cached list, or fetch it
    // (only if Market querying is enabled).
    let commodities: Vec<Commodity> = if !saved_commodities.is_empty() {
        saved_commodities
            .into_iter()
            .map(|c| {
                Commodity {
                    id: c.id,
                    name: c.name,
                }
            })
            .collect()
    } else if args.query_market {
        eprintln!("Fetching commodities...");
        api::fetch_commodities()
            .await
            .expect("failed to fetch commodities")
    } else {
        eprintln!("note: no cached commodities — commodity list is empty.");
        Vec::new()
    };

    // Validate alias targets against commodity list.
    for (&alias, &target) in aliases::get().iter() {
        if !commodities
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(target))
        {
            eprintln!(
                "warning: alias '{}' targets unknown commodity '{}'",
                alias, target
            );
        }
    }

    let mut shell = AppShell::new(commodities);
    // Seed the learned NPC naming vocabulary (swabbie vs mercenary) from the
    // cache; it grows further as brigand-victory rosters are parsed this
    // session.
    shell.chatlog.name_segments = saved_name_segments;
    shell.cached_offers = this_ocean.market;
    shell.map.memorized = this_ocean.memorized;
    shell.ocean = ocean;
    shell.query_market = args.query_market;
    // Voyage history (per-human-behind-keyboard). Load it now so it's available
    // across sessions; new runs are appended when the user confirms the save
    // prompt.
    shell.voyages_path = voyages_path;
    if let Some(path) = &shell.voyages_path {
        shell.voyage_history = voyage::persistence::load(path);
    }
    shell.profits.show_co_rate = args.pay_commanding_officer;
    shell.profits.show_donation = args.donate_to_crew;
    // Pre-seed pirate stats from the cache. They're refreshed lazily: a cached
    // pirate is only re-queried once it's both relevant (seen in the log) and
    // past its staleness TTL, so startup never blocks on a refetch burst.
    shell.pirate_cache.fetched = this_ocean.players;
    // Fold in our own pirate if the setup popup just verified (and thus
    // fetched) it — otherwise the verification fetch would be thrown away
    // and re-queried every run. `apply_update` builds the cache entry,
    // leaving trophies stale for the lazy background fetcher.
    if let (Some(update), Some(name)) = (self_update, user.as_deref())
        && let Ok(norm) = pirate::normalize_name(name)
    {
        shell.pirate_cache.apply_update(norm, update);
    }

    // -- Load inventory --
    {
        let loaded = profits::persistence::from_saved(
            saved_inventory,
            &shell.commodities,
        );
        shell.profits.rows = loaded.rows;
        shell.profits.panel[0].value = loaded.restocking_island.clone();
        shell.profits.panel[0].cursor = loaded.restocking_island.len();
        shell.profits.panel[1].value = loaded.selling_island.clone();
        shell.profits.panel[1].cursor = loaded.selling_island.len();
        // The saved `panel` vec is everything after the two Place fields, so it
        // lands at panel[2..]. Old caches (no Selling Place) slot in
        // identically.
        for (i, val) in loaded.panel_values.into_iter().enumerate() {
            if i + 2 < profits::PANEL_COUNT {
                shell.profits.panel[i + 2].value = val.clone();
                shell.profits.panel[i + 2].cursor = val.len();
            }
        }
    }

    // -- Auto-fetch missing market data (only on Market oceans, and only
    //    when Market querying is enabled) --
    if let Some(o) =
        ocean.filter(|o| o.market_supported() && args.query_market)
        && !shell.profits.rows.is_empty()
    {
        let missing: Vec<String> = shell
            .profits
            .rows
            .iter()
            .map(|r| {
                app::commod_name(&shell.commodities, r.commod_id).to_owned()
            })
            .filter(|name| !shell.cached_offers.contains_key(name.as_str()))
            .collect();

        if !missing.is_empty() {
            eprintln!(
                "Fetching market data for {} missing commodities...",
                missing.len()
            );
            match api::fetch_offers_for(&http, &missing, o).await {
                Ok(new_offers) => {
                    shell.cached_offers.extend(new_offers);
                }
                Err(e) => {
                    eprintln!(
                        "warning: failed to fetch missing market data: {}",
                        e
                    );
                }
            }
        }
    }

    shell.rebuild_island_list();

    // -- Chat log: read existing content, then tail live --
    let (chat_tx, mut chat_rx) =
        tokio::sync::mpsc::unbounded_channel::<String>();
    shell.chatlog.player_name = user.as_deref().map(Arc::from);
    if let Some(ref path) = args.chat_log {
        shell.chatlog.attached = true;
        let data = std::fs::read(path).unwrap_or_default();
        let offset = shell.chatlog.process_existing(&data);
        chatlog::spawn_tailer(path.clone(), offset, chat_tx);
    }

    // -- Clipboard: offer a copied hold to the Profits page (opt-in) --
    let (hold_tx, mut hold_rx) =
        tokio::sync::mpsc::unbounded_channel::<hold::HoldContents>();
    if args.clipboard {
        hold::spawn_watcher(hold_tx);
    }

    // -- Background pirate-stat fetching (yoweb) --
    // A single greedy worker: at most one page in flight, chosen by priority
    // (on-demand ▸ aboard/self ▸ planked) and recomputed every tick from live
    // state, so it cancels/repriorities as the crew changes.
    let basic_ttl = chrono::Duration::days(args.pirate_ttl_days.max(0));
    let trophy_ttl = chrono::Duration::days(args.trophy_ttl_days.max(0));
    let (pirate_tx, mut pirate_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, pirate::PirateUpdate)>(
        );
    let pirate_client = reqwest::Client::new();
    // The one in-flight page fetch, if any: (normalized name, page, abort
    // handle).
    let mut current_fetch: Option<(
        String,
        jobbers::PiratePage,
        tokio::task::JoinHandle<()>,
    )> = None;

    // -- Terminal setup --
    // Once the alternate screen is up, stderr still points at this terminal, so
    // any stray `eprintln!` (notably the best-effort save messages) paints
    // over the frame and garbles the render. Redirect diagnostics to a log
    // file for the TUI's lifetime; startup progress above this point still
    // goes to stderr.
    if let Some(log) = exe_adjacent("ypp_quartermaster.log") {
        utils::init_diag_log(&log);
    }
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<
        Result<HashMap<String, CachedOffers>, String>,
    >();

    // -- Event loop --
    loop {
        terminal.draw(|frame| shell.render(frame))?;

        if let Ok(result) = rx.try_recv() {
            shell.handle_fetch_result(result);
        }

        while let Ok(line) = chat_rx.try_recv() {
            shell.feed_chat_line(&line);
        }

        while let Ok(hold) = hold_rx.try_recv() {
            shell.queue_hold_import(&hold);
        }
        shell.surface_hold_import();

        // Absorb completed pirate fetches, folding each into the cache. Clear
        // the in-flight slot when its own result lands (a stale result
        // from an aborted fetch for a different name still gets applied
        // — it's real data — but won't disturb the current slot).
        while let Ok((norm, update)) = pirate_rx.try_recv() {
            if current_fetch.as_ref().is_some_and(|(n, ..)| n == &norm) {
                current_fetch = None;
            }
            shell.pirate_cache.apply_update(norm, update);
        }

        // Drive the single fetch worker. yoweb is per-ocean, so this only runs
        // when an ocean is known and a chat log is attached.
        if let Some(pirate_ocean) = ocean.filter(|_| args.chat_log.is_some()) {
            let now = chrono::Utc::now();

            // Normalized priority sets for the *selected* vessel: aboard (+
            // self) and planked. Pirates elsewhere aren't
            // background-fetched.
            let mut aboard: HashSet<String> = HashSet::new();
            let mut planked: HashSet<String> = HashSet::new();
            if let Some(key) = shell.jobbers_ui.selected.clone() {
                for name in shell.chatlog.aboard(&key) {
                    if let Ok(n) = pirate::normalize_name(&name) {
                        aboard.insert(n);
                    }
                }
                if let Some(v) = shell.chatlog.vessels.get(&key) {
                    for name in &v.planked_by_us {
                        if let Ok(n) = pirate::normalize_name(name) {
                            planked.insert(n);
                        }
                    }
                }
            }
            if let Some(me) = shell.chatlog.player_name.as_deref()
                && let Ok(n) = pirate::normalize_name(me)
            {
                aboard.insert(n);
            }

            let order = shell.pirate_cache.next_order(
                &aboard, &planked, basic_ttl, trophy_ttl, now,
            );

            // Replace the in-flight fetch when it's gone irrelevant, or when a
            // strictly higher-priority page is now wanted; otherwise let it
            // run.
            let dispatch = match &current_fetch {
                None => order.is_some(),
                Some((cn, ..)) => {
                    match shell.pirate_cache.tier_of(cn, &aboard, &planked) {
                        None => true,
                        Some(cur_tier) => {
                            order.as_ref().is_some_and(|o| o.tier < cur_tier)
                        }
                    }
                }
            };

            if dispatch {
                if let Some((_, _, handle)) = current_fetch.take() {
                    handle.abort(); // frees the throttle gate for the urgent page
                }
                if let Some(o) = order {
                    let plan = match o.page {
                        jobbers::PiratePage::Basic => {
                            pirate::FetchPlan {
                                basic: true,
                                trophies: false,
                            }
                        }
                        jobbers::PiratePage::Trophies => {
                            pirate::FetchPlan {
                                basic: false,
                                trophies: true,
                            }
                        }
                    };
                    let tx = pirate_tx.clone();
                    let client = pirate_client.clone();
                    let norm = o.norm.clone();
                    let handle = tokio::spawn(async move {
                        let update = pirate::fetch_pirate_update(
                            &client,
                            &norm,
                            pirate_ocean,
                            plan,
                        )
                        .await;
                        let _ = tx.send((norm, update));
                    });
                    current_fetch = Some((o.norm, o.page, handle));
                }
            }
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if shell.handle_key(key, &tx) {
                    break;
                }
            }
            Event::Mouse(mouse) => {
                shell.handle_mouse(mouse, &tx);
            }
            _ => {}
        }
    }

    // -- Teardown --
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;

    // -- Save the unified cache --
    if let Some(ref path) = cache_path {
        // Fold the current ocean's market + players back into the per-ocean
        // map, leaving other oceans' buckets intact.
        if let Some(o) = shell.ocean {
            oceans.insert(
                o.name().to_owned(),
                OceanCache {
                    market: shell.cached_offers,
                    players: shell.pirate_cache.fetched,
                    memorized: shell.map.memorized,
                },
            );
        }
        let saved = SavedCache {
            inventory: profits::persistence::to_saved(
                &shell.profits.rows,
                &shell.profits.panel[2 ..],
                &shell.profits.panel[0].value,
                &shell.profits.panel[1].value,
                |id| app::commod_name(&shell.commodities, id).to_owned(),
            ),
            commodities: shell
                .commodities
                .iter()
                .map(|c| {
                    SavedCommodity {
                        id: c.id,
                        name: c.name.clone(),
                    }
                })
                .collect(),
            oceans,
            name_segments: std::mem::take(&mut shell.chatlog.name_segments),
        };
        crate::utils::write_json_atomic(path, &saved, "cache");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_log_identity_reads_name_and_ocean() {
        let (ocean, user) = chat_log_identity(Some(Path::new(
            "Somepirate_emerald_ypp_log.txt",
        )));
        assert_eq!(user.as_deref(), Some("Somepirate"));
        assert_eq!(ocean, Some(Ocean::Emerald));
    }

    #[test]
    fn chat_log_identity_keeps_hyphenated_name() {
        let (ocean, user) = chat_log_identity(Some(Path::new(
            "Somepirate-East_emerald_ypp.log.txt",
        )));
        assert_eq!(user.as_deref(), Some("Somepirate-East"));
        assert_eq!(ocean, Some(Ocean::Emerald));
    }

    #[test]
    fn chat_log_identity_ignores_a_full_directory_path() {
        let (ocean, user) = chat_log_identity(Some(Path::new(
            "/home/someone/logs/Somepirate_meridian_ypp_log.txt",
        )));
        assert_eq!(user.as_deref(), Some("Somepirate"));
        assert_eq!(ocean, Some(Ocean::Meridian));
    }

    #[test]
    fn chat_log_identity_fails_closed_on_unknown_ocean() {
        assert_eq!(
            chat_log_identity(Some(Path::new(
                "Somepirate_atlantis_ypp.txt"
            ))),
            (None, None),
        );
        assert_eq!(
            chat_log_identity(Some(Path::new("plain_notes.txt"))),
            (None, None),
        );
        assert_eq!(chat_log_identity(None), (None, None));
    }
}
