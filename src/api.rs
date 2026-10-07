use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ocean::Ocean,
    ratelimit::{Service, throttled},
};

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Commodity {
    pub id: u64,
    #[serde(rename = "commodname")]
    pub name: String,
}

#[derive(Deserialize)]
pub struct BuySellResponse {
    #[allow(dead_code)]
    commodity: String,
    offers: Vec<RawOffer>,
}

#[derive(Deserialize)]
struct RawOfferIsland {
    islandname: String,
}

#[derive(Deserialize)]
struct RawOffer {
    stallname: String,
    island: RawOfferIsland,
    buyprice: u64,
    sellprice: u64,
    buyqty: u64,
    sellqty: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Offer {
    pub stallname: String,
    pub islandname: String,
    pub buyprice: u64,
    pub sellprice: u64,
    pub buyqty: u64,
    pub sellqty: u64,
}

impl From<RawOffer> for Offer {
    fn from(r: RawOffer) -> Self {
        Self {
            stallname: r.stallname,
            islandname: r.island.islandname,
            buyprice: r.buyprice,
            sellprice: r.sellprice,
            buyqty: r.buyqty,
            sellqty: r.sellqty,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct CachedOffers {
    pub offers: Vec<Offer>,
    /// When these offers were last fetched from the market.
    pub fetched_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Persistence types (shared across apps)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct SavedCommodity {
    pub id: u64,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Fetch logic
// ---------------------------------------------------------------------------

pub async fn fetch_commodities() -> Result<Vec<Commodity>, String> {
    let mut commodities: Vec<Commodity> = throttled(Service::Market, || {
        reqwest::get("https://api.plunderly.app/commods")
    })
    .await
    .map_err(|e| format!("failed to fetch commodities: {}", e))?
    .json()
    .await
    .map_err(|e| format!("failed to parse commodities: {}", e))?;
    // Canonical (in-game) order; anything not in our list sorts last.
    commodities.sort_by_key(|c| crate::commodities::sort_key(&c.name));
    Ok(commodities)
}

pub async fn fetch_offers_for(
    client: &reqwest::Client,
    names: &[String],
    ocean: Ocean,
) -> Result<HashMap<String, CachedOffers>, String> {
    let mut map = HashMap::new();
    for name in names {
        let mut url = reqwest::Url::parse(
            "https://api.plunderly.app/buysells/by-commodity",
        )
        .unwrap();
        url.query_pairs_mut()
            .append_pair("ocean", ocean.name())
            .append_pair("commodity", name);

        let resp = throttled(Service::Market, || {
            client.get(url).send()
        })
        .await
        .map_err(|e| format!("Fetch error: {}", e))?;

        let data: BuySellResponse = resp
            .json()
            .await
            .map_err(|e| format!("Parse error: {}", e))?;

        let offers = data.offers.into_iter().map(Offer::from).collect();
        map.insert(
            name.clone(),
            CachedOffers {
                offers,
                fetched_at: Utc::now(),
            },
        );
    }
    Ok(map)
}
