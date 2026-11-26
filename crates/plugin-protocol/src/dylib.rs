//! The native side of the contract: what crosses the boundary between the
//! daemon and a built-in plugin loaded as a dynamic library.
//!
//! Both sides build against the two stabby interfaces below, and the loader
//! checks stabby's type report for the entry symbol, so a library built
//! against another interface is refused at load. Calls are JSON bytes: the
//! host sends a [`PluginCall`] and the plugin answers a [`PluginReply`];
//! while a call runs, the plugin sends [`HostCall`]s and the host answers
//! [`HostReply`]s. A panic must never unwind across the boundary (that aborts
//! the daemon), so each side catches its own panics and answers with an
//! error instead.

use std::any::Any;
use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use stabby::vec::Vec as StableVec;

use crate::envelope::{HostCall, HostReply, PluginCall, PluginReply};
use crate::types::{HostApi, HttpError, HttpRequest, HttpResponse, LogLevel, Plugin};

/// The host as a plugin sees it: [`HostCall`] in, [`HostReply`] out, as JSON.
#[stabby::stabby]
pub trait DylibHost {
    extern "C" fn call(&self, input: StableVec<u8>) -> StableVec<u8>;
}

/// A plugin as the host sees it: [`PluginCall`] in, [`PluginReply`] out, as JSON.
#[stabby::stabby]
pub trait DylibPlugin {
    extern "C" fn call(&self, input: StableVec<u8>) -> StableVec<u8>;
}

pub type BoxedDylibHost = stabby::dynptr!(stabby::boxed::Box<dyn DylibHost + Send + Sync>);
pub type BoxedDylibPlugin = stabby::dynptr!(stabby::boxed::Box<dyn DylibPlugin + Send + Sync>);

/// The library's entry: it receives the host and returns the plugin.
pub type DylibEntry = extern "C" fn(BoxedDylibHost) -> BoxedDylibPlugin;

/// The entry's symbol, as [`export_dylib_plugin!`](crate::export_dylib_plugin)
/// names it. A new version gets a new name.
pub const DYLIB_ENTRY_SYMBOL: &[u8] = b"smithagents_plugin_create_v1";

/// Answers the host's calls on the plugin side of the boundary.
///
/// `export_dylib_plugin!(plugin)` serves a [`Plugin`] through
/// [`PluginHandler`]. A crate that needs the envelope itself, such as the call
/// numbers the `Plugin` trait hides, implements this and exports it with
/// `export_dylib_plugin!(handler: value)`.
pub trait DylibHandler: Send + Sync {
    /// Answers one call. A panic here is caught and answered as
    /// [`PluginReply::Panicked`].
    fn handle(&self, host: &HostConnection, call: PluginCall) -> PluginReply;
}

/// Serves a [`Plugin`]: each sync call runs [`Plugin::sync`] with a
/// [`HostApi`] that sends its requests and logs under the call's number.
pub struct PluginHandler<P>(P);

impl<P: Plugin> PluginHandler<P> {
    pub fn new(plugin: P) -> PluginHandler<P> {
        PluginHandler(plugin)
    }
}

impl<P: Plugin> DylibHandler for PluginHandler<P> {
    fn handle(&self, host: &HostConnection, call: PluginCall) -> PluginReply {
        let PluginCall::Sync { call, request } = call;
        PluginReply::Sync(self.0.sync(&CallHost { host, call }, request))
    }
}

/// The plugin's line to the host.
pub struct HostConnection {
    host: BoxedDylibHost,
}

impl HostConnection {
    /// Sends `call` to the host and returns its answer. A reply the plugin
    /// cannot read comes back as [`HostReply::BadCall`].
    pub fn call(&self, call: &HostCall) -> HostReply {
        let input = match serde_json::to_vec(call) {
            Ok(input) => input,
            Err(e) => {
                return HostReply::BadCall {
                    message: format!("the plugin cannot encode its call: {e}"),
                }
            }
        };
        let output = self.host.call(StableVec::from(input.as_slice()));
        serde_json::from_slice(output.as_slice()).unwrap_or_else(|e| HostReply::BadCall {
            message: format!("the plugin cannot read the host's reply: {e}"),
        })
    }
}

/// The host during one call, as [`Plugin::sync`] sees it.
struct CallHost<'a> {
    host: &'a HostConnection,
    call: u64,
}

impl HostApi for CallHost<'_> {
    fn http(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let call = HostCall::Http {
            call: self.call,
            request,
        };
        match self.host.call(&call) {
            HostReply::Http(result) => result,
            HostReply::BadCall { message } => Err(HttpError::Denied { reason: message }),
            HostReply::Done => Err(HttpError::Transport {
                message: "the host answered an HTTP request without a response".into(),
            }),
        }
    }

    fn log(&self, level: LogLevel, message: &str) {
        let call = HostCall::Log {
            call: self.call,
            level,
            message: message.to_string(),
        };
        // Logging is best effort: there is nothing to do if the host refuses.
        let _ = self.host.call(&call);
    }
}

/// What the entry hands the host: the handler, or why it could not be built.
struct Exported<H> {
    handler: Result<H, String>,
    host: HostConnection,
}

impl<H: DylibHandler> Exported<H> {
    fn reply(&self, input: &[u8]) -> PluginReply {
        let handler = match &self.handler {
            Ok(handler) => handler,
            Err(message) => {
                return PluginReply::Panicked {
                    message: message.clone(),
                }
            }
        };
        match serde_json::from_slice(input) {
            Ok(call) => handler.handle(&self.host, call),
            Err(e) => PluginReply::BadCall {
                message: format!("the plugin cannot read the call: {e}"),
            },
        }
    }
}

impl<H: DylibHandler> DylibPlugin for Exported<H> {
    extern "C" fn call(&self, input: StableVec<u8>) -> StableVec<u8> {
        let reply = catch(|| encode(&self.reply(input.as_slice())))
            .unwrap_or_else(|message| encode(&PluginReply::Panicked { message }));
        StableVec::from(reply.as_slice())
    }
}

impl<H> Drop for Exported<H> {
    fn drop(&mut self) {
        // The host drops the plugin through the boundary: a panic while
        // dropping the handler must not unwind into it.
        let handler = std::mem::replace(&mut self.handler, Err(String::new()));
        let _ = catch(move || drop(handler));
    }
}

fn encode(reply: &PluginReply) -> Vec<u8> {
    // Serialising the contract's types does not fail. If it ever did, the
    // empty answer reaches the host as a reply it cannot read.
    serde_json::to_vec(reply).unwrap_or_default()
}

thread_local! {
    /// Set while the plugin answers a call: its panics become replies.
    static ANSWERING: Cell<bool> = const { Cell::new(false) };
    /// Where the last panic caught while answering happened.
    static PANICKED_AT: Cell<Option<String>> = const { Cell::new(None) };
}

/// A panic caught while answering is reported in the reply, so it is not also
/// printed: the hook records where it happened instead. Other panics in the
/// library get the previous hook. A plugin library carries its own copy of
/// `std`, so the hook is the library's alone.
fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if ANSWERING.get() {
                PANICKED_AT.set(info.location().map(ToString::to_string));
            } else {
                previous(info);
            }
        }));
    });
}

/// Runs `f`, turning a panic into its message and location.
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let outer = ANSWERING.replace(true);
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    ANSWERING.set(outer);
    result.map_err(|payload| {
        let message = panic_message(payload.as_ref());
        match PANICKED_AT.take() {
            Some(at) => format!("{message} (at {at})"),
            None => message,
        }
    })
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the plugin panicked".to_string())
}

#[doc(hidden)]
pub mod __private {
    pub use stabby::export;

    use super::*;

    /// The body of the entry [`export_dylib_plugin!`](crate::export_dylib_plugin)
    /// defines. If `make` panics, every call is answered as
    /// [`PluginReply::Panicked`].
    pub fn entry<H: DylibHandler + 'static>(
        host: BoxedDylibHost,
        make: impl FnOnce() -> H,
    ) -> BoxedDylibPlugin {
        install_panic_hook();
        let handler =
            catch(make).map_err(|message| format!("the plugin panicked while starting: {message}"));
        let host = HostConnection { host };
        stabby::boxed::Box::new(Exported { handler, host }).into()
    }
}

/// Exports a built-in plugin from a `cdylib` crate: defines the entry
/// [`DYLIB_ENTRY_SYMBOL`](crate::dylib::DYLIB_ENTRY_SYMBOL) that the daemon's
/// loader looks for. Use it once per crate.
///
/// - `export_dylib_plugin!(plugin)`: `plugin` is an expression whose value
///   implements [`Plugin`](crate::Plugin), served through
///   [`PluginHandler`](crate::dylib::PluginHandler).
/// - `export_dylib_plugin!(handler: value)`: `value` implements
///   [`DylibHandler`](crate::dylib::DylibHandler) and sees the envelope itself.
///
/// The expression is evaluated once, when the library is loaded. Every call
/// runs under `catch_unwind`: a panic is answered as `PluginReply::Panicked`,
/// and input the plugin cannot read as `PluginReply::BadCall`.
///
/// The crate depends on this crate with the `dylib` feature and on `stabby`
/// itself: the export attribute looks `stabby` up by name in the calling
/// crate's dependencies.
#[macro_export]
macro_rules! export_dylib_plugin {
    (handler: $handler:expr) => {
        #[$crate::dylib::__private::export]
        pub extern "C" fn smithagents_plugin_create_v1(
            host: $crate::dylib::BoxedDylibHost,
        ) -> $crate::dylib::BoxedDylibPlugin {
            $crate::dylib::__private::entry(host, || $handler)
        }
    };
    ($plugin:expr) => {
        $crate::export_dylib_plugin!(handler: $crate::dylib::PluginHandler::new($plugin));
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::types::{PluginError, SyncPage, SyncRequest};

    /// A host that records each call and refuses it.
    struct RefusingHost(&'static Mutex<Vec<HostCall>>);

    impl DylibHost for RefusingHost {
        extern "C" fn call(&self, input: StableVec<u8>) -> StableVec<u8> {
            let call: HostCall = serde_json::from_slice(input.as_slice()).unwrap();
            self.0.lock().unwrap().push(call);
            let reply = HostReply::BadCall {
                message: "no call is running".into(),
            };
            StableVec::from(serde_json::to_vec(&reply).unwrap().as_slice())
        }
    }

    fn refusing_host(seen: &'static Mutex<Vec<HostCall>>) -> BoxedDylibHost {
        stabby::boxed::Box::new(RefusingHost(seen)).into()
    }

    struct Probe;

    impl Plugin for Probe {
        fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
            match req.resource.as_str() {
                "panic" => panic!("probe panic"),
                "http" => {
                    host.log(LogLevel::Info, "fetching");
                    let err = host
                        .http(HttpRequest {
                            method: crate::types::HttpMethod::Get,
                            url: "https://fixture.invalid/".into(),
                            headers: Vec::new(),
                            body: None,
                            auth: None,
                        })
                        .unwrap_err();
                    Err(PluginError::Http {
                        message: err.to_string(),
                    })
                }
                _ => Ok(SyncPage {
                    upserts: Vec::new(),
                    deletes: Vec::new(),
                    next_cursor: None,
                    done: true,
                    snapshot_complete: true,
                }),
            }
        }
    }

    fn sync(call: u64, resource: &str) -> Vec<u8> {
        let call = PluginCall::Sync {
            call,
            request: SyncRequest {
                resource: resource.into(),
                cursor: None,
                settings: json!({}),
            },
        };
        serde_json::to_vec(&call).unwrap()
    }

    fn ask(plugin: &BoxedDylibPlugin, input: &[u8]) -> PluginReply {
        let output = plugin.call(StableVec::from(input));
        serde_json::from_slice(output.as_slice()).unwrap()
    }

    fn probe() -> (BoxedDylibPlugin, &'static Mutex<Vec<HostCall>>) {
        let seen = Box::leak(Box::new(Mutex::new(Vec::new())));
        let plugin = __private::entry(refusing_host(seen), || PluginHandler::new(Probe));
        (plugin, seen)
    }

    #[test]
    fn a_sync_call_is_answered_with_the_plugins_result() {
        let (plugin, _) = probe();
        match ask(&plugin, &sync(1, "ok")) {
            PluginReply::Sync(Ok(page)) => assert!(page.done),
            other => panic!("expected a page, got {other:?}"),
        }
    }

    #[test]
    fn input_the_plugin_cannot_read_is_a_bad_call() {
        let (plugin, _) = probe();
        match ask(&plugin, b"not json") {
            PluginReply::BadCall { message } => {
                assert!(message.contains("cannot read the call"), "{message}")
            }
            other => panic!("expected BadCall, got {other:?}"),
        }
    }

    #[test]
    fn a_panic_is_answered_with_its_message_and_place_and_the_next_call_works() {
        let (plugin, _) = probe();
        match ask(&plugin, &sync(1, "panic")) {
            PluginReply::Panicked { message } => {
                assert!(message.starts_with("probe panic (at "), "{message}");
                assert!(message.contains("dylib.rs"), "{message}");
            }
            other => panic!("expected Panicked, got {other:?}"),
        }
        assert!(matches!(
            ask(&plugin, &sync(2, "ok")),
            PluginReply::Sync(Ok(_))
        ));
    }

    #[test]
    fn a_plugin_that_panics_while_starting_answers_every_call_with_the_panic() {
        let seen = Box::leak(Box::new(Mutex::new(Vec::new())));
        let plugin = __private::entry(refusing_host(seen), || -> PluginHandler<Probe> {
            panic!("no settings")
        });
        for call in 1..=2 {
            match ask(&plugin, &sync(call, "ok")) {
                PluginReply::Panicked { message } => assert!(
                    message.starts_with("the plugin panicked while starting: no settings"),
                    "{message}"
                ),
                other => panic!("expected Panicked, got {other:?}"),
            }
        }
    }

    #[test]
    fn host_calls_carry_the_call_number_and_a_refusal_is_a_denied_request() {
        let (plugin, seen) = probe();
        match ask(&plugin, &sync(7, "http")) {
            PluginReply::Sync(Err(PluginError::Http { message })) => {
                assert_eq!(message, "denied: no call is running")
            }
            other => panic!("expected a denied request, got {other:?}"),
        }
        let seen = seen.lock().unwrap();
        assert!(
            matches!(&seen[0], HostCall::Log { call: 7, message, .. } if message == "fetching")
        );
        assert!(matches!(&seen[1], HostCall::Http { call: 7, .. }));
    }
}
