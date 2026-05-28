//! Persistent `:` command-line history.
//!
//! Behavior models vim's cmdline history (`c_<Up>` / `c_<Down>` /
//! prefix filter), with two policy tweaks:
//!
//! 1. **Dedup-and-promote.** Submitting a command that already
//!    exists removes the earlier copy and appends a fresh one at
//!    the tail. Vim's strict default is "skip only if equals the
//!    immediate predecessor", which produces noise like
//!    `[apl, dashboard ls, apl, edit foo, apl]`. Promote-and-dedup
//!    is what most modern shells do and what makes `:history`
//!    actually scannable.
//!
//! 2. **Bounded ring of 500.** Vim defaults to 50 (too small for
//!    modern terminals); 500 keeps the on-disk JSON well under
//!    32 KB and lets the user scroll back through a sensible
//!    session worth of work.
//!
//! ### On-disk location
//!
//! The store lives next to the rest of the user's persistent app
//! data, **not** in the cache directory. Cache is "safe to delete"
//! by definition (see [`crate::cache`], which uses `cache_dir()`
//! for the discovery snapshot); history isn't — wiping `~/.cache`
//! should not vaporise the user's command history. We resolve
//! via [`etcetera::BaseStrategy::data_dir`] which gives:
//!
//! - Linux:   `$XDG_DATA_HOME/mcu/history.json`
//!   (default `~/.local/share/mcu/history.json`)
//! - macOS:   `~/Library/Application Support/mcu/history.json`
//! - Windows: `%APPDATA%\mcu\history.json`
//!
//! Strict XDG would put this under `$XDG_STATE_HOME` on Linux,
//! but `etcetera::state_dir` returns `Option<PathBuf>` and only
//! the XDG strategy implements it — so going state-with-fallback
//! would scatter the file between two dirs on Linux vs. macOS/Win
//! for no user-visible benefit. Data dir is uniform across all
//! three platforms.
//!
//! ### Cursor convention
//!
//! - `None` = "live buffer" (below the most-recent entry; what
//!   the user typed before pressing Up)
//! - `Some(i)` = pointing at `entries[i]` (0-based, 0 = oldest)
//!
//! [`walk_back`] moves toward older entries (lower index) and
//! returns `None` when there's no older match (caller stays put).
//! [`walk_forward`] moves toward newer; returning `None` past the
//! most-recent match is the signal to restore the stashed live
//! buffer.
//!
//! [`walk_back`]: History::walk_back
//! [`walk_forward`]: History::walk_forward

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default cap. See module docs for rationale.
pub const DEFAULT_CAP: usize = 500;

/// Versioned wire format. `version` lets future readers detect
/// breaking format changes; today's reader is permissive (unknown
/// versions still try to consume the `entries` field).
#[derive(Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    entries: Vec<String>,
}

/// In-memory ring of cmdline entries plus the persistence path.
#[derive(Debug)]
pub struct History {
    entries: VecDeque<String>,
    cap: usize,
    path: Option<PathBuf>,
    /// Set by [`push`] when the entries actually change. [`save`]
    /// is a no-op when not dirty so noisy callers don't thrash
    /// disk.
    ///
    /// [`push`]: History::push
    /// [`save`]: History::save
    pub dirty: bool,
}

impl Default for History {
    /// Default = in-memory, default cap, no on-disk path. Matches
    /// what the test-only `App::with_cache` constructor wants.
    fn default() -> Self {
        Self::in_memory()
    }
}

impl History {
    /// Construct with a custom cap. Used by tests; production
    /// callers want [`load`].
    ///
    /// [`load`]: History::load
    pub fn with_cap(cap: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            cap: cap.max(1),
            path: None,
            dirty: false,
        }
    }

    /// In-memory history (no on-disk path). Used by tests and by
    /// the test-only `App::with_cache` injection point so the
    /// suite never touches the user's real history file.
    pub fn in_memory() -> Self {
        Self::with_cap(DEFAULT_CAP)
    }

    /// Load from the platform's data dir (see module docs).
    /// Missing or corrupt files yield an empty history — we never
    /// want a stray byte on disk to wedge mcu's startup.
    pub fn load() -> Self {
        Self::load_from_optional_path(default_path(), DEFAULT_CAP)
    }

    /// Load from a caller-supplied path. The path is also remembered
    /// so subsequent [`save`] calls write back to the same place.
    /// Used by tests via tempdir; production goes through [`load`].
    ///
    /// [`save`]: History::save
    /// [`load`]: History::load
    #[cfg(test)]
    pub fn load_from(path: PathBuf, cap: usize) -> Self {
        Self::load_from_optional_path(Some(path), cap)
    }

    fn load_from_optional_path(path: Option<PathBuf>, cap: usize) -> Self {
        let mut hist = match path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => Self::from_json(&text, cap),
            None => Self::with_cap(cap),
        };
        hist.path = path;
        hist
    }

    /// Read-only view of the entries, oldest first.
    pub fn entries(&self) -> &VecDeque<String> {
        &self.entries
    }

    /// Append `cmd` to history with dedup-promote semantics. See
    /// module docs.
    pub fn push(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }
        // Remove the earlier copy if present so the new push lands
        // at the tail and the entry is unique.
        let before = self.entries.len();
        self.entries.retain(|e| e != trimmed);
        let removed_existing = self.entries.len() != before;
        self.entries.push_back(trimmed.to_string());
        while self.entries.len() > self.cap {
            self.entries.pop_front();
        }
        // Even if we just promoted an existing entry, the on-disk
        // order changed, so we still need to persist.
        let _ = removed_existing;
        self.dirty = true;
    }

    /// Step toward older entries. `cursor` is the current position
    /// (`None` = live buffer); `prefix` is the active filter. Returns
    /// the next-older index whose entry starts with `prefix`, or
    /// `None` when nothing older matches.
    pub fn walk_back(&self, cursor: Option<usize>, prefix: &str) -> Option<usize> {
        let start = cursor.unwrap_or(self.entries.len());
        // Indices strictly below `start`, newest-to-oldest.
        (0..start).rev().find(|&i| self.matches(i, prefix))
    }

    /// Step toward newer entries. Returns the next-newer matching
    /// index, or `None` when no newer match exists. The caller
    /// reads `None` as "past the most-recent entry; restore the
    /// stashed live buffer".
    pub fn walk_forward(&self, cursor: Option<usize>, prefix: &str) -> Option<usize> {
        let Some(start) = cursor else {
            // Already at live buffer; nothing newer.
            return None;
        };
        ((start + 1)..self.entries.len()).find(|&i| self.matches(i, prefix))
    }

    fn matches(&self, i: usize, prefix: &str) -> bool {
        self.entries
            .get(i)
            .map(|e| e.starts_with(prefix))
            .unwrap_or(false)
    }

    /// Direct accessor for the entry at `i`. Returns `None` for
    /// out-of-range indices so callers don't have to track the
    /// ring's exact length.
    pub fn get(&self, i: usize) -> Option<&str> {
        self.entries.get(i).map(String::as_str)
    }

    /// Atomically persist to disk. No-op when the path is unset
    /// (tests) or `dirty` is false.
    pub fn save(&mut self) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let json = self.to_json();
        crate::util::atomic::atomic_write_text(path, &json)?;
        self.dirty = false;
        Ok(())
    }

    pub(crate) fn to_json(&self) -> String {
        let wire = OnDisk {
            version: 1,
            entries: self.entries.iter().cloned().collect(),
        };
        // `to_string` (not pretty) keeps the file small. Tests use
        // `from_json` so the on-disk form stays free to evolve.
        serde_json::to_string(&wire).unwrap_or_else(|_| String::from("{}"))
    }

    pub(crate) fn from_json(text: &str, cap: usize) -> Self {
        // Tolerant parse: accept any version that carries an
        // `entries` array; corrupt JSON yields an empty history.
        // Clamp to cap so a stale on-disk file with a larger cap
        // doesn't blow past the in-memory budget.
        let mut hist = Self::with_cap(cap);
        if let Ok(parsed) = serde_json::from_str::<OnDisk>(text) {
            let take_from = parsed.entries.len().saturating_sub(cap);
            hist.entries = parsed.entries.into_iter().skip(take_from).collect();
        }
        hist
    }
}

fn default_path() -> Option<PathBuf> {
    use etcetera::BaseStrategy;
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.data_dir().join("mcu").join("history.json"))
}

#[cfg(test)]
mod tests;
