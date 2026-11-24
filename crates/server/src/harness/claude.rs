//! Harness that drives the `claude` CLI as a child process.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;

use super::claude_parser::parse_events;
use super::{Harness, HarnessError, HarnessEvent, TurnRequest};

const STDERR_MAX_LINES: usize = 20;
const STDERR_MAX_BYTES: usize = 4096;
/// How long to keep waiting for more output once claude has exited. A process
/// claude started may keep stdout or stderr open long after, so their end is
/// not waited for. Output already in the pipe is still read after this.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);
/// Reading stdout stops this long after claude exited even if more output
/// keeps arriving (from a process it left behind).
const READ_AFTER_EXIT_MAX: Duration = Duration::from_secs(10);

/// Runs turns by spawning the Claude Code CLI at `bin`.
#[derive(Debug, Clone)]
pub struct ClaudeCodeHarness {
    pub bin: PathBuf,
}

/// The command line of a turn. `-p` is a flag: the prompt itself is written
/// to claude's stdin, so a prompt starting with `-` is never read as an
/// option and its size is not bounded by the argv limit.
fn build_args(req: &TurnRequest) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--dangerously-skip-permissions",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if let Some(id) = &req.resume_session_id {
        args.extend(["--resume".into(), id.clone()]);
    }
    if let Some(m) = &req.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(s) = &req.system_prompt_append {
        args.extend(["--append-system-prompt".into(), s.clone()]);
    }
    args
}

/// Keep only the tail of stderr (last lines, bounded in bytes).
fn tail_of(lines: &VecDeque<String>) -> String {
    let joined = lines.iter().cloned().collect::<Vec<_>>().join("\n");
    if joined.len() <= STDERR_MAX_BYTES {
        return joined;
    }
    let mut start = joined.len() - STDERR_MAX_BYTES;
    while !joined.is_char_boundary(start) {
        start += 1;
    }
    joined[start..].to_owned()
}

/// A task that is aborted when this handle is dropped, so no exit path of the
/// turn can leave it running.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The last lines claude wrote to stderr, shared with the task reading them so
/// they can be reported even when the pipe never closes.
type StderrTail = Arc<Mutex<VecDeque<String>>>;

fn lock_tail(tail: &StderrTail) -> std::sync::MutexGuard<'_, VecDeque<String>> {
    // Pushing and popping lines cannot leave the deque inconsistent.
    tail.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl Harness for ClaudeCodeHarness {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        let mut child = Command::new(&self.bin)
            .args(build_args(&req))
            .current_dir(&req.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        // Written from its own task: a prompt larger than the pipe buffer must
        // not hold up reading claude's output. Dropping `stdin` at the end
        // closes it, which ends the prompt.
        let prompt = req.prompt;
        let stdin_task = AbortOnDrop(tokio::spawn(async move {
            if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
                // claude exited (or closed stdin) before reading all of it;
                // how the turn ended is reported from its exit and output.
                tracing::debug!("could not write the whole prompt to claude: {e}");
            }
        }));

        // Drain stderr concurrently so a full pipe can never block the child.
        let tail: StderrTail = Arc::default();
        let stderr_task = AbortOnDrop(tokio::spawn({
            let tail = Arc::clone(&tail);
            async move {
                let mut lines = BufReader::new(&mut stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut tail = lock_tail(&tail);
                    if tail.len() == STDERR_MAX_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                // Ensure the pipe is fully drained even after a read error.
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            }
        }));

        let (tx, rx) = mpsc::channel::<HarnessEvent>(64);
        tokio::spawn(async move {
            // `child` lives in this task; dropping it (kill_on_drop) kills the
            // process, and dropping the task handles stops the prompt writer
            // (which a process holding stdin open could otherwise block
            // forever) and the stderr reader.
            let _stdin_task = stdin_task;
            let mut stderr_task = stderr_task;
            let mut lines = BufReader::new(stdout).lines();
            let mut status = None;
            // When claude exited; the deadlines below only apply from then on.
            let mut exited_at: Option<Instant> = None;
            let mut read_error = None;
            loop {
                let since_exit = |limit: Duration| exited_at.unwrap_or_else(Instant::now) + limit;
                // Biased: a line already in the pipe always wins over the
                // drain deadline, so the final `result` line cannot be lost to
                // a consumer that was slow to take the lines before it.
                let line = tokio::select! {
                    biased;
                    () = tx.closed() => return,
                    () = tokio::time::sleep_until(since_exit(READ_AFTER_EXIT_MAX)),
                        if exited_at.is_some() => break,
                    exited = child.wait(), if status.is_none() => {
                        status = Some(exited);
                        exited_at = Some(Instant::now());
                        continue;
                    }
                    l = lines.next_line() => l,
                    // Everything claude printed was in the pipe when it exited
                    // and has been read; only a process it left behind can
                    // still hold stdout open, so stop waiting for the end.
                    () = tokio::time::sleep_until(since_exit(DRAIN_AFTER_EXIT)),
                        if exited_at.is_some() => break,
                };
                match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match parse_events(&line) {
                            Ok(events) => {
                                for ev in events {
                                    if tx.send(ev).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(e) => tracing::warn!("skipping unparsable claude line: {e}"),
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        read_error = Some(e);
                        break;
                    }
                }
            }
            let failure = match read_error {
                Some(e) => {
                    // Its output can no longer be followed: stop claude rather
                    // than wait for it.
                    tracing::warn!("error reading claude stdout: {e}; stopping claude");
                    let _ = child.kill().await;
                    Some(format!("could not read claude's output: {e}"))
                }
                None => {
                    let status = match status {
                        Some(status) => status,
                        None => tokio::select! {
                            () = tx.closed() => return,
                            s = child.wait() => s,
                        },
                    };
                    match status {
                        Ok(s) if s.success() => None,
                        Ok(s) => Some(format!("claude exited with {s}")),
                        Err(e) => Some(format!("failed to wait for claude: {e}")),
                    }
                }
            };
            let Some(mut message) = failure else {
                return;
            };
            // A process claude started may keep stderr open; do not wait for
            // its end past the drain deadline. The tail read so far is kept.
            let deadline = exited_at.unwrap_or_else(Instant::now) + DRAIN_AFTER_EXIT;
            let _ = tokio::time::timeout_at(deadline, &mut stderr_task.0).await;
            drop(stderr_task);
            let tail = tail_of(&lock_tail(&tail));
            if !tail.is_empty() {
                message.push_str(&format!("; stderr: {tail}"));
            }
            let _ = tx.send(HarnessEvent::Failed { message }).await;
        });

        Ok(ReceiverStream::new(rx).boxed())
    }
}
