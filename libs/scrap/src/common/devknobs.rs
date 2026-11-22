//! Development-time knobs. **DEVKNOB — measure branches only; never in a PR.**
//!
//! Rules live in `NOTES/DEVKNOBS.md` (rustdesk worktree; that directory is
//! git-excluded, so it is not in this repo). In short: knobs exist only on
//! `measure/*` or `ci/measure-*` branches, every knob's default equals the
//! shipping behaviour, and this whole module plus its call sites are removed
//! before a PR. `gates/check.sh` J1/J2/J4 fail if any of it leaks.

use std::sync::{Mutex, OnceLock};

fn flag(name: &str) -> bool {
    // Read once per process: per-frame paths must not call getenv.
    static CACHE: OnceLock<Mutex<Vec<(String, bool)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, v)) = guard.iter().find(|(k, _)| k == name) {
        return *v;
    }
    let v = matches!(std::env::var(name), Ok(x) if x == "1");
    guard.push((name.to_owned(), v));
    v
}

/// Force the session to ask for software frames even when a surface is
/// available, so one binary can be its own zero-copy-off control.
/// Default: off (the shipping behaviour).
pub fn no_surface() -> bool {
    flag("RD_DEV_NO_SURFACE")
}
