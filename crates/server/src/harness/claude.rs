//! Harness that drives the `claude` CLI as a child process.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;

use super::claude_parser::parse_events;
use super::structured::parse_cli_result;
use super::{
    Harness, HarnessError, HarnessEvent, StructuredRequest, StructuredResult, TurnRequest,
};
use crate::child_env::{remove_plugin_secrets, GroupKiller};

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

/// Writes the prompt to claude's stdin from its own task: a prompt larger than
/// the pipe buffer must not hold up reading claude's output. Dropping `stdin`
/// at the end closes it, which ends the prompt.
fn spawn_prompt_writer(mut stdin: tokio::process::ChildStdin, prompt: String) -> AbortOnDrop<()> {
    AbortOnDrop(tokio::spawn(async move {
        if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
            // claude exited (or closed stdin) before reading all of it;
            // how the run ended is reported from its exit and output.
            tracing::debug!("could not write the whole prompt to claude: {e}");
        }
    }))
}

/// Drains stderr concurrently so a full pipe can never block the child,
/// keeping its tail.
fn spawn_stderr_tail(mut stderr: tokio::process::ChildStderr) -> (AbortOnDrop<()>, StderrTail) {
    let tail: StderrTail = Arc::default();
    let task = AbortOnDrop(tokio::spawn({
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
    (task, tail)
}

/// The command line of a structured run (prompt on stdin, as for turns).
///
/// `--tools` limits only the built-in tools. MCP servers from the user's
/// config or plugins would still load and, with permissions bypassed, run
/// unprompted, so `--strict-mcp-config` (with no `--mcp-config`) loads none:
/// a structured run can only use the tools it lists. Chat turns keep them.
fn build_structured_args(req: &StructuredRequest) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "json", "--json-schema"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    args.push(req.schema.to_string());
    args.extend(
        [
            "--dangerously-skip-permissions",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--tools",
        ]
        .map(str::to_owned),
    );
    args.push(req.allowed_tools.join(","));
    if let Some(m) = &req.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(s) = &req.system_prompt_append {
        args.extend(["--append-system-prompt".into(), s.clone()]);
    }
    args
}

/// A turn is given up on while claude may still run: claude is asked to stop
/// first, so it can stop what it started itself (see [`GroupKiller::stop`]).
/// What is left then is killed when the guard is dropped.
async fn give_up(group: &GroupKiller) {
    group.stop().await;
}

impl ClaudeCodeHarness {
    /// claude with `args` in `cwd`, its stdio piped, killed when dropped,
    /// leading a process group of its own (so it can be stopped together
    /// with what it started, see [`GroupKiller`]), and without any plugin
    /// secret in its environment.
    fn command(&self, args: Vec<String>, cwd: &Path) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        remove_plugin_secrets(&mut cmd);
        cmd
    }

    /// Starts [`Self::command`]. A spawn fails with the same "not found"
    /// whether claude is missing or the folder to run it in is, so a failed
    /// one is told apart by looking at the folder.
    async fn spawn(&self, args: Vec<String>, cwd: &Path) -> Result<Child, HarnessError> {
        match self.command(args, cwd).spawn() {
            Ok(child) => Ok(child),
            Err(e) if folder_is_missing(&tokio::fs::metadata(cwd).await) => {
                tracing::debug!("claude was not started, its working folder being gone: {e}");
                Err(HarnessError::MissingCwd(cwd.to_owned()))
            }
            Err(e) => Err(HarnessError::Spawn(e)),
        }
    }
}

/// Whether a look at the working folder (`fs::metadata` of it) shows that it
/// is not there: nothing at the path, something on the way to it that is not
/// a folder, or something there that is not one. A folder that could not be
/// looked at for another reason is not known to be missing.
fn folder_is_missing(looked: &std::io::Result<std::fs::Metadata>) -> bool {
    use std::io::ErrorKind;

    match looked {
        Ok(meta) => !meta.is_dir(),
        Err(e) => matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory),
    }
}

#[async_trait]
impl Harness for ClaudeCodeHarness {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        let mut child = self.spawn(build_args(&req), &req.cwd).await?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stdin_task = spawn_prompt_writer(stdin, req.prompt);
        let (stderr_task, tail) = spawn_stderr_tail(stderr);

        let (tx, rx) = mpsc::channel::<HarnessEvent>(64);
        tokio::spawn(async move {
            // `child` lives in this task; dropping it (kill_on_drop) kills the
            // process, and dropping the task handles stops the prompt writer
            // (which a process holding stdin open could otherwise block
            // forever) and the stderr reader.
            let mut child = child;
            // A turn that is given up on (the stream is dropped, or the
            // daemon stops) takes down what claude started as well: a dev
            // server, an MCP server. Once claude has ended on its own, what it
            // left running is left alone.
            let mut group = GroupKiller::of(child.id());
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
                    () = tx.closed() => {
                        give_up(&group).await;
                        return;
                    }
                    () = tokio::time::sleep_until(since_exit(READ_AFTER_EXIT_MAX)),
                        if exited_at.is_some() => break,
                    exited = child.wait(), if status.is_none() => {
                        // Reaped only when the wait succeeded.
                        if exited.is_ok() {
                            group.disarm();
                        }
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
                                        give_up(&group).await;
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
                    group.kill();
                    let _ = child.kill().await;
                    Some(format!("could not read claude's output: {e}"))
                }
                None => {
                    let status = match status {
                        Some(status) => status,
                        None => tokio::select! {
                            () = tx.closed() => {
                                give_up(&group).await;
                                return;
                            }
                            s = child.wait() => {
                                if s.is_ok() {
                                    group.disarm();
                                }
                                s
                            }
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

    async fn run_structured(
        &self,
        req: StructuredRequest,
    ) -> Result<StructuredResult, HarnessError> {
        let mut child = self.spawn(build_structured_args(&req), &req.cwd).await?;
        // A timeout, or dropping this call, takes down whatever claude started.
        let mut group = GroupKiller::of(child.id());
        let stdin = child.stdin.take().expect("stdin piped");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let _stdin_task = spawn_prompt_writer(stdin, req.prompt.clone());
        let (mut stderr_task, tail) = spawn_stderr_tail(stderr);

        // Output is collected as it arrives so what was printed before claude
        // exited is kept even if a process it left behind holds the pipe open.
        let out: Arc<Mutex<Vec<u8>>> = Arc::default();
        let mut stdout_task = AbortOnDrop(tokio::spawn({
            let out = Arc::clone(&out);
            async move {
                let mut buf = [0u8; 8192];
                while let Ok(n) = tokio::io::AsyncReadExt::read(&mut stdout, &mut buf).await {
                    if n == 0 {
                        break;
                    }
                    out.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&buf[..n]);
                }
            }
        }));

        let waited = tokio::time::timeout(req.timeout, child.wait()).await;
        let status = match waited {
            Err(_) => {
                group.kill();
                let _ = child.kill().await;
                return Err(HarnessError::Timeout);
            }
            Ok(status) => {
                if status.is_ok() {
                    group.disarm();
                }
                // claude did start: this is not a failure to start it.
                status
                    .map_err(|e| HarnessError::Failed(format!("failed to wait for claude: {e}")))?
            }
        };
        // Everything claude printed was in the pipe when it exited.
        let _ = tokio::time::timeout(DRAIN_AFTER_EXIT, &mut stdout_task.0).await;
        drop(stdout_task);
        let stdout_text =
            String::from_utf8_lossy(&out.lock().unwrap_or_else(|e| e.into_inner())).into_owned();

        let result = parse_cli_result(&stdout_text);
        if status.success() || result.is_ok() {
            return result;
        }
        if let Err(HarnessError::Failed(_)) = &result {
            return result;
        }
        // Non-zero exit and nothing usable on stdout.
        let _ =
            tokio::time::timeout_at(Instant::now() + DRAIN_AFTER_EXIT, &mut stderr_task.0).await;
        let mut message = format!("claude exited with {status}");
        let tail = tail_of(&lock_tail(&tail));
        if !tail.is_empty() {
            message.push_str(&format!("; stderr: {tail}"));
        }
        Err(HarnessError::Failed(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_folder_known_to_be_missing_is_called_missing() {
        // After a failed spawn the folder is looked at. Not there, or not a
        // folder: that is why. Anything else about it (no permission on a
        // parent, a disk error) is not known to be the reason, and the
        // spawn's own error is what gets reported.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        let kind = |kind: std::io::ErrorKind| Err(std::io::Error::from(kind));

        assert!(!folder_is_missing(&std::fs::metadata(dir.path())));
        assert!(folder_is_missing(&std::fs::metadata(&file)));
        assert!(folder_is_missing(&std::fs::metadata(
            dir.path().join("gone")
        )));
        assert!(folder_is_missing(&kind(std::io::ErrorKind::NotFound)));
        assert!(folder_is_missing(&kind(std::io::ErrorKind::NotADirectory)));
        assert!(!folder_is_missing(&kind(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!folder_is_missing(&kind(std::io::ErrorKind::Other)));
    }

    #[test]
    fn claude_does_not_inherit_plugin_secrets() {
        // Set once and never removed: a name no other test uses.
        std::env::set_var("SMITHAGENTS_SECRET_HARNESS_TEST__TOKEN", "not-for-agents");
        let harness = ClaudeCodeHarness {
            bin: PathBuf::from("claude"),
        };
        let cmd = harness.command(vec!["-p".into()], Path::new("."));
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        let name = std::ffi::OsStr::new("SMITHAGENTS_SECRET_HARNESS_TEST__TOKEN");
        assert_eq!(envs.get(name).copied(), Some(None));
    }
}
