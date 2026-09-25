//! Resolving "the app I care about" into the identifiers a filter can use.
//!
//! The user types one thing — a PID, a package name or a UID — and this module
//! turns it into all three, using only device-side commands that work in both
//! Adb and Root mode:
//!
//! ```text
//! package --(dumpsys | cmd package list -U | /data/system/packages.list)--> uid
//! uid     --(ps -A)--> pid list            package --(ps -A)--> pid list (incl. pkg:remote)
//! pid     --(/proc/<pid>/cmdline)--> package
//! ```
//!
//! Two design points worth stating up front, because they are what make the
//! feature actually usable:
//!
//! * **A PID list is not stable.** Android gives an app new PIDs after every
//!   restart, so a resolution is cached for [`CACHE_TTL`] and refreshed by the
//!   device poller; the UI shows the refresh. Anything that pushes a PID down to
//!   the device would go stale with it, which is why the *UID* is preferred for
//!   device-side narrowing (see [`AppTarget::prefilter`]).
//! * **Numeric input is ambiguous** — 1234 could be a PID or a UID. Rather than
//!   guess from the magnitude, the live `ps` table is consulted: a number that is
//!   running as a PID *is* a PID; one that appears as a UID *is* a UID. If it is
//!   neither, the app is not running, and the UI says so.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::adb::Adb;
use crate::device::probe::validate_package;
use crate::error::{DroidLogError, Result};
use crate::executor::ExecMode;
use crate::parser::LogRecord;
use crate::state::LockExt;

/// How long a resolution stays valid before it is recomputed.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// What the user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppTargetKind {
    /// A process id.
    Pid,
    /// A Linux uid.
    Uid,
    /// An Android package name.
    Package,
}

impl AppTargetKind {
    /// Short label for the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pid => "PID",
            Self::Uid => "UID",
            Self::Package => "包名",
        }
    }
}

/// How the captured stream was narrowed on the device, if at all.
///
/// Recorded so the client-side filter knows whether it still has to check the
/// app match: when logcat itself was told `--uid=`, everything the session
/// produces already belongs to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Prefilter {
    /// The device was told to filter by uid (`logcat --uid=`).
    Uid,
    /// The device was told to filter by pid (`logcat --pid=`).
    Pid,
    /// Nothing was pushed; the filter runs entirely on the host.
    None,
}

/// A resolved application, ready to be both displayed and matched against.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppTarget {
    /// Exactly what the user typed.
    pub input: String,
    /// What the input was interpreted as.
    pub kind: AppTargetKind,
    /// Device the resolution was taken against.
    ///
    /// Stored so the poller can re-resolve without the UI having to tell it —
    /// a pid list is only meaningful for the device it came from.
    pub serial: String,
    /// Execution mode the resolution used.
    pub mode: ExecMode,
    /// Package name, when known.
    pub package: Option<String>,
    /// Uid, when known.
    pub uid: Option<i32>,
    /// Every live process of the app, including `pkg:remote` children.
    pub pids: Vec<i32>,
    /// False when the app is not installed or not running.
    pub found: bool,
    /// Why nothing was found, for the UI to show.
    pub reason: Option<String>,
    /// Host time the resolution was taken, for "refreshed N s ago" in the UI.
    pub resolved_at_ms: u64,
    /// Device-side narrowing in effect for the current session.
    pub prefilter: Prefilter,
}

impl AppTarget {
    /// A target that resolved to nothing, with a reason for the UI.
    #[must_use]
    pub fn missing(
        input: &str,
        kind: AppTargetKind,
        serial: &str,
        mode: ExecMode,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            input: input.to_owned(),
            kind,
            serial: serial.to_owned(),
            mode,
            package: None,
            uid: None,
            pids: Vec::new(),
            found: false,
            reason: Some(reason.into()),
            resolved_at_ms: crate::process::now_ms(),
            prefilter: Prefilter::None,
        }
    }

    /// True when this resolution describes the same thing as `other`.
    ///
    /// Used by the poller to decide whether a refresh is worth telling the UI
    /// about: a new pid list after an app restart is; the same list is not.
    #[must_use]
    pub fn same_identity(&self, other: &Self) -> bool {
        self.found == other.found
            && self.uid == other.uid
            && self.package == other.package
            && self.pids == other.pids
    }

    /// One-line summary for the chip in the filter panel.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(package) = &self.package {
            parts.push(package.clone());
        }
        if let Some(uid) = self.uid {
            parts.push(format!("UID {uid}"));
        }
        if !self.pids.is_empty() {
            let pids = self
                .pids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("PIDs [{pids}]"));
        }
        if parts.is_empty() {
            parts.push(self.input.clone());
        }
        parts.join(" · ")
    }

    /// Whether a record belongs to the targeted application.
    ///
    /// Matching is deliberately generous across the three identities: a record
    /// matches if *any* known identity agrees. That keeps the filter correct for
    /// sources that report different fields — `logcat -v threadtime` carries pid
    /// and tag but no uid, while a `--uid=`-narrowed session carries nothing to
    /// check at all.
    #[must_use]
    pub fn matches(&self, record: &LogRecord) -> bool {
        if !self.found {
            // An unresolved target must not silently show the whole device:
            // the UI says "not running" instead, and this stays empty.
            return false;
        }
        if self.prefilter == Prefilter::Uid {
            // The device already restricted the stream to this uid.
            return true;
        }
        if let Some(pid) = record.pid {
            if self.pids.contains(&pid) {
                return true;
            }
        }
        if let (Some(uid), Some(record_uid)) = (self.uid, record.uid) {
            if uid == record_uid {
                return true;
            }
        }
        if let (Some(package), Some(record_package)) = (&self.package, &record.package) {
            if package == record_package {
                return true;
            }
        }
        false
    }

    /// Marks the device-side narrowing that a session will use.
    pub fn set_prefilter(&mut self, prefilter: Prefilter) {
        self.prefilter = prefilter;
    }
}

/* -------------------------------------------------------------------------- */
/* Pure parsers                                                              */
/* -------------------------------------------------------------------------- */

/// One row of `ps -A`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsEntry {
    /// Process id.
    pub pid: i32,
    /// The USER column, e.g. `u0_a123`, `root`, `system`.
    pub user: String,
    /// The NAME column: a package, `pkg:remote`, or a native binary.
    pub name: String,
}

/// Token position of a named column in a `ps` header.
///
/// Matching whole tokens rather than searching for a substring is what keeps
/// `PPID` from being read as the `PID` column — a mistake that silently reports
/// every process's parent as its own id.
fn token_index(header: &str, name: &str) -> Option<usize> {
    header.split_whitespace().position(|token| token == name)
}

/// Process-state letters used by Linux, for anchoring the trailing columns.
const STATE_LETTERS: &str = "SDIRTWZXVt";

/// Finds where the `NAME` column starts, by scanning the row **from the end**.
///
/// Counting columns from the header does not work on real output: `ps` leaves a
/// column blank when it has no value, and `WCHAN` is blank for most processes, so
/// an ordinary row (`root 1 0 10952440 9124 0 S init`) has eight tokens where the
/// header has nine. Addressing `tokens[8]` then reads past the end and the row is
/// dropped — on this device that discarded every process, including all apps.
///
/// Anchoring instead on the state letter (a single letter immediately preceded by
/// a hexadecimal `ADDR`) is stable whether or not the optional columns are
/// present.
fn name_token_index(tokens: &[&str]) -> Option<usize> {
    for index in (1..tokens.len()).rev() {
        let token = *tokens.get(index)?;
        let previous = *tokens.get(index - 1)?;
        let is_state = token.chars().count() == 1
            && token.chars().all(|c| STATE_LETTERS.contains(c));
        if is_state && previous.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(index + 1);
        }
    }
    None
}

/// Parses `ps -A` into rows.
///
/// `USER` and `PID` are the first two columns and are always present, so they are
/// addressed by their header position; everything from the state letter onwards
/// is anchored from the end (see [`name_token_index`]).
#[must_use]
pub fn parse_ps(text: &str) -> Vec<PsEntry> {
    let Some(header) = text.lines().find(|line| line.contains("PID")) else {
        return Vec::new();
    };
    let (Some(user_at), Some(pid_at)) = (token_index(header, "USER"), token_index(header, "PID"))
    else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for line in text.lines().skip_while(|line| *line != header).skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let (Some(user), Some(pid)) = (tokens.get(user_at), tokens.get(pid_at)) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        let Some(name_at) = name_token_index(&tokens) else {
            continue;
        };
        // NAME is the trailing column and may contain spaces.
        let Some(name) = tokens.get(name_at..).map(|rest| rest.join(" ")) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        rows.push(PsEntry {
            pid,
            user: (*user).to_owned(),
            name,
        });
    }
    rows
}

/// Maps a `ps` USER value to a uid.
///
/// Android prints `u<user>_a<app>` for app processes, where the uid is
/// `user * 100000 + 10000 + app`; daemons print a name or a bare number.
#[must_use]
pub fn uid_from_user(user: &str) -> Option<i32> {
    if let Ok(uid) = user.parse::<i32>() {
        return Some(uid);
    }
    if let Some(rest) = user.strip_prefix('u') {
        if let Some((user_id, app)) = rest.split_once("_a") {
            let user_id = user_id.parse::<i32>().ok()?;
            let app_id = app.parse::<i32>().ok()?;
            return Some(user_id.saturating_mul(100_000) + 10_000 + app_id);
        }
    }
    match user {
        "root" => Some(0),
        "system" => Some(1000),
        "radio" => Some(1001),
        "shell" => Some(2000),
        _ => None,
    }
}

/// Processes belonging to `package`, including its `package:remote` children.
#[must_use]
pub fn pids_for_package(rows: &[PsEntry], package: &str) -> Vec<i32> {
    let prefix = format!("{package}:");
    let mut pids: Vec<i32> = rows
        .iter()
        .filter(|row| row.name == package || row.name.starts_with(&prefix))
        .map(|row| row.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Processes owned by `uid`.
#[must_use]
pub fn pids_for_uid(rows: &[PsEntry], uid: i32) -> Vec<i32> {
    let mut pids: Vec<i32> = rows
        .iter()
        .filter(|row| uid_from_user(&row.user) == Some(uid))
        .map(|row| row.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Extracts the uid for `package` from `cmd package list packages -U` output.
///
/// Shape: `package:com.example uid:10123`.
#[must_use]
pub fn parse_uid_from_cmd_package(text: &str, package: &str) -> Option<i32> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("package:")?;
        let (name, uid) = rest.split_once(" uid:")?;
        if name.trim() != package {
            return None;
        }
        uid.split_whitespace().next()?.parse::<i32>().ok()
    })
}

/// Extracts the uid for `package` from `/data/system/packages.list`.
///
/// Shape: `com.example 10123 0 /data/user/0/com.example default:targetSdkVersion=34 ...`.
#[must_use]
pub fn parse_uid_from_package_list(text: &str, package: &str) -> Option<i32> {
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let name = fields.next()?;
        if name != package {
            return None;
        }
        fields.next()?.parse::<i32>().ok()
    })
}

/// Derives a package name from a `/proc/<pid>/cmdline` payload.
///
/// Reads the first NUL-separated argument and drops any `:remote` suffix, so a
/// child process maps back to the app that owns it. Returns `None` for native
/// binaries, which are paths or bracketed kernel threads rather than packages.
#[must_use]
pub fn package_from_cmdline(cmdline: &str) -> Option<String> {
    let first = cmdline.split('\0').next()?.trim();
    if first.is_empty() || first.starts_with('[') {
        return None;
    }
    let base = first.split(':').next().unwrap_or(first);
    let looks_like_package = base.contains('.')
        && !base.contains('/')
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
    if looks_like_package {
        Some(base.to_owned())
    } else {
        None
    }
}

/// One entry of the "running applications" picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningApp {
    /// Base package name.
    pub package: String,
    /// Uid derived from the `ps` USER column, when it could be read.
    pub uid: Option<i32>,
    /// Every live pid of the app, including `pkg:remote` children.
    pub pids: Vec<i32>,
}

/// Collapses a `ps` table into the distinct applications it is running.
///
/// Native binaries (`init`, `surfaceflinger`, …) are dropped: they are processes
/// but not applications, and offering them would make the picker useless.
#[must_use]
pub fn running_apps(rows: &[PsEntry]) -> Vec<RunningApp> {
    let mut by_package: HashMap<String, (Option<i32>, Vec<i32>)> = HashMap::new();
    for row in rows {
        // `pkg:remote` belongs to `pkg`.
        let base = row.name.split(':').next().unwrap_or(&row.name);
        let looks_like_package = base.contains('.')
            && !base.contains('/')
            && base
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
        if !looks_like_package {
            continue;
        }
        let entry = by_package
            .entry(base.to_owned())
            .or_insert((uid_from_user(&row.user), Vec::new()));
        if entry.0.is_none() {
            entry.0 = uid_from_user(&row.user);
        }
        entry.1.push(row.pid);
    }

    let mut apps: Vec<RunningApp> = by_package
        .into_iter()
        .map(|(package, (uid, mut pids))| {
            pids.sort_unstable();
            pids.dedup();
            RunningApp { package, uid, pids }
        })
        .collect();
    apps.sort_by(|a, b| a.package.cmp(&b.package));
    apps
}

/* -------------------------------------------------------------------------- */
/* Resolution with caching                                                    */
/* -------------------------------------------------------------------------- */

/// Cached resolutions, keyed by the raw user input.
#[derive(Debug)]
pub struct AppResolver {
    cache: Mutex<HashMap<String, (Instant, AppTarget)>>,
    /// Whether `logcat --uid=` works, per device serial. Not time-limited: a
    /// device does not change its logcat between polls.
    uid_support: Mutex<HashMap<String, bool>>,
    ttl: Duration,
}

impl AppResolver {
    /// Creates a resolver with the given cache lifetime.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            uid_support: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// A cached resolution, if one is still fresh.
    #[must_use]
    pub fn cached(&self, input: &str) -> Option<AppTarget> {
        let cache = self.cache.lock_ignore_poison();
        let (taken, target) = cache.get(input)?;
        if taken.elapsed() <= self.ttl {
            Some(target.clone())
        } else {
            None
        }
    }

    /// Whether a cached entry exists but has expired.
    #[must_use]
    pub fn is_stale(&self, input: &str) -> bool {
        let cache = self.cache.lock_ignore_poison();
        match cache.get(input) {
            Some((taken, _)) => taken.elapsed() > self.ttl,
            None => true,
        }
    }

    /// Stores a resolution.
    pub fn store(&self, target: &AppTarget) {
        self.cache
            .lock_ignore_poison()
            .insert(target.input.clone(), (Instant::now(), target.clone()));
    }

    /// Drops one entry.
    pub fn invalidate(&self, input: &str) {
        self.cache.lock_ignore_poison().remove(input);
    }

    /// Drops every entry.
    pub fn clear(&self) {
        self.cache.lock_ignore_poison().clear();
    }

    /// Records whether a device supports `logcat --uid=`.
    pub fn remember_uid_support(&self, serial: &str, supported: bool) {
        self.uid_support
            .lock_ignore_poison()
            .insert(serial.to_owned(), supported);
    }

    /// The remembered `logcat --uid=` support for a device.
    #[must_use]
    pub fn uid_support(&self, serial: &str) -> Option<bool> {
        self.uid_support.lock_ignore_poison().get(serial).copied()
    }
}

impl Default for AppResolver {
    fn default() -> Self {
        Self::new(CACHE_TTL)
    }
}

/// Runs `ps -A` and returns the parsed table.
///
/// # Errors
///
/// Propagates transport failures.
pub async fn ps_table(adb: &Adb, mode: ExecMode, serial: &str) -> Result<Vec<PsEntry>> {
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant("ps -A")
        .await?;
    Ok(parse_ps(&output.stdout))
}

/// Resolves `package` to a uid, trying all three sources in order.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] for a malformed package name, and
/// propagates transport failures. A package that simply has no uid returns
/// `Ok(None)`.
pub async fn package_uid(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    package: &str,
) -> Result<Option<i32>> {
    validate_package(package)?;
    let target = adb.target(mode, Some(serial));

    // 1. `dumpsys package` — always available, carries userId=.
    if let Ok(output) = target.run_tolerant(&format!("dumpsys package {package}")).await {
        if let Some(uid) = crate::device::probe::parse_uid_from_dumpsys(&output.stdout) {
            return Ok(Some(uid));
        }
    }

    // 2. `cmd package list packages -U` — no root needed, one shot for all apps.
    if let Ok(output) = target.run_tolerant("cmd package list packages -U").await {
        if let Some(uid) = parse_uid_from_cmd_package(&output.stdout, package) {
            return Ok(Some(uid));
        }
    }

    // 3. `/data/system/packages.list` — root only, but the most direct answer.
    if let Ok(output) = target
        .run_tolerant("cat /data/system/packages.list")
        .await
    {
        if let Some(uid) = parse_uid_from_package_list(&output.stdout, package) {
            return Ok(Some(uid));
        }
    }

    Ok(None)
}

/// Resolves the package that owns `pid`, via `/proc/<pid>/cmdline`.
///
/// # Errors
///
/// Propagates transport failures.
pub async fn pid_package(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    pid: i32,
) -> Result<Option<String>> {
    if pid <= 0 {
        return Err(DroidLogError::InvalidInput(format!("PID 无效：{pid}")));
    }
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant(&format!("cat /proc/{pid}/cmdline"))
        .await?;
    Ok(package_from_cmdline(&output.stdout))
}

/// Resolves one line of user input into a full [`AppTarget`].
///
/// A fresh resolution is reused from the cache while it is younger than the
/// resolver's TTL; pass `force` to bypass it.
///
/// # Errors
///
/// Propagates transport failures. "The app is not running" is **not** an error:
/// it comes back as an [`AppTarget`] with `found == false`, because it is a
/// normal state the UI renders.
pub async fn resolve(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    input: &str,
    resolver: &AppResolver,
    force: bool,
) -> Result<AppTarget> {
    let input = input.trim();
    if input.is_empty() {
        return Err(DroidLogError::InvalidInput("请输入 PID / 包名 / UID".to_owned()));
    }
    if !force {
        if let Some(hit) = resolver.cached(input) {
            return Ok(hit);
        }
    }

    let rows = ps_table(adb, mode, serial).await?;
    let target = resolve_from(input, &rows, adb, mode, serial).await?;
    resolver.store(&target);
    Ok(target)
}

/// The pure-ish core of [`resolve`], taking an already-fetched `ps` table.
async fn resolve_from(
    input: &str,
    rows: &[PsEntry],
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
) -> Result<AppTarget> {
    // Numeric input: let the live process table decide whether it is a PID or a
    // UID, instead of guessing from the magnitude (they overlap).
    if let Ok(number) = input.parse::<i32>() {
        if rows.iter().any(|row| row.pid == number) {
            let package = pid_package(adb, mode, serial, number).await?;
            let uid = match &package {
                Some(package) => package_uid(adb, mode, serial, package).await?,
                None => uid_from_user(
                    rows.iter()
                        .find(|row| row.pid == number)
                        .map(|row| row.user.as_str())
                        .unwrap_or_default(),
                ),
            };
            let pids = match &package {
                Some(package) => pids_for_package(rows, package),
                None => vec![number],
            };
            return Ok(AppTarget {
                input: input.to_owned(),
                kind: AppTargetKind::Pid,
                serial: serial.to_owned(),
                mode,
                package,
                uid,
                pids,
                found: true,
                reason: None,
                resolved_at_ms: crate::process::now_ms(),
                prefilter: Prefilter::None,
            });
        }

        let pids = pids_for_uid(rows, number);
        if !pids.is_empty() {
            return Ok(AppTarget {
                input: input.to_owned(),
                kind: AppTargetKind::Uid,
                serial: serial.to_owned(),
                mode,
                package: None,
                uid: Some(number),
                pids,
                found: true,
                reason: None,
                resolved_at_ms: crate::process::now_ms(),
                prefilter: Prefilter::None,
            });
        }

        return Ok(AppTarget::missing(
            input,
            AppTargetKind::Pid,
            serial,
            mode,
            format!("{input} 既不是运行中的 PID，也不是任何进程的 UID —— 应用未开启或不存在"),
        ));
    }

    // Otherwise it must be a package name.
    validate_package(input)?;
    let uid = package_uid(adb, mode, serial, input).await?;
    let mut pids = pids_for_package(rows, input);
    if pids.is_empty() {
        if let Some(uid) = uid {
            pids = pids_for_uid(rows, uid);
        }
    }

    if uid.is_none() && pids.is_empty() {
        return Ok(AppTarget::missing(
            input,
            AppTargetKind::Package,
            serial,
            mode,
            format!("未找到包 {input} —— 应用未安装或不存在"),
        ));
    }
    if pids.is_empty() {
        return Ok(AppTarget::missing(
            input,
            AppTargetKind::Package,
            serial,
            mode,
            format!("包 {input} 已安装但当前没有进程运行 —— 应用未开启"),
        ));
    }

    Ok(AppTarget {
        input: input.to_owned(),
        kind: AppTargetKind::Package,
        serial: serial.to_owned(),
        mode,
        package: Some(input.to_owned()),
        uid,
        pids,
        found: true,
        reason: None,
        resolved_at_ms: crate::process::now_ms(),
        prefilter: Prefilter::None,
    })
}

/// Probes whether this device's logcat understands `--uid=`.
///
/// Asked once per device and remembered: the answer is a property of the
/// installed logcat, not of the moment. `--help` is used rather than a live
/// query so the probe costs nothing and cannot disturb a running capture.
///
/// # Errors
///
/// Propagates transport failures.
pub async fn probe_logcat_uid(
    adb: &Adb,
    mode: ExecMode,
    serial: &str,
    resolver: &AppResolver,
) -> Result<bool> {
    if let Some(known) = resolver.uid_support(serial) {
        return Ok(known);
    }
    let output = adb
        .target(mode, Some(serial))
        .run_tolerant("logcat --help")
        .await?;
    // The usage text lists `--uid=<uid>...` on builds that support it.
    let supported = output.stdout.contains("--uid") || output.stderr.contains("--uid");
    resolver.remember_uid_support(serial, supported);
    Ok(supported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "\
USER      PID   PPID  VSZ    RSS  WCHAN    ADDR  S NAME
root        1     0   12345  1234 0        0     S init
system   1612     1  234567 23456 0        0     S system_server
u0_a123  5063  1612 1234567 45678 0       0     S com.example.app
u0_a123  5602  1612 1234567 45678 0       0     S com.example.app:remote
u0_a200  6000  1612 1234567 45678 0       0     S com.other.app
u10_a45  7000  1612 1234567 45678 0       0     S com.work.profile
";

    #[test]
    fn ps_columns_come_from_the_header() {
        let rows = parse_ps(PS);
        assert_eq!(rows.len(), 6, "every data row must parse: {rows:?}");
        assert_eq!(rows.first().map(|r| r.pid), Some(1));
        assert_eq!(rows.first().map(|r| r.name.as_str()), Some("init"));
        assert_eq!(rows.get(1).map(|r| r.user.as_str()), Some("system"));
    }

    #[test]
    fn pid_is_not_read_from_the_ppid_column() {
        // `PPID` contains `PID`; a naive `find` would return the parent's id for
        // every row, which is exactly the kind of bug that looks like "the
        // filter matches the wrong app".
        let rows = parse_ps(PS);
        let app = rows.iter().find(|row| row.name == "com.example.app");
        assert_eq!(app.map(|row| row.pid), Some(5063));
    }

    #[test]
    fn package_pids_include_remote_processes() {
        let rows = parse_ps(PS);
        assert_eq!(pids_for_package(&rows, "com.example.app"), vec![5063, 5602]);
        assert_eq!(pids_for_package(&rows, "com.other.app"), vec![6000]);
        assert!(pids_for_package(&rows, "com.absent").is_empty());
    }

    #[test]
    fn package_match_does_not_cross_into_a_similar_name() {
        // `com.example.app2` must not be captured by the `com.example.app` prefix.
        let rows = parse_ps(
            "USER PID PPID S NAME\n\
             u0_a123 1 0 S com.example.app\n\
             u0_a123 2 0 S com.example.app2\n",
        );
        assert_eq!(pids_for_package(&rows, "com.example.app"), vec![1]);
    }

    #[test]
    fn uid_parsing_covers_app_users_and_daemons() {
        assert_eq!(uid_from_user("u0_a123"), Some(10123));
        assert_eq!(uid_from_user("u10_a45"), Some(1_010_045));
        assert_eq!(uid_from_user("root"), Some(0));
        assert_eq!(uid_from_user("system"), Some(1000));
        assert_eq!(uid_from_user("10123"), Some(10123));
        assert_eq!(uid_from_user("weird"), None);
    }

    #[test]
    fn uid_lookup_returns_running_processes() {
        let rows = parse_ps(PS);
        assert_eq!(pids_for_uid(&rows, 10123), vec![5063, 5602]);
        assert_eq!(pids_for_uid(&rows, 1_010_045), vec![7000]);
        assert!(pids_for_uid(&rows, 99999).is_empty());
    }

    #[test]
    fn uid_from_cmd_package_is_read() {
        let text = "package:com.android.phone uid:1001\npackage:com.example.app uid:10123\n";
        assert_eq!(parse_uid_from_cmd_package(text, "com.example.app"), Some(10123));
        assert_eq!(parse_uid_from_cmd_package(text, "com.absent"), None);
    }

    #[test]
    fn uid_from_packages_list_is_read() {
        let text = "com.example.app 10123 0 /data/user/0/com.example.app default:targetSdkVersion=34 none 0 1 0\n\
                    com.other.app 10200 1 /data/user/0/com.other.app default:targetSdkVersion=33 none 0 0 0\n";
        assert_eq!(parse_uid_from_package_list(text, "com.other.app"), Some(10200));
        assert_eq!(parse_uid_from_package_list(text, "com.absent"), None);
    }

    #[test]
    fn cmdline_yields_the_base_package() {
        assert_eq!(
            package_from_cmdline("com.example.app\0"),
            Some("com.example.app".to_owned())
        );
        assert_eq!(
            package_from_cmdline("com.example.app:remote\0"),
            Some("com.example.app".to_owned())
        );
    }

    #[test]
    fn cmdline_rejects_native_processes() {
        assert_eq!(package_from_cmdline("/system/bin/surfaceflinger\0"), None);
        assert_eq!(package_from_cmdline("[kworker/0:1]\0"), None);
        assert_eq!(package_from_cmdline("\0"), None);
        assert_eq!(package_from_cmdline(""), None);
    }

    fn target(pids: Vec<i32>, uid: Option<i32>, prefilter: Prefilter) -> AppTarget {
        AppTarget {
            input: "x".to_owned(),
            kind: AppTargetKind::Package,
            serial: "SER".to_owned(),
            mode: ExecMode::Adb,
            package: Some("com.example.app".to_owned()),
            uid,
            pids,
            found: true,
            reason: None,
            resolved_at_ms: 0,
            prefilter,
        }
    }

    fn record(pid: Option<i32>) -> LogRecord {
        let mut record = LogRecord::new(crate::source::LogSourceKind::Logcat, 1, "raw");
        record.pid = pid;
        record
    }

    #[test]
    fn matching_accepts_any_known_identity() {
        let with_pids = target(vec![1234], None, Prefilter::None);
        assert!(with_pids.matches(&record(Some(1234))));
        assert!(!with_pids.matches(&record(Some(9999))));

        let mut with_uid = target(Vec::new(), Some(10123), Prefilter::None);
        let mut record_with_uid = record(None);
        record_with_uid.uid = Some(10123);
        assert!(with_uid.matches(&record_with_uid));
        with_uid.uid = Some(1);
        assert!(!with_uid.matches(&record_with_uid));
    }

    #[test]
    fn a_uid_prefiltered_session_accepts_everything_it_emits() {
        // The device already narrowed the stream, so re-checking a (possibly
        // stale) pid list here would drop the app's logs after a restart.
        let prefiltered = target(vec![1234], Some(10123), Prefilter::Uid);
        assert!(prefiltered.matches(&record(Some(4321))));
        assert!(prefiltered.matches(&record(None)));
    }

    #[test]
    fn an_unresolved_target_matches_nothing() {
        // Showing the whole device because the app is not running would be the
        // worst possible failure mode for "only this app".
        let missing = AppTarget::missing(
            "com.absent",
            AppTargetKind::Package,
            "SER",
            ExecMode::Adb,
            "未安装",
        );
        assert!(!missing.matches(&record(Some(1))));
        assert!(!missing.found);
    }

    #[test]
    fn summary_reads_like_the_ui_chip() {
        let full = target(vec![5063, 5602], Some(10123), Prefilter::Uid);
        assert_eq!(
            full.summary(),
            "com.example.app · UID 10123 · PIDs [5063,5602]"
        );
        let bare = target(Vec::new(), None, Prefilter::None);
        assert_eq!(bare.summary(), "com.example.app");
    }

    #[test]
    fn the_cache_honours_its_ttl() {
        let resolver = AppResolver::new(Duration::from_millis(50));
        let value = target(vec![1], Some(2), Prefilter::None);
        resolver.store(&value);
        assert!(resolver.cached("x").is_some());
        assert!(!resolver.is_stale("x"));
        std::thread::sleep(Duration::from_millis(70));
        assert!(resolver.cached("x").is_none(), "expired entries must not be served");
        assert!(resolver.is_stale("x"), "the poller uses this to refresh");
        resolver.invalidate("x");
        assert!(resolver.cached("x").is_none());
    }

    #[test]
    fn uid_support_is_remembered_per_device() {
        let resolver = AppResolver::default();
        assert_eq!(resolver.uid_support("SER"), None);
        resolver.remember_uid_support("SER", true);
        assert_eq!(resolver.uid_support("SER"), Some(true));
        assert_eq!(resolver.uid_support("OTHER"), None);
    }

    /// Verbatim `ps -A` from a Xiaomi pad (Android 16), trimmed to a few rows.
    ///
    /// Kept exactly as the device printed it, including the blank `WCHAN` column
    /// on most rows, because that blank column is precisely what a tidy fixture
    /// hides — and hiding it cost every process on the device.
    const REAL_PS: &str = "\
USER           PID  PPID        VSZ    RSS WCHAN            ADDR S NAME                       
root             1     0   10952440   9124                     0 S init
root             2     0          0      0                     0 S [kthreadd]
system        1612     1    5060380  88632                     0 S system_server
u0_a123       5063  1612    5312740 182456                     0 S com.example.app
u0_a123       5602  1612    5312740  45120                     0 S com.example.app:remote
u0_a200       6000  1612    4200000  51200       0             0 S com.other.app
";

    #[test]
    fn real_device_output_parses_including_the_blank_wchan_column() {
        let rows = parse_ps(REAL_PS);
        assert_eq!(
            rows.len(),
            6,
            "rows with a blank WCHAN must not be dropped: {rows:?}"
        );
        assert_eq!(rows.first().map(|r| r.pid), Some(1));
        assert_eq!(rows.first().map(|r| r.name.as_str()), Some("init"));
        assert_eq!(rows.get(2).map(|r| r.name.as_str()), Some("system_server"));
    }

    #[test]
    fn real_device_output_resolves_app_identities() {
        let rows = parse_ps(REAL_PS);
        assert_eq!(pids_for_package(&rows, "com.example.app"), vec![5063, 5602]);
        assert_eq!(pids_for_uid(&rows, 10123), vec![5063, 5602]);
        assert_eq!(pids_for_package(&rows, "com.other.app"), vec![6000]);

        let apps = running_apps(&rows);
        let packages: Vec<&str> = apps.iter().map(|app| app.package.as_str()).collect();
        assert_eq!(
            packages,
            vec!["com.example.app", "com.other.app"],
            "daemons and kernel threads are not applications"
        );
    }

    #[test]
    fn a_blank_addr_column_is_tolerated() {
        let rows = parse_ps(
            "USER PID PPID VSZ RSS WCHAN ADDR S NAME\n\
             root 1 0 100 200 S init\n",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().map(|r| r.name.as_str()), Some("init"));
    }

    #[test]
    fn ps_without_a_header_yields_nothing_rather_than_guessing() {
        assert!(parse_ps("garbage\nmore garbage\n").is_empty());
        assert!(parse_ps("").is_empty());
    }

    #[test]
    fn running_apps_groups_remotes_and_drops_native_binaries() {
        let rows = parse_ps(PS);
        let apps = running_apps(&rows);

        let packages: Vec<&str> = apps.iter().map(|app| app.package.as_str()).collect();
        assert_eq!(
            packages,
            vec!["com.example.app", "com.other.app", "com.work.profile"],
            "init and system_server are processes, not applications"
        );

        let example = apps.iter().find(|app| app.package == "com.example.app");
        assert_eq!(example.map(|app| app.uid), Some(Some(10123)));
        assert_eq!(
            example.map(|app| app.pids.clone()),
            Some(vec![5063, 5602]),
            "the :remote child must be folded into its app"
        );

        let profile = apps.iter().find(|app| app.package == "com.work.profile");
        assert_eq!(profile.map(|app| app.uid), Some(Some(1_010_045)));
    }
}
