//! Decision registry: an append-only log of the API decisions a
//! conversion made.
//!
//! Every time [`crate::convert::convert`] acts on an Electron API finding
//! it records one [`DecisionEntry`]: which Electron API, which Tauri
//! replacement (if any), the classification, the rationale, and how
//! confident the converter was. The log is the audit trail for a
//! migration — it answers "why did this become that?" after the fact,
//! and the MCP tool `get_decision_log` reads it back for agents.
//!
//! **Format v1, append-only JSONL.** One JSON object per line; the file
//! is only ever appended to, never rewritten. Shape:
//!
//! ```json
//! {
//!   "version": 1,
//!   "timestamp": "2026-11-25T06:30:00Z",
//!   "electron_api": "dialog.showOpenDialog",
//!   "tauri_replacement": "tauri-plugin-dialog",
//!   "classification": "auto",
//!   "rationale": "Direct Tauri equivalent exists; conversion is mechanical.",
//!   "confidence": "high",
//!   "source": "mappings-db"
//! }
//! ```
//!
//! `timestamp` is RFC 3339 UTC (`...Z`); `tauri_replacement` is `null`
//! when no Tauri equivalent exists (manual ports). A corrupt line is
//! skipped on read (counted, see [`read_with_stats`]) rather than
//! failing the read, so a damaged log never breaks the tools that
//! consume it.
//!
//! **Paths.** A converted project keeps its log at
//! `<project-dir>/.e2r/decisions.jsonl` (see [`default_path`]); the
//! fallback when no project directory is given is
//! `~/.cache/e2r/decisions.jsonl`, overridable via the
//! `E2R_DECISIONS_LOG` environment variable. This mirrors
//! [`crate::calibrate::calibration_log_path`]: an explicit project dir
//! wins, then the env var, then the cache default. When `HOME` is unset
//! the default falls back to `.e2r/decisions.jsonl` under the current
//! directory.
//!
//! Writing the log must never fail a conversion: `convert` records a
//! warning in its [`crate::convert::ConvertReport`] and continues when
//! the append fails.

use crate::model::{Classification, Finding};
use crate::simulate::rfc3339_now;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Environment variable overriding the decision-log location (used only
/// when no project directory is given — see [`default_path`]).
pub const DECISIONS_LOG_ENV: &str = "E2R_DECISIONS_LOG";

/// Decision-entry format version. Always `1` in this release.
pub const DECISION_FORMAT_VERSION: u32 = 1;

/// How confident the converter was in a recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionConfidence {
    /// Direct Tauri equivalent from the mappings database.
    High,
    /// An equivalent exists but needs human review (assisted).
    Medium,
    /// No direct equivalent; the human decides (manual).
    Low,
}

/// Where a recorded decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionSource {
    /// The Electron → Tauri mapping table ([`crate::mappings`]).
    MappingsDb,
    /// A scanner/converter rule (no mapping-table row for the API).
    Rule,
    /// A human overrode the converter's choice. (No writer produces
    /// this yet — it exists so a future override tool can append
    /// entries without a format bump.)
    HumanOverride,
}

/// One decision in the registry: what the converter did with one
/// Electron API, and why.
///
/// **Stable format.** Field names and the `version` must not change
/// without bumping [`DECISION_FORMAT_VERSION`] and updating the
/// readers. New optional fields may be added with `#[serde(default)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    /// Entry format version. Always [`DECISION_FORMAT_VERSION`].
    pub version: u32,
    /// When the decision was recorded (RFC 3339 UTC).
    pub timestamp: String,
    /// Electron API name, e.g. `"dialog.showOpenDialog"`.
    pub electron_api: String,
    /// The Tauri replacement chosen, if any (`None` for manual ports
    /// with no Tauri equivalent).
    pub tauri_replacement: Option<String>,
    /// How convertible the API is (see [`Classification`]).
    pub classification: Classification,
    /// Why this replacement was chosen, in one line.
    pub rationale: String,
    /// How confident the converter was in this decision.
    pub confidence: DecisionConfidence,
    /// Where the decision came from.
    pub source: DecisionSource,
}

impl DecisionEntry {
    /// Build an entry, stamping it with the current format version and
    /// the current time (RFC 3339 UTC).
    pub fn new(
        electron_api: impl Into<String>,
        tauri_replacement: Option<String>,
        classification: Classification,
        rationale: impl Into<String>,
        confidence: DecisionConfidence,
        source: DecisionSource,
    ) -> Self {
        Self {
            version: DECISION_FORMAT_VERSION,
            timestamp: rfc3339_now(),
            electron_api: electron_api.into(),
            tauri_replacement,
            classification,
            rationale: rationale.into(),
            confidence,
            source,
        }
    }
}

/// Result of reading a decision log with corruption accounting.
#[derive(Debug, Clone, Default)]
pub struct DecisionLogRead {
    /// Entries parsed successfully, in file order (oldest first).
    pub entries: Vec<DecisionEntry>,
    /// Lines skipped because they did not parse as a [`DecisionEntry`]
    /// (blank lines do not count).
    pub skipped: usize,
}

/// Resolve the decision-log path: `<project-dir>/.e2r/decisions.jsonl`
/// when a project directory is given; otherwise the
/// `E2R_DECISIONS_LOG` environment variable when set and non-empty;
/// otherwise the default `~/.cache/e2r/decisions.jsonl`. When `HOME`
/// is unset the default falls back to `.e2r/decisions.jsonl` under the
/// current directory.
pub fn default_path(project_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = project_dir {
        return dir.join(".e2r").join("decisions.jsonl");
    }
    if let Ok(path) = std::env::var(DECISIONS_LOG_ENV) {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home)
            .join(".cache")
            .join("e2r")
            .join("decisions.jsonl"),
        _ => PathBuf::from(".e2r").join("decisions.jsonl"),
    }
}

/// Append `entries` to the decision log at `path`, creating parent
/// directories as needed. Returns the number of entries written.
///
/// The file is opened in append mode, so concurrent writers interleave
/// whole lines rather than corrupting each other (each entry is a
/// single `writeln!` call).
pub fn append(path: &Path, entries: &[DecisionEntry]) -> Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open decision log {}", path.display()))?;
    for entry in entries {
        let line =
            serde_json::to_string(entry).context("failed to serialize decision entry as JSON")?;
        writeln!(file, "{line}")
            .with_context(|| format!("failed to append to decision log {}", path.display()))?;
    }
    Ok(entries.len())
}

/// Read the decision log at `path`, in file order (oldest first).
/// Corrupt lines are skipped; a missing file reads as an empty log.
/// Use [`read_with_stats`] when the skipped-line count matters.
pub fn read(path: &Path) -> Vec<DecisionEntry> {
    read_with_stats(path).entries
}

/// Read the decision log at `path` with corruption accounting: the
/// entries in file order, plus how many non-blank lines were skipped
/// because they did not parse as a [`DecisionEntry`]. A missing file
/// reads as an empty log with zero skipped lines.
pub fn read_with_stats(path: &Path) -> DecisionLogRead {
    let Ok(contents) = fs::read_to_string(path) else {
        return DecisionLogRead::default();
    };
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<DecisionEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => skipped += 1,
        }
    }
    DecisionLogRead { entries, skipped }
}

/// Build decision entries from scan findings — one entry per finding,
/// in scan order. This is what [`crate::convert::convert`] records.
///
/// The rationale is the finding's note (the scanner's honest line about
/// what the conversion involves); findings with an empty note get a
/// rationale derived from their classification. Confidence follows the
/// classification (`auto` → high, `assisted` → medium, `manual` →
/// low). The source is `mappings-db` when the mappings table has a row
/// for the API, `rule` otherwise.
pub fn entries_from_findings(findings: &[Finding]) -> Vec<DecisionEntry> {
    findings
        .iter()
        .map(|finding| {
            let rationale = if finding.note.is_empty() {
                match finding.classification {
                    Classification::Auto => {
                        "Direct Tauri equivalent exists; conversion is mechanical."
                    }
                    Classification::Assisted => {
                        "A Tauri equivalent exists but needs human decisions."
                    }
                    Classification::Manual => {
                        "No direct Tauri equivalent; the feature needs a manual port."
                    }
                }
                .to_string()
            } else {
                finding.note.clone()
            };
            let confidence = match finding.classification {
                Classification::Auto => DecisionConfidence::High,
                Classification::Assisted => DecisionConfidence::Medium,
                Classification::Manual => DecisionConfidence::Low,
            };
            let source = if crate::mappings::lookup(&finding.api).is_some() {
                DecisionSource::MappingsDb
            } else {
                DecisionSource::Rule
            };
            DecisionEntry::new(
                finding.api.clone(),
                finding.tauri_target.clone(),
                finding.classification,
                rationale,
                confidence,
                source,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A fresh scratch dir per test (no `tempfile` dependency: process id
    /// + timestamp is unique enough for a test). Under the crate target
    ///   dir, not `/tmp` (see the golden tests' note on the 512 MB tmpfs).
    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("decisions-{tag}-{}-{nanos}", std::process::id()))
    }

    fn sample_entry(api: &str) -> DecisionEntry {
        DecisionEntry::new(
            api,
            Some("tauri-plugin-dialog".to_string()),
            Classification::Auto,
            "Direct Tauri equivalent exists.",
            DecisionConfidence::High,
            DecisionSource::MappingsDb,
        )
    }

    #[test]
    fn test_append_read_round_trip() {
        let dir = scratch_dir("round-trip");
        let path = dir.join(".e2r").join("decisions.jsonl");
        let entries = vec![sample_entry("dialog.showOpenDialog"), sample_entry("shell")];
        let written = append(&path, &entries).expect("append must succeed");
        assert_eq!(written, 2);
        let back = read(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].electron_api, "dialog.showOpenDialog");
        assert_eq!(back[1].electron_api, "shell");
        assert_eq!(back[0].version, DECISION_FORMAT_VERSION);
        assert_eq!(
            back[0].tauri_replacement.as_deref(),
            Some("tauri-plugin-dialog")
        );
        assert!(back[0].timestamp.ends_with('Z'));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_append_is_append_only() {
        let dir = scratch_dir("append-only");
        let path = dir.join("decisions.jsonl");
        append(&path, &[sample_entry("app")]).expect("first append");
        append(&path, &[sample_entry("shell")]).expect("second append");
        let back = read(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].electron_api, "app");
        assert_eq!(back[1].electron_api, "shell");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_missing_file_is_empty() {
        let dir = scratch_dir("missing");
        let back = read_with_stats(&dir.join("nope.jsonl"));
        assert!(back.entries.is_empty());
        assert_eq!(back.skipped, 0);
    }

    #[test]
    fn test_read_skips_corrupt_lines() {
        let dir = scratch_dir("corrupt");
        let path = dir.join("decisions.jsonl");
        fs::create_dir_all(&dir).unwrap();
        let good = serde_json::to_string(&sample_entry("app")).unwrap();
        let good2 = serde_json::to_string(&sample_entry("shell")).unwrap();
        fs::write(
            &path,
            format!("{good}\nthis is not json\n{{\"version\": 1}}\n\n{good2}\n"),
        )
        .unwrap();
        let back = read_with_stats(&path);
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.skipped, 2, "two corrupt lines, blank line not counted");
        assert_eq!(back.entries[0].electron_api, "app");
        assert_eq!(back.entries[1].electron_api, "shell");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_default_path_project_dir() {
        let path = default_path(Some(Path::new("/tmp/some-project")));
        assert_eq!(
            path,
            PathBuf::from("/tmp/some-project/.e2r/decisions.jsonl")
        );
    }

    #[test]
    fn test_entries_from_findings() {
        let findings = vec![
            Finding {
                api: "dialog.showOpenDialog".to_string(),
                category: "dialog".to_string(),
                file: "main.js".to_string(),
                line: 3,
                classification: Classification::Auto,
                tauri_target: Some("tauri-plugin-dialog".to_string()),
                note: "Maps to the dialog plugin.".to_string(),
                occurrences: 1,
            },
            Finding {
                api: "some-unknown-api".to_string(),
                category: "other".to_string(),
                file: "main.js".to_string(),
                line: 9,
                classification: Classification::Manual,
                tauri_target: None,
                note: String::new(),
                occurrences: 2,
            },
        ];
        let entries = entries_from_findings(&findings);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, DecisionSource::MappingsDb);
        assert_eq!(entries[0].confidence, DecisionConfidence::High);
        assert_eq!(entries[0].rationale, "Maps to the dialog plugin.");
        assert_eq!(entries[1].source, DecisionSource::Rule);
        assert_eq!(entries[1].confidence, DecisionConfidence::Low);
        assert_eq!(entries[1].tauri_replacement, None);
        assert!(
            !entries[1].rationale.is_empty(),
            "empty note gets a fallback"
        );
    }
}
