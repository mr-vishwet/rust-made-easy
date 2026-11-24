//! The Electron → Tauri API mapping table.
//!
//! The table lives in `converters/electron/mappings/electron-to-tauri.json`
//! (the converter's compatibility database, the single source of truth)
//! (mirrored at the repo root for humans) and is embedded into the binary
//! with [`include_str!`], so the CLI works with no runtime files next to it.
//!
//! Each entry maps one Electron API to its Tauri target and rates the
//! conversion: `"auto"`, `"assisted"` or `"manual"`.

use serde::Deserialize;

/// One row of the Electron → Tauri mapping table.
#[derive(Debug, Clone, Deserialize)]
pub struct Mapping {
    /// Electron API name, e.g. `"ipcMain.handle"`.
    pub electron_api: String,
    /// The Tauri equivalent, e.g. `"#[tauri::command]"`.
    /// `None` when no Tauri equivalent exists at all (manual ports).
    pub tauri_target: Option<String>,
    /// Tauri plugin crate that provides the target, if any.
    pub plugin: Option<String>,
    /// `"auto"`, `"assisted"` or `"manual"`.
    pub status: String,
    /// One honest line about what the conversion involves.
    pub notes: String,
}

/// The embedded mapping table, parsed verbatim from the crate's JSON file.
const MAPPINGS_JSON: &str = include_str!("../../../mappings/electron-to-tauri.json");

/// Lazily parsed copy of the table, shared by [`load_mappings`] and [`lookup`].
static MAPPINGS: std::sync::OnceLock<Vec<Mapping>> = std::sync::OnceLock::new();

/// Parse (once) and return the embedded mapping table.
fn parsed() -> &'static Vec<Mapping> {
    MAPPINGS.get_or_init(|| {
        serde_json::from_str(MAPPINGS_JSON).expect("embedded mappings JSON must parse")
    })
}

/// Load the full Electron → Tauri mapping table.
///
/// The table is parsed once and cloned on later calls; use [`lookup`] when
/// only a single API is needed.
pub fn load_mappings() -> Vec<Mapping> {
    parsed().clone()
}

/// Look up one Electron API in the mapping table.
///
/// Returns `None` when the table has no entry for `api`; callers should
/// treat unknown APIs as [`crate::model::Classification::Assisted`] with a
/// note, not as an error — the table is a living document.
pub fn lookup(api: &str) -> Option<&'static Mapping> {
    parsed().iter().find(|m| m.electron_api == api)
}
