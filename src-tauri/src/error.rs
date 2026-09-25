//! Single error type for the whole crate.
//!
//! Two things matter here:
//!
//! 1. **One enum, one alias.** Modules never invent their own error types, so
//!    `?` composes freely and the Tauri command boundary stays a one-liner.
//! 2. **It serialises.** The frontend gets `{ kind, message, detail }` instead
//!    of `"Err(AdbCommandFailed { .. })"`, which is what makes error surfaces in
//!    the UI writable without string matching.

use serde::{Serialize, Serializer};

/// Everything that can go wrong in droidlog.
#[derive(Debug, thiserror::Error)]
pub enum DroidLogError {
    /// The `adb` binary could not be located or executed.
    #[error("adb 不可用：{0}")]
    AdbUnavailable(String),

    /// `adb` ran but exited non-zero.
    #[error("adb 命令失败（{command}，退出码 {code}）：{stderr}")]
    AdbCommandFailed {
        command: String,
        code: i32,
        stderr: String,
    },

    /// The requested serial is not in `adb devices`.
    #[error("设备不存在：{0}")]
    DeviceNotFound(String),

    /// Root mode was requested but `su` refused.
    #[error("设备 {serial} 无法使用 root：{reason}")]
    RootUnavailable { serial: String, reason: String },

    /// A subprocess could not be started at all.
    #[error("无法启动进程 `{program}`：{source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    /// Child process lost its stdout/stderr pipe.
    #[error("无法读取进程输出：{0}")]
    PipeUnavailable(String),

    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("文本编码错误：{0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),

    /// A [`crate::filter::FilterRule`] failed validation before being applied.
    #[error("过滤规则无效：{0}")]
    InvalidFilter(String),

    /// A caller-supplied argument (package name, PID list, ...) was rejected.
    #[error("参数无效：{0}")]
    InvalidInput(String),

    #[error("未知采集源：{0}")]
    UnknownSource(String),

    #[error("采集会话不存在：{0}")]
    SessionNotFound(String),

    #[error("采集会话已在运行：{0}")]
    SessionAlreadyRunning(String),

    /// Nothing sensible to add — used for invariant breaches we cannot name yet.
    #[error("内部错误：{0}")]
    Internal(String),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, DroidLogError>;

impl DroidLogError {
    /// Stable machine-readable discriminant, mirrored by `DroidLogErrorKind` in TS.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AdbUnavailable(_) => "adbUnavailable",
            Self::AdbCommandFailed { .. } => "adbCommandFailed",
            Self::DeviceNotFound(_) => "deviceNotFound",
            Self::RootUnavailable { .. } => "rootUnavailable",
            Self::Spawn { .. } => "spawn",
            Self::PipeUnavailable(_) => "pipeUnavailable",
            Self::Io(_) => "io",
            Self::Utf8(_) => "utf8",
            Self::Json(_) => "json",
            Self::InvalidFilter(_) => "invalidFilter",
            Self::InvalidInput(_) => "invalidInput",
            Self::UnknownSource(_) => "unknownSource",
            Self::SessionNotFound(_) => "sessionNotFound",
            Self::SessionAlreadyRunning(_) => "sessionAlreadyRunning",
            Self::Internal(_) => "internal",
        }
    }

    /// Optional extra context for the UI (a stderr tail, a rejected pattern, ...).
    ///
    /// `InvalidInput` and `InvalidFilter` deliberately return `None`: their
    /// `Display` text already *is* the reason, and the UI renders message and
    /// detail as two lines, so surfacing it twice reads as a stutter.
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::AdbCommandFailed { stderr, .. } if !stderr.is_empty() => Some(stderr.clone()),
            Self::RootUnavailable { reason, .. } => Some(reason.clone()),
            Self::Internal(detail) => Some(detail.clone()),
            _ => None,
        }
    }

    /// True when the failure is environmental (no device / no adb) rather than a bug.
    ///
    /// The UI uses this to pick between an empty-state hint and an error banner.
    #[must_use]
    pub fn is_environmental(&self) -> bool {
        matches!(
            self,
            Self::AdbUnavailable(_)
                | Self::DeviceNotFound(_)
                | Self::RootUnavailable { .. }
                | Self::Spawn { .. }
                | Self::PipeUnavailable(_)
        )
    }

    /// Attaches command context to a spawn failure.
    pub fn spawn(program: impl Into<String>, source: std::io::Error) -> Self {
        Self::Spawn {
            program: program.into(),
            source,
        }
    }

    /// Builds the standard error for a non-zero exit.
    pub fn command_failed(command: impl Into<String>, code: i32, stderr: impl Into<String>) -> Self {
        Self::AdbCommandFailed {
            command: command.into(),
            code,
            stderr: stderr.into(),
        }
    }
}

/// Serialised shape handed to the frontend.
#[derive(Serialize)]
struct ErrorPayload<'a> {
    kind: &'a str,
    message: String,
    detail: Option<String>,
    environmental: bool,
}

impl Serialize for DroidLogError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ErrorPayload {
            kind: self.kind(),
            message: self.to_string(),
            detail: self.detail(),
            environmental: self.is_environmental(),
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_detail_are_stable() {
        let err = DroidLogError::command_failed("adb devices", 1, "boom");
        assert_eq!(err.kind(), "adbCommandFailed");
        assert_eq!(err.detail().as_deref(), Some("boom"));
        assert!(!err.is_environmental());
    }

    #[test]
    fn environmental_errors_are_classified() {
        let err = DroidLogError::AdbUnavailable("not found".to_owned());
        assert!(err.is_environmental());
        assert_eq!(err.detail(), None);
    }

    #[test]
    fn serialises_to_structured_payload() -> Result<()> {
        let err = DroidLogError::DeviceNotFound("emulator-5554".to_owned());
        let json = serde_json::to_value(&err)?;
        assert_eq!(json.get("kind").and_then(|v| v.as_str()), Some("deviceNotFound"));
        assert_eq!(
            json.get("environmental").and_then(|v| v.as_bool()),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn io_errors_convert_via_question_mark() {
        fn fallible() -> Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"))?;
            Ok(())
        }
        assert_eq!(fallible().map_err(|e| e.kind()), Err("io"));
    }
}
