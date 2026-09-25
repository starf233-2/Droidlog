//! Shared application state.
//!
//! [`AppState`] is registered with Tauri and handed to every command. It holds
//! only process-lifetime data: the record sequence counter, the active filter
//! rules, and the running sessions.
//!
//! Locking rules, so the hot path stays predictable:
//!
//! * every lock is a **std** lock with a short, `await`-free critical section —
//!   the reader task pushes into a ring without ever yielding while holding it;
//! * poisoning is recovered rather than propagated (see [`LockExt`]), because a
//!   panicking command must not permanently brick the UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::device::resolve::{AppResolver, AppTarget};
use crate::device::watch::DeviceWatcher;
use crate::device::DeviceInfo;
use crate::error::{DroidLogError, Result};
use crate::filter::{FilterRule, FilterSet};
use crate::parser::LogRecord;
use crate::process::{SessionHandle, SessionSummary};

/// Recovers a poisoned mutex instead of propagating the panic.
pub trait LockExt<T> {
    /// Locks, ignoring poisoning.
    fn lock_ignore_poison(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_ignore_poison(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Recovers a poisoned read/write lock instead of propagating the panic.
pub trait RwLockExt<T> {
    /// Takes a read guard, ignoring poisoning.
    fn read_ignore_poison(&self) -> RwLockReadGuard<'_, T>;
    /// Takes a write guard, ignoring poisoning.
    fn write_ignore_poison(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_ignore_poison(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_ignore_poison(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Process-wide state shared by all Tauri commands.
pub struct AppState {
    seq: AtomicU64,
    session_seq: AtomicU64,
    filters: RwLock<Vec<FilterRule>>,
    filters_version: AtomicU64,
    sessions: Mutex<HashMap<String, SessionHandle>>,
    /// Handle for poking the device poller from a command.
    device_watcher: Arc<DeviceWatcher>,
    /// Last successfully polled device list. Kept so one failed poll cannot
    /// blank the device rail.
    last_devices: Mutex<Vec<DeviceInfo>>,
    /// The application the user asked to follow, if any.
    app_target: RwLock<Option<AppTarget>>,
    /// Bumped whenever the target changes, so capture loops know to re-read it.
    app_target_version: AtomicU64,
    /// Cached package/uid/pid resolutions.
    app_resolver: Arc<AppResolver>,
    /// Reports of finished boot/crash/recovery collections, keyed by session id.
    collect_reports: RwLock<HashMap<String, crate::collect::CollectionReport>>,
}

impl AppState {
    /// Creates empty state with no sessions and no filters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(1),
            session_seq: AtomicU64::new(1),
            filters: RwLock::new(Vec::new()),
            filters_version: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
            device_watcher: Arc::new(DeviceWatcher::new()),
            last_devices: Mutex::new(Vec::new()),
            app_target: RwLock::new(None),
            app_target_version: AtomicU64::new(0),
            app_resolver: Arc::new(AppResolver::default()),
            collect_reports: RwLock::new(HashMap::new()),
        }
    }

    /// The cached package/uid/pid resolver.
    #[must_use]
    pub fn app_resolver(&self) -> Arc<AppResolver> {
        Arc::clone(&self.app_resolver)
    }

    /// The application currently being followed, if any.
    #[must_use]
    pub fn app_target(&self) -> Option<AppTarget> {
        self.app_target.read_ignore_poison().clone()
    }

    /// Revision of the app target, for capture loops to poll cheaply.
    #[must_use]
    pub fn app_target_version(&self) -> u64 {
        self.app_target_version.load(Ordering::Relaxed)
    }

    /// Replaces (or clears) the followed application.
    pub fn set_app_target(&self, target: Option<AppTarget>) {
        *self.app_target.write_ignore_poison() = target;
        self.app_target_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Replaces the target without bumping the revision.
    ///
    /// Used by the poller when it re-resolves the same app and the identity is
    /// unchanged: capture loops keep the copy they already hold, and no
    /// needless event is emitted.
    pub fn refresh_app_target_quietly(&self, target: AppTarget) {
        *self.app_target.write_ignore_poison() = Some(target);
    }

    /// A handle to the device poller, for waking it or forcing a re-probe.
    #[must_use]
    pub fn device_watcher(&self) -> Arc<DeviceWatcher> {
        Arc::clone(&self.device_watcher)
    }

    /// The most recent device list the poller obtained.
    #[must_use]
    pub fn last_devices(&self) -> Vec<DeviceInfo> {
        self.last_devices.lock_ignore_poison().clone()
    }

    /// Records the latest device list from a successful poll.
    pub fn set_last_devices(&self, devices: Vec<DeviceInfo>) {
        *self.last_devices.lock_ignore_poison() = devices;
    }

    /// Allocates the next record sequence number.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Allocates a session id.
    pub fn next_session_id(&self) -> String {
        let n = self.session_seq.fetch_add(1, Ordering::Relaxed);
        format!("session-{n}")
    }

    /// A snapshot of the active filter rules.
    #[must_use]
    pub fn filters(&self) -> Vec<FilterRule> {
        self.filters.read_ignore_poison().clone()
    }

    /// Monotonic revision of the filter rules.
    ///
    /// The reader task caches its compiled [`FilterSet`] against this value, so
    /// editing filters costs one recompile per session, not one per record.
    #[must_use]
    pub fn filters_version(&self) -> u64 {
        self.filters_version.load(Ordering::Relaxed)
    }

    /// Validates and stores `rules`, bumping the revision.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::InvalidFilter`] and leaves the previous rules in
    /// place when any rule fails to compile.
    pub fn set_filters(&self, rules: Vec<FilterRule>) -> Result<()> {
        // Compile first: rejecting the edit must not disturb the running filter.
        FilterSet::compile(&rules)?;
        *self.filters.write_ignore_poison() = rules;
        self.filters_version.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// The compiled form of the current rules.
    ///
    /// # Errors
    ///
    /// Propagates compile failures (only reachable if state was mutated outside
    /// [`AppState::set_filters`]).
    pub fn compiled_filters(&self) -> Result<FilterSet> {
        FilterSet::compile(&self.filters())
    }

    /// Registers a session.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::SessionAlreadyRunning`] if the id is taken.
    pub fn insert_session(&self, handle: SessionHandle) -> Result<()> {
        let id = handle.info().id.clone();
        let mut sessions = self.sessions.lock_ignore_poison();
        if sessions.contains_key(&id) {
            return Err(DroidLogError::SessionAlreadyRunning(id));
        }
        sessions.insert(id, handle);
        Ok(())
    }

    /// Removes and returns a session.
    pub fn remove_session(&self, session_id: &str) -> Option<SessionHandle> {
        self.sessions.lock_ignore_poison().remove(session_id)
    }

    /// Removes every session, returning them for shutdown.
    pub fn take_sessions(&self) -> Vec<SessionHandle> {
        let mut sessions = self.sessions.lock_ignore_poison();
        sessions.drain().map(|(_, handle)| handle).collect()
    }

    /// Ids of all registered sessions.
    #[must_use]
    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.lock_ignore_poison().keys().cloned().collect()
    }

    /// A serialisable view of every session.
    #[must_use]
    pub fn session_summaries(&self) -> Vec<SessionSummary> {
        self.sessions
            .lock_ignore_poison()
            .values()
            .map(SessionHandle::summary)
            .collect()
    }

    /// Records the outcome of a session's reader task.
    ///
    /// Silently does nothing when the session was already removed — the UI
    /// stopped it and no longer cares.
    pub fn set_session_status(&self, session_id: &str, status: crate::process::SessionStatus) {
        if let Some(handle) = self.sessions.lock_ignore_poison().get_mut(session_id) {
            handle.set_status(status);
        }
    }

    /// Records collection progress for the UI countdown.
    ///
    /// Like [`Self::set_session_status`], a missing session is not an error: the
    /// UI may have stopped the collection while a probe was still running.
    pub fn set_session_progress(
        &self,
        session_id: &str,
        progress: crate::process::SessionProgress,
    ) {
        if let Some(handle) = self.sessions.lock_ignore_poison().get_mut(session_id) {
            handle.set_progress(progress);
        }
    }

    /// Stores the report of a finished collection run, keyed by session.
    pub fn set_collect_report(&self, report: crate::collect::CollectionReport) {
        self.collect_reports
            .write_ignore_poison()
            .insert(report.session_id.clone(), report);
    }

    /// The report of a collection run, if it has finished.
    #[must_use]
    pub fn collect_report(&self, session_id: &str) -> Option<crate::collect::CollectionReport> {
        self.collect_reports
            .read_ignore_poison()
            .get(session_id)
            .cloned()
    }

    /// Every finished collection report, newest session id last.
    #[must_use]
    pub fn collect_reports(&self) -> Vec<crate::collect::CollectionReport> {
        self.collect_reports
            .read_ignore_poison()
            .values()
            .cloned()
            .collect()
    }

    /// Snapshots up to `limit` of the newest records from a session.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::SessionNotFound`] for an unknown session.
    pub fn session_records(&self, session_id: &str, limit: Option<usize>) -> Result<Vec<LogRecord>> {
        let buffer = {
            let sessions = self.sessions.lock_ignore_poison();
            let handle = sessions
                .get(session_id)
                .ok_or_else(|| DroidLogError::SessionNotFound(session_id.to_owned()))?;
            handle.buffer()
        };

        let records = buffer.lock_ignore_poison().snapshot();
        Ok(match limit {
            // Keep the newest `limit` records, preserving oldest-first order.
            Some(limit) if records.len() > limit => {
                let start = records.len().saturating_sub(limit);
                records.into_iter().skip(start).collect()
            }
            _ => records,
        })
    }

    /// Number of registered sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.lock_ignore_poison().len()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{FilterField, FilterOp};
    use crate::parser::LogLevel;
    use crate::source::LogSourceKind;

    fn record(seq: u64) -> LogRecord {
        LogRecord::new(LogSourceKind::Logcat, seq, "raw")
    }

    #[test]
    fn sequence_numbers_are_unique_and_monotonic() {
        let state = AppState::new();
        let first = state.next_seq();
        let second = state.next_seq();
        assert!(second > first);
    }

    #[test]
    fn session_ids_are_unique() {
        let state = AppState::new();
        assert_ne!(state.next_session_id(), state.next_session_id());
    }

    #[test]
    fn filters_start_empty_and_accept_all() -> Result<()> {
        let state = AppState::new();
        assert!(state.filters().is_empty());
        assert!(state.compiled_filters()?.matches(&record(1)));
        Ok(())
    }

    #[test]
    fn setting_filters_bumps_the_version() -> Result<()> {
        let state = AppState::new();
        let before = state.filters_version();
        state.set_filters(vec![FilterRule::min_level(LogLevel::Warn)])?;
        assert_eq!(state.filters_version(), before + 1);
        Ok(())
    }

    #[test]
    fn invalid_filters_are_rejected_without_clobbering_state() -> Result<()> {
        let state = AppState::new();
        state.set_filters(vec![FilterRule::min_level(LogLevel::Warn)])?;
        let version = state.filters_version();

        let bad = vec![FilterRule::new(
            "bad",
            FilterField::Message,
            FilterOp::Regex,
            "([unclosed",
        )];
        assert_eq!(
            state.set_filters(bad).map_err(|e| e.kind()),
            Err("invalidFilter")
        );

        assert_eq!(state.filters_version(), version, "version must not move");
        assert_eq!(state.filters().len(), 1, "previous rules must survive");
        Ok(())
    }

    #[test]
    fn session_records_reports_unknown_sessions() {
        let state = AppState::new();
        assert_eq!(
            state
                .session_records("nope", None)
                .map_err(|e| e.kind()),
            Err("sessionNotFound")
        );
        assert_eq!(state.session_count(), 0);
        assert!(state.session_ids().is_empty());
    }

    #[test]
    fn locks_recover_from_poisoning() {
        let mutex = Mutex::new(1_i32);
        let _ = std::panic::catch_unwind(|| {
            let _guard = mutex.lock().unwrap_or_else(PoisonError::into_inner);
            // Poison the mutex by unwinding while the guard is held.
            std::panic::resume_unwind(Box::new(()));
        });
        // Would return Err if poisoning were propagated.
        assert_eq!(*mutex.lock_ignore_poison(), 1);
    }
}
