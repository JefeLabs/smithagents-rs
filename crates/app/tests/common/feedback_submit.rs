//! [`FakeFeedback`]: feedback commands that record each call, then answer it
//! with the next result queued, or else wait until the test answers it (or
//! forever), like `FakeIntents`. As there, the link's `Action` for an answer
//! is left to the test.

use std::cell::RefCell;
use std::collections::VecDeque;

use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use smithagents_app::link::{FeedbackCommands, LinkError};
use smithagents_core::feedback::FeedbackSettings;

/// One feedback command, as a view sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackCall {
    SetSettings(FeedbackSettings),
}

type Reply = Result<FeedbackSettings, LinkError>;

#[derive(Default)]
pub struct FakeFeedback {
    pub calls: RefCell<Vec<FeedbackCall>>,
    results: RefCell<VecDeque<Reply>>,
    pending: RefCell<VecDeque<oneshot::Sender<Reply>>>,
}

impl FakeFeedback {
    /// The answer of the next call.
    pub fn queue(&self, result: Reply) {
        self.results.borrow_mut().push_back(result);
    }

    /// Answers the oldest call still waiting.
    pub fn answer(&self, result: Reply) {
        let sender = self
            .pending
            .borrow_mut()
            .pop_front()
            .expect("a feedback command waits for an answer");
        let _ = sender.send(result);
    }

    fn reply(&self, call: FeedbackCall) -> LocalBoxFuture<'static, Reply> {
        self.calls.borrow_mut().push(call);
        if let Some(result) = self.results.borrow_mut().pop_front() {
            return async move { result }.boxed_local();
        }
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().push_back(tx);
        async move { rx.await.unwrap_or(Err(LinkError::Stopped)) }.boxed_local()
    }
}

impl FeedbackCommands for FakeFeedback {
    fn set_feedback_settings(
        &self,
        settings: FeedbackSettings,
    ) -> LocalBoxFuture<'static, Result<FeedbackSettings, LinkError>> {
        self.reply(FeedbackCall::SetSettings(settings))
    }
}
