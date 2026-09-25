//! Device model and `adb devices -l` parsing.
//!
//! [`DeviceInfo`] is the type the UI binds its device list to, so the parser is
//! deliberately permissive: unknown `key:value` pairs and future device states
//! are preserved rather than rejected.

pub mod probe;
pub mod resolve;
pub mod watch;

use serde::{Deserialize, Serialize};

pub use probe::{probe_device, resolve_pid, resolve_pids, resolve_uid, DeviceProbe};
pub use resolve::{
    AppResolver, AppTarget, AppTargetKind, Prefilter, RunningApp, CACHE_TTL as APP_TARGET_TTL,
};
pub use watch::{default_mode, root_is_offerable, DeviceWatcher, EVENT_DEVICES, POLL_INTERVAL};

/// Connection state reported by adb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceState {
    /// Ready for commands.
    Device,
    /// Visible but not answering.
    Offline,
    /// USB debugging not yet authorised on the device.
    Unauthorized,
    /// In the bootloader / fastboot menu.
    Bootloader,
    /// Booted into recovery.
    Recovery,
    /// In sideload mode.
    Sideload,
    /// A state this build does not know about; see `state_raw`.
    Unknown,
}

impl DeviceState {
    /// Classifies an adb state token.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "device" => Self::Device,
            "offline" => Self::Offline,
            "unauthorized" => Self::Unauthorized,
            "bootloader" => Self::Bootloader,
            "recovery" => Self::Recovery,
            "sideload" => Self::Sideload,
            _ => Self::Unknown,
        }
    }

    /// True when the device can accept shell commands.
    #[must_use]
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Device)
    }
}

/// One attached device, as shown in the left-hand rail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// adb serial, e.g. `emulator-5554` or `192.168.1.5:5555`.
    pub serial: String,
    /// Normalised connection state.
    pub state: DeviceState,
    /// Raw state token, kept for forward compatibility.
    pub state_raw: String,
    /// `ro.product.model`, underscores converted to spaces.
    pub model: Option<String>,
    /// `ro.product.name`.
    pub product: Option<String>,
    /// `ro.product.device`.
    pub device: Option<String>,
    /// adb transport id.
    pub transport_id: Option<String>,
    /// `ro.build.version.release`, filled in by [`probe_device`].
    pub android_version: Option<String>,
    /// `ro.build.version.sdk`, filled in by [`probe_device`].
    pub sdk: Option<i32>,
    /// Whether `su -c` works, filled in by [`probe_device`].
    pub root_available: Option<bool>,
    /// Why root is unavailable, when it is.
    pub root_reason: Option<String>,
    /// Whether the device booted into recovery or sideload.
    ///
    /// Recovery is a different world: there is no `logcat`, and the only useful
    /// log is `/tmp/recovery.log`, so the UI has to know before offering a
    /// collector that cannot work.
    pub recovery: bool,
}

impl DeviceInfo {
    /// A device with only the fields `adb devices -l` provides.
    #[must_use]
    pub fn new(serial: impl Into<String>, state: DeviceState, raw_state: impl Into<String>) -> Self {
        let serial = serial.into();
        let raw_state = raw_state.into();
        let recovery = is_recovery(&serial, state, &raw_state);
        Self {
            serial,
            state,
            state_raw: raw_state,
            model: None,
            product: None,
            device: None,
            transport_id: None,
            android_version: None,
            sdk: None,
            root_available: None,
            root_reason: None,
            recovery,
        }
    }

    /// Best available display name: model, else serial.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.model.as_deref().unwrap_or(&self.serial)
    }

    /// `Android 14 (SDK 34)` when known.
    #[must_use]
    pub fn android_label(&self) -> Option<String> {
        match (&self.android_version, self.sdk) {
            (Some(version), Some(sdk)) => Some(format!("Android {version} (SDK {sdk})")),
            (Some(version), None) => Some(format!("Android {version}")),
            (None, Some(sdk)) => Some(format!("SDK {sdk}")),
            (None, None) => None,
        }
    }

    /// True when the device is ready for capture.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.state.is_usable()
    }
}

/// Android reports product/model names with spaces replaced by underscores.
#[must_use]
pub fn prettify_prop(value: &str) -> String {
    value.trim().replace('_', " ")
}

/// Whether a transport is a device running recovery or sideload.
///
/// Two independent signals are used because neither is universal: `adb` reports
/// `recovery`/`sideload` as the transport state on most builds, and the serial of
/// a device booted into recovery carries a `_recovery` suffix (this is how the
/// same handset appears twice when it is still in the list from before the
/// reboot). Sideload counts as recovery: `adb shell` does not work there, and the
/// recovery collector is the only one that has anything to read.
#[must_use]
pub fn is_recovery(serial: &str, state: DeviceState, raw_state: &str) -> bool {
    matches!(state, DeviceState::Recovery | DeviceState::Sideload)
        || matches!(
            raw_state.trim().to_ascii_lowercase().as_str(),
            "recovery" | "sideload"
        )
        || serial.to_ascii_lowercase().ends_with("_recovery")
}

/// Parses `adb devices -l` output.
///
/// Tolerates the banner line, `* daemon started *` notices, trailing blanks and
/// `key:value` pairs added by future adb releases.
#[must_use]
pub fn parse_device_list(raw: &str) -> Vec<DeviceInfo> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("List of devices"))
        .filter(|line| !line.starts_with('*'))
        .filter_map(parse_device_line)
        .collect()
}

/// Parses one `serial state key:value ...` line.
#[must_use]
pub fn parse_device_line(line: &str) -> Option<DeviceInfo> {
    let mut tokens = line.split_whitespace();
    let serial = tokens.next()?;
    let raw_state = tokens.next()?;

    let mut device = DeviceInfo::new(serial, DeviceState::from_token(raw_state), raw_state);

    for token in tokens {
        let Some((key, value)) = token.split_once(':') else {
            continue;
        };
        match key {
            "model" => device.model = Some(prettify_prop(value)),
            "product" => device.product = Some(value.to_owned()),
            "device" => device.device = Some(value.to_owned()),
            "transport_id" => device.transport_id = Some(value.to_owned()),
            // `usb:` and anything newer is intentionally ignored for now.
            _ => {}
        }
    }

    Some(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "List of devices attached\n\
emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64x transport_id:1\n\
R58M12ABCDE            unauthorized transport_id:2\n\
192.168.1.5:5555       offline\n\
\n";

    #[test]
    fn parses_all_device_lines() {
        let devices = parse_device_list(SAMPLE);
        assert_eq!(devices.len(), 3);
    }

    #[test]
    fn banner_and_blank_lines_are_skipped() {
        let devices = parse_device_list(SAMPLE);
        assert!(
            devices
                .iter()
                .all(|device| !device.serial.contains("List of devices")),
            "banner leaked into the device list"
        );
    }

    #[test]
    fn long_format_properties_are_captured() {
        let devices = parse_device_list(SAMPLE);
        let first = devices.first();
        assert_eq!(first.map(|d| d.serial.as_str()), Some("emulator-5554"));
        assert_eq!(
            first.and_then(|d| d.model.as_deref()),
            Some("sdk gphone64 x86 64")
        );
        assert_eq!(
            first.and_then(|d| d.product.as_deref()),
            Some("sdk_gphone64_x86_64")
        );
        assert_eq!(first.and_then(|d| d.transport_id.as_deref()), Some("1"));
        assert_eq!(
            first.map(|d| d.state),
            Some(DeviceState::Device),
            "expected the device to be usable"
        );
    }

    #[test]
    fn unusable_states_are_classified() {
        let devices = parse_device_list(SAMPLE);
        assert_eq!(
            devices.get(1).map(|d| d.state),
            Some(DeviceState::Unauthorized)
        );
        assert_eq!(devices.get(2).map(|d| d.state), Some(DeviceState::Offline));
        assert_eq!(devices.get(2).map(DeviceInfo::is_usable), Some(false));
    }

    #[test]
    fn daemon_notices_are_ignored() {
        let raw = "* daemon not running; starting now at tcp:5037\n\
* daemon started successfully\n\
List of devices attached\n\
SER device\n";
        let devices = parse_device_list(raw);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices.first().map(|d| d.serial.as_str()), Some("SER"));
    }

    #[test]
    fn unknown_state_is_preserved_raw() {
        let devices = parse_device_list("SER fastbootd\n");
        assert_eq!(devices.first().map(|d| d.state), Some(DeviceState::Unknown));
        assert_eq!(
            devices.first().map(|d| d.state_raw.as_str()),
            Some("fastbootd")
        );
    }

    #[test]
    fn android_label_degrades_gracefully() {
        let mut device = DeviceInfo::new("SER", DeviceState::Device, "device");
        assert_eq!(device.android_label(), None);
        device.android_version = Some("14".to_owned());
        assert_eq!(device.android_label().as_deref(), Some("Android 14"));
        device.sdk = Some(34);
        assert_eq!(
            device.android_label().as_deref(),
            Some("Android 14 (SDK 34)")
        );
    }

    #[test]
    fn display_name_falls_back_to_serial() {
        let mut device = DeviceInfo::new("SER", DeviceState::Device, "device");
        assert_eq!(device.display_name(), "SER");
        device.model = Some("Pixel 8".to_owned());
        assert_eq!(device.display_name(), "Pixel 8");
    }

    #[test]
    fn malformed_lines_are_dropped() {
        assert!(parse_device_list("justoneserial\n").is_empty());
    }

    #[test]
    fn prettify_replaces_underscores() {
        assert_eq!(prettify_prop("Pixel_8_Pro"), "Pixel 8 Pro");
        assert_eq!(prettify_prop("  spaced  "), "spaced");
    }

    #[test]
    fn recovery_is_detected_from_state_or_serial() {
        // Transport state says so.
        let by_state = DeviceInfo::new("83048a94", DeviceState::Recovery, "recovery");
        assert!(by_state.recovery);
        // Sideload cannot run `adb shell`, so it counts as recovery too.
        let sideload = DeviceInfo::new("83048a94", DeviceState::Sideload, "sideload");
        assert!(sideload.recovery);
        // The suffix adb uses when the same handset is also listed as booted.
        let by_serial = DeviceInfo::new("83048a94_recovery", DeviceState::Device, "device");
        assert!(by_serial.recovery);
        // A raw token this build does not model yet, but which adb reports.
        assert!(is_recovery("X", DeviceState::Unknown, "RECOVERY"));

        let normal = DeviceInfo::new("83048a94", DeviceState::Device, "device");
        assert!(!normal.recovery);
    }
}
