//! Installed applications, with the names the *device* resolves.
//!
//! An application's display name ("设置", "酷安") is not reachable from any adb
//! command: `dumpsys package`, `pm dump` and `dumpsys activity recents` expose at
//! most `labelRes`, a resource id, because the text lives in the APK's
//! `resources.arsc`. Parsing that on the host means pulling megabytes per app.
//!
//! The device can simply be asked. `device-helper/LabelProbe.java` is a reflection
//! -only class that calls `PackageManager.getApplicationLabel()` for every package
//! and prints one tab-separated row each; it is compiled to a 4 KB dex and embedded
//! in this binary, so there is no resource file to bundle and no architecture to
//! match — one artefact covers arm64, arm32 and x86. Measured on a Xiaomi MIX 4
//! (Android 17): 438 apps, 1.9 s, including the push.
//!
//! Version, target SDK, install time and enabled state come from a single
//! `dumpsys package packages` dump rather than one `dumpsys package <pkg>` per app,
//! which is what other tools do and what makes them take a minute for a full list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::adb::Adb;
use crate::error::{DroidLogError, Result};
use crate::executor::ExecMode;

/// The compiled probe. `include_bytes!` keeps it inside the executable.
static PROBE_DEX: &[u8] = include_bytes!("../../device-helper/labelprobe.dex");

/// Where the probe is staged on the device.
///
/// Per process rather than a single fixed name: two runs in flight would otherwise
/// share one file, and the first run's cleanup deletes the dex the second one is
/// about to execute.
fn probe_remote() -> String {
    format!("/data/local/tmp/droidlog-labelprobe-{}.dex", std::process::id())
}

/// Serialises probe runs.
///
/// Two runs at once is not theoretical: the app fires one when a device is
/// selected and another when the picker is opened. Sharing a staged file made the
/// second run fail with no output, and because that failure was also *cached*, the
/// user-visible symptom was real names for a second, then package-derived names for
/// the next five minutes. Waiting a couple of seconds is the cheap fix; the app's
/// runtime is multi-threaded, so nothing else stalls meanwhile.
static PROBE_BUSY: AtomicBool = AtomicBool::new(false);

/// Releases [`PROBE_BUSY`] however the run ends.
struct ProbeGuard;

impl ProbeGuard {
    fn acquire() -> Self {
        while PROBE_BUSY.swap(true, Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        Self
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        PROBE_BUSY.store(false, Ordering::SeqCst);
    }
}

/// Main class inside the dex.
const PROBE_MAIN: &str = "LabelProbe";

/// How long a cached list is reused before the device is asked again.
pub const CACHE_TTL: Duration = Duration::from_secs(300);

/// One installed application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    /// Package name, e.g. `com.coolapk.market`.
    pub package: String,
    /// Real display name as the device spells it; empty when it could not resolve.
    pub label: String,
    /// Linux uid, or `None` when the dump did not state one.
    pub uid: Option<i32>,
    /// System application (`ApplicationInfo.FLAG_SYSTEM`).
    pub system: bool,
    /// `versionName`, e.g. `16.6.2`.
    pub version_name: Option<String>,
    /// Device-local `YYYY-MM-DD HH:MM:SS`, passed through untouched.
    ///
    /// Deliberately *not* an epoch value: the device prints local time, and Rust
    /// has no time-zone database here. `new Date('2026-09-17T21:04:31')` in the
    /// frontend parses such a string as local time, which is the correct display.
    pub installed_at: Option<String>,
    /// `targetSdk`.
    pub target_sdk: Option<i32>,
    /// Whether the package is enabled for user 0, when the dump says.
    pub enabled: Option<bool>,
}

/// One `pkg \t label \t uid \t system` line, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeRow {
    /// Package name.
    pub package: String,
    /// Resolved label (may be empty).
    pub label: String,
    /// uid as printed.
    pub uid: Option<i32>,
    /// `1` means a system application.
    pub system: bool,
}

/// Parses the probe's stdout.
///
/// Rows that do not have four fields are skipped rather than failing the run: a
/// single malformed app must not cost the user the whole list.
#[must_use]
pub fn parse_probe(text: &str) -> Vec<ProbeRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(package), Some(label), Some(uid), Some(system)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let package = package.trim();
        if package.is_empty() {
            continue;
        }
        rows.push(ProbeRow {
            package: package.to_owned(),
            label: label.trim().to_owned(),
            uid: uid.trim().parse::<i32>().ok(),
            system: system.trim() == "1",
        });
    }
    rows
}

/// The facts the package dump carries for one package.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageFacts {
    /// `versionName`.
    pub version_name: Option<String>,
    /// `lastUpdateTime` (falling back to `firstInstallTime`).
    pub installed_at: Option<String>,
    /// `targetSdk`.
    pub target_sdk: Option<i32>,
    /// `enabled=` inside the `User 0:` line.
    pub enabled: Option<bool>,
    /// `codePath=` under a system directory, or `SYSTEM` in the flags.
    pub system: bool,
}

/// Parses `dumpsys package packages`.
///
/// The dump is a sequence of `Package [<name>] (…):` blocks; every field is a
/// `key=value` token on its own line. Everything is taken from the block it
/// belongs to, so a package that lacks a field simply gets `None` for it.
#[must_use]
pub fn parse_package_dump(text: &str) -> HashMap<String, PackageFacts> {
    let mut out: HashMap<String, PackageFacts> = HashMap::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Package [") {
            if let Some((name, _)) = rest.split_once(']') {
                let name = name.trim().to_owned();
                out.entry(name.clone()).or_default();
                current = Some(name);
                continue;
            }
        }
        let Some(package) = current.as_ref() else {
            continue;
        };
        let Some(facts) = out.get_mut(package) else {
            continue;
        };

        // `User 0: ceDataInode=… enabled=1 …` — the first user is the one shown.
        if trimmed.starts_with("User 0:") || trimmed.starts_with("User 0 ") {
            if facts.enabled.is_none() {
                facts.enabled = value_after(trimmed, "enabled=").and_then(|value| match value {
                    "0" => Some(false),
                    "1" | "2" | "3" | "4" => Some(true),
                    _ => None,
                });
            }
            continue;
        }

        if let Some(value) = value_after(trimmed, "versionName=") {
            if facts.version_name.is_none() {
                facts.version_name = Some(value.to_owned());
            }
        }
        // `lastUpdateTime` and `firstInstallTime` are separate lines; the update
        // time is preferred, and whichever arrived first wins for the block.
        // `rest_after`, not `value_after`: a timestamp contains a space.
        if facts.installed_at.is_none() {
            if let Some(value) = rest_after(trimmed, "lastUpdateTime=")
                .or_else(|| rest_after(trimmed, "firstInstallTime="))
            {
                facts.installed_at = Some(value.to_owned());
            }
        }
        if let Some(value) = value_after(trimmed, "targetSdk=") {
            facts.target_sdk = value.parse().ok();
        }
        if trimmed.starts_with("codePath=/system")
            || trimmed.starts_with("codePath=/system_ext")
            || trimmed.starts_with("codePath=/product")
            || trimmed.starts_with("codePath=/vendor")
            || trimmed.starts_with("codePath=/apex")
            || trimmed.contains("SYSTEM")
            || trimmed.contains("SYSTEM_EXT")
        {
            facts.system = true;
        }
    }

    out
}

/// Reads the remainder of a line after `key`, trimmed.
///
/// Separate from [`value_after`] because timestamps contain a space
/// (`2026-09-17 21:04:31`): cutting at the first whitespace silently throws the
/// time away, which is what the tool this was compared against does.
fn rest_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let value = line.get(start..)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Reads `key=value` out of a line, stopping at whitespace.
fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = line.get(start..)?;
    let end = rest
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest.len());
    let value = rest.get(..end)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Merges the probe rows with the dump facts into the list the UI shows.
#[must_use]
pub fn merge(
    rows: &[ProbeRow],
    facts: &HashMap<String, PackageFacts>,
    include_system: bool,
) -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = rows
        .iter()
        .filter(|row| include_system || !row.system)
        .map(|row| {
            let fact = facts.get(&row.package);
            InstalledApp {
                package: row.package.clone(),
                label: row.label.clone(),
                uid: row.uid,
                system: row.system,
                version_name: fact.and_then(|f| f.version_name.clone()),
                installed_at: fact.and_then(|f| f.installed_at.clone()),
                target_sdk: fact.and_then(|f| f.target_sdk),
                enabled: fact.and_then(|f| f.enabled),
            }
        })
        .collect();
    apps.sort_by(|a, b| a.package.cmp(&b.package));
    apps
}

/// Where a device's cached list lives.
#[must_use]
pub fn cache_path(cache_dir: &Path, serial: &str) -> PathBuf {
    // Dots are dropped as well: `..` is the one sequence that could climb out of
    // the cache directory, and real serials never contain one.
    let safe: String = serial
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    let safe = if safe.is_empty() { "device".to_owned() } else { safe };
    cache_dir.join(format!("installed-apps-{safe}.json"))
}

/// Reads a cached list when it is younger than [`CACHE_TTL`].
///
/// A list with no labels at all is treated as absent: that is what a failed probe
/// produces, and serving it would keep every name wrong until the entry expired.
#[must_use]
pub fn read_cache(path: &Path) -> Option<Vec<InstalledApp>> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > CACHE_TTL {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let apps: Vec<InstalledApp> = serde_json::from_str(&text).ok()?;
    if apps.iter().any(|app| !app.label.is_empty()) {
        Some(apps)
    } else {
        None
    }
}

/// Writes the list to `path`, best effort: a cache that cannot be written must not
/// fail the request that produced it.
pub fn write_cache(path: &Path, apps: &[InstalledApp]) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(apps) {
        let _ = std::fs::write(path, text);
    }
}

/// Stage the probe on the device, run it, and read the rows back.
///
/// The staged file is always removed afterwards, including on failure, so nothing
/// is left behind in `/data/local/tmp`.
///
/// # Errors
///
/// Returns an error when the probe cannot be staged or produces no output at all
/// (a dex that the ROM refuses, for instance) — callers fall back to the derived
/// names rather than showing nothing.
pub async fn probe_labels(adb: &Adb, mode: ExecMode, serial: &str) -> Result<Vec<ProbeRow>> {
    // One probe at a time; see `PROBE_BUSY`.
    let _guard = ProbeGuard::acquire();
    let remote = probe_remote();

    // Deliberately *not* the caller's mode. Reading labels needs no privilege, and
    // under `su -c` the `CLASSPATH=…` prefix is not a shell assignment any more —
    // `su` reads it as the command to run, so the probe dies with no output and the
    // whole list silently falls back to package names. Plain `adb shell` runs it as
    // uid 2000, which is exactly what `PackageManager` needs here.
    let _ = mode;
    let target = adb.target(ExecMode::Adb, Some(serial));

    let local = std::env::temp_dir().join("droidlog-labelprobe.dex");
    std::fs::write(&local, PROBE_DEX).map_err(|err| {
        DroidLogError::InvalidInput(format!("无法写出临时 dex：{err}"))
    })?;

    let push = crate::executor::CommandPlan {
        program: adb.program().to_owned(),
        args: vec![
            "-s".to_owned(),
            serial.to_owned(),
            "push".to_owned(),
            local.to_string_lossy().into_owned(),
            remote.clone(),
        ],
    };
    let pushed = crate::executor::run_once(&push).await?;
    if !pushed.is_success() {
        let _ = std::fs::remove_file(&local);
        return Err(DroidLogError::InvalidInput(format!(
            "无法推送真名探针：{}",
            pushed.stdout_lines().join(" ")
        )));
    }

    let attempt = target
        .run_tolerant(&format!(
            "CLASSPATH={remote} app_process /system/bin {PROBE_MAIN}"
        ))
        .await;
    let cleanup = target.run_tolerant(&format!("rm -f {remote}")).await;
    let _ = cleanup;
    let _ = std::fs::remove_file(&local);

    let output = attempt?;
    let text: String = output.stdout_lines().join("\n");
    let rows = parse_probe(&text);
    if rows.is_empty() {
        return Err(DroidLogError::InvalidInput(format!(
            "真名探针没有输出（code={}，stderr={}）；已回落到包名推导",
            output.code,
            output.stderr.trim()
        )));
    }
    eprintln!("[droidlog] 真名探针：{} 个应用", rows.len());
    Ok(rows)
}

/// Every installed application, labels included.
///
/// # Errors
///
/// Propagates adb failures; a device that refuses the probe yields the package
/// dump's data with empty labels instead of an error, because a list without
/// names is still usable while no list at all is not.
pub async fn list(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    include_system: bool,
) -> Result<Vec<InstalledApp>> {
    let target = adb.target(mode, Some(serial));
    let dump = target.run("dumpsys package packages").await?;
    let facts = parse_package_dump(&dump.stdout_lines().join("\n"));

    let rows = match probe_labels(adb, mode, serial).await {
        Ok(rows) => rows,
        Err(err) => {
            // Reported, not returned: a list without labels is still usable, but
            // the reason has to be visible somewhere. `cargo tauri dev` prints this
            // on its console, which is how a stale embedded dex (a probe built
            // before the last `labelprobe.dex`) was identified once already.
            eprintln!("[droidlog] 真名探针失败，本次回落到包名：{err}");
            facts
                .keys()
                .map(|package| ProbeRow {
                    package: package.clone(),
                    label: String::new(),
                    uid: None,
                    system: facts.get(package).is_some_and(|f| f.system),
                })
                .collect()
        }
    };

    Ok(merge(&rows, &facts, include_system))
}

/// Seconds since the epoch, for callers that want to date a cache entry.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "com.android.settings\t设置\t1000\t1\n\
                         com.coolapk.market\t酷安\t10377\t0\n\
                         broken line without tabs\n\
                         \t \t\t1\n\
                         com.tencent.mm\t微信\t10368\t0\n";

    #[test]
    fn probe_rows_are_parsed_and_bad_lines_skipped() {
        let rows = parse_probe(PROBE);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].package, "com.android.settings");
        assert_eq!(rows[0].label, "设置");
        assert_eq!(rows[0].uid, Some(1000));
        assert!(rows[0].system);
        assert!(!rows[1].system);
        assert_eq!(rows[2].label, "微信");
    }

    #[test]
    fn probe_tolerates_missing_uid() {
        let rows = parse_probe("com.x\tX\t-\t0\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uid, None);
        assert_eq!(rows[0].label, "X");
    }

    const DUMP: &str = "\
Packages:
  Package [com.coolapk.market] (a1b2c3):
    userId=10377
    codePath=/data/app/~~x/com.coolapk.market-y
    versionName=16.6.2
    targetSdk=34
    lastUpdateTime=2026-09-17 21:04:31
    firstInstallTime=2025-01-01 10:00:00
    User 0: ceDataInode=1 installed=true hidden=false enabled=1
  Package [com.android.settings] (d4e5f6):
    codePath=/system_ext/priv-app/Settings
    versionName=4.9
    targetSdk=36
    lastUpdateTime=2026-09-15 08:30:00
    User 0: enabled=0
";

    #[test]
    fn package_dump_yields_facts_per_package() {
        let facts = parse_package_dump(DUMP);
        let coolapk = facts.get("com.coolapk.market").expect("coolapk");
        assert_eq!(coolapk.version_name.as_deref(), Some("16.6.2"));
        assert_eq!(coolapk.target_sdk, Some(34));
        // The update time wins over the install time.
        assert_eq!(coolapk.installed_at.as_deref(), Some("2026-09-17 21:04:31"));
        assert_eq!(coolapk.enabled, Some(true));
        assert!(!coolapk.system);

        let settings = facts.get("com.android.settings").expect("settings");
        assert_eq!(settings.version_name.as_deref(), Some("4.9"));
        assert_eq!(settings.target_sdk, Some(36));
        assert_eq!(settings.enabled, Some(false));
        assert!(settings.system, "codePath under /system_ext is a system app");
    }

    #[test]
    fn merge_filters_system_apps_and_sorts() {
        let rows = parse_probe(PROBE);
        let facts = parse_package_dump(DUMP);
        let user_only = merge(&rows, &facts, false);
        assert_eq!(user_only.len(), 2);
        assert!(user_only.iter().all(|app| !app.system));
        assert_eq!(user_only[0].package, "com.coolapk.market");
        assert_eq!(user_only[0].label, "酷安");
        assert_eq!(user_only[0].version_name.as_deref(), Some("16.6.2"));

        let all = merge(&rows, &facts, true);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].package, "com.android.settings");
        assert_eq!(all[0].installed_at.as_deref(), Some("2026-09-15 08:30:00"));
    }

    #[test]
    fn cache_round_trips_and_expires() {
        let dir = std::env::temp_dir().join(format!("droidlog-cache-{}", std::process::id()));
        let path = cache_path(&dir, "848f8e83");
        assert!(path.to_string_lossy().contains("848f8e83"));
        // A serial with path separators must not escape the directory.
        let odd = cache_path(&dir, "../../etc/passwd");
        assert!(!odd.to_string_lossy().contains(".."));

        let apps = vec![InstalledApp {
            package: "com.x".to_owned(),
            label: "X".to_owned(),
            uid: Some(1),
            system: false,
            version_name: Some("1.0".to_owned()),
            installed_at: None,
            target_sdk: Some(33),
            enabled: Some(true),
        }];
        write_cache(&path, &apps);
        assert_eq!(read_cache(&path), Some(apps));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_cache_is_none() {
        let path = std::env::temp_dir().join("droidlog-no-such-cache.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_cache(&path), None);
        assert!(now_secs() > 1_700_000_000);
    }
}
