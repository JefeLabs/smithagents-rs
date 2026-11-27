//! Why the app does not start its logging with `init()`, and the guard of
//! the test that says so.
//!
//! `logging_does_not_forward_the_log_crate` (in src/main.rs) can fail
//! only in a build where tracing-subscriber's `tracing-log` feature is on,
//! which this crate's dev-dependency arranges. If that line is ever dropped,
//! that test passes whatever the code does, and this one fails: it shows that
//! in this build `init()` does forward the `log` crate's records, at whatever
//! level the subscriber has. Like every file in `tests/`, it runs in a process
//! of its own, so it can set the one global subscriber.

use tracing_subscriber::util::SubscriberInitExt;

#[test]
fn init_would_forward_the_log_crate() {
    assert_eq!(log::max_level(), log::LevelFilter::Off);

    tracing_subscriber::fmt()
        .with_writer(std::io::sink)
        .with_max_level(tracing::Level::TRACE)
        .finish()
        .init();

    assert_eq!(
        log::max_level(),
        log::LevelFilter::Trace,
        "tracing-subscriber's `tracing-log` feature is off in this test build, \
         so the logging test beside the binary can no longer fail"
    );
}
