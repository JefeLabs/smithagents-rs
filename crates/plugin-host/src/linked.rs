//! Runs a plugin linked into the daemon's own process.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use smithagents_plugin_protocol::{
    HostApi, HostMode, HttpError, HttpRequest, HttpResponse, LogLevel, Plugin, SyncPage,
    SyncRequest,
};

use crate::runtime::{CallError, PluginRuntime};
use crate::services::CallHandle;

pub struct LinkedRuntime {
    plugin: Arc<dyn Plugin>,
    call_timeout: Duration,
}

impl LinkedRuntime {
    pub fn new(plugin: Arc<dyn Plugin>, call_timeout: Duration) -> LinkedRuntime {
        LinkedRuntime {
            plugin,
            call_timeout,
        }
    }
}

/// The plugin's view of the host: blocking calls into the async services,
/// on the runtime that called `sync` (its connection pool lives there).
struct LinkedHost {
    call: CallHandle,
    handle: tokio::runtime::Handle,
}

impl HostApi for LinkedHost {
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.handle.block_on(self.call.http(req))
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.call.log(level, message);
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the plugin panicked".to_string())
}

#[async_trait]
impl PluginRuntime for LinkedRuntime {
    fn mode(&self) -> HostMode {
        HostMode::InProcess
    }

    async fn sync(&self, call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError> {
        let plugin = Arc::clone(&self.plugin);
        let host = LinkedHost {
            call,
            handle: tokio::runtime::Handle::current(),
        };
        let task = tokio::task::spawn_blocking(move || {
            catch_unwind(AssertUnwindSafe(|| plugin.sync(&host, req)))
        });
        // On a timeout the blocking thread is abandoned: it cannot be stopped.
        let outcome = tokio::time::timeout(self.call_timeout, task)
            .await
            .map_err(|_| CallError::Timeout)?;
        let page = match outcome {
            Ok(Ok(result)) => result.map_err(CallError::Plugin)?,
            Ok(Err(payload)) => {
                return Err(CallError::Crashed {
                    message: panic_message(payload.as_ref()),
                })
            }
            Err(join) => {
                return Err(CallError::Crashed {
                    message: join.to_string(),
                })
            }
        };
        page.validate()?;
        Ok(page)
    }
}
