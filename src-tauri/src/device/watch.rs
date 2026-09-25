//! Device discovery: a 2-second poll that pushes *changes* to the frontend.
//!
//! Why a watcher rather than a command the UI calls:
//!
//! * "plug in the phone and it appears" cannot be built on pull alone — the app
//!   has no way to learn that the USB cable was attached;
//! * `adb devices -l` is cheap, but probing a device (`getprop` + `su -c id`) is
//!   two extra round trips, so probes are cached per device appearance and only
//!   re-run when a device is new, changed, or explicitly re-requested.
//!
//! The loop emits only on **change** (compared by serialised fingerprint), so a
//! steady device list costs the webview nothing.
//!
//! Deliberate details:
//!
//! * the server is ensured once on the first tick and then *reused*, so a poll
//!   every two seconds does not spawn `adb start-server` forever
//!   ([`Adb::devices_output_reusing_server`]);
//! * a device that cannot be probed still appears in the list, with the missing
//!   fields left empty — an offline device is information, not a failure;
//! * a probe runs in `Adb` mode (`getprop` needs no privilege) while the root
//!   check is `su -c id`, which is the same command the Root executor path uses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::adb::{locate::AdbSource, Adb};
use crate::device::{self, DeviceInfo, DeviceState};
use crate::executor::ExecMode;
use crate::state::AppState;

/// Event carrying the full device list whenever it changes.
pub const EVENT_DEVICES: &str = "droidlog://devices";

/// Event carrying the followed application whenever its identity changes.
///
/// Emitted when the app's pids change — the normal case being an app restart,
/// which hands it a brand new pid and would otherwise leave the filter matching
/// nothing.
pub const EVENT_APP_TARGET: &str = "droidlog://app-target";

/// Poll period required by the spec.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Payload of [`EVENT_DEVICES`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicesEvent {
    /// Whether a usable adb binary was found.
    pub adb_available: bool,
    /// The resolved adb path, when available.
    pub adb_program: Option<String>,
    /// How it was resolved.
    pub adb_source: Option<AdbSource>,
    /// adb's version string, when it could be read.
    pub adb_version: Option<String>,
    /// The current device list.
    pub devices: Vec<DeviceInfo>,
    /// Why adb could not be queried, when that is the case.
    pub error: Option<String>,
}

/// Handle used by commands to poke the poller.
#[derive(Debug, Default)]
pub struct DeviceWatcher {
    /// Wakes the loop for an immediate tick.
    wake: Notify,
    /// Forces a full re-probe (version + root) of every device on the next tick.
    force_probe: AtomicBool,
    /// Forces the next tick to emit even when nothing changed.
    ///
    /// Needed because the poller's first emit routinely happens *before* the
    /// webview has registered its listener, and the poller only speaks on change
    /// — so without a way to ask for a repeat, the frontend would permanently
    /// miss the probe results (no version, no root badge).
    resend: AtomicBool,
}

impl DeviceWatcher {
    /// Creates a watcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests an immediate tick. A permit is stored if the loop is not
    /// currently waiting, so a request made between ticks is never lost.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Requests an immediate tick **and** a fresh probe of every device.
    ///
    /// Also forces the result to be re-emitted, which is what a UI refresh
    /// button wants: it must never be a no-op just because the data happens to
    /// match what was sent before.
    pub fn request_probe(&self) {
        self.force_probe.store(true, Ordering::Relaxed);
        self.resend.store(true, Ordering::Relaxed);
        self.wake();
    }

    /// Consumes the pending force-probe request.
    fn take_force_probe(&self) -> bool {
        self.force_probe.swap(false, Ordering::Relaxed)
    }

    /// Consumes the pending re-send request.
    fn take_resend(&self) -> bool {
        self.resend.swap(false, Ordering::Relaxed)
    }

    /// Waits for the poll interval or for a wake-up, whichever comes first.
    async fn wait_for_next_tick(&self) {
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = self.wake.notified() => {}
        }
    }
}

/// Cached probe result for one device, keyed by something that invalidates it.
#[derive(Debug, Clone)]
struct ProbeCache {
    /// The `state`/`model` this probe was taken under. A reconnect or a
    /// different device on the same serial invalidates the entry.
    key: String,
    android_version: Option<String>,
    sdk: Option<i32>,
    root_available: bool,
    root_reason: Option<String>,
}

/// Identity of a device appearance, used as the probe cache key.
fn probe_key(device: &DeviceInfo) -> String {
    format!("{}|{}|{}", device.state_raw, device.model.as_deref().unwrap_or(""), device.serial)
}

/// Serialised identity of the whole list, used to suppress duplicate events.
fn fingerprint(devices: &[DeviceInfo]) -> String {
    serde_json::to_string(devices).unwrap_or_else(|_| {
        // Serialising a plain struct cannot realistically fail; if it somehow
        // does, fall back to a value that forces the next tick to re-emit rather
        // than silently freezing the UI.
        devices
            .iter()
            .map(|device| format!("{}:{}", device.serial, device.state_raw))
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// Starts the polling loop on Tauri's async runtime.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run(app).await;
    });
}

/// The polling loop. Never returns except when the app is shutting down.
async fn run(app: AppHandle) {
    let mut cache: HashMap<String, ProbeCache> = HashMap::new();
    let mut last_fingerprint: Option<String> = None;
    let mut last_payload: Option<DevicesEvent> = None;
    let mut first_tick = true;

    loop {
        let watcher = {
            let state = app.state::<AppState>();
            state.device_watcher()
        };
        let force = watcher.take_force_probe();
        let resend = watcher.take_resend();

        let event = poll_once(&app, &mut cache, force, first_tick).await;
        first_tick = false;

        let changed = resend
            || last_fingerprint.as_deref() != Some(fingerprint(&event.devices).as_str())
            || last_payload.as_ref().is_none_or(|previous| {
                previous.adb_available != event.adb_available
                    || previous.error != event.error
                    || previous.adb_program != event.adb_program
            });

        if changed {
            last_fingerprint = Some(fingerprint(&event.devices));
            last_payload = Some(event.clone());
            // A failed emit means no window is listening yet; the next tick
            // re-evaluates and the frontend can also pull via `list_devices`.
            let _ = app.emit(EVENT_DEVICES, event);
        }

        watcher.wait_for_next_tick().await;
    }
}

/// One poll: resolve adb, list devices, probe what needs probing.
///
/// Always returns a payload — a missing device or a broken adb is reported in
/// the payload rather than as an error, because the UI has a state for each.
async fn poll_once(
    app: &AppHandle,
    cache: &mut HashMap<String, ProbeCache>,
    force_probe: bool,
    first_tick: bool,
) -> DevicesEvent {
    let adb = match Adb::discover() {
        Ok(adb) => adb,
        Err(err) => {
            cache.clear();
            return DevicesEvent {
                adb_available: false,
                adb_program: None,
                adb_source: None,
                adb_version: None,
                devices: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let version = adb.version().await.ok();

    // The first tick must not let a piped client bootstrap the daemon; later
    // ticks reuse the server and rely on the query's own recovery.
    let queried = if first_tick {
        adb.devices_output().await
    } else {
        adb.devices_output_reusing_server().await
    };

    let output = match queried {
        Ok(output) => output,
        Err(err) => {
            // Keep the last known device list rather than blanking the UI: a
            // single failed poll usually means the daemon hiccuped, not that the
            // phone was unplugged.
            let devices = app
                .state::<AppState>()
                .last_devices();
            return DevicesEvent {
                adb_available: true,
                adb_program: Some(adb.program().to_owned()),
                adb_source: Some(adb.source()),
                adb_version: version,
                devices,
                error: Some(err.to_string()),
            };
        }
    };

    let mut devices = device::parse_device_list(&output.stdout);

    // Drop cache entries for devices that are gone so a re-plug re-probes.
    let present: Vec<String> = devices.iter().map(|d| d.serial.clone()).collect();
    cache.retain(|serial, _| present.contains(serial));

    for device in devices.iter_mut() {
        if !device.is_usable() {
            continue;
        }

        let key = probe_key(device);
        let needs_probe = force_probe
            || cache
                .get(&device.serial)
                .is_none_or(|entry| entry.key != key);

        if needs_probe {
            // A failed probe leaves the fingerprint fields empty on purpose; the
            // device row is still worth showing.
            let entry = match device::probe_device(&adb, &device.serial).await {
                Ok(probe) => ProbeCache {
                    key: key.clone(),
                    android_version: probe.android_version,
                    sdk: probe.sdk,
                    root_available: probe.root_available,
                    root_reason: probe.root_reason,
                },
                Err(err) => ProbeCache {
                    key: key.clone(),
                    android_version: None,
                    sdk: None,
                    root_available: false,
                    root_reason: Some(err.to_string()),
                },
            };
            cache.insert(device.serial.clone(), entry);
        }

        if let Some(entry) = cache.get(&device.serial) {
            device.android_version.clone_from(&entry.android_version);
            device.sdk = entry.sdk;
            device.root_available = Some(entry.root_available);
            device.root_reason.clone_from(&entry.root_reason);
        }
    }

    // Publish so a later failed poll can keep showing this list.
    {
        let state = app.state::<AppState>();
        state.set_last_devices(devices.clone());
    }

    // Re-resolve the followed application when its cache has aged out. Piggy-
    // backing on the 2 s poll is what makes "the app restarted, so its pid
    // changed" self-healing without polling anything extra.
    refresh_active_target(app, &adb).await;

    DevicesEvent {
        adb_available: true,
        adb_program: Some(adb.program().to_owned()),
        adb_source: Some(adb.source()),
        adb_version: version,
        devices,
        error: None,
    }
}

/// Re-resolves the followed application once its cached resolution expires.
///
/// This is what satisfies "the app restarted, so its pid changed": the pid list
/// is refreshed within [`crate::device::resolve::CACHE_TTL`] of the restart, and
/// the frontend is told when the identity actually changed. A refresh that
/// yields the same identity updates state silently — there is nothing to show.
async fn refresh_active_target(app: &AppHandle, adb: &Adb) {
    let (previous, resolver) = {
        let state = app.state::<AppState>();
        let Some(previous) = state.app_target() else {
            return;
        };
        (previous, state.app_resolver())
    };

    if !resolver.is_stale(&previous.input) {
        return;
    }

    let Ok(fresh) = crate::device::resolve::resolve(
        adb,
        previous.mode,
        &previous.serial,
        &previous.input,
        &resolver,
        true,
    )
    .await
    else {
        // A failed refresh leaves the existing target alone: a transient adb
        // hiccup must not silently drop the user's filter.
        return;
    };

    let state = app.state::<AppState>();
    if previous.same_identity(&fresh) {
        // Keep the timestamps fresh without waking the frontend.
        state.refresh_app_target_quietly(fresh);
        return;
    }

    eprintln!(
        "droidlog: app target '{}' changed: {:?} -> {:?}",
        fresh.input, previous.pids, fresh.pids
    );
    state.set_app_target(Some(fresh.clone()));
    let _ = app.emit(EVENT_APP_TARGET, Some(fresh));
}

/// Modes a device can be driven in, given its probe result.
///
/// The UI uses this to decide whether Root mode is offerable; it lives here so
/// the rule sits next to the probe that produces it.
#[must_use]
pub fn root_is_offerable(device: &DeviceInfo) -> bool {
    device.state == DeviceState::Device && device.root_available != Some(false)
}

/// The mode to default to for a device: `Root` when it works, else `Adb`.
#[must_use]
pub fn default_mode(device: &DeviceInfo) -> ExecMode {
    if device.root_available == Some(true) {
        ExecMode::Root
    } else {
        ExecMode::Adb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(serial: &str, state_raw: &str, model: Option<&str>) -> DeviceInfo {
        let mut device = DeviceInfo::new(serial, DeviceState::from_token(state_raw), state_raw);
        device.model = model.map(str::to_owned);
        device
    }

    #[test]
    fn probe_key_changes_when_the_device_appearance_changes() {
        let base = device("SER", "device", Some("Pixel"));
        assert_eq!(probe_key(&base), probe_key(&base.clone()));

        // A reconnect (state change) or a different model must invalidate it.
        let offline = device("SER", "offline", Some("Pixel"));
        assert_ne!(probe_key(&base), probe_key(&offline));

        let other_model = device("SER", "device", Some("Other"));
        assert_ne!(probe_key(&base), probe_key(&other_model));
    }

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        let a = device("A", "device", Some("M"));
        let b = device("B", "device", Some("M"));
        assert_eq!(fingerprint(&[a.clone(), b.clone()]), fingerprint(&[a.clone(), b.clone()]));
        assert_ne!(fingerprint(&[a.clone(), b.clone()]), fingerprint(&[b, a]));
    }

    #[test]
    fn fingerprint_notices_field_level_changes() {
        // The whole point of polling: a root status or version change must
        // produce a new payload even though serial and model are unchanged.
        let plain = device("A", "device", Some("M"));
        let mut probed = plain.clone();
        probed.android_version = Some("16".to_owned());
        probed.root_available = Some(true);
        assert_ne!(fingerprint(&[plain]), fingerprint(&[probed]));
    }

    #[test]
    fn poll_interval_matches_the_spec() {
        assert_eq!(POLL_INTERVAL, Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_watcher_wake_is_not_lost_between_ticks() {
        // `Notify` stores a permit, so a request made while the loop is busy is
        // still honoured the next time it waits.
        let watcher = DeviceWatcher::new();
        watcher.wake();
        tokio::time::timeout(Duration::from_millis(500), watcher.wake.notified())
            .await
            .expect("a wake requested before waiting must be delivered");
    }

    #[test]
    fn force_probe_request_is_consumed_once() {
        let watcher = DeviceWatcher::new();
        assert!(!watcher.take_force_probe());
        watcher.request_probe();
        assert!(watcher.take_force_probe());
        assert!(!watcher.take_force_probe(), "must not stay latched");
    }

    #[test]
    fn a_probe_request_also_forces_a_resend() {
        // A refresh button that silently does nothing because the data matches
        // the previous payload is the bug this guards.
        let watcher = DeviceWatcher::new();
        assert!(!watcher.take_resend());
        watcher.request_probe();
        assert!(watcher.take_resend(), "probe request must re-emit");
        assert!(!watcher.take_resend(), "must not stay latched");
    }

    #[test]
    fn a_plain_wake_does_not_force_a_resend() {
        // Ordinary ticks stay quiet when nothing changed.
        let watcher = DeviceWatcher::new();
        watcher.wake();
        assert!(!watcher.take_resend());
        assert!(!watcher.take_force_probe());
    }

    #[test]
    fn root_mode_is_offered_only_where_root_was_proven_or_unknown() {
        let mut usable = device("A", "device", None);
        usable.root_available = Some(true);
        assert!(root_is_offerable(&usable));
        assert_eq!(default_mode(&usable), ExecMode::Root);

        let mut no_root = device("A", "device", None);
        no_root.root_available = Some(false);
        assert!(!root_is_offerable(&no_root));
        assert_eq!(default_mode(&no_root), ExecMode::Adb);

        // Unknown root (not probed yet) stays offerable so the user is not
        // blocked before the first probe completes.
        let unknown = device("A", "device", None);
        assert!(root_is_offerable(&unknown));

        // An offline device cannot be captured from at all.
        let mut offline = device("A", "offline", None);
        offline.root_available = Some(true);
        assert!(!root_is_offerable(&offline));
    }
}
