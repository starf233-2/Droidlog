//! The crate's public type surface.
//!
//! Each type is defined in the module that owns its behaviour; this module only
//! re-exports them, so consumers (and the TypeScript mirror in `src/types`) have
//! one obvious place to look.
//!
//! Every type here is `serde`-serialisable with `camelCase` field names.

pub use crate::adb::locate::AdbSource;
pub use crate::collect::probe::{ProbeOutcome, ProbeStatus};
pub use crate::collect::{CollectRequest, CollectionReport, CrashEntry, CrashEvent};
pub use crate::commands::{AdbProbe, AppInfo};
pub use crate::crash::CrashKind;
pub use crate::device::{DeviceInfo, DeviceProbe, DeviceState};
pub use crate::error::DroidLogError;
pub use crate::executor::{ExecMode, ExecOutput};
pub use crate::export::{ExportFormat, ExportOutcome};
pub use crate::filter::{FilterField, FilterOp, FilterRule};
pub use crate::parser::{LogLevel, LogRecord};
pub use crate::process::{
    CaptureRequest, CaptureSession, SessionProgress, SessionStatus, SessionSummary,
};
pub use crate::ring::RingStats;
pub use crate::source::{
    LogSourceKind, LogSourceSpec, SourceAvailability, SourceOptions, LogcatBuffer,
};
