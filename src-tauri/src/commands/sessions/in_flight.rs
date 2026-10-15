//! Registry of in-flight AI generations, keyed by chat-session id.
//!
//! Replaces the former process-global `AI_CANCEL` flag (`ai/mod.rs`), which
//! made `POST /api/ai/cancel` an app-wide "stop everything": the desktop chat,
//! the Web UI and the Feishu bot all shared one flag, so stopping one end's
//! answer aborted the others (`docs/LARK_BOT.md` §5.5 C1).
//!
//! Lifecycle is RAII — [`InFlight::begin`] registers, `Drop` removes — so the
//! registry cannot leak entries even when a generation ends early (cancelled,
//! errored, or panicked in its blocking task).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::ai::CancelToken;

/// Registry key for generations that carry no session id (legacy entry points
/// such as `smart_search`). Kept separate so that a session-scoped cancel can
/// never accidentally hit one, and vice versa.
const NO_SESSION: &str = "";

fn registry() -> &'static Mutex<HashMap<String, Arc<CancelToken>>> {
    static REG: OnceLock<Mutex<HashMap<String, Arc<CancelToken>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> MutexGuard<'static, HashMap<String, Arc<CancelToken>>> {
    // Poisoned: entries are only ever swapped under the lock and the token
    // itself is an atomic, so a panic in a holder leaves no half-written state
    // — recover (same rationale as `commands/ai/session.rs::history_lock`).
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII handle for one in-flight generation, held for the duration of the
/// call that owns it.
pub struct InFlight {
    key: String,
    token: Arc<CancelToken>,
}

impl InFlight {
    /// Register a generation. `session_id: None` uses the shared session-less
    /// slot (see [`NO_SESSION`]).
    ///
    /// Starting a second generation for the same session **supersedes** the
    /// first: the previous token is cancelled immediately. Per session only one
    /// generation is authoritative, which is what the desktop UI already
    /// assumes (it enforces single-flight) and what the bot gets by queueing.
    pub fn begin(session_id: Option<&str>) -> Self {
        let key = session_id.unwrap_or(NO_SESSION).to_string();
        let token = Arc::new(CancelToken::default());
        let superseded = lock().insert(key.clone(), token.clone());
        if let Some(previous) = superseded {
            previous.cancel();
            log::info!("[AI-CANCEL] superseded in-flight generation for session '{key}'");
        }
        Self { key, token }
    }

    /// Borrowed token, for the synchronous streaming/waiting call.
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// Owning handle, for moving into `spawn_blocking` closures.
    pub fn token_arc(&self) -> Arc<CancelToken> {
        Arc::clone(&self.token)
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut map = lock();
        // Only remove our own entry: a superseding guard may already have
        // replaced it, and `ptr_eq` keeps us from deleting the newcomer.
        if map
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.token))
        {
            map.remove(&self.key);
        }
    }
}

/// Cancel one session's in-flight generation.
///
/// Returns `false` when nothing was running — cancelling is idempotent and
/// never an error (a cancel can always lose the race against completion).
pub fn cancel(session_id: &str) -> bool {
    let found = lock().get(session_id).cloned();
    match found {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

/// Cancel every in-flight generation, returning how many were affected.
///
/// Serves callers that predate per-session cancellation (`§5.5` decision D3:
/// a parameterless cancel still means "stop everything").
pub fn cancel_all() -> usize {
    let tokens: Vec<Arc<CancelToken>> = lock().values().cloned().collect();
    for token in &tokens {
        token.cancel();
    }
    tokens.len()
}

/// Session ids with a generation currently in flight. Diagnostics and tests.
pub fn active_sessions() -> Vec<String> {
    lock().keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(id: &str) -> bool {
        active_sessions().iter().any(|s| s.as_str() == id)
    }

    /// The registry is process-global, so this exercises the whole lifecycle in
    /// one test and asserts on *our own* ids rather than global counts — other
    /// tests running in parallel must not make this flaky (same rationale as
    /// `webapi::session`'s lifecycle test).
    #[test]
    fn lifecycle_register_cancel_supersede_and_drop() {
        // 1. Registration is visible; the token starts clean. Dropping
        //    unregisters, so the registry cannot leak.
        {
            let a = InFlight::begin(Some("sess-a"));
            assert!(!a.is_cancelled());
            assert!(has("sess-a"));
        }
        assert!(!has("sess-a"), "Drop must unregister the generation");

        // 2. A session-scoped cancel hits only its own session — and an unknown
        //    session is a no-op, not an error (cancel always races completion).
        {
            let a = InFlight::begin(Some("sess-b"));
            assert!(cancel("sess-b"));
            assert!(a.is_cancelled());
            assert!(!cancel("sess-b-not-running"), "unknown session must not match");
        }

        // 3. Session-less generations live in their own slot: a session-scoped
        //    cancel must not touch them.
        {
            let keyed = InFlight::begin(Some("sess-c"));
            let sessionless = InFlight::begin(None);
            assert!(!sessionless.is_cancelled());
            assert!(cancel("sess-c"));
            assert!(keyed.is_cancelled());
            assert!(
                !sessionless.is_cancelled(),
                "a session-scoped cancel must not reach the session-less slot"
            );
        }

        // 4. `cancel_all` reaches every slot and reports how many it hit
        //    (the parameterless-cancel compatibility path, §5.5 D3).
        {
            let keyed = InFlight::begin(Some("sess-d"));
            let sessionless = InFlight::begin(None);
            let affected = cancel_all();
            assert!(keyed.is_cancelled());
            assert!(sessionless.is_cancelled());
            assert!(affected >= 2, "expected at least 2, got {affected}");
        }

        // 5. Supersede: a second generation for the same session cancels the
        //    first, and dropping the *first* must not evict the *second*.
        let first = InFlight::begin(Some("sess-e"));
        let first_token = first.token_arc();
        let second = InFlight::begin(Some("sess-e"));
        assert!(first_token.is_cancelled(), "superseded token must be cancelled");
        assert!(!second.is_cancelled());
        drop(first);
        assert!(has("sess-e"), "dropping the superseded guard must not evict the newer one");
        drop(second);
        assert!(!has("sess-e"), "the newest guard must still unregister on drop");
    }
}
