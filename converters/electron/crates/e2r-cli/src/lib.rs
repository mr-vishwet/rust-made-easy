//! `electron-to-rust`: scan Electron desktop apps and convert them to
//! Rust (Tauri 2).
//!
//! The library exposes the scanner ([`scan`]), the converter ([`convert`]),
//! the shared report types ([`model`]) and the Electron-to-Tauri API
//! mapping table ([`mappings`]) so integration tests and other tools can
//! reuse them without shelling out to the `e2r` binary.

pub mod convert;
pub mod mappings;
pub mod model;
pub mod scan;
