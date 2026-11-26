mod common;
use common::*;
use smithagents_plugin_protocol::{HttpError, PluginError};

const REPO: &str = "https://api.github.com/repos/octo-org/widgets";

fn call(
    host: &dyn smithagents_plugin_protocol::HostApi,
) -> Result<smithagents_plugin_protocol::SyncPage, PluginError> {
    sync(host, "repository", None, settings(&["octo-org/widgets"]))
}

fn status(
    code: u16,
    headers: &[(&str, &str)],
) -> Result<smithagents_plugin_protocol::SyncPage, PluginError> {
    call(&Scripted::new().on(REPO, code, headers, b"{}".to_vec()))
}

fn wait(r: Result<smithagents_plugin_protocol::SyncPage, PluginError>) -> u32 {
    match r.unwrap_err() {
        PluginError::RateLimited {
            retry_after_secs, ..
        } => retry_after_secs,
        other => panic!("not rate limited: {other:?}"),
    }
}

#[test]
fn unauthorised_is_an_auth_error() {
    assert_eq!(
        status(401, &[]).unwrap_err(),
        PluginError::Auth {
            message: "GitHub rejected the token".into()
        }
    );
}

#[test]
fn retry_after_wins_and_is_clamped() {
    assert_eq!(wait(status(429, &[("Retry-After", "120")])), 120);
    assert_eq!(wait(status(403, &[("retry-after", "7")])), 60, "clamped up");
    assert_eq!(wait(status(403, &[("retry-after", "0")])), 60);
    assert_eq!(wait(status(403, &[("retry-after", "soon")])), 60);
    assert_eq!(wait(status(403, &[("retry-after", "-5")])), 60);
    assert_eq!(
        wait(status(429, &[("retry-after", "86400")])),
        3600,
        "clamped down"
    );
    assert_eq!(
        wait(status(429, &[("retry-after", "18446744073709551615")])),
        3600
    );
    assert_eq!(
        wait(status(429, &[("retry-after", "99999999999999999999")])),
        60
    );
}

#[test]
fn an_exhausted_quota_waits_until_the_reset_clamped() {
    let date = "Thu, 26 Nov 2026 10:00:00 GMT";
    let base = 1_795_687_200_i64; // 2026-11-26T10:00:00Z
    let reset = |n: i64| n.to_string();
    let w = |secs: i64| {
        wait(status(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &reset(base + secs)),
                ("date", date),
            ],
        ))
    };
    assert_eq!(w(600), 600);
    assert_eq!(w(5), 60, "clamped up");
    assert_eq!(w(-500), 60, "a reset in the past");
    assert_eq!(w(90_000), 3600, "clamped down");
}

#[test]
fn unusable_rate_limit_headers_fall_back_to_a_minute_without_panicking() {
    let h = |reset: &str, date: &str| {
        wait(status(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", reset),
                ("date", date),
            ],
        ))
    };
    assert_eq!(h("nope", "Thu, 26 Nov 2026 10:00:00 GMT"), 60);
    assert_eq!(h("1795687300", "not a date"), 60);
    assert_eq!(
        h("9223372036854775807", "Thu, 26 Nov 2026 10:00:00 GMT"),
        3600
    );
    assert_eq!(
        h("-9223372036854775808", "Thu, 26 Nov 2026 10:00:00 GMT"),
        60
    );
    assert_eq!(wait(status(429, &[("x-ratelimit-remaining", "0")])), 60);
}

#[test]
fn a_forbidden_answer_that_is_not_rate_limiting_is_an_http_error() {
    assert!(matches!(
        status(403, &[("x-ratelimit-remaining", "12")]).unwrap_err(),
        PluginError::Http { .. }
    ));
    assert!(matches!(
        status(403, &[]).unwrap_err(),
        PluginError::Http { .. }
    ));
}

#[test]
fn not_found_and_moved_are_settings_errors_naming_the_repository() {
    match status(404, &[]).unwrap_err() {
        PluginError::Settings { message } => {
            assert!(message.contains("octo-org/widgets"));
            assert!(message.contains("not found or not visible to this token"));
        }
        other => panic!("{other:?}"),
    }
    match status(
        301,
        &[("location", "https://api.github.com/repositories/9")],
    )
    .unwrap_err()
    {
        PluginError::Settings { message } => {
            assert!(message.contains("octo-org/widgets") && message.contains("repository moved"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn other_statuses_are_http_errors_carrying_the_status() {
    for code in [500, 502, 503, 400, 302] {
        match status(code, &[]).unwrap_err() {
            PluginError::Http { message } => {
                assert!(message.contains(&code.to_string()), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn invalid_json_and_unexpected_shapes_are_other_errors() {
    let host = Scripted::new().ok(REPO, b"<html>".to_vec());
    assert!(matches!(
        call(&host).unwrap_err(),
        PluginError::Other { .. }
    ));
    let host = Scripted::new().ok(REPO, b"[]".to_vec());
    assert!(matches!(
        call(&host).unwrap_err(),
        PluginError::Other { .. }
    ));
    let issues = "https://api.github.com/repos/octo-org/widgets/issues?state=all&sort=updated&direction=desc&per_page=100";
    let host = Scripted::new().ok(issues, b"{}".to_vec());
    let r = sync(&host, "issue", None, settings(&["octo-org/widgets"]));
    assert!(matches!(r.unwrap_err(), PluginError::Other { .. }));
}

#[test]
fn transport_failures_map_without_leaking_anything() {
    assert!(matches!(
        call(&Failing(HttpError::Timeout)).unwrap_err(),
        PluginError::Http { .. }
    ));
    assert!(matches!(
        call(&Failing(HttpError::TooLarge)).unwrap_err(),
        PluginError::Http { .. }
    ));
    assert!(matches!(
        call(&Failing(HttpError::Transport {
            message: "dns".into()
        }))
        .unwrap_err(),
        PluginError::Http { .. }
    ));
    assert!(matches!(
        call(&Failing(HttpError::SecretMissing {
            key: "token".into()
        }))
        .unwrap_err(),
        PluginError::Auth { .. }
    ));
    assert!(matches!(
        call(&Failing(HttpError::Denied {
            reason: "policy".into()
        }))
        .unwrap_err(),
        PluginError::Other { .. }
    ));
}

#[test]
fn malformed_settings_are_settings_errors_before_any_request() {
    let host = Scripted::new();
    let bad = [
        serde_json::json!({}),
        serde_json::json!({"repositories": "octo-org/widgets"}),
        serde_json::json!({"repositories": [1]}),
        settings(&["widgets"]),
        settings(&["octo-org/"]),
        settings(&["/widgets"]),
        settings(&["octo-org/widgets/extra"]),
        settings(&["octo org/widgets"]),
        settings(&["octo-org/wid gets"]),
        settings(&["octo-org/.."]),
        settings(&["../widgets"]),
        settings(&["octo-org/widgets?x=1"]),
        settings(&["octo-org/widgets\n"]),
        settings(&[""]),
    ];
    for s in bad {
        for resource in ["repository", "issue", "pull_request"] {
            let err = sync(&host, resource, None, s.clone()).unwrap_err();
            assert!(matches!(err, PluginError::Settings { .. }), "{s} {err:?}");
        }
    }
    assert!(host.requests().is_empty());
}

#[test]
fn more_than_fifty_repositories_is_a_settings_error() {
    let names: Vec<String> = (0..51).map(|i| format!("octo-org/r{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    match sync(&Scripted::new(), "issue", None, settings(&refs)).unwrap_err() {
        PluginError::Settings { message } => assert!(message.contains("at most 50"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unknown_resource_is_an_error() {
    let err = sync(
        &Scripted::new(),
        "gist",
        None,
        settings(&["octo-org/widgets"]),
    )
    .unwrap_err();
    assert!(matches!(err, PluginError::Other { .. }));
}

#[test]
fn a_list_whose_cursor_could_not_fit_is_refused_before_any_request() {
    let long = "a".repeat(100);
    let names: Vec<String> = (0..50)
        .map(|i| format!("octo-org/{i:02}{}", &long[..96]))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let host = Scripted::new();
    for resource in ["repository", "issue", "pull_request"] {
        match sync(&host, resource, None, settings(&refs)).unwrap_err() {
            PluginError::Settings { message } => {
                assert!(message.contains("remove some"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(host.requests().is_empty());
}
