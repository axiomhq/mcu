//! Tiny terminal-capability probe.
//!
//! Cached on first call; cheap enough that a re-probe per frame wouldn't
//! hurt, but caching makes the heatmap renderer's hot path branch-free.
//!
//! We deliberately don't pull in a full terminfo crate. The only
//! capability the renderers ask about is whether the terminal accepts
//! 24-bit (truecolor) backgrounds — that's a one-env-var check.

use std::sync::OnceLock;

static TRUECOLOR: OnceLock<bool> = OnceLock::new();

/// `true` if the terminal advertises 24-bit colour support via the
/// conventional `COLORTERM` env var. False is the safe answer everywhere
/// else; callers fall back to 256-colour palettes.
pub fn supports_truecolor() -> bool {
    *TRUECOLOR.get_or_init(|| match std::env::var("COLORTERM") {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            v == "truecolor" || v == "24bit"
        }
        Err(_) => false,
    })
}

#[cfg(test)]
mod tests {
    // We can't test `supports_truecolor` directly without mucking with
    // `COLORTERM` in the global env (race-prone under cargo test's thread
    // pool). The function is trivial enough that a manual probe + the
    // surrounding heatmap golden-style tests cover it. If we ever need
    // tighter coverage, plumb the lookup through an injected closure.
}
