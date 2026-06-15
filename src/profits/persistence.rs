use std::path::Path;

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

#[derive(Serialize, Deserialize)]
pub struct SavedInventory {
    pub rows: Vec<SavedInventoryRow>,
    pub panel: Vec<String>,
    #[serde(default)]
    pub restocking_island: String,
}

pub struct LoadedInventory {
    pub rows: Vec<InventoryRow>,
    pub restocking_island: String,
    pub panel_values: Vec<String>,
}

pub fn load_inventory(path: &Path, commodities: &[Commodity]) -> Option<LoadedInventory> {
    let data = std::fs::read_to_string(path).ok()?;
    let inv: SavedInventory = match serde_json::from_str(&data) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("warning: failed to parse inventory: {}", e);
            return None;
        }
    };

    eprintln!("Loaded inventory from {}", path.display());
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

    Some(LoadedInventory {
        rows,
        restocking_island: inv.restocking_island,
        panel_values: inv.panel,
    })
}

pub fn save_inventory(
    path: &Path,
    rows: &[InventoryRow],
    panel: &[crate::utils::PromptField],
    restocking_island: &str,
    commod_name: impl Fn(u64) -> String,
) {
    let saved = SavedInventory {
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
    };
    let json = match serde_json::to_string_pretty(&saved) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("error: failed to serialize inventory: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("error: failed to write inventory to {}: {}", path.display(), e);
    } else {
        eprintln!("Saved inventory to {}", path.display());
    }
}
