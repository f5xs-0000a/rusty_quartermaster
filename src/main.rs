use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;

mod aliases;
mod api;
mod app;
mod cache;
mod chatlog;
mod clickmap;
mod damage;
mod jobbers;
mod ocean;
mod pirate;
mod profits;
mod ratelimit;
mod ships;
mod startup;
mod utils;

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
    long_about = "A terminal toolkit for Yohoho! Puzzle Pirates players.\n\n\
        Market prices are fetched from the Market API. Use --cache to \
        avoid re-fetching every run.",
)]
struct Args {
    /// Path to save/load the unified cache JSON.
    ///
    /// One file holds the inventory and commodity list (global) plus, per
    /// ocean, market prices and fetched pirate stats. Loaded on startup and
    /// saved on exit.
    #[arg(long, value_name = "PATH")]
    cache: Option<PathBuf>,

    /// Path to the Puzzle Pirates client chat log to monitor.
    ///
    /// When set, the existing log is read in full, then tailed live for new
    /// lines. When omitted, no chat-log monitoring happens.
    #[arg(long, value_name = "PATH")]
    chat_log: Option<PathBuf>,

    /// Your pirate name, used to attribute planks to you in the chat log.
    #[arg(long, value_name = "NAME")]
    user: Option<String>,

    /// Ocean (server) to use. Case-insensitive. One of the seven live oceans:
    /// Emerald, Meridian, Cerulean, Obsidian, Opal, Jade, Ice. Profit
    /// calculation needs a Market ocean (Emerald, Meridian, or Cerulean).
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
}

fn parse_ocean(s: &str) -> Result<Ocean, String> {
    s.parse()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    // -- Load the unified cache (inventory + commodities global; market +
    //    players per-ocean). Loaded before the setup popup so the popup can
    //    confirm an already-cached pirate without a yoweb round-trip. --
    let SavedCache {
        inventory: saved_inventory,
        commodities: saved_commodities,
        mut oceans,
    } = args.cache.as_deref().map(cache::load).unwrap_or_default();

    // -- Resolve ocean + pirate name (interactive popup if either is missing) --
    let http = reqwest::Client::new();
    let (ocean, user): (Option<Ocean>, Option<String>) =
        if args.ocean.is_none() || args.user.is_none() {
            startup::prompt(&http, args.ocean, args.user.clone(), &oceans).await?
        } else {
            (args.ocean, args.user.clone())
        };

    match ocean {
        None => eprintln!(
            "warning: no ocean selected — market prices and pirate stats are unavailable."
        ),
        Some(o) if !o.market_supported() => eprintln!(
            "note: {o} has no Market market data — profit calculation is disabled (inventory still works)."
        ),
        _ => {}
    }
    if user.is_none() {
        eprintln!("warning: no pirate name set — Jobbers pirate-stat lookups are limited.");
    }

    // The selected ocean's bucket. Other oceans' data stays in `oceans` and is
    // written back untouched on save.
    let this_ocean = ocean
        .and_then(|o| oceans.remove(o.name()))
        .unwrap_or_default();

    // Commodities are ocean-independent: reuse the cached list, or fetch it.
    let commodities: Vec<Commodity> = if saved_commodities.is_empty() {
        eprintln!("Fetching commodities from market...");
        api::fetch_commodities()
            .await
            .expect("failed to fetch commodities")
    } else {
        saved_commodities
            .into_iter()
            .map(|c| Commodity { id: c.id, name: c.name })
            .collect()
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
    shell.cached_offers = this_ocean.market;
    shell.ocean = ocean;
    // Pre-seed pirate stats from the cache. They're refreshed lazily: a cached
    // pirate is only re-queried once it's both relevant (seen in the log) and
    // past its staleness TTL, so startup never blocks on a refetch burst.
    shell.pirate_cache.fetched = this_ocean.players;

    // -- Load inventory --
    {
        let loaded =
            profits::persistence::from_saved(saved_inventory, &shell.commodities);
        shell.profits.rows = loaded.rows;
        shell.profits.panel[0].value = loaded.restocking_island.clone();
        shell.profits.panel[0].cursor = loaded.restocking_island.len();
        for (i, val) in loaded.panel_values.into_iter().enumerate() {
            if i + 1 < profits::PANEL_COUNT {
                shell.profits.panel[i + 1].value = val.clone();
                shell.profits.panel[i + 1].cursor = val.len();
            }
        }
    }

    // -- Auto-fetch missing market data (only on Market oceans) --
    if let Some(o) = ocean.filter(|o| o.market_supported()) {
        if !shell.profits.rows.is_empty() {
            let missing: Vec<String> = shell
                .profits
                .rows
                .iter()
                .map(|r| app::commod_name(&shell.commodities, r.commod_id).to_owned())
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
                        eprintln!("warning: failed to fetch missing market data: {}", e);
                    }
                }
            }
        }
    }

    shell.rebuild_island_list();

    // -- Chat log: read existing content, then tail live --
    let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    shell.chatlog.player_name = user.as_deref().map(Arc::from);
    if let Some(ref path) = args.chat_log {
        shell.chatlog.attached = true;
        let data = std::fs::read(path).unwrap_or_default();
        let offset = shell.chatlog.process_existing(&data);
        chatlog::spawn_tailer(path.clone(), offset, chat_tx);
    }

    // -- Background pirate-stat fetching (yoweb), deduped + throttled --
    const MAX_PIRATE_FETCHES: usize = 4;
    let basic_ttl = chrono::Duration::days(args.pirate_ttl_days.max(0));
    let trophy_ttl = chrono::Duration::days(args.trophy_ttl_days.max(0));
    let (pirate_tx, mut pirate_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, pirate::PirateUpdate)>();
    let pirate_client = reqwest::Client::new();

    // -- Terminal setup --
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<HashMap<String, CachedOffers>, String>>();

    // -- Event loop --
    loop {
        terminal.draw(|frame| shell.render(frame))?;

        if let Ok(result) = rx.try_recv() {
            shell.handle_fetch_result(result);
        }

        while let Ok(line) = chat_rx.try_recv() {
            shell.chatlog.process_line(&line);
        }

        // Absorb completed pirate fetches, folding each into the cache.
        while let Ok((key, update)) = pirate_rx.try_recv() {
            shell.pirate_cache.apply_update(key, update);
        }

        // Queue (re)fetches for relevant pirates: those seen in the log, plus
        // ourselves and anything explicitly forced. Each is fetched only when a
        // page is missing or past its TTL, bounded by the concurrency cap.
        // yoweb is per-ocean, so this only runs when an ocean is known.
        if let Some(pirate_ocean) = ocean.filter(|_| args.chat_log.is_some()) {
            let now = chrono::Utc::now();
            let mut names = shell.chatlog.all_pirate_names();
            if let Some(ref me) = shell.chatlog.player_name {
                names.insert(me.to_string());
            }
            names.extend(shell.pirate_cache.forced.iter().cloned());

            // Decide the worklist first (immutable borrows), then dispatch.
            let mut to_fetch: Vec<(String, pirate::FetchPlan)> = Vec::new();
            let mut slots =
                MAX_PIRATE_FETCHES.saturating_sub(shell.pirate_cache.in_flight.len());
            for name in names {
                if slots == 0 {
                    break;
                }
                let Ok(norm) = pirate::normalize_name(&name) else {
                    continue;
                };
                if shell.pirate_cache.in_flight.contains(&norm) {
                    continue;
                }
                // A confirmed not-found pirate is skipped unless forced.
                if shell.pirate_cache.gone.contains(&norm)
                    && !shell.pirate_cache.forced.contains(&norm)
                {
                    continue;
                }
                let Some(plan) =
                    shell.pirate_cache.fetch_plan(&norm, basic_ttl, trophy_ttl, now)
                else {
                    continue;
                };
                shell.pirate_cache.in_flight.insert(norm.clone());
                to_fetch.push((norm, plan));
                slots -= 1;
            }

            for (norm, plan) in to_fetch {
                let tx = pirate_tx.clone();
                let client = pirate_client.clone();
                tokio::spawn(async move {
                    let update =
                        pirate::fetch_pirate_update(&client, &norm, pirate_ocean, plan).await;
                    let _ = tx.send((norm, update));
                });
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
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;

    // -- Cleanup temp images --
    shell.damage.cleanup_temp_images();

    // -- Save the unified cache --
    if let Some(ref path) = args.cache {
        // Fold the current ocean's market + players back into the per-ocean map,
        // leaving other oceans' buckets intact.
        if let Some(o) = shell.ocean {
            oceans.insert(
                o.name().to_owned(),
                OceanCache {
                    market: shell.cached_offers,
                    players: shell.pirate_cache.fetched,
                },
            );
        }
        let saved = SavedCache {
            inventory: profits::persistence::to_saved(
                &shell.profits.rows,
                &shell.profits.panel[1..],
                &shell.profits.panel[0].value,
                |id| app::commod_name(&shell.commodities, id).to_owned(),
            ),
            commodities: shell
                .commodities
                .iter()
                .map(|c| SavedCommodity { id: c.id, name: c.name.clone() })
                .collect(),
            oceans,
        };
        cache::save(path, &saved);
    }

    Ok(())
}
