//! Integrity check: how much of the capture can be trusted.
//!
//! A crash report is only worth reading if the reader knows what is *missing* from
//! it. The failure modes are mundane and they all look alike from the outside — an
//! empty crash list can mean "nothing crashed" or "the buffer this ROM does not have
//! was never read", and those two conclusions lead in opposite directions.
//!
//! So this module turns the numbers a capture already has into explicit statements:
//! dropped records, lines nothing could parse, buffers that were asked for but do
//! not exist, gaps in time, and crash events with no stack. Each check carries a
//! status, and the wording says what to do about it — the same style as the probe
//! report, so the two read as one document.
//!
//! Nothing here needs a device: it is arithmetic over counters, which is what makes
//! it testable and what keeps it out of the capture path.

use serde::{Deserialize, Serialize};

use crate::crash::structured::CrashEvent;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    /// The capture looks complete for this aspect.
    Ok,
    /// Worth knowing; the capture is still usable.
    Notice,
    /// The conclusion drawn from this capture may be wrong.
    Warning,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityCheck {
    /// Stable id, used by the UI to key rows.
    pub id: String,
    /// Short human label.
    pub label: String,
    /// How serious it is.
    pub status: CheckStatus,
    /// What was measured and what to do about it.
    pub detail: String,
}

/// Everything the checks need, already counted by the capture.
#[derive(Debug, Clone, Default)]
pub struct IntegrityInput {
    /// Records the capture accepted.
    pub records: u64,
    /// Records the ring buffer dropped because it was full.
    pub dropped: u64,
    /// Lines that matched no grammar and were kept verbatim.
    pub unparsed: u64,
    /// Buffers the capture asked for.
    pub requested_buffers: Vec<String>,
    /// Buffers the device reported (empty means unknown).
    pub available_buffers: Vec<String>,
    /// Gaps between consecutive records, in milliseconds.
    pub gaps_ms: Vec<i64>,
    /// Structured crashes found in the capture.
    pub crashes: Vec<CrashEvent>,
}

/// Thresholds, kept in one place so the wording and the numbers cannot drift.
pub struct Limits {
    /// Share of unparsed lines that stops being noise.
    pub unparsed_notice: f64,
    /// Share of unparsed lines that suggests the wrong parser was used.
    pub unparsed_warning: f64,
    /// A silence longer than this is worth reporting.
    pub gap_notice_ms: i64,
    /// A silence longer than this is likely a reconnect or a stalled device.
    pub gap_warning_ms: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Vendor lines that no grammar knows are normal; a few percent are not.
            unparsed_notice: 0.01,
            unparsed_warning: 0.10,
            gap_notice_ms: 5_000,
            gap_warning_ms: 30_000,
        }
    }
}

/// Formats a share as a percentage with one decimal.
fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_owned();
    }
    let ratio = part as f64 / whole as f64 * 100.0;
    format!("{ratio:.1}%")
}

/// Runs every check.
#[must_use]
pub fn assess(input: &IntegrityInput, limits: &Limits) -> Vec<IntegrityCheck> {
    let mut checks = Vec::new();

    // ---- records -----------------------------------------------------------
    if input.records == 0 {
        checks.push(IntegrityCheck {
            id: "records".to_owned(),
            label: "日志量".to_owned(),
            status: CheckStatus::Notice,
            detail: "没有收到任何日志行。请确认设备在线，且采集源与过滤条件没有把日志全部挡掉。"
                .to_owned(),
        });
    } else {
        checks.push(IntegrityCheck {
            id: "records".to_owned(),
            label: "日志量".to_owned(),
            status: CheckStatus::Ok,
            detail: format!("收到 {} 行日志。", input.records),
        });
    }

    // ---- drops -------------------------------------------------------------
    if input.dropped > 0 {
        checks.push(IntegrityCheck {
            id: "dropped".to_owned(),
            label: "缓冲丢弃".to_owned(),
            status: CheckStatus::Warning,
            detail: format!(
                "丢弃 {} 行（占 {}{}）：最早的日志已不在缓冲内，崩溃可能落在被丢弃的区段。建议缩小过滤范围后重采。",
                input.dropped,
                percent(input.dropped, input.records.saturating_add(input.dropped)),
                if input.dropped > input.records {
                    "，丢弃多于保留"
                } else {
                    ""
                }
            ),
        });
    } else {
        checks.push(IntegrityCheck {
            id: "dropped".to_owned(),
            label: "缓冲丢弃".to_owned(),
            status: CheckStatus::Ok,
            detail: "没有丢弃日志。".to_owned(),
        });
    }

    // ---- unparsed ----------------------------------------------------------
    let share = if input.records == 0 {
        0.0
    } else {
        input.unparsed as f64 / input.records as f64
    };
    let status = if share >= limits.unparsed_warning {
        CheckStatus::Warning
    } else if share >= limits.unparsed_notice {
        CheckStatus::Notice
    } else {
        CheckStatus::Ok
    };
    checks.push(IntegrityCheck {
        id: "unparsed".to_owned(),
        label: "未解析行".to_owned(),
        status,
        detail: if status == CheckStatus::Ok {
            format!("未解析 {} 行（{}），在正常范围内。", input.unparsed, percent(input.unparsed, input.records))
        } else {
            format!(
                "未解析 {} 行（{}）。这些行按原文保留；比例偏高通常说明采集源输出格式与解析器不匹配，例如自定义命令用了别的格式。",
                input.unparsed,
                percent(input.unparsed, input.records)
            )
        },
    });

    // ---- buffers -----------------------------------------------------------
    let missing: Vec<&str> = input
        .requested_buffers
        .iter()
        .filter(|buffer| !input.available_buffers.iter().any(|have| have == *buffer))
        .map(String::as_str)
        .collect();
    let buffers_known = !input.available_buffers.is_empty();
    checks.push(IntegrityCheck {
        id: "buffers".to_owned(),
        label: "缓冲区".to_owned(),
        status: if !buffers_known {
            CheckStatus::Notice
        } else if missing.is_empty() {
            CheckStatus::Ok
        } else {
            CheckStatus::Warning
        },
        detail: if !buffers_known {
            "设备未报告 logcat 缓冲区，无法确认请求的缓冲区是否存在。".to_owned()
        } else if missing.is_empty() {
            format!("请求的缓冲区都存在：{}。", input.requested_buffers.join(", "))
        } else {
            format!(
                "这些缓冲区在这台设备上不存在，相关来源不会出现在结果里：{}。设备提供：{}。空崩溃列表通常由此造成。",
                missing.join(", "),
                input.available_buffers.join(", ")
            )
        },
    });

    // ---- gaps --------------------------------------------------------------
    let largest = input.gaps_ms.iter().copied().max().unwrap_or(0);
    let status = if largest >= limits.gap_warning_ms {
        CheckStatus::Warning
    } else if largest >= limits.gap_notice_ms {
        CheckStatus::Notice
    } else {
        CheckStatus::Ok
    };
    checks.push(IntegrityCheck {
        id: "gaps".to_owned(),
        label: "时间断档".to_owned(),
        status,
        detail: if status == CheckStatus::Ok {
            "日志时间连续，没有明显断档。".to_owned()
        } else {
            format!(
                "最长断档 {} ms，共 {} 处超过 {} ms。断档期间的日志未被采集，崩溃可能就发生在其中。",
                largest,
                input.gaps_ms.iter().filter(|gap| **gap >= limits.gap_notice_ms).count(),
                limits.gap_notice_ms
            )
        },
    });

    // ---- stacks ------------------------------------------------------------
    let stackless = input
        .crashes
        .iter()
        .filter(|crash| !crash.has_stack())
        .count();
    checks.push(IntegrityCheck {
        id: "stacks".to_owned(),
        label: "崩溃堆栈".to_owned(),
        status: if stackless == 0 {
            CheckStatus::Ok
        } else {
            CheckStatus::Warning
        },
        detail: if input.crashes.is_empty() {
            "这次采集没有发现崩溃事件。".to_owned()
        } else if stackless == 0 {
            format!("{} 个崩溃事件都带堆栈。", input.crashes.len())
        } else {
            format!(
                "{} 个崩溃事件里 {} 个没有堆栈：通常是读取被截断，完整现场在 Dropbox 归档或墓碑文件里，崩溃日志采集已包含这两项。",
                input.crashes.len(),
                stackless
            )
        },
    });

    checks
}

/// Counters a running capture keeps, so the checks can run when it ends.
///
/// The reader already knows everything here — whether a line parsed, its timestamp,
/// and how many records the ring dropped — and this type is deliberately the only
/// place that accumulates it. Keeping the arithmetic out of the read loop means the
/// hot path costs a few increments, and the numbers the report quotes are computed by
/// code that has tests rather than by the loop that has a deadline.
#[derive(Debug, Clone, Default)]
pub struct CaptureCounters {
    records: u64,
    unparsed: u64,
    dropped: u64,
    last_ms: Option<i64>,
    gaps_ms: Vec<i64>,
    /// Lines skipped because they were not text (tombstone `.pb`, gzipped dropbox).
    binary: u64,
}

impl CaptureCounters {
    /// A fresh set of counters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One accepted record.
    ///
    /// `parsed` is false when the line matched no grammar and was kept verbatim;
    /// `timestamp_ms` is whatever the record's own time text resolved to, if it did.
    pub fn observe(&mut self, parsed: bool, timestamp_ms: Option<i64>) {
        self.records = self.records.saturating_add(1);
        if !parsed {
            self.unparsed = self.unparsed.saturating_add(1);
        }
        if let Some(now) = timestamp_ms {
            if let Some(previous) = self.last_ms {
                // Out-of-order lines are not a gap; only forward progress counts.
                let gap = now.saturating_sub(previous);
                if gap > 0 {
                    self.gaps_ms.push(gap);
                }
            }
            self.last_ms = Some(now);
        }
    }

    /// Records the ring buffer dropped because it was full.
    pub fn note_dropped(&mut self, count: u64) {
        self.dropped = self.dropped.saturating_add(count);
    }

    /// Accepted records.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Records kept verbatim because nothing could parse them.
    #[must_use]
    pub fn unparsed(&self) -> u64 {
        self.unparsed
    }

    /// Records the ring dropped.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The largest gap seen and how many gaps were recorded.
    #[must_use]
    pub fn gaps(&self) -> (i64, usize) {
        (self.gaps_ms.iter().copied().max().unwrap_or(0), self.gaps_ms.len())
    }

    /// Everything [`assess`] needs.
    #[must_use]
    pub fn into_input(
        self,
        requested_buffers: Vec<String>,
        available_buffers: Vec<String>,
        crashes: Vec<CrashEvent>,
    ) -> IntegrityInput {
        IntegrityInput {
            records: self.records,
            dropped: self.dropped,
            unparsed: self.unparsed,
            requested_buffers,
            available_buffers,
            gaps_ms: self.gaps_ms,
            crashes,
        }
    }
    /// Sets the drop count to an absolute value.
    ///
    /// The ring's counter is absolute and belongs to the session, while this type is
    /// additive by nature. A report can be built several times for the same session, so
    /// the value is *set* rather than added — otherwise the second report would claim
    /// twice the drops of the first.
    pub fn set_dropped(&mut self, count: u64) {
        self.dropped = count;
    }

    /// Counts a line that was not text and was therefore kept out of the stream.
    pub fn note_binary(&mut self, count: u64) {
        self.binary = self.binary.saturating_add(count);
    }

    /// Lines recognised as binary and skipped.
    #[must_use]
    pub fn binary(&self) -> u64 {
        self.binary
    }
}

/// The checks for a session that has just finished.
///
/// One place assembles the counters, the buffers the capture asked for and the
/// crashes it found, because the checks are only meaningful together: "the `events`
/// buffer does not exist" is what explains "no crashes were found", and a caller that
/// supplies one without the other produces a report that misleads. A missing counters
/// value is not an error — it means the capture never counted, and an empty set of
/// counters still yields the six checks (with "no logs received" as the headline).
#[must_use]
pub fn checks_for_session(
    counters: Option<CaptureCounters>,
    requested_buffers: Vec<String>,
    available_buffers: Vec<String>,
    crashes: Vec<CrashEvent>,
    limits: &Limits,
) -> Vec<IntegrityCheck> {
    let input = match counters {
        Some(counters) => counters.into_input(requested_buffers, available_buffers, crashes),
        None => IntegrityInput {
            requested_buffers,
            available_buffers,
            crashes,
            ..IntegrityInput::default()
        },
    };
    let mut checks = assess(&input, limits);
    // Worst first, so a report header can show `checks.first()` and be right.
    checks.sort_by_key(|check| std::cmp::Reverse(check.status));
    checks
}

#[cfg(test)]
mod session_checks_tests {
    use super::*;

    #[test]
    fn counters_are_carried_into_the_checks() {
        let mut counters = CaptureCounters::new();
        counters.observe(true, Some(1_000));
        counters.observe(false, Some(1_100));
        counters.note_dropped(9);
        let checks = checks_for_session(
            Some(counters),
            vec!["main".to_owned()],
            vec!["main".to_owned()],
            Vec::new(),
            &Limits::default(),
        );
        assert_eq!(checks.len(), 6);
        let records = checks.iter().find(|check| check.id == "records").expect("records");
        assert!(records.detail.contains('2'), "{}", records.detail);
        let dropped = checks.iter().find(|check| check.id == "dropped").expect("dropped");
        assert_eq!(dropped.status, CheckStatus::Warning);
        // Warnings lead.
        assert_eq!(checks.first().map(|check| check.status), Some(CheckStatus::Warning));
    }

    #[test]
    fn a_session_that_never_counted_still_gets_a_full_report() {
        let checks = checks_for_session(None, Vec::new(), Vec::new(), Vec::new(), &Limits::default());
        assert_eq!(checks.len(), 6);
        let records = checks.iter().find(|check| check.id == "records").expect("records");
        assert_eq!(records.status, CheckStatus::Notice);
        assert!(records.detail.contains("没有收到任何日志行"));
        assert_eq!(checks.first().map(|check| check.status), Some(CheckStatus::Notice));
    }
}
#[cfg(test)]
mod counter_tests {
    use super::*;

    #[test]
    fn counters_accumulate_what_the_reader_sees() {
        let mut counters = CaptureCounters::new();
        counters.observe(true, Some(1_000));
        counters.observe(false, Some(1_100));
        counters.observe(true, None);
        counters.note_dropped(3);
        counters.note_dropped(4);

        assert_eq!(counters.records(), 3);
        assert_eq!(counters.unparsed(), 1);
        assert_eq!(counters.dropped(), 7);
        let input = counters.into_input(vec!["main".to_owned()], Vec::new(), Vec::new());
        assert_eq!(input.records, 3);
        assert_eq!(input.unparsed, 1);
        assert_eq!(input.dropped, 7);
        assert_eq!(input.gaps_ms, vec![100]);
    }

    #[test]
    fn only_forward_progress_is_a_gap_and_counts_saturate() {
        let mut counters = CaptureCounters::new();
        counters.observe(true, Some(5_000));
        counters.observe(true, Some(5_050));
        // Out of order (a device clock adjustment) is not a gap.
        counters.observe(true, Some(4_000));
        counters.observe(true, Some(9_000));
        let (largest, count) = counters.gaps();
        assert_eq!(count, 2, "5000 to 5050 and 4000 to 9000");
        assert_eq!(largest, 5_000);

        let mut timestampless = CaptureCounters::new();
        for _ in 0..10 {
            timestampless.observe(true, None);
        }
        assert!(timestampless.into_input(Vec::new(), Vec::new(), Vec::new()).gaps_ms.is_empty());

        // Saturation, not overflow: a pathological drop count cannot wrap.
        let mut heavy = CaptureCounters::new();
        heavy.note_dropped(u64::MAX);
        heavy.note_dropped(u64::MAX);
        assert_eq!(heavy.dropped(), u64::MAX);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::structured::{self, CrashOrigin};

    fn crash(with_stack: bool) -> CrashEvent {
        let text = if with_stack {
            "FATAL EXCEPTION: main\nProcess: com.example, PID: 1\njava.lang.RuntimeException: boom\n\tat com.example.Main.onCreate(Main.java:1)\n"
        } else {
            "FATAL EXCEPTION: main\n"
        };
        structured::parse_entry(CrashOrigin::CrashBuffer, text)
            .into_iter()
            .next()
            .expect("event")
    }

    fn healthy() -> IntegrityInput {
        IntegrityInput {
            records: 10_000,
            dropped: 0,
            unparsed: 10,
            requested_buffers: vec!["main".to_owned(), "system".to_owned()],
            available_buffers: vec!["main".to_owned(), "system".to_owned(), "crash".to_owned()],
            gaps_ms: vec![120, 300, 1_000],
            crashes: vec![crash(true)],
        }
    }

    fn find<'a>(checks: &'a [IntegrityCheck], id: &str) -> &'a IntegrityCheck {
        checks.iter().find(|check| check.id == id).expect("check")
    }

    #[test]
    fn a_clean_capture_reports_all_clear() {
        let checks = assess(&healthy(), &Limits::default());
        assert_eq!(checks.len(), 6);
        assert!(
            checks.iter().all(|check| check.status == CheckStatus::Ok),
            "{checks:#?}"
        );
        assert!(find(&checks, "records").detail.contains("10000"));
        assert!(find(&checks, "stacks").detail.contains("1"));
    }

    #[test]
    fn dropped_records_are_a_warning_that_says_what_to_do() {
        let input = IntegrityInput {
            records: 5_000,
            dropped: 5_000,
            ..healthy()
        };
        let checks = assess(&input, &Limits::default());
        let dropped = find(&checks, "dropped");
        assert_eq!(dropped.status, CheckStatus::Warning);
        assert!(dropped.detail.contains("5000"), "{}", dropped.detail);
        assert!(dropped.detail.contains("重采"), "{}", dropped.detail);
    }

    #[test]
    fn the_unparsed_share_escalates() {
        let quiet = IntegrityInput {
            records: 1_000,
            unparsed: 5,
            ..healthy()
        };
        assert_eq!(find(&assess(&quiet, &Limits::default()), "unparsed").status, CheckStatus::Ok);

        let noticeable = IntegrityInput {
            records: 1_000,
            unparsed: 50,
            ..healthy()
        };
        assert_eq!(
            find(&assess(&noticeable, &Limits::default()), "unparsed").status,
            CheckStatus::Notice
        );

        let broken = IntegrityInput {
            records: 1_000,
            unparsed: 400,
            ..healthy()
        };
        let checks = assess(&broken, &Limits::default());
        assert_eq!(find(&checks, "unparsed").status, CheckStatus::Warning);
        assert!(find(&checks, "unparsed").detail.contains("40.0%"));
    }

    #[test]
    fn a_missing_buffer_is_named_and_linked_to_an_empty_crash_list() {
        let input = IntegrityInput {
            requested_buffers: vec!["main".to_owned(), "events".to_owned()],
            available_buffers: vec!["main".to_owned(), "system".to_owned()],
            crashes: Vec::new(),
            ..healthy()
        };
        let checks = assess(&input, &Limits::default());
        let buffers = find(&checks, "buffers");
        assert_eq!(buffers.status, CheckStatus::Warning);
        assert!(buffers.detail.contains("events"), "{}", buffers.detail);
        assert!(buffers.detail.contains("空崩溃列表"), "{}", buffers.detail);
    }

    #[test]
    fn unknown_buffers_are_a_notice_not_a_warning() {
        let input = IntegrityInput {
            available_buffers: Vec::new(),
            ..healthy()
        };
        let checks = assess(&input, &Limits::default());
        assert_eq!(find(&checks, "buffers").status, CheckStatus::Notice);
        assert!(find(&checks, "buffers").detail.contains("无法确认"));
    }

    #[test]
    fn gaps_escalate_and_the_largest_one_is_quoted() {
        let small = IntegrityInput {
            gaps_ms: vec![100, 6_000, 200],
            ..healthy()
        };
        let checks = assess(&small, &Limits::default());
        assert_eq!(find(&checks, "gaps").status, CheckStatus::Notice);
        assert!(find(&checks, "gaps").detail.contains("6000"));

        let huge = IntegrityInput {
            gaps_ms: vec![45_000],
            ..healthy()
        };
        assert_eq!(
            find(&assess(&huge, &Limits::default()), "gaps").status,
            CheckStatus::Warning
        );
    }

    #[test]
    fn a_stackless_crash_points_at_dropbox() {
        let input = IntegrityInput {
            crashes: vec![crash(true), crash(false)],
            ..healthy()
        };
        let checks = assess(&input, &Limits::default());
        let stacks = find(&checks, "stacks");
        assert_eq!(stacks.status, CheckStatus::Warning);
        assert!(stacks.detail.contains("Dropbox"), "{}", stacks.detail);
        assert!(stacks.detail.contains("1 个"), "{}", stacks.detail);
    }

    #[test]
    fn an_empty_capture_is_described_rather_than_reported_as_clean() {
        let checks = assess(&IntegrityInput::default(), &Limits::default());
        assert_eq!(checks.len(), 6);
        assert_eq!(find(&checks, "records").status, CheckStatus::Notice);
        assert!(find(&checks, "records").detail.contains("没有收到任何日志行"));
        // No crashes is stated plainly, not as a warning.
        assert_eq!(find(&checks, "stacks").status, CheckStatus::Ok);
        assert!(find(&checks, "stacks").detail.contains("没有发现崩溃事件"));
        // Division by zero is impossible: percentages come out as 0.0%.
        assert!(find(&checks, "unparsed").detail.contains("0.0%"));
    }

    #[test]
    fn limits_are_configurable() {
        let input = IntegrityInput {
            records: 1_000,
            unparsed: 20,
            ..healthy()
        };
        let strict = Limits {
            unparsed_notice: 0.001,
            unparsed_warning: 0.01,
            ..Limits::default()
        };
        assert_eq!(
            find(&assess(&input, &strict), "unparsed").status,
            CheckStatus::Warning
        );
        assert_eq!(
            find(&assess(&input, &Limits::default()), "unparsed").status,
            CheckStatus::Notice
        );
    }

    #[test]
    fn statuses_are_ordered() {
        assert!(CheckStatus::Warning > CheckStatus::Notice);
        assert!(CheckStatus::Notice > CheckStatus::Ok);
    }
}
