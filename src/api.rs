use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ocean::Ocean;
use crate::ratelimit::{throttled, Service};

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
    pub fetched_at: u64,
}

// ---------------------------------------------------------------------------
// Persistence types (shared across apps)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct SavedCommodity {
    pub id: u64,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct SavedMarketCache {
    /// Ocean whose prices the `offers` were fetched from. Older caches predate
    /// this field, so it defaults to `None` (treated as "unknown ocean").
    #[serde(default)]
    pub ocean: Option<String>,
    pub commodities: Vec<SavedCommodity>,
    pub offers: HashMap<String, CachedOffers>,
}

impl SavedMarketCache {
    /// Split a loaded cache into `(commodities, offers)` for `ocean`.
    ///
    /// The commodity list comes from Market's ocean-independent `/commods`
    /// endpoint, so it is always reusable. Offers (prices) are per-ocean: they
    /// are kept only when the cache was built for the same ocean, and dropped
    /// (returned empty, forcing a refetch) otherwise — including when the
    /// current ocean is unknown.
    pub fn into_parts(
        self,
        ocean: Option<Ocean>,
    ) -> (Vec<Commodity>, HashMap<String, CachedOffers>) {
        let commodities = self
            .commodities
            .into_iter()
            .map(|c| Commodity { id: c.id, name: c.name })
            .collect();
        let offers = match ocean {
            Some(o) if self.ocean.as_deref() == Some(o.name()) => self.offers,
            _ => HashMap::new(),
        };
        (commodities, offers)
    }
}

// ---------------------------------------------------------------------------
// Fetch logic
// ---------------------------------------------------------------------------

pub async fn fetch_commodities() -> Result<Vec<Commodity>, String> {
    let mut commodities: Vec<Commodity> =
        throttled(Service::Market, || reqwest::get("https://api.plunderly.app/commods"))
            .await
            .map_err(|e| format!("failed to fetch commodities: {}", e))?
            .json()
            .await
            .map_err(|e| format!("failed to parse commodities: {}", e))?;
    commodities.sort_by_key(|c| c.id);
    Ok(commodities)
}

pub async fn fetch_offers_for(
    client: &reqwest::Client,
    names: &[String],
    ocean: Ocean,
) -> Result<HashMap<String, CachedOffers>, String> {
    let mut map = HashMap::new();
    for name in names {
        let mut url =
            reqwest::Url::parse("https://api.plunderly.app/buysells/by-commodity").unwrap();
        url.query_pairs_mut()
            .append_pair("ocean", ocean.name())
            .append_pair("commodity", name);

        let resp = throttled(Service::Market, || client.get(url).send())
            .await
            .map_err(|e| format!("Fetch error: {}", e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let data: BuySellResponse = resp
            .json()
            .await
            .map_err(|e| format!("Parse error: {}", e))?;

        let offers = data.offers.into_iter().map(Offer::from).collect();
        map.insert(name.clone(), CachedOffers { offers, fetched_at: now });
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cache(ocean: Option<&str>) -> SavedMarketCache {
        let mut offers = HashMap::new();
        offers.insert(
            "Swill".to_owned(),
            CachedOffers { offers: Vec::new(), fetched_at: 0 },
        );
        SavedMarketCache {
            ocean: ocean.map(str::to_owned),
            commodities: vec![SavedCommodity { id: 1, name: "Swill".to_owned() }],
            offers,
        }
    }

    #[test]
    fn matching_ocean_keeps_offers() {
        let (commods, offers) = sample_cache(Some("Cerulean")).into_parts(Some(Ocean::Cerulean));
        assert_eq!(commods.len(), 1);
        assert_eq!(offers.len(), 1);
    }

    #[test]
    fn mismatched_ocean_drops_offers_keeps_commodities() {
        let (commods, offers) = sample_cache(Some("Emerald")).into_parts(Some(Ocean::Cerulean));
        assert_eq!(commods.len(), 1, "commodities are ocean-independent");
        assert!(offers.is_empty(), "offers from another ocean must be dropped");
    }

    #[test]
    fn unknown_ocean_drops_offers() {
        // Legacy cache with no ocean recorded, or no ocean selected this run.
        let (_, offers) = sample_cache(None).into_parts(Some(Ocean::Emerald));
        assert!(offers.is_empty());
        let (_, offers) = sample_cache(Some("Emerald")).into_parts(None);
        assert!(offers.is_empty());
    }
}
