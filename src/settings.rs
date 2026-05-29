//! App-private settings.
//!
//! Lives **next to** the rest of ax's persistent state, not inside
//! `~/.axiom.toml` — that file is shared with the official Axiom CLI
//! and ax deliberately doesn't put ax-only keys in it. Settings
//! here are ax's own preferences (trace defaults today; UI/picker
//! defaults eventually).
//!
//! ### On-disk location
//!
//! `etcetera::BaseStrategy::config_dir().join("ax/settings.toml")` —
//! - Linux:   `$XDG_CONFIG_HOME/ax/settings.toml`
//!   (default `~/.config/ax/settings.toml`)
//! - macOS:   `~/Library/Application Support/ax/settings.toml`
//! - Windows: `%APPDATA%\ax\settings.toml`
//!
//! Writes go through [`crate::util::atomic::atomic_write_text`] so a
//! crash mid-write can't leave a torn file (same guarantee `cache.rs`
//! and `history.rs` already buy).
//!
//! ### Schema philosophy
//!
//! Every field is `#[serde(default)]` and lives under a named
//! `[section]` table. That way adding a future `[ui]` / `[picker]`
//! group doesn't break old readers, and an entirely missing file is
//! the same shape as a fresh `Settings::default()`.
//!
//! Today's only table is `[trace]`:
//!
//! ```toml
//! [trace]
//! dataset    = "axiom-traces-dev"
//! deployment = "staging"
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Root settings document. Sub-tables are optional so a brand-new
/// `settings.toml` (or a missing file) round-trips through
/// `Settings::default()` unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub trace: TraceSettings,
}

/// Defaults consulted by the upcoming trace view (steps 22+) when
/// `:trace <id>` is invoked without an explicit dataset/deployment.
///
/// Both fields are `Option` because "unset" is a real state — the
/// trace view should surface a clear "no default trace dataset
/// configured" error rather than silently picking an arbitrary one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<String>,
}

/// Settings handle: in-memory document + optional path for
/// persistence. Mirrors [`crate::cache::Cache`] / [`crate::history`]
/// shape so callers don't have to learn a new vocabulary per
/// persistence layer.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: Option<PathBuf>,
    data: Settings,
}

impl SettingsStore {
    /// Load from the standard path. Missing files yield defaults;
    /// malformed files yield defaults *and* leave the file alone —
    /// the next [`save`](Self::save) re-serialises a valid
    /// document, but we never want a stray byte at startup to wedge
    /// ax.
    pub fn load() -> Self {
        let path = default_path();
        let data = path.as_deref().and_then(read_from_disk).unwrap_or_default();
        Self { path, data }
    }

    /// In-memory store; never touches disk. Used by tests and by
    /// `App::with_cache` test-only injection.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            data: Settings::default(),
        }
    }

    /// Load from a caller-supplied path. The path is remembered so
    /// subsequent [`save`](Self::save) calls write back there. Used
    /// by unit tests via tempdir; production goes through
    /// [`load`](Self::load).
    #[cfg(test)]
    pub fn load_from(path: PathBuf) -> Self {
        let data = read_from_disk(&path).unwrap_or_default();
        Self {
            path: Some(path),
            data,
        }
    }

    /// Whole-document accessor. Test-only today; production reads go
    /// through field-shaped accessors like [`trace`](Self::trace) so
    /// future schema reshuffles don't break call sites.
    #[cfg(test)]
    pub fn settings(&self) -> &Settings {
        &self.data
    }

    pub fn trace(&self) -> &TraceSettings {
        &self.data.trace
    }

    pub fn set_trace_dataset(&mut self, value: Option<String>) {
        self.data.trace.dataset = normalise(value);
    }

    pub fn set_trace_deployment(&mut self, value: Option<String>) {
        self.data.trace.deployment = normalise(value);
    }

    /// Atomically persist to disk. No-op when path is unset
    /// (tests / in-memory store).
    pub fn save(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let text = toml::to_string_pretty(&self.data).context("serializing settings")?;
        crate::util::atomic::atomic_write_text(path, &text)
    }

    /// Parse + return the on-disk shape from a TOML string.
    /// Test-only entry point that lets the unit tests assert
    /// round-trip behaviour without touching the filesystem;
    /// production reads go through [`load`](Self::load) which
    /// already calls into the same parser via
    /// [`read_from_disk`].
    #[cfg(test)]
    pub fn parse(text: &str) -> Result<Settings> {
        toml::from_str::<Settings>(text).context("parsing settings")
    }
}

/// Trim and reject the empty case. The `:trace set dataset=` form
/// would otherwise persist an empty string and present as "set"
/// instead of "unset" — surprising. Empty value collapses to `None`
/// here so the get/unset semantics stay symmetric.
fn normalise(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn default_path() -> Option<PathBuf> {
    use etcetera::BaseStrategy;
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.config_dir().join("ax").join("settings.toml"))
}

fn read_from_disk(p: &Path) -> Option<Settings> {
    let text = fs::read_to_string(p).ok()?;
    toml::from_str(&text).ok()
}

#[cfg(test)]
mod tests;
