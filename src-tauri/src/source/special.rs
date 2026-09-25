//! The post-mortem / boot / recovery collectors.
//!
//! These three sources are *orchestrations*, not single commands: each one reads
//! several device paths in turn (feature 1 and 3 of the spec), or polls one
//! command for a bounded time (feature 2). The work itself lives in
//! [`crate::collect`]; what this module provides is the [`LogSource`] face the
//! rest of the application already understands, so the UI can list, select and
//! start them exactly like `logcat`.
//!
//! One shared implementation is enough because the three differ only in metadata
//! and in which probe list the collector runs — both of which are keyed by
//! [`LogSourceKind`].

use crate::parser::{parse_auto, LogRecord};
use crate::source::{effective_command, spec, LogSource, LogSourceKind, LogSourceSpec, SourceOptions};

/// A collector whose records come from several device reads rather than one
/// stream.
#[derive(Debug, Clone, Copy)]
pub struct SpecialSource {
    kind: LogSourceKind,
}

impl SpecialSource {
    /// Creates the collector for one of the orchestrated kinds.
    ///
    /// # Panics
    ///
    /// Never: the kind is only ever constructed by `source::build`, and the
    /// debug assertion documents the invariant for future callers.
    #[must_use]
    pub fn new(kind: LogSourceKind) -> Self {
        debug_assert!(matches!(
            kind,
            LogSourceKind::Crash | LogSourceKind::Boot | LogSourceKind::Recovery
        ));
        Self { kind }
    }
}

impl LogSource for SpecialSource {
    fn kind(&self) -> LogSourceKind {
        self.kind
    }

    fn spec(&self) -> &'static LogSourceSpec {
        spec(self.kind)
    }

    /// The command shown in the UI.
    ///
    /// Orchestrated sources have no single command, so the spec's
    /// `default_command` is a readable summary of what will be read; a custom
    /// command is not offered for them (`supports_custom_command: false`), which
    /// is why this never diverges from the spec.
    fn command(&self, options: &SourceOptions) -> String {
        effective_command(options, self.spec().default_command)
    }

    /// Decodes one line without knowing which probe produced it.
    ///
    /// The probes mix grammars — `logcat -L -d` prints threadtime lines while
    /// `cat /sys/fs/pstore/console-ramoops` prints raw kernel text — and the
    /// collector hands every line through this one function, so a record's source
    /// label stays the orchestrated kind rather than the probe's.
    fn parse_line(&self, line: &str, seq: u64) -> LogRecord {
        parse_auto(self.kind, line, seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrated_sources_parse_both_grammars() {
        let source = SpecialSource::new(LogSourceKind::Crash);

        // logcat -L -d output
        let logcat = source.parse_line("05-01 10:00:00.123  1234  1300 E AndroidRuntime: FATAL EXCEPTION: main", 1);
        assert!(logcat.parsed);
        assert_eq!(logcat.source, LogSourceKind::Crash);
        assert_eq!(logcat.pid, Some(1234));
        assert_eq!(logcat.tag.as_deref(), Some("AndroidRuntime"));

        // raw kernel text from pstore
        let kernel = source.parse_line("[    1.234567] Kernel panic - not syncing: Fatal exception", 2);
        assert!(kernel.parsed, "kernel text must parse");
        assert_eq!(kernel.source, LogSourceKind::Crash);
        assert!(kernel.uptime_seconds.is_some());

        // anything else is preserved verbatim rather than dropped
        let raw = source.parse_line("-- bootloader: unlocked", 3);
        assert!(!raw.parsed);
        assert_eq!(raw.source, LogSourceKind::Crash);
        assert!(raw.raw.contains("bootloader"));
    }

    #[test]
    fn every_orchestrated_kind_has_a_spec_and_no_custom_command() {
        for kind in [
            LogSourceKind::Crash,
            LogSourceKind::Boot,
            LogSourceKind::Recovery,
        ] {
            let source = SpecialSource::new(kind);
            assert_eq!(source.kind(), kind);
            assert_eq!(source.spec().kind, kind);
            assert!(!source.spec().supports_custom_command);
            assert!(!source.command(&SourceOptions::unrestricted()).is_empty());
        }
    }
}
