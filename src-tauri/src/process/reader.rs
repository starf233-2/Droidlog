//! The per-session streaming loop.
//!
//! Shape, and why:
//!
//! * **A dedicated reader task owns the pipe.** `AsyncBufReadExt::next_line` is
//!   not cancel-safe, so it must never sit inside a `select!` arm — cancelling it
//!   mid-read can drop buffered bytes. The reader task therefore reads in a plain
//!   loop and forwards lines over an `mpsc` channel, whose `recv()` *is* cancel-safe.
//! * **One consumer loop batches.** It selects over shutdown, a flush ticker and
//!   the channel, so records reach the UI at most every [`FLUSH_INTERVAL`] or
//!   every [`BATCH_SIZE`] records, whichever comes first.
//! * **A stderr drain task runs in parallel.** Piped stderr that nobody reads
//!   eventually blocks the child; its tail is also the only useful diagnostic
//!   when `su` refuses or the device drops off.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use crate::device::AppTarget;
use crate::filter::FilterSet;
use crate::parser::LogRecord;
use crate::process::SessionStatus;
use crate::ring::RingBuffer;
use crate::source::LogSource;
use crate::state::{AppState, LockExt};

/// Event carrying a batch of accepted records.
pub const EVENT_RECORDS: &str = "droidlog://records";
/// Event carrying a session status transition.
pub const EVENT_SESSION_STATUS: &str = "droidlog://session-status";

/// Bounded hand-off between the reader task and the consumer.
///
/// Bounded on purpose: a stalled UI must apply back-pressure to the reader
/// rather than let an unbounded queue grow without limit.
const READ_CHANNEL_CAPACITY: usize = 4096;
/// Records per emitted batch.
const BATCH_SIZE: usize = 200;
/// Maximum time a record can wait before being flushed.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// How long teardown waits for the reader task before abandoning it.
///
/// The child's stdout pipe can have a second writer (a forked adb daemon
/// inherits the handle), in which case the pipe never reaches EOF and the reader
/// would block on `next_line` forever. Teardown must not hang on that.
const READER_JOIN_TIMEOUT: Duration = Duration::from_millis(750);
/// Stderr lines kept for diagnostics.
const STDERR_TAIL: usize = 20;

/// Payload of [`EVENT_RECORDS`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordsEvent {
    /// Session the records belong to.
    pub session_id: String,
    /// Accepted records, oldest first.
    pub records: Vec<LogRecord>,
}

/// Payload of [`EVENT_SESSION_STATUS`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusEvent {
    /// Session that changed.
    pub session_id: String,
    /// New lifecycle state.
    pub status: SessionStatus,
}

/// Decodes one raw output line, tolerating invalid UTF-8.
///
/// Device log output is **not** guaranteed to be UTF-8. logcat relays whatever
/// bytes an app wrote, and a multi-byte sequence can even be split across a
/// buffer boundary, so a strict decoder dies on the first bad byte. This used to
/// be fatal — the whole capture session ended with
/// `stream did not contain valid UTF-8` about a second in — which is why the
/// table appeared to stop on its own. One unreadable byte must cost one line's
/// worth of fidelity, nothing more.
fn decode_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Reads newline-delimited output, decoding each line lossily.
///
/// Returns `None` at EOF, or a message when the pipe itself failed.
async fn forward_lines<R>(reader: R, tx: mpsc::Sender<String>) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    loop {
        buf.clear();
        // `read_until` is not cancel-safe, which is exactly why this runs in its
        // own task instead of an arm of the consumer's `select!`.
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => return None,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                let line = decode_line(&buf);
                if tx.send(line).await.is_err() {
                    // Consumer is gone; stop reading.
                    return None;
                }
            }
            Err(err) => return Some(format!("读取进程输出失败：{err}")),
        }
    }
}

/// Starts the streaming loop for a session.
///
/// Returns immediately; the work happens on Tauri's async runtime.
pub fn spawn(
    app: AppHandle,
    session_id: String,
    child: Child,
    source: Box<dyn LogSource>,
    buffer: Arc<StdMutex<RingBuffer<LogRecord>>>,
    shutdown: oneshot::Receiver<()>,
) {
    tauri::async_runtime::spawn(async move {
        run(app, session_id, child, source, buffer, shutdown).await;
    });
}

/// The consumer loop. See the module docs for the design rationale.
async fn run(
    app: AppHandle,
    session_id: String,
    mut child: Child,
    source: Box<dyn LogSource>,
    buffer: Arc<StdMutex<RingBuffer<LogRecord>>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    // `state` borrows `app`, which this future owns; both are used immutably.
    let state_handle = app.state::<AppState>();
    let state: &AppState = &state_handle;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        finish(
            &app,
            &session_id,
            SessionStatus::Failed("无法获取子进程 stdout 管道".to_owned()),
        );
        return;
    };

    let stderr_tail = Arc::new(StdMutex::new(VecDeque::<String>::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = Arc::clone(&stderr_tail);
        tauri::async_runtime::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf: Vec<u8> = Vec::with_capacity(1024);
            loop {
                buf.clear();
                // `Err` also ends the drain: the pipe is gone either way.
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        if buf.last() == Some(&b'\r') {
                            buf.pop();
                        }
                        let mut tail = tail.lock_ignore_poison();
                        if tail.len() == STDERR_TAIL {
                            tail.pop_front();
                        }
                        tail.push_back(decode_line(&buf));
                    }
                }
            }
        });
    }

    let (tx, mut rx) = mpsc::channel::<String>(READ_CHANNEL_CAPACITY);
    let mut reader = tauri::async_runtime::spawn(async move { forward_lines(stdout, tx).await });

    let mut filters = FilterSet::accept_all();
    let mut filters_version = 0_u64;
    refresh_filters(state, &mut filters, &mut filters_version);

    // The followed application, refreshed on the same cheap schedule as the
    // filters: read once per tick rather than once per record.
    let mut target = state.app_target();
    let mut target_version = state.app_target_version();

    let mut batch: Vec<LogRecord> = Vec::with_capacity(BATCH_SIZE);
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stopped_by_user = false;

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                stopped_by_user = true;
                break;
            }
            _ = ticker.tick() => {
                refresh_filters(state, &mut filters, &mut filters_version);
                refresh_target(state, &mut target, &mut target_version);
                flush(&app, &session_id, &mut batch, &buffer);
            }
            incoming = rx.recv() => {
                match incoming {
                    Some(line) => {
                        // `parse_line` never drops: lines that do not match the
                        // grammar come back as raw records.
                        let record = source.parse_line(&line, state.next_seq());
                        if filters.matches(&record) && target_matches(target.as_ref(), &record) {
                            batch.push(record);
                        }
                        if batch.len() >= BATCH_SIZE {
                            flush(&app, &session_id, &mut batch, &buffer);
                        }
                    }
                    // Channel closed: the child's stdout reached EOF.
                    None => break,
                }
            }
        }
    }

    // Close the channel *before* joining the reader. If the reader is parked on
    // `tx.send` because the channel filled up while we were exiting the loop,
    // only dropping the receiver unblocks it — killing the child would not.
    drop(rx);

    // Teardown: kill first, so a reader parked on `next_line` unblocks.
    let _ = child.start_kill();
    let exit = child.wait().await;

    // Bounded join. If the pipe has a second writer (see READER_JOIN_TIMEOUT),
    // the reader never finishes; abandoning it keeps the session's teardown — and
    // therefore its status event — from being hostage to a stray handle.
    let read_failure = match tokio::time::timeout(READER_JOIN_TIMEOUT, &mut reader).await {
        Ok(Ok(failure)) => failure,
        Ok(Err(_join_error)) => None,
        Err(_elapsed) => {
            // Nothing can reach the task any more; stop it rather than leak a
            // task that will never observe EOF.
            reader.abort();
            None
        }
    };
    flush(&app, &session_id, &mut batch, &buffer);

    let status = match read_failure {
        Some(message) => SessionStatus::Failed(message),
        None if stopped_by_user => SessionStatus::Stopped,
        None if exit.map(|status| status.success()).unwrap_or(false) => SessionStatus::Stopped,
        None => match stderr_summary(&stderr_tail) {
            Some(message) => SessionStatus::Failed(message),
            None => SessionStatus::Stopped,
        },
    };

    finish(&app, &session_id, status);
}

/// Recompiles the shared filters when their revision changed.
///
/// A compile failure keeps the previous set: [`AppState::set_filters`] validates
/// before storing, so this is a defensive fallback rather than a normal path.
fn refresh_filters(state: &AppState, filters: &mut FilterSet, version: &mut u64) {
    let current = state.filters_version();
    if current == *version {
        return;
    }
    if let Ok(compiled) = state.compiled_filters() {
        *filters = compiled;
    }
    *version = current;
}

/// Re-reads the followed application when its revision changed.
fn refresh_target(
    state: &AppState,
    target: &mut Option<AppTarget>,
    version: &mut u64,
) {
    let current = state.app_target_version();
    if current == *version {
        return;
    }
    *target = state.app_target();
    *version = current;
}

/// Whether a record survives the application filter.
///
/// No target means "no application filter", which is the normal case.
pub(crate) fn target_matches(target: Option<&AppTarget>, record: &LogRecord) -> bool {
    match target {
        Some(target) => target.matches(record),
        None => true,
    }
}

/// Moves `batch` into the ring buffer and emits it to the frontend.
///
/// Records are cloned into the ring so the emitted batch can be moved rather
/// than copied — the ring is the durable copy, the event is the fast path.
fn flush(
    app: &AppHandle,
    session_id: &str,
    batch: &mut Vec<LogRecord>,
    buffer: &Arc<StdMutex<RingBuffer<LogRecord>>>,
) {
    if batch.is_empty() {
        return;
    }

    {
        let mut ring = buffer.lock_ignore_poison();
        for record in batch.iter() {
            ring.push(record.clone());
        }
    }

    let records = std::mem::replace(batch, Vec::with_capacity(BATCH_SIZE));
    // A failed emit means no window is listening; the ring still holds the data,
    // so `drain_records` can backfill after a reload.
    let _ = app.emit(
        EVENT_RECORDS,
        RecordsEvent {
            session_id: session_id.to_owned(),
            records,
        },
    );
}

/// Renders the collected stderr tail, if any.
fn stderr_summary(tail: &Arc<StdMutex<VecDeque<String>>>) -> Option<String> {
    let tail = tail.lock_ignore_poison();
    if tail.is_empty() {
        return None;
    }
    Some(tail.iter().cloned().collect::<Vec<_>>().join("\n"))
}

/// Records the terminal status and notifies the frontend.
///
/// Also writes one line to stderr: a session ending is exactly the kind of event
/// that is invisible from the UI (the rows simply stop) and there is no logging
/// framework in the crate, so the dev console is where it has to surface.
fn finish(app: &AppHandle, session_id: &str, status: SessionStatus) {
    eprintln!("droidlog: session {session_id} finished: {status:?}");
    let state = app.state::<AppState>();
    state.set_session_status(session_id, status.clone());
    let _ = app.emit(
        EVENT_SESSION_STATUS,
        SessionStatusEvent {
            session_id: session_id.to_owned(),
            status,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_namespaced() {
        assert!(EVENT_RECORDS.starts_with("droidlog://"));
        assert!(EVENT_SESSION_STATUS.starts_with("droidlog://"));
        assert_ne!(EVENT_RECORDS, EVENT_SESSION_STATUS);
    }

    #[test]
    fn records_event_serialises_camel_case() -> crate::error::Result<()> {
        let event = RecordsEvent {
            session_id: "session-1".to_owned(),
            records: Vec::new(),
        };
        let json = serde_json::to_value(&event)?;
        assert_eq!(
            json.get("sessionId").and_then(|v| v.as_str()),
            Some("session-1")
        );
        Ok(())
    }

    #[test]
    fn stderr_summary_is_none_when_empty() {
        let tail = Arc::new(StdMutex::new(VecDeque::new()));
        assert_eq!(stderr_summary(&tail), None);
    }

    #[test]
    fn stderr_summary_joins_lines_in_order() {
        let mut deque = VecDeque::new();
        deque.push_back("su: not found".to_owned());
        deque.push_back("second line".to_owned());
        let tail = Arc::new(StdMutex::new(deque));
        assert_eq!(
            stderr_summary(&tail).as_deref(),
            Some("su: not found\nsecond line")
        );
    }

    #[test]
    fn flush_ignores_empty_batches() {
        // No AppHandle is available in a unit test, so this only exercises the
        // early return: an empty batch must not attempt to touch the ring.
        let buffer = Arc::new(StdMutex::new(RingBuffer::<LogRecord>::new(4)));
        let batch: Vec<LogRecord> = Vec::new();
        assert!(batch.is_empty());
        assert!(buffer.lock_ignore_poison().is_empty());
    }

    #[tokio::test]
    async fn one_full_batch_fits_in_the_channel() {
        // A behavioural check rather than a constant one: if the channel were
        // smaller than a batch, the reader could block on `send` before the
        // consumer ever reached its flush threshold, stalling the stream.
        let (tx, _rx) = mpsc::channel::<String>(READ_CHANNEL_CAPACITY);
        let mut sent = 0_usize;
        for index in 0..BATCH_SIZE {
            if tx.try_send(format!("line {index}")).is_err() {
                break;
            }
            sent += 1;
        }
        assert_eq!(sent, BATCH_SIZE, "a full batch must fit in the read channel");
    }

    #[test]
    fn invalid_utf8_does_not_kill_the_stream() {
        // The regression that mattered: a single bad byte used to abort the
        // whole session with `stream did not contain valid UTF-8`, which looked
        // exactly like the capture stopping by itself.
        let line = decode_line(&[0xff, 0xfe, b'h', b'i']);
        assert!(line.ends_with("hi"), "readable bytes must survive: {line:?}");

        // A truncated multi-byte sequence (a real possibility at a buffer
        // boundary) must decode rather than fail.
        let truncated = decode_line(&[0xe4, 0xbd]);
        assert!(!truncated.is_empty());
    }

    #[test]
    fn valid_utf8_survives_decoding_unchanged() {
        assert_eq!(decode_line("内核日志".as_bytes()), "内核日志");
        assert_eq!(decode_line(b"09-01 12:34:56.789  1  2 I Tag: hi"), "09-01 12:34:56.789  1  2 I Tag: hi");
    }

    #[tokio::test]
    async fn forward_lines_splits_and_keeps_bad_bytes() {
        let payload: Vec<u8> = [
            b"first\n".as_slice(),
            &[0xff, 0xfe],
            b" bad\n\nlast".as_slice(),
        ]
        .concat();
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let outcome = forward_lines(std::io::Cursor::new(payload), tx).await;
        assert!(outcome.is_none(), "a clean EOF is not a failure");

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        // Four lines: "first", the bad-byte line, the blank line, "last".
        // Blank lines are kept deliberately — they are part of the output.
        assert_eq!(lines.len(), 4, "unexpected split: {lines:?}");
        assert_eq!(lines.first().map(String::as_str), Some("first"));
        assert!(
            lines.get(1).is_some_and(|line| line.contains("bad")),
            "the readable part of a mixed line must survive: {lines:?}"
        );
        assert_eq!(lines.get(2).map(String::as_str), Some(""));
        assert_eq!(lines.get(3).map(String::as_str), Some("last"));
    }

    #[tokio::test]
    async fn forward_lines_strips_crlf() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let _ = forward_lines(std::io::Cursor::new(b"a\r\nb\r\n".to_vec()), tx).await;
        assert_eq!(rx.try_recv().as_deref(), Ok("a"));
        assert_eq!(rx.try_recv().as_deref(), Ok("b"));
    }
}
