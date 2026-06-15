//! Per-service request throttling.
//!
//! We talk to two external hosts — puzzlepirates (yoweb pirate/trophy pages)
//! and market (commodity market data) — and don't want to hammer either.
//! Each service gets its own gate so requests to it are spaced at least one
//! interval apart. The interval is measured from the *start of the previous
//! response* to the *next request*: the moment a send resolves (headers in) we
//! stamp the clock, and the next request waits out the remainder before firing.
//!
//! The gates live in a single global `OnceLock` map keyed by `Service`, and
//! each gate's `Mutex` doubles as the serialization lock — only one request
//! per service is in flight (or waiting out the interval) at a time.

use std::collections::HashMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// External hosts we rate-limit independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// puzzlepirates yoweb (pirate stats, trophies).
    PuzzlePirates,
    /// market API (commodity market data).
    Market,
}

impl Service {
    /// Minimum spacing between requests to this service.
    fn interval(self) -> Duration {
        match self {
            Service::PuzzlePirates => Duration::from_secs(1),
            Service::Market => Duration::from_secs(1),
        }
    }
}

/// One service's gate: the lock serializes requests, and the inner value is the
/// instant the previous response started (`None` until the first request).
#[derive(Default)]
struct Gate {
    last_response_start: Mutex<Option<Instant>>,
}

fn gates() -> &'static HashMap<Service, Gate> {
    static GATES: OnceLock<HashMap<Service, Gate>> = OnceLock::new();
    GATES.get_or_init(|| {
        let mut map = HashMap::new();
        map.insert(Service::PuzzlePirates, Gate::default());
        map.insert(Service::Market, Gate::default());
        map
    })
}

/// Run a single request to `service`, pacing it behind the previous one.
///
/// Acquires the service's gate (serializing concurrent callers), sleeps for any
/// remaining time since the last response started, awaits `f` (which should
/// perform the send), then stamps the clock at the moment `f` resolves.
pub async fn throttled<F, Fut, T>(service: Service, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let gate = gates().get(&service).expect("gate registered for every service");
    let mut last = gate.last_response_start.lock().await;

    if let Some(prev) = *last {
        let remaining = service.interval().saturating_sub(prev.elapsed());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining).await;
        }
    }

    let out = f().await;
    *last = Some(Instant::now());
    out
}
