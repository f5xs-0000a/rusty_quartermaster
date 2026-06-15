use std::collections::HashMap;
use std::io;
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
mod chatlog;
mod clickmap;
mod damage;
mod jobbers;
mod pirate;
mod profits;
mod ships;
mod utils;

use api::{CachedOffers, Commodity, SavedCommodity, SavedMarketCache};
use app::AppShell;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    about = "A terminal toolkit for Yohoho! Puzzle Pirates players.",
    long_about = "A terminal toolkit for Yohoho! Puzzle Pirates players.\n\n\
        Market prices are fetched from the Market API. Use --market-cache to \
        avoid re-fetching every run.",
)]
struct Args {
    /// Path to save/load inventory JSON.
    ///
    /// Commodity quantities, panel settings, and restocking island are
    /// loaded on startup and saved on exit.
    #[arg(long, value_name = "PATH")]
    inventory: Option<String>,

    /// Path to save/load market cache JSON.
    ///
    /// Caches commodity list and pricing data from Market so
    /// subsequent runs don't need an internet connection.
    #[arg(long, value_name = "PATH")]
    market_cache: Option<String>,

    /// Path to the Puzzle Pirates client chat log to monitor.
    ///
    /// When set, the existing log is read in full, then tailed live for new
    /// lines. When omitted, no chat-log monitoring happens.
    #[arg(long, value_name = "PATH")]
    chat_log: Option<String>,

    /// Your pirate name, used to attribute planks to you in the chat log.
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    // -- Load market cache or fetch commodities from API --
    let mut cached_offers: HashMap<String, CachedOffers> = HashMap::new();

    let commodities: Vec<Commodity> = if let Some(ref path) = args.market_cache {
        load_market_cache(path, &mut cached_offers).await
    } else {
        eprintln!("Fetching commodities from market...");
        api::fetch_commodities()
            .await
            .expect("failed to fetch commodities")
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
    shell.cached_offers = cached_offers;

    // -- Load inventory --
    if let Some(ref path) = args.inventory {
        if let Some(loaded) =
            profits::persistence::load_inventory(path, &shell.commodities)
        {
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
    }

    // -- Auto-fetch missing market data --
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
            let client = reqwest::Client::new();
            match api::fetch_offers_for(&client, &missing).await {
                Ok(new_offers) => {
                    shell.cached_offers.extend(new_offers);
                }
                Err(e) => {
                    eprintln!("warning: failed to fetch missing market data: {}", e);
                }
            }
        }
    }

    shell.rebuild_island_list();

    // -- Chat log: read existing content, then tail live --
    let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    shell.chatlog.player_name = args.user.as_deref().map(Arc::from);
    if let Some(ref path) = args.chat_log {
        shell.chatlog.attached = true;
        let data = std::fs::read(path).unwrap_or_default();
        let offset = shell.chatlog.process_existing(&data);
        chatlog::spawn_tailer(path.clone(), offset, chat_tx);
    }

    // -- Background pirate-stat fetching (yoweb), deduped + throttled --
    const MAX_PIRATE_FETCHES: usize = 4;
    let (pirate_tx, mut pirate_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<pirate::Pirate, String>)>();
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

        // Absorb completed pirate fetches.
        while let Ok((key, result)) = pirate_rx.try_recv() {
            shell.pirate_cache.in_flight = shell.pirate_cache.in_flight.saturating_sub(1);
            if let Ok(pirate) = result {
                shell.pirate_cache.fetched.insert(key, pirate);
            }
            // On error we keep `key` in `requested` so we don't retry-storm.
        }

        // Queue fetches for any newly-seen pirates (plus ourselves), throttled.
        if args.chat_log.is_some() {
            let mut names = shell.chatlog.all_pirate_names();
            if let Some(ref me) = shell.chatlog.player_name {
                names.insert(me.to_string());
            }
            for name in names {
                if MAX_PIRATE_FETCHES <= shell.pirate_cache.in_flight {
                    break;
                }
                let Ok(norm) = pirate::normalize_name(&name) else {
                    continue;
                };
                if shell.pirate_cache.fetched.contains_key(&norm)
                    || shell.pirate_cache.requested.contains(&norm)
                {
                    continue;
                }
                shell.pirate_cache.requested.insert(norm.clone());
                shell.pirate_cache.in_flight += 1;
                let tx = pirate_tx.clone();
                let client = pirate_client.clone();
                tokio::spawn(async move {
                    let result = pirate::fetch_pirate(&client, &norm).await;
                    let _ = tx.send((norm, result));
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

    // -- Save inventory --
    if let Some(ref path) = args.inventory {
        profits::persistence::save_inventory(
            path,
            &shell.profits.rows,
            &shell.profits.panel[1..],
            &shell.profits.panel[0].value,
            |id| app::commod_name(&shell.commodities, id).to_owned(),
        );
    }

    // -- Save market cache --
    if let Some(ref path) = args.market_cache {
        save_market_cache(path, &shell.commodities, shell.cached_offers);
    }

    Ok(())
}

async fn load_market_cache(
    path: &str,
    cached_offers: &mut HashMap<String, CachedOffers>,
) -> Vec<Commodity> {
    let Ok(data) = std::fs::read_to_string(path) else {
        eprintln!("Fetching commodities from market...");
        return api::fetch_commodities()
            .await
            .expect("failed to fetch commodities");
    };

    match serde_json::from_str::<SavedMarketCache>(&data) {
        Ok(cache) => {
            eprintln!("Loaded market cache from {}", path);
            *cached_offers = cache.offers;
            cache
                .commodities
                .into_iter()
                .map(|c| Commodity { id: c.id, name: c.name })
                .collect()
        }
        Err(e) => {
            eprintln!("warning: failed to parse market cache: {}", e);
            eprintln!("Fetching commodities from market...");
            api::fetch_commodities()
                .await
                .expect("failed to fetch commodities")
        }
    }
}

fn save_market_cache(
    path: &str,
    commodities: &[Commodity],
    cached_offers: HashMap<String, CachedOffers>,
) {
    let saved = SavedMarketCache {
        commodities: commodities
            .iter()
            .map(|c| SavedCommodity {
                id: c.id,
                name: c.name.clone(),
            })
            .collect(),
        offers: cached_offers,
    };
    let json = match serde_json::to_string_pretty(&saved) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("error: failed to serialize market cache: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("error: failed to write market cache to {}: {}", path, e);
    } else {
        eprintln!("Saved market cache to {}", path);
    }
}
