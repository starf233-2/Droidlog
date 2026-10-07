//! Known crash signatures, so the common ones get a cause instead of a stack.
//!
//! The point is not to classify everything — it is to stop the *frequent* failures from
//! making the user read twenty frames to learn one sentence. Each pattern carries a cause
//! and an action, and the table is deliberately short and specific: a wrong "known cause"
//! is worse than no entry at all, so nothing goes in here that has not been seen in a real
//! capture (this project's own MIX 4 logs are the source).
//!
//! Matching is on the exception text and message, case-sensitively, on the *needle*
//! being present. Patterns are tried in order, so put the specific ones first.

use serde::{Deserialize, Serialize};

use crate::crash::structured::CrashEvent;

/// One recognised signature.
struct Pattern {
    /// Substring that identifies it, matched against exception then message.
    needle: &'static str,
    /// What it is, in a few words.
    title: &'static str,
    /// The root cause.
    cause: &'static str,
    /// What to do about it.
    action: &'static str,
}

/// The table, most specific first.
const PATTERNS: &[Pattern] = &[
    Pattern {
        needle: "Cannot initialize effect engine for type:",
        title: "音效 effect 初始化失败（Dirac / 厂商音效）",
        cause: "音频 HAL 拒绝创建该 effect：厂商音效库（Dirac 等）与当前音频驱动不匹配，或该 effect 已被系统禁用。",
        action: "关闭对应音效（设置 → 声音 → 音效）或更新系统；这是音频模块的问题，不是应用自身的 bug。",
    },
    Pattern {
        needle: "java.lang.OutOfMemoryError",
        title: "内存不足",
        cause: "进程在分配时被拒：堆已满、或系统整体内存压力过大（崩溃常紧随 lowmemorykiller 之后）。",
        action: "查看同一次采集里的「内存回收」异常与堆栈中的分配点；关注图片/大缓冲的持有位置。",
    },
    Pattern {
        needle: "TransactionTooLargeException",
        title: "Binder 事务过大",
        cause: "一次 IPC 传输的数据超过 Binder 缓冲上限（约 1 MB）：通常是把大对象塞进了 Intent/Bundle 或返回值。",
        action: "缩小跨进程传递的数据（改用文件/ContentProvider），这是调用方需要改的代码问题。",
    },
    Pattern {
        needle: "DeadSystemException",
        title: "system_server 已死亡",
        cause: "系统进程正在重启，应用在这次调用中撞上了死亡的系统服务。",
        action: "真正的根因在 system_server 的崩溃里：查看同一次采集的 Dropbox（system_server_crash）与内核日志。",
    },
    Pattern {
        needle: "Permission Denial",
        title: "权限被拒",
        cause: "调用了未在清单声明或用户未授权的受保护接口（组件/Provider/服务）。",
        action: "核对调用方的权限声明与运行时授权，以及被调组件的 exported 设置。",
    },
    Pattern {
        needle: "FileUriExposedException",
        title: "跨应用暴露 file:// URI",
        cause: "把 file:// 传给了另一个应用（Android 7 起禁止），应使用 FileProvider。",
        action: "改用 FileProvider 的 content:// URI。",
    },
    Pattern {
        needle: "Resources$NotFoundException",
        title: "资源找不到",
        cause: "引用的资源在该配置/该设备上不存在（常与按 density/locale 拆分资源、或热更新后资源表不一致有关）。",
        action: "核对资源是否落在正确的限定目录；热更新场景下检查资源表与代码是否同版本。",
    },
    Pattern {
        needle: "FORTIFY: ",
        title: "原生库 FORTIFY 检查失败",
        cause: "libc 的加固检查发现了非法内存调用（空指针、越界、已释放对象），随后 abort。",
        action: "从墓碑的 backtrace 找第一个业务库帧；这是原生代码的内存错误。",
    },
    Pattern {
        needle: "pthread_create failed",
        title: "线程创建失败",
        cause: "进程或系统线程数达到上限（或内存不足），无法再创建线程。",
        action: "检查线程泄漏；系统级耗尽见同一次采集的「线程耗尽」异常。",
    },
    Pattern {
        needle: "Too many open files",
        title: "文件描述符耗尽",
        cause: "进程打开的 fd 达到上限，通常是 fd 泄漏。",
        action: "检查未关闭的 Socket/文件/Cursor；同一次采集的「fd 耗尽」异常会给出证据行。",
    },
];

/// A crash that matched a pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownNote {
    /// The crash this describes — the same id the timeline uses.
    pub event_id: String,
    /// What it is, in a few words.
    pub title: String,
    /// The root cause.
    pub cause: String,
    /// What to do about it.
    pub action: String,
}

/// Matches one crash against the table.
#[must_use]
fn pattern_for(event: &CrashEvent) -> Option<&'static Pattern> {
    let haystacks = [event.exception.as_deref(), event.message.as_deref()];
    PATTERNS
        .iter()
        .find(|pattern| haystacks.iter().flatten().any(|text| text.contains(pattern.needle)))
}

/// The known-cause note for one crash, when it matches.
#[must_use]
pub fn note_for(event: &CrashEvent) -> Option<KnownNote> {
    let pattern = pattern_for(event)?;
    Some(KnownNote {
        event_id: event.id.clone(),
        title: pattern.title.to_owned(),
        cause: pattern.cause.to_owned(),
        action: pattern.action.to_owned(),
    })
}

/// Known-cause notes for a whole capture, in the order the crashes appear.
#[must_use]
pub fn notes_for(events: &[CrashEvent]) -> Vec<KnownNote> {
    events.iter().filter_map(note_for).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::structured::{parse_entry, CrashOrigin};

    fn event_from(text: &str) -> CrashEvent {
        parse_entry(CrashOrigin::CrashBuffer, text)
            .into_iter()
            .next()
            .expect("one event")
    }

    #[test]
    fn the_dirac_effect_failure_is_recognised() {
        // Verbatim from the capture this project debugged on a MIX 4.
        let text = "FATAL EXCEPTION: main\nProcess: com.miui.misound, PID: 23453\njava.lang.RuntimeException: Cannot initialize effect engine for type: ec7178ec-e5e1-4329-9651-166e2a5b5c3c\n\tat android.media.audiofx.AudioEffect.<init>(AudioEffect.java:466)\n";
        let note = note_for(&event_from(text)).expect("recognised");
        assert!(note.title.contains("effect"), "{}", note.title);
        assert!(note.cause.contains("音频"), "{}", note.cause);
        assert_eq!(note.event_id, "crashBuffer#0");
    }

    #[test]
    fn an_unknown_crash_gets_no_note() {
        // A wrong "known cause" is worse than none, so this must stay empty.
        let text = "FATAL EXCEPTION: main\nProcess: com.example, PID: 7\njava.lang.IllegalStateException: something specific to this app\n\tat com.example.Main.onCreate(Main.java:1)\n";
        assert!(note_for(&event_from(text)).is_none());
        assert!(notes_for(&[]).is_empty());
    }

    #[test]
    fn malformed_input_never_panics_and_matches_nothing_by_accident() {
        for text in ["", "   ", "FATAL EXCEPTION", "\u{0}\u{1}\u{2}", "not a crash at all"] {
            for event in parse_entry(CrashOrigin::CrashBuffer, text) {
                // The only requirement: no panic, and no invented cause.
                assert!(note_for(&event).is_none(), "{text:?}");
            }
        }
    }

    #[test]
    fn notes_follow_the_order_of_the_crashes() {
        let first = "FATAL EXCEPTION: main\nProcess: com.a, PID: 1\njava.lang.OutOfMemoryError: Failed to allocate\n";
        let second = "FATAL EXCEPTION: main\nProcess: com.b, PID: 2\njava.lang.RuntimeException: Cannot initialize effect engine for type: x\n";
        let unknown = "FATAL EXCEPTION: main\nProcess: com.c, PID: 3\njava.lang.IllegalStateException: nope\n";
        let mut events = parse_entry(CrashOrigin::CrashBuffer, first);
        events.extend(parse_entry(CrashOrigin::CrashBuffer, second));
        events.extend(parse_entry(CrashOrigin::CrashBuffer, unknown));
        let notes = notes_for(&events);
        assert_eq!(notes.len(), 2, "{notes:#?}");
        assert!(notes[0].title.contains("内存"), "{}", notes[0].title);
        assert!(notes[1].title.contains("effect"), "{}", notes[1].title);
    }
}
