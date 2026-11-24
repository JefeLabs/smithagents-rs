//! Harness that drives the `claude` CLI as a child process.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::claude_parser::parse_events;
use super::{Harness, HarnessError, HarnessEvent, TurnRequest};

const STDERR_MAX_LINES: usize = 20;
const STDERR_MAX_BYTES: usize = 4096;

/// Runs turns by spawning the Claude Code CLI at `bin`.
#[derive(Debug, Clone)]
pub struct ClaudeCodeHarness {
    pub bin: PathBuf,
}

fn build_args(req: &TurnRequest) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        &req.prompt,
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

#[async_trait]
impl Harness for ClaudeCodeHarness {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        let mut child = Command::new(&self.bin)
            .args(build_args(&req))
            .current_dir(&req.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        // Drain stderr concurrently so a full pipe can never block the child.
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut stderr).lines();
            let mut tail: VecDeque<String> = VecDeque::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if tail.len() == STDERR_MAX_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            // Ensure the pipe is fully drained even after a read error.
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            tail_of(&tail)
        });

        let (tx, rx) = mpsc::channel::<HarnessEvent>(64);
        tokio::spawn(async move {
            // `child` lives in this task; dropping it (kill_on_drop) kills the process.
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = tokio::select! {
                    () = tx.closed() => break,
                    l = lines.next_line() => l,
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
                        tracing::warn!("error reading claude stdout: {e}");
                        break;
                    }
                }
            }
            let status = tokio::select! {
                () = tx.closed() => return,
                s = child.wait() => s,
            };
            let tail = stderr_task.await.unwrap_or_default();
            let failure = match status {
                Ok(s) if s.success() => None,
                Ok(s) => Some(format!("claude exited with {s}")),
                Err(e) => Some(format!("failed to wait for claude: {e}")),
            };
            if let Some(mut message) = failure {
                if !tail.is_empty() {
                    message.push_str(&format!("; stderr: {tail}"));
                }
                let _ = tx.send(HarnessEvent::Failed { message }).await;
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }
}
