//! droidlog — Android log collection desktop backend.
//!
//! Layering (each layer only depends on the ones to its left):
//!
//! ```text
//! error -> executor -> adb -> device  --\
//!             \-> source -> parser    ----> process -> commands
//!                     \-> ring, filter  /
//!                          state (shared, owns sessions)
//! ```
//!
//! Hard rules enforced across this crate:
//! * production paths never `unwrap` / `expect` / `panic` / index a slice;
//!   everything returns [`error::Result`], and the `clippy` deny attributes below
//!   make the rule machine-checkable rather than a matter of discipline;
//! * every type that crosses the IPC boundary is `serde`-serialisable with
//!   `camelCase` field names, so the TypeScript mirror in `src/types` stays a
//!   literal translation.

// The no-unwrap / no-panic policy is machine-enforced rather than a convention.
// It is applied to production paths only: unit tests may use `panic!`-style
// assertion helpers, which is what `cfg(not(test))` scopes out here. `cargo
// clippy` (and any normal build) checks the non-test compilation, so the
// restriction is still verified on every run.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod adb;
pub mod collect;
pub mod commands;
pub mod crash;
pub mod device;
pub mod error;
pub mod executor;
pub mod filter;
pub mod parser;
pub mod process;
pub mod ring;
pub mod source;
pub mod state;
pub mod types;

use tauri::Manager;

use state::AppState;

/// Registers the app's resource directory as a place to look for a bundled adb.
///
/// Failing to register is not fatal — the search simply continues with the SDK,
/// `PATH` and the well-known locations — so the error is reported on stderr
/// rather than aborting startup.
fn register_bundled_adb_dir(app: &tauri::AppHandle) {
    let Ok(resource_dir) = app.path().resource_dir() else {
        return;
    };
    if let Err(err) = adb::locate::register_resource_dirs(vec![resource_dir]) {
        eprintln!("droidlog: {err}");
    }
}

/// How long to wait for the frontend to reveal the window before doing it here.
const WINDOW_REVEAL_FALLBACK: std::time::Duration = std::time::Duration::from_secs(5);

/// Shows the main window if the frontend has not managed to.
///
/// The window is created hidden to avoid a white flash, which puts the app one
/// frontend bug away from being invisible with no way to tell why. A timer that
/// only ever *shows* cannot fight the frontend for control, so it is a safe net.
fn arm_window_reveal_fallback(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(WINDOW_REVEAL_FALLBACK).await;
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        // `unwrap_or(true)` treats an unreadable state as "already visible", so
        // a query failure never forces the window open.
        if window.is_visible().unwrap_or(true) {
            return;
        }
        eprintln!("droidlog: frontend did not reveal the window; showing it now");
        let _ = window.show();
    });
}

/// Builds and runs the Tauri application.
///
/// Never panics: both fallible steps (`build` and `run`) report through stderr
/// and the process exit code instead of unwinding.
pub fn run() {
    let context = tauri::generate_context!();

    let app = tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::probe_adb,
            commands::list_devices,
            commands::refresh_devices,
            commands::list_sources,
            commands::get_filters,
            commands::set_filters,
            commands::start_capture,
            commands::stop_capture,
            commands::stop_all_captures,
            commands::list_sessions,
            commands::drain_records,
            commands::resolve_app,
            commands::set_app_target,
            commands::get_app_target,
            commands::list_running_apps,
        commands::collect_crash,
        commands::collect_boot,
        commands::collect_recovery,
        commands::get_collect_report,
        ])
        .setup(|app| {
            // Register the bundled-resource directory before anything resolves
            // adb, so a shipped platform-tools outranks whatever is on PATH.
            register_bundled_adb_dir(app.handle());

            // Start the 2 s device poller. It pushes `droidlog://devices`
            // whenever the list or any device's probe result changes, which is
            // what makes a newly plugged phone appear without user action.
            device::watch::spawn(app.handle().clone());

            // The window starts hidden so it cannot flash before the frontend has
            // painted; the frontend reveals it. This is the safety net for the
            // case where the frontend never gets that far.
            arm_window_reveal_fallback(app.handle().clone());
            Ok(())
        })
        .build(context);

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            eprintln!("droidlog: failed to build application: {err}");
            std::process::exit(1);
        }
    };

    // `App::run` drives the platform event loop and returns `()`; failures inside
    // it are reported by Tauri itself rather than propagated here.
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            // Dropping each session handle signals its reader task, which kills
            // the adb child. Without this, closing the window during a capture
            // would leave orphaned `adb logcat` processes behind.
            let state = handle.state::<AppState>();
            process::stop_all(&state);
        }
    });
}
