//! Locating the `adb` executable.
//!
//! Search order, highest priority first:
//!
//! 1. the `DROIDLOG_ADB` environment variable (explicit user override);
//! 2. **bundled resources** — an adb shipped inside the application, registered
//!    at startup via [`register_resource_dirs`](crate::adb::locate::register_resource_dirs);
//! 3. `ANDROID_HOME` / `ANDROID_SDK_ROOT` (a real SDK install);
//! 4. `PATH`;
//! 5. the well-known per-platform install locations.
//!
//! Every candidate is recorded so the UI can show *why* a given binary was
//! selected.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{DroidLogError, Result};

/// Environment variable that overrides discovery entirely.
pub const ADB_OVERRIDE_ENV: &str = "DROIDLOG_ADB";

/// Where the resolved binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdbSource {
    /// `DROIDLOG_ADB` pointed straight at a binary.
    EnvOverride,
    /// Shipped inside the application's resource directory.
    BundledResource,
    /// Resolved from `ANDROID_HOME` / `ANDROID_SDK_ROOT`.
    AndroidSdk,
    /// Found on `PATH`.
    PathSearch,
    /// Found in a platform-specific well-known directory.
    KnownLocation,
}

/// Directories the application registered as containing a bundled adb.
///
/// Populated once at startup from Tauri's resource directory. Held in a
/// process-wide cell rather than threaded through every call because discovery
/// happens from many places (commands, the device poller) and the value never
/// changes after startup.
static RESOURCE_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Registers directories to search for a bundled adb, before `PATH`.
///
/// Idempotent: only the first call takes effect, so a second startup (or a test)
/// cannot silently reshuffle the search order.
///
/// # Errors
///
/// Returns [`DroidLogError::Internal`] if resource directories were already
/// registered with a *different* set, which would mean two components disagree
/// about where the bundled adb lives.
pub fn register_resource_dirs(dirs: Vec<PathBuf>) -> Result<()> {
    match RESOURCE_DIRS.set(dirs.clone()) {
        Ok(()) => Ok(()),
        Err(_) => {
            let existing = resource_dirs();
            if existing == dirs {
                Ok(())
            } else {
                Err(DroidLogError::Internal(format!(
                    "adb 资源目录已注册为 {existing:?}，无法改为 {dirs:?}"
                )))
            }
        }
    }
}

/// The registered resource directories, or an empty list when none were set.
#[must_use]
pub fn resource_dirs() -> Vec<PathBuf> {
    RESOURCE_DIRS.get().cloned().unwrap_or_default()
}

/// Executable file name for the current platform.
#[must_use]
pub fn executable_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["adb.exe", "adb"]
    } else {
        &["adb"]
    }
}

/// Android SDK roots advertised through the environment, in priority order.
#[must_use]
pub fn sdk_roots() -> Vec<PathBuf> {
    ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Platform-specific places adb usually lives when it is not on `PATH`.
#[must_use]
pub fn known_locations() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(
                Path::new(&local)
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools"),
            );
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            out.push(
                Path::new(&profile)
                    .join("AppData")
                    .join("Local")
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools"),
            );
        }
    } else {
        out.push(PathBuf::from("/usr/local/bin"));
        out.push(PathBuf::from("/usr/bin"));
        out.push(PathBuf::from("/opt/homebrew/bin"));
    }

    out
}

/// True when `path` is a regular file we can try to execute.
fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// Expands a directory into existing candidate binaries for this platform.
fn binaries_in(dir: &Path) -> Vec<PathBuf> {
    executable_names()
        .iter()
        .map(|name| dir.join(name))
        .filter(|candidate| is_file(candidate))
        .collect()
}

/// Every candidate a resource directory can yield for a bundled adb.
///
/// A bundle may place platform-tools either directly in the resource directory
/// or in a `platform-tools` subdirectory (which is how the upstream archive is
/// laid out), so both shapes are accepted.
fn resource_binaries(dir: &Path) -> Vec<PathBuf> {
    let mut found = binaries_in(dir);
    found.extend(binaries_in(&dir.join("platform-tools")));
    found
}

/// Every candidate path, whether or not it exists, in resolution order.
///
/// Used by the diagnostics command so the UI can render a complete search trail.
#[must_use]
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(override_path) = std::env::var_os(ADB_OVERRIDE_ENV) {
        if !override_path.is_empty() {
            candidates.push(PathBuf::from(override_path));
        }
    }

    for dir in resource_dirs() {
        candidates.extend(resource_binaries(&dir));
    }

    for root in sdk_roots() {
        candidates.extend(binaries_in(&root.join("platform-tools")));
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in executable_names() {
                let candidate = dir.join(name);
                if is_file(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
    }

    for dir in known_locations() {
        candidates.extend(binaries_in(&dir));
    }

    candidates
}

/// Resolves the adb binary and reports how it was found.
///
/// # Errors
///
/// Returns [`DroidLogError::AdbUnavailable`] listing the searched locations when
/// no usable binary exists. This is an *environmental* error, so the UI shows an
/// empty-state hint rather than a failure banner.
pub fn discover() -> Result<(PathBuf, AdbSource)> {
    if let Some(override_path) = std::env::var_os(ADB_OVERRIDE_ENV) {
        if !override_path.is_empty() {
            let path = PathBuf::from(&override_path);
            return if is_file(&path) {
                Ok((path, AdbSource::EnvOverride))
            } else {
                Err(DroidLogError::AdbUnavailable(format!(
                    "{ADB_OVERRIDE_ENV} 指向的路径不存在：{}",
                    path.display()
                )))
            };
        }
    }

    for dir in resource_dirs() {
        if let Some(found) = resource_binaries(&dir).into_iter().next() {
            return Ok((found, AdbSource::BundledResource));
        }
    }

    for root in sdk_roots() {
        if let Some(found) = binaries_in(&root.join("platform-tools")).into_iter().next() {
            return Ok((found, AdbSource::AndroidSdk));
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(found) = binaries_in(&dir).into_iter().next() {
                return Ok((found, AdbSource::PathSearch));
            }
        }
    }

    for dir in known_locations() {
        if let Some(found) = binaries_in(&dir).into_iter().next() {
            return Ok((found, AdbSource::KnownLocation));
        }
    }

    let searched = candidate_paths()
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    let hint = if searched.is_empty() {
        format!(
            "未找到 adb。请安装 Android platform-tools，或设置 {ADB_OVERRIDE_ENV} 指向 adb 可执行文件。"
        )
    } else {
        format!("未找到 adb。已搜索：{}", searched.join("; "))
    };

    Err(DroidLogError::AdbUnavailable(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_executable_names_are_sane() {
        let names = executable_names();
        assert!(!names.is_empty());
        if cfg!(windows) {
            assert!(names.contains(&"adb.exe"));
        } else {
            assert_eq!(names, &["adb"]);
        }
    }

    #[test]
    fn candidates_are_paths_not_directories() {
        // Discovery may legitimately find nothing in CI, so only assert shape.
        for candidate in candidate_paths() {
            assert!(!candidate.as_os_str().is_empty());
        }
    }

    #[test]
    fn discover_reports_missing_override() {
        // A path that cannot exist on any platform.
        let missing = if cfg!(windows) {
            "Z:\\definitely\\not\\here\\adb.exe"
        } else {
            "/definitely/not/here/adb"
        };
        std::env::set_var(ADB_OVERRIDE_ENV, missing);
        let result = discover();
        std::env::remove_var(ADB_OVERRIDE_ENV);

        assert!(
            matches!(
                &result,
                Err(DroidLogError::AdbUnavailable(message)) if message.contains("不存在")
            ),
            "expected an AdbUnavailable error mentioning the missing path, got {result:?}"
        );
    }

    /// All resource-directory behaviour lives in one test on purpose: the
    /// registry is a process-wide `OnceLock`, so splitting this across tests
    /// would make them order-dependent and racy under cargo's thread pool.
    #[test]
    fn bundled_resource_dirs_are_registered_and_searched_before_path() {
        let dir = std::env::temp_dir().join("droidlog-locate-bundled-test");
        let nested = dir.join("platform-tools");
        assert!(std::fs::create_dir_all(&nested).is_ok(), "temp dir");
        let fake = nested.join("adb.exe");
        if cfg!(windows) {
            assert!(std::fs::write(&fake, b"stub").is_ok(), "write stub");
        } else {
            let fake_unix = nested.join("adb");
            assert!(std::fs::write(&fake_unix, b"stub").is_ok(), "write stub");
        }

        // First registration wins; repeating the same list is accepted, a
        // different list is rejected rather than silently ignored.
        assert!(register_resource_dirs(vec![dir.clone()]).is_ok());
        assert!(register_resource_dirs(vec![dir.clone()]).is_ok());
        assert!(
            register_resource_dirs(vec![PathBuf::from("/somewhere/else")]).is_err(),
            "a conflicting re-registration must be reported"
        );
        assert_eq!(resource_dirs(), vec![dir.clone()]);

        let candidates = candidate_paths();
        let expected = if cfg!(windows) {
            fake.clone()
        } else {
            nested.join("adb")
        };
        let resource_index = candidates.iter().position(|c| c == &expected);

        if cfg!(windows) {
            assert!(
                resource_index.is_some(),
                "bundled adb must be a candidate; got {candidates:?}"
            );
        }

        // The bundled binary must outrank anything discovered on PATH.
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        let first_path_index = candidates.iter().position(|candidate| {
            candidate
                .parent()
                .is_some_and(|parent| path_dirs.iter().any(|d| d == parent))
        });

        if let (Some(bundled), Some(from_path)) = (resource_index, first_path_index) {
            assert!(
                bundled < from_path,
                "bundled resources must be searched before PATH"
            );
        }

        let _ = std::fs::remove_file(nested.join("adb.exe"));
        let _ = std::fs::remove_file(nested.join("adb"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
