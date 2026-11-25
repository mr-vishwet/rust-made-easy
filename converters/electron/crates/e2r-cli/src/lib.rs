//! `electron-to-rust`: scan Electron desktop apps and convert them to
//! Rust (Tauri 2).
//!
//! The library exposes the scanner ([`scan`]), the converter ([`convert`]),
//! the pre-migration simulator ([`simulate`]) with its model constants
//! ([`reference`]) and its calibration loop ([`calibrate`]), the shared
//! report types ([`model`]), the Electron-to-Tauri API mapping table
//! ([`mappings`]), the decision registry ([`decisions`]), and the MCP
//! server ([`mcp`]) so integration tests and other tools can reuse them
//! without shelling out to the `e2r` binary.

pub mod calibrate;
pub mod convert;
pub mod decisions;
pub mod mappings;
pub mod mcp;
pub mod model;
pub mod reference;
pub mod scan;
pub mod simulate;
