//! Runs a built-in plugin from a native library in the daemon's process.
//!
//! The library is loaded by `smithagents-plugin-loader`; calls cross the
//! boundary as JSON envelopes. While a call runs, the plugin reaches the host
//! services through [`DylibHost`], which finds the call by its number. A
//! native call cannot be stopped: after a timeout the runtime refuses every
//! further call until the daemon restarts, so stuck threads do not pile up.

// Catching a panic needs it to unwind; under `panic = "abort"` a plugin's
// panic would take the daemon down.
#[cfg(panic = "abort")]
compile_error!("smithagents plugins rely on unwinding to contain panics");

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use smithagents_plugin_loader::LoadedPlugin;
use smithagents_plugin_protocol::dylib::{BoxedDylibHost, DylibHost};
use smithagents_plugin_protocol::{
    HostCall, HostMode, HostReply, PluginCall, PluginReply, SyncPage, SyncRequest,
};
use stabby::vec::Vec as StableVec;

use crate::runtime::{CallError, LoadError, PluginRuntime};
use crate::services::CallHandle;

const STOPPED: &str = "plugin stopped after a call timed out; restart the daemon";

pub struct DylibRuntime {
    plugin: Arc<LoadedPlugin>,
    calls: Arc<LiveCalls>,
    next_call: AtomicU64,
    call_timeout: Duration,
    /// Set after a timeout: the stuck call still holds a thread.
    stopped: AtomicBool,
}

impl DylibRuntime {
    /// Loads the library at `path`. Only built-in plugins may be loaded: the
    /// library runs in the daemon's process.
    pub fn load(path: &Path, call_timeout: Duration) -> Result<DylibRuntime, LoadError> {
        let calls = Arc::new(LiveCalls::default());
        let host: BoxedDylibHost = stabby::boxed::Box::new(HostSide {
            calls: Arc::clone(&calls),
        })
        .into();
        let plugin = smithagents_plugin_loader::load(path, host).map_err(|e| LoadError::Dylib {
            message: e.to_string(),
        })?;
        Ok(DylibRuntime {
            plugin: Arc::new(plugin),
            calls,
            next_call: AtomicU64::new(1),
            call_timeout,
            stopped: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl PluginRuntime for DylibRuntime {
    fn mode(&self) -> HostMode {
        HostMode::Dylib
    }

    async fn sync(&self, call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(crashed(STOPPED));
        }
        let number = self.next_call.fetch_add(1, Ordering::Relaxed);
        let input = serde_json::to_vec(&PluginCall::Sync {
            call: number,
            request: req,
        })
        .map_err(|e| crashed(&format!("cannot encode the call: {e}")))?;

        // The plugin reaches the services under `number` until the call ends,
        // on time or not: a stuck call's later requests are refused.
        let _live = self.calls.start(number, call);
        let plugin = Arc::clone(&self.plugin);
        let task = tokio::task::spawn_blocking(move || plugin.call(input));
        let output = match tokio::time::timeout(self.call_timeout, task).await {
            Err(_) => {
                self.stopped.store(true, Ordering::SeqCst);
                return Err(CallError::Timeout);
            }
            Ok(Err(join)) => return Err(crashed(&join.to_string())),
            Ok(Ok(output)) => output,
        };
        let reply = serde_json::from_slice(&output).map_err(|e| CallError::Contract {
            message: format!("the plugin's reply is not valid: {e}"),
        })?;
        let page = match reply {
            PluginReply::Sync(result) => result.map_err(CallError::Plugin)?,
            PluginReply::Panicked { message } => return Err(CallError::Crashed { message }),
            PluginReply::BadCall { message } => {
                return Err(CallError::Contract {
                    message: format!("the plugin could not read the call: {message}"),
                })
            }
        };
        page.validate()?;
        Ok(page)
    }
}

fn crashed(message: &str) -> CallError {
    CallError::Crashed {
        message: message.to_string(),
    }
}

/// The calls running now, by number, with the runtime each was made from.
#[derive(Default)]
struct LiveCalls(Mutex<HashMap<u64, LiveCall>>);

#[derive(Clone)]
struct LiveCall {
    handle: CallHandle,
    /// The runtime that called `sync`: the services' connection pool lives
    /// there, so blocking requests run on it.
    runtime: tokio::runtime::Handle,
}

impl LiveCalls {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<u64, LiveCall>> {
        // The map stays consistent even if a holder panicked.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers call `number` until the returned guard is dropped.
    fn start(&self, number: u64, handle: CallHandle) -> Ended<'_> {
        let runtime = tokio::runtime::Handle::current();
        self.map().insert(number, LiveCall { handle, runtime });
        Ended {
            calls: self,
            number,
        }
    }

    fn get(&self, number: u64) -> Option<LiveCall> {
        self.map().get(&number).cloned()
    }
}

/// Removes its call from [`LiveCalls`] when dropped.
struct Ended<'a> {
    calls: &'a LiveCalls,
    number: u64,
}

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        self.calls.map().remove(&self.number);
    }
}

/// The host as the plugin sees it.
struct HostSide {
    calls: Arc<LiveCalls>,
}

impl HostSide {
    fn answer(&self, input: &[u8]) -> HostReply {
        let call: HostCall = match serde_json::from_slice(input) {
            Ok(call) => call,
            Err(e) => {
                return HostReply::BadCall {
                    message: format!("the host cannot read the call: {e}"),
                }
            }
        };
        let number = match &call {
            HostCall::Http { call, .. } | HostCall::Log { call, .. } => *call,
        };
        let Some(live) = self.calls.get(number) else {
            return HostReply::BadCall {
                message: format!("no call {number} is running"),
            };
        };
        match call {
            // The plugin calls from a blocking thread, outside the runtime.
            HostCall::Http { request, .. } => {
                HostReply::Http(live.runtime.block_on(live.handle.http(request)))
            }
            HostCall::Log { level, message, .. } => {
                live.handle.log(level, &message);
                HostReply::Done
            }
        }
    }
}

impl DylibHost for HostSide {
    extern "C" fn call(&self, input: StableVec<u8>) -> StableVec<u8> {
        // A panic must not unwind into the plugin: that aborts the daemon.
        let reply = catch_unwind(AssertUnwindSafe(|| self.answer(input.as_slice())))
            .unwrap_or_else(|_| HostReply::BadCall {
                message: "the host failed while answering".into(),
            });
        // Serialising the contract's types does not fail; if it ever did, the
        // empty answer reaches the plugin as a reply it cannot read.
        let output = serde_json::to_vec(&reply).unwrap_or_default();
        StableVec::from(output.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(host: &HostSide, input: &[u8]) -> HostReply {
        let output = host.call(StableVec::from(input));
        serde_json::from_slice(output.as_slice()).unwrap()
    }

    #[test]
    fn a_call_the_host_cannot_read_is_a_bad_call() {
        let host = HostSide {
            calls: Arc::default(),
        };
        match ask(&host, b"{\"type\":\"teleport\"}") {
            HostReply::BadCall { message } => {
                assert!(
                    message.starts_with("the host cannot read the call"),
                    "{message}"
                )
            }
            other => panic!("expected BadCall, got {other:?}"),
        }
    }

    fn log(call: u64) -> Vec<u8> {
        serde_json::to_vec(&HostCall::Log {
            call,
            level: smithagents_plugin_protocol::LogLevel::Info,
            message: "hello".into(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn a_log_is_taken_only_while_its_call_runs() {
        use crate::{CallPurpose, HostServices, HttpPolicy, StaticSecrets};

        let manifest = smithagents_plugin_protocol::Manifest::parse(
            "id = \"probe\"\nname = \"Probe\"\nversion = \"0.1.0\"\ncontract = 1\n\
             host = \"in-process\"\nhosts = [\"probe.invalid\"]\n",
        )
        .unwrap();
        let secrets = Arc::new(StaticSecrets(Default::default()));
        let services =
            Arc::new(HostServices::new(&manifest, secrets, HttpPolicy::default()).unwrap());
        let host = HostSide {
            calls: Arc::default(),
        };
        let not_running = HostReply::BadCall {
            message: "no call 9 is running".into(),
        };
        assert_eq!(ask(&host, &log(9)), not_running);
        {
            let _live = host.calls.start(9, services.begin_call(CallPurpose::Sync));
            assert_eq!(ask(&host, &log(9)), HostReply::Done);
        }
        assert_eq!(ask(&host, &log(9)), not_running);
    }
}
