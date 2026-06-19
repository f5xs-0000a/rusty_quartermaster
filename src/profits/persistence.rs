use serde::{Deserialize, Serialize};

use crate::api::Commodity;
use super::InventoryRow;

#[derive(Serialize, Deserialize)]
pub struct SavedInventoryRow {
    pub commodity: String,
    pub restock: String,
    pub stock: String,
    pub booty: String,
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
        let pos = rows
            .binary_search_by_key(&id, |r: &InventoryRow| r.commod_id)
            .unwrap_err();
        rows.insert(
            pos,
            InventoryRow {
                commod_id: id,
                restock: saved_row.restock,
                stock: saved_row.stock,
                booty: saved_row.booty,
            },
        );
    }

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
            })
            .collect(),
        restocking_island: restocking_island.to_owned(),
        panel: panel.iter().map(|f| f.value.clone()).collect(),
    }
}
