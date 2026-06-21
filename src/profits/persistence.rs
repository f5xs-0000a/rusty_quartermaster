use serde::{Deserialize, Serialize};

use crate::api::Commodity;
use super::InventoryRow;

#[derive(Serialize, Deserialize)]
pub struct SavedInventoryRow {
    pub commodity: String,
    pub restock: String,
    pub stock: String,
    pub booty: String,
    /// Manually-entered prices (used when Market is unavailable). Defaulted
    /// for backward compatibility with caches written before they existed.
    #[serde(default)]
    pub sell: String,
    #[serde(default)]
    pub buy: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct SavedInventory {
    #[serde(default)]
    pub rows: Vec<SavedInventoryRow>,
    #[serde(default)]
    pub panel: Vec<String>,
    #[serde(default)]
    pub restocking_island: String,
}

pub struct LoadedInventory {
    pub rows: Vec<InventoryRow>,
    pub restocking_island: String,
    pub panel_values: Vec<String>,
}

/// Resolve a deserialized [`SavedInventory`] against the known commodity list,
/// dropping rows whose commodity name is unknown.
pub fn from_saved(inv: SavedInventory, commodities: &[Commodity]) -> LoadedInventory {
    let mut rows = Vec::new();
    for saved_row in inv.rows {
        let Some(c) = commodities
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&saved_row.commodity))
        else {
            eprintln!(
                "warning: unknown commodity '{}' in inventory, skipping",
                saved_row.commodity
            );
            continue;
        };
        let id = c.id;
        if rows.iter().any(|r: &InventoryRow| r.commod_id == id) {
            continue;
        }
        rows.push(InventoryRow {
            commod_id: id,
            restock: saved_row.restock,
            stock: saved_row.stock,
            booty: saved_row.booty,
            sell: saved_row.sell,
            buy: saved_row.buy,
        });
    }

    // Keep rows in canonical (in-game) commodity order.
    rows.sort_by_key(|r| crate::commodities::sort_key(crate::app::commod_name(commodities, r.commod_id)));

    LoadedInventory {
        rows,
        restocking_island: inv.restocking_island,
        panel_values: inv.panel,
    }
}

/// Build a savable [`SavedInventory`] snapshot from live app state.
pub fn to_saved(
    rows: &[InventoryRow],
    panel: &[crate::utils::PromptField],
    restocking_island: &str,
    commod_name: impl Fn(u64) -> String,
) -> SavedInventory {
    SavedInventory {
        rows: rows
            .iter()
            .map(|r| SavedInventoryRow {
                commodity: commod_name(r.commod_id),
                restock: r.restock.clone(),
                stock: r.stock.clone(),
                booty: r.booty.clone(),
                sell: r.sell.clone(),
                buy: r.buy.clone(),
            })
            .collect(),
        restocking_island: restocking_island.to_owned(),
        panel: panel.iter().map(|f| f.value.clone()).collect(),
    }
}
