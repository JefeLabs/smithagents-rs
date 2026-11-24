//! Scripted in-memory harness for tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};

use super::{Harness, HarnessError, HarnessEvent, TurnRequest};

/// Replays pre-scripted turns and records every request it receives.
#[derive(Debug, Clone)]
pub struct FakeHarness {
    scripts: Arc<Mutex<VecDeque<Vec<HarnessEvent>>>>,
    requests: Arc<Mutex<Vec<TurnRequest>>>,
    delay: Duration,
}

impl FakeHarness {
    /// Each `run_turn` call consumes the next script, in order.
    pub fn scripted(turns: Vec<Vec<HarnessEvent>>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(turns.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::ZERO,
        }
    }

    /// Sleep this long before yielding each event.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Harness for FakeHarness {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        self.requests.lock().unwrap().push(req);
        let events = self.scripts.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![HarnessEvent::Failed {
                message: "FakeHarness: no scripted turn left".into(),
            }]
        });
        let delay = self.delay;
        Ok(stream::iter(events)
            .then(move |event| async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                event
            })
            .boxed())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;

    fn req(prompt: &str) -> TurnRequest {
        TurnRequest {
            prompt: prompt.into(),
            cwd: PathBuf::from("/tmp/x"),
            resume_session_id: None,
            model: None,
            system_prompt_append: None,
        }
    }

    fn text(t: &str) -> HarnessEvent {
        HarnessEvent::AssistantText { text: t.into() }
    }

    #[tokio::test]
    async fn replays_scripts_in_order_and_records_requests() {
        let h = FakeHarness::scripted(vec![vec![text("a"), text("b")], vec![text("c")]]);
        let first: Vec<_> = h.run_turn(req("one")).await.unwrap().collect().await;
        let second: Vec<_> = h.run_turn(req("two")).await.unwrap().collect().await;
        assert_eq!(first, vec![text("a"), text("b")]);
        assert_eq!(second, vec![text("c")]);
        let prompts: Vec<_> = h.requests().into_iter().map(|r| r.prompt).collect();
        assert_eq!(prompts, ["one", "two"]);
    }

    #[tokio::test]
    async fn exhausted_script_yields_failed_event() {
        let h = FakeHarness::scripted(vec![]);
        let events: Vec<_> = h.run_turn(req("x")).await.unwrap().collect().await;
        assert!(matches!(events.as_slice(), [HarnessEvent::Failed { .. }]));
    }

    #[tokio::test]
    async fn delay_is_applied_per_event() {
        let h = FakeHarness::scripted(vec![vec![text("a"), text("b")]])
            .with_delay(Duration::from_millis(30));
        let start = Instant::now();
        let events: Vec<_> = h.run_turn(req("x")).await.unwrap().collect().await;
        assert_eq!(events.len(), 2);
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
