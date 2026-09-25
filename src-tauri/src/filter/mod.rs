//! Real-time filtering.
//!
//! A [`FilterRule`] is an editable, serialisable description; a [`FilterSet`] is
//! the compiled, reusable form the capture loop evaluates per record. Compiling
//! once (including regexes and level parsing) is what keeps the hot path
//! allocation-light.
//!
//! Semantics: a record passes when **every enabled rule** matches (conjunction).
//! This is the useful default for log triage — "level >= warn AND from this
//! package" — and leaves room to add OR groups later without changing the shape.
//!
//! Client-side filtering is authoritative: sources may also push filters down to
//! the device (see [`crate::source::logcat`]), but a record still has to satisfy
//! the [`FilterSet`] to reach the ring buffer.

use std::borrow::Cow;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{DroidLogError, Result};
use crate::parser::{LogLevel, LogRecord};

/// Which part of a record a rule inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterField {
    /// logcat tag.
    Tag,
    /// Message body.
    Message,
    /// Process id.
    Pid,
    /// Thread id.
    Tid,
    /// Owning uid.
    Uid,
    /// Resolved package name.
    Package,
    /// Severity.
    Level,
    /// Host receive time; only meaningful with [`FilterOp::WithinLast`].
    Received,
    /// Originating collector.
    Source,
}

impl FilterField {
    /// Every field, in UI display order.
    #[must_use]
    pub fn all() -> [Self; 9] {
        [
            Self::Tag,
            Self::Message,
            Self::Pid,
            Self::Tid,
            Self::Uid,
            Self::Package,
            Self::Level,
            Self::Received,
            Self::Source,
        ]
    }

    /// Short label for the filter panel.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tag => "TAG",
            Self::Message => "消息",
            Self::Pid => "PID",
            Self::Tid => "TID",
            Self::Uid => "UID",
            Self::Package => "包名",
            Self::Level => "级别",
            Self::Received => "接收时间",
            Self::Source => "采集源",
        }
    }

    /// Reads this field out of a record, borrowing where possible.
    fn extract<'a>(self, record: &'a LogRecord) -> Option<Cow<'a, str>> {
        match self {
            Self::Tag => record.tag.as_deref().map(Cow::Borrowed),
            Self::Message => Some(Cow::Borrowed(record.message.as_str())),
            Self::Package => record.package.as_deref().map(Cow::Borrowed),
            Self::Pid => record.pid.map(|v| Cow::Owned(v.to_string())),
            Self::Tid => record.tid.map(|v| Cow::Owned(v.to_string())),
            Self::Uid => record.uid.map(|v| Cow::Owned(v.to_string())),
            Self::Level => Some(Cow::Owned(record.level.to_string())),
            Self::Received => Some(Cow::Owned(record.received_at_ms.to_string())),
            Self::Source => Some(Cow::Owned(record.source.to_string())),
        }
    }
}

/// How a rule compares a field against its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    /// Substring match.
    Contains,
    /// Substring must be absent.
    NotContains,
    /// Exact match.
    Equals,
    /// Exact mismatch.
    NotEquals,
    /// Regular expression match.
    Regex,
    /// Severity is at least the rule's level.
    MinLevel,
    /// Comma-separated membership test.
    In,
    /// Host receive time is within the last N seconds (rolling).
    ///
    /// Rolling rather than an absolute range because a live tail moves: "the
    /// last 30 seconds" stays meaningful, while "since 10:04" stops matching the
    /// moment the interesting line scrolls past.
    WithinLast,
}

impl FilterOp {
    /// Every operator, in UI display order.
    #[must_use]
    pub fn all() -> [Self; 8] {
        [
            Self::Contains,
            Self::NotContains,
            Self::Equals,
            Self::NotEquals,
            Self::Regex,
            Self::MinLevel,
            Self::In,
            Self::WithinLast,
        ]
    }

    /// Short label for the filter panel.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "包含",
            Self::NotContains => "不包含",
            Self::Equals => "等于",
            Self::NotEquals => "不等于",
            Self::Regex => "正则",
            Self::MinLevel => "不低于",
            Self::In => "属于",
            Self::WithinLast => "最近 N 秒",
        }
    }
}

/// A user-authored filter rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterRule {
    /// Stable id, so the UI can edit and delete individual rules.
    pub id: String,
    /// Disabled rules are kept but ignored.
    pub enabled: bool,
    /// Field to inspect.
    pub field: FilterField,
    /// Comparison to apply.
    pub op: FilterOp,
    /// User-supplied comparison value.
    pub value: String,
    /// Case sensitivity for the string operators.
    #[serde(default)]
    pub case_sensitive: bool,
}

impl FilterRule {
    /// Builds an enabled, case-sensitive rule.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        field: FilterField,
        op: FilterOp,
        value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            field,
            op,
            value: value.into(),
            case_sensitive: false,
        }
    }

    /// A rule that keeps only records at or above `level`.
    #[must_use]
    pub fn min_level(level: LogLevel) -> Self {
        Self {
            id: "min-level".to_owned(),
            enabled: true,
            field: FilterField::Level,
            op: FilterOp::MinLevel,
            value: level.to_string(),
            case_sensitive: false,
        }
    }
}

/// A rule plus everything precomputed for the hot path.
#[derive(Debug, Clone)]
struct CompiledRule {
    rule: FilterRule,
    regex: Option<Regex>,
    level: Option<LogLevel>,
    members: Vec<String>,
    /// Window length in milliseconds, for [`FilterOp::WithinLast`].
    window_ms: Option<u64>,
}

/// A compiled collection of rules.
#[derive(Debug, Clone, Default)]
pub struct FilterSet {
    rules: Vec<CompiledRule>,
}

impl FilterSet {
    /// Compiles `rules`, skipping disabled ones.
    ///
    /// # Errors
    ///
    /// Returns [`DroidLogError::InvalidFilter`] for an uncompilable regex, an
    /// unparseable level, or an empty value on the operators that need one.
    pub fn compile(rules: &[FilterRule]) -> Result<Self> {
        let mut compiled = Vec::with_capacity(rules.len());

        for rule in rules.iter().filter(|rule| rule.enabled) {
            compiled.push(compile_rule(rule)?);
        }

        Ok(Self { rules: compiled })
    }

    /// An empty set that accepts every record.
    #[must_use]
    pub fn accept_all() -> Self {
        Self::default()
    }

    /// Number of active rules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when no rule constrains the stream.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// True when `record` satisfies every active rule.
    #[must_use]
    pub fn matches(&self, record: &LogRecord) -> bool {
        self.rules.iter().all(|rule| rule.matches(record))
    }

    /// Convenience: filter a slice, preserving order.
    #[must_use]
    pub fn apply<'a>(&self, records: &'a [LogRecord]) -> Vec<&'a LogRecord> {
        records
            .iter()
            .filter(|record| self.matches(record))
            .collect()
    }
}

/// Builds a [`CompiledRule`], precomputing whatever the operator needs.
fn compile_rule(rule: &FilterRule) -> Result<CompiledRule> {
    let regex = if rule.op == FilterOp::Regex {
        let pattern = if rule.case_sensitive {
            rule.value.clone()
        } else {
            format!("(?i){}", rule.value)
        };
        Some(
            Regex::new(&pattern)
                .map_err(|err| DroidLogError::InvalidFilter(format!("正则无效：{err}")))?,
        )
    } else {
        None
    };

    let level = if rule.op == FilterOp::MinLevel {
        Some(LogLevel::from_name(&rule.value).ok_or_else(|| {
            DroidLogError::InvalidFilter(format!("日志级别无效：{}", rule.value))
        })?)
    } else {
        None
    };

    if matches!(rule.op, FilterOp::Contains | FilterOp::Equals | FilterOp::In)
        && rule.value.is_empty()
    {
        return Err(DroidLogError::InvalidFilter(format!(
            "规则 {} 的比较值为空",
            rule.id
        )));
    }

    let members = if rule.op == FilterOp::In {
        rule.value
            .split(',')
            .map(|member| member.trim().to_lowercase())
            .filter(|member| !member.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    let window_ms = if rule.op == FilterOp::WithinLast {
        let seconds = rule.value.trim().parse::<u64>().map_err(|_| {
            DroidLogError::InvalidFilter(format!("时间窗口需要秒数：{}", rule.value))
        })?;
        if seconds == 0 {
            return Err(DroidLogError::InvalidFilter(
                "时间窗口必须大于 0 秒".to_owned(),
            ));
        }
        Some(seconds.saturating_mul(1000))
    } else {
        None
    };

    Ok(CompiledRule {
        rule: rule.clone(),
        regex,
        level,
        members,
        window_ms,
    })
}

/// Case-folds `text` unless the rule is case sensitive.
fn fold<'a>(text: &'a str, case_sensitive: bool) -> Cow<'a, str> {
    if case_sensitive {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.to_lowercase())
    }
}

impl CompiledRule {
    /// Evaluates this rule against `record`.
    fn matches(&self, record: &LogRecord) -> bool {
        // The rolling window compares clocks rather than text, so it does not go
        // through the field-extraction path.
        if self.rule.op == FilterOp::WithinLast {
            let Some(window_ms) = self.window_ms else {
                return false;
            };
            return record
                .received_at_ms
                .saturating_add(window_ms)
                >= crate::process::now_ms();
        }

        let Some(raw) = self.rule.field.extract(record) else {
            // The field is absent on this record. Negative operators are
            // vacuously satisfied ("no tag" trivially "does not contain X");
            // positive ones fail rather than matching by accident.
            return matches!(
                self.rule.op,
                FilterOp::NotContains | FilterOp::NotEquals
            );
        };

        match self.rule.op {
            FilterOp::Contains => {
                fold(&raw, self.rule.case_sensitive)
                    .contains(&*fold(&self.rule.value, self.rule.case_sensitive))
            }
            FilterOp::NotContains => !fold(&raw, self.rule.case_sensitive)
                .contains(&*fold(&self.rule.value, self.rule.case_sensitive)),
            FilterOp::Equals => {
                *fold(&raw, self.rule.case_sensitive)
                    == *fold(&self.rule.value, self.rule.case_sensitive)
            }
            FilterOp::NotEquals => {
                *fold(&raw, self.rule.case_sensitive)
                    != *fold(&self.rule.value, self.rule.case_sensitive)
            }
            FilterOp::Regex => self
                .regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(&raw)),
            FilterOp::MinLevel => self
                .level
                .is_some_and(|min| record.level.passes_min(min)),
            FilterOp::In => {
                let folded = fold(&raw, false);
                self.members
                    .iter()
                    .any(|member| member.as_str() == &*folded)
            }
            // Handled before extraction.
            FilterOp::WithinLast => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::LogSourceKind;

    fn record(level: LogLevel, tag: &str, message: &str) -> LogRecord {
        let mut record = LogRecord::new(LogSourceKind::Logcat, 1, "raw");
        record.level = level;
        record.tag = Some(tag.to_owned());
        record.message = message.to_owned();
        record.pid = Some(1234);
        record
    }

    fn compile_set(rules: Vec<FilterRule>) -> FilterSet {
        match FilterSet::compile(&rules) {
            Ok(set) => set,
            Err(err) => unreachable!("rules should compile: {err}"),
        }
    }

    #[test]
    fn empty_set_accepts_everything() {
        let set = FilterSet::accept_all();
        assert!(set.is_empty());
        assert!(set.matches(&record(LogLevel::Verbose, "Any", "text")));
    }

    #[test]
    fn contains_is_case_insensitive_by_default() {
        let set = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Message,
            FilterOp::Contains,
            "TIMEOUT",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "Net", "connect timeout")));
        assert!(!set.matches(&record(LogLevel::Info, "Net", "connected")));
    }

    #[test]
    fn case_sensitive_rule_respects_case() {
        let mut rule = FilterRule::new("r1", FilterField::Tag, FilterOp::Contains, "Activity");
        rule.case_sensitive = true;
        let set = compile_set(vec![rule]);
        assert!(set.matches(&record(LogLevel::Info, "ActivityManager", "x")));
        assert!(!set.matches(&record(LogLevel::Info, "activitymanager", "x")));
    }

    #[test]
    fn min_level_keeps_severity_and_above() {
        let set = compile_set(vec![FilterRule::min_level(LogLevel::Warn)]);
        assert!(!set.matches(&record(LogLevel::Info, "T", "x")));
        assert!(set.matches(&record(LogLevel::Warn, "T", "x")));
        assert!(set.matches(&record(LogLevel::Error, "T", "x")));
        assert!(
            set.matches(&record(LogLevel::Unknown, "T", "x")),
            "unclassified records must not be hidden"
        );
    }

    #[test]
    fn rules_are_conjunctive() {
        let set = compile_set(vec![
            FilterRule::min_level(LogLevel::Error),
            FilterRule::new("r2", FilterField::Tag, FilterOp::Equals, "AndroidRuntime"),
        ]);
        assert!(set.matches(&record(LogLevel::Error, "AndroidRuntime", "boom")));
        assert!(!set.matches(&record(LogLevel::Info, "AndroidRuntime", "boom")));
        assert!(!set.matches(&record(LogLevel::Error, "Other", "boom")));
    }

    #[test]
    fn disabled_rules_are_ignored() {
        let mut rule = FilterRule::min_level(LogLevel::Fatal);
        rule.enabled = false;
        let set = compile_set(vec![rule]);
        assert!(set.is_empty());
        assert!(set.matches(&record(LogLevel::Verbose, "T", "x")));
    }

    #[test]
    fn regex_matches_and_is_case_insensitive_by_default() {
        let set = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Message,
            FilterOp::Regex,
            r"pid\s+\d+",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "T", "start PID 42 done")));
        assert!(!set.matches(&record(LogLevel::Info, "T", "nothing here")));
    }

    #[test]
    fn numeric_fields_are_compared_as_text() {
        let set = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Pid,
            FilterOp::Equals,
            "1234",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "T", "x")));

        let other = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Pid,
            FilterOp::Equals,
            "9999",
        )]);
        assert!(!other.matches(&record(LogLevel::Info, "T", "x")));
    }

    #[test]
    fn in_operator_checks_membership() {
        let set = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Tag,
            FilterOp::In,
            "ActivityManager, AndroidRuntime ,binder",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "binder", "x")));
        assert!(set.matches(&record(LogLevel::Info, "androidruntime", "x")));
        assert!(!set.matches(&record(LogLevel::Info, "Other", "x")));
    }

    #[test]
    fn absent_field_satisfies_negative_operators_only() {
        let mut bare = record(LogLevel::Info, "T", "x");
        bare.tag = None;

        let negative = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Tag,
            FilterOp::NotContains,
            "Activity",
        )]);
        assert!(negative.matches(&bare));

        let positive = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Tag,
            FilterOp::Contains,
            "Activity",
        )]);
        assert!(!positive.matches(&bare));
    }

    #[test]
    fn source_field_is_matchable() {
        let set = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Source,
            FilterOp::Equals,
            "logcat",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "T", "x")));

        let kernel = compile_set(vec![FilterRule::new(
            "r1",
            FilterField::Source,
            FilterOp::Equals,
            "dmesg",
        )]);
        assert!(!kernel.matches(&record(LogLevel::Info, "T", "x")));
    }

    #[test]
    fn invalid_regex_is_rejected() {
        let rules = vec![FilterRule::new(
            "r1",
            FilterField::Message,
            FilterOp::Regex,
            "([unclosed",
        )];
        assert_eq!(
            FilterSet::compile(&rules).map(|_| ()).map_err(|e| e.kind()),
            Err("invalidFilter")
        );
    }

    #[test]
    fn invalid_level_is_rejected() {
        let rules = vec![FilterRule::new(
            "r1",
            FilterField::Level,
            FilterOp::MinLevel,
            "shouting",
        )];
        assert_eq!(
            FilterSet::compile(&rules).map(|_| ()).map_err(|e| e.kind()),
            Err("invalidFilter")
        );
    }

    #[test]
    fn empty_value_is_rejected_for_string_operators() {
        let rules = vec![FilterRule::new(
            "r1",
            FilterField::Message,
            FilterOp::Contains,
            "",
        )];
        assert_eq!(
            FilterSet::compile(&rules).map(|_| ()).map_err(|e| e.kind()),
            Err("invalidFilter")
        );
    }

    #[test]
    fn apply_preserves_order() {
        let set = compile_set(vec![FilterRule::min_level(LogLevel::Warn)]);
        let records = vec![
            record(LogLevel::Info, "T", "a"),
            record(LogLevel::Warn, "T", "b"),
            record(LogLevel::Error, "T", "c"),
        ];
        let kept: Vec<&str> = set
            .apply(&records)
            .iter()
            .map(|record| record.message.as_str())
            .collect();
        assert_eq!(kept, vec!["b", "c"]);
    }

    #[test]
    fn levels_can_be_selected_as_a_set() {
        // Requirement: "filter by log level (I/D/W/E/F)". `In` is what the level
        // multi-select compiles to.
        let set = compile_set(vec![FilterRule::new(
            "levels",
            FilterField::Level,
            FilterOp::In,
            "info,warn,error",
        )]);
        assert!(set.matches(&record(LogLevel::Info, "T", "x")));
        assert!(set.matches(&record(LogLevel::Warn, "T", "x")));
        assert!(set.matches(&record(LogLevel::Error, "T", "x")));
        assert!(!set.matches(&record(LogLevel::Debug, "T", "x")));
        assert!(!set.matches(&record(LogLevel::Fatal, "T", "x")));
    }

    #[test]
    fn a_rolling_window_keeps_recent_records_only() {
        let set = compile_set(vec![FilterRule::new(
            "window",
            FilterField::Received,
            FilterOp::WithinLast,
            "30",
        )]);

        let mut fresh = record(LogLevel::Info, "T", "x");
        fresh.received_at_ms = crate::process::now_ms();
        assert!(set.matches(&fresh), "a just-received record is inside the window");

        let mut old = record(LogLevel::Info, "T", "x");
        old.received_at_ms = crate::process::now_ms().saturating_sub(60_000);
        assert!(!set.matches(&old), "a minute-old record is outside 30 s");
    }

    #[test]
    fn a_zero_or_unparseable_window_is_rejected() {
        for value in ["0", "abc", "", "-5"] {
            let rules = vec![FilterRule::new(
                "window",
                FilterField::Received,
                FilterOp::WithinLast,
                value,
            )];
            assert_eq!(
                FilterSet::compile(&rules).map(|_| ()).map_err(|e| e.kind()),
                Err("invalidFilter"),
                "value {value:?} must be rejected"
            );
        }
    }

    #[test]
    fn rules_round_trip_through_json() -> Result<()> {
        let rule = FilterRule::new("r1", FilterField::Message, FilterOp::Contains, "boom");
        let json = serde_json::to_string(&rule)?;
        let back: FilterRule = serde_json::from_str(&json)?;
        assert_eq!(rule, back);
        assert!(json.contains("caseSensitive"), "expected camelCase: {json}");
        Ok(())
    }

    #[test]
    fn case_sensitive_defaults_when_absent_on_the_wire() -> Result<()> {
        let json = r#"{"id":"r1","enabled":true,"field":"tag","op":"contains","value":"x"}"#;
        let rule: FilterRule = serde_json::from_str(json)?;
        assert!(!rule.case_sensitive);
        Ok(())
    }
}
