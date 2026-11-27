use chrono::TimeZone;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use smithagents_plugin_protocol::*;

fn item(id: &str) -> Item {
    Item {
        id: id.into(),
        title: "t".into(),
        url: None,
        updated_at: None,
        version: None,
        fields: json!({}),
    }
}

fn page(upserts: Vec<Item>) -> SyncPage {
    SyncPage {
        upserts,
        deletes: vec![],
        next_cursor: None,
        done: true,
        snapshot_complete: true,
    }
}

#[test]
fn valid_page_passes() {
    page(vec![item("1"), item("2")]).validate().unwrap();
}

#[test]
fn too_many_upserts() {
    let p = page((0..501).map(|i| item(&i.to_string())).collect());
    assert_eq!(
        p.validate().unwrap_err(),
        ContractError::TooManyUpserts(501)
    );
    page((0..500).map(|i| item(&i.to_string())).collect())
        .validate()
        .unwrap();
}

#[test]
fn too_many_deletes() {
    let mut p = page(vec![]);
    p.deletes = (0..501).map(|i| i.to_string()).collect();
    assert_eq!(
        p.validate().unwrap_err(),
        ContractError::TooManyDeletes(501)
    );
}

#[test]
fn fields_too_large() {
    let mut it = item("1");
    // `{"k":"<n>"}` is n + 8 bytes; make the whole object exactly 64 KiB + 1.
    it.fields = json!({"k": "x".repeat(65_536 + 1 - 8)});
    assert_eq!(serde_json::to_vec(&it.fields).unwrap().len(), 65_537);
    assert_eq!(
        page(vec![it.clone()]).validate().unwrap_err(),
        ContractError::FieldsTooLarge("1".into())
    );
    it.fields = json!({"k": "x".repeat(65_536 - 8)});
    page(vec![it]).validate().unwrap();
}

#[test]
fn fields_must_be_object() {
    let mut it = item("1");
    it.fields = json!([1]);
    assert_eq!(
        page(vec![it]).validate().unwrap_err(),
        ContractError::FieldsNotObject("1".into())
    );
}

#[test]
fn title_too_long() {
    let mut it = item("1");
    it.title = "é".repeat(513);
    assert_eq!(
        page(vec![it.clone()]).validate().unwrap_err(),
        ContractError::TitleTooLong("1".into())
    );
    it.title = "é".repeat(512);
    page(vec![it]).validate().unwrap();
}

#[test]
fn duplicate_and_empty_ids() {
    assert_eq!(
        page(vec![item("1"), item("1")]).validate().unwrap_err(),
        ContractError::DuplicateId("1".into())
    );
    assert_eq!(
        page(vec![item("")]).validate().unwrap_err(),
        ContractError::EmptyId
    );
    assert_eq!(
        page(vec![item(&"i".repeat(4097))]).validate().unwrap_err(),
        ContractError::IdTooLong
    );
}

#[test]
fn cursor_too_long() {
    let mut p = page(vec![]);
    p.next_cursor = Some("c".repeat(4097));
    assert_eq!(p.validate().unwrap_err(), ContractError::CursorTooLong);
    p.next_cursor = Some("c".repeat(4096));
    p.validate().unwrap();
}

fn with_url(url: &str) -> SyncPage {
    let mut it = item("1");
    it.url = Some(url.to_string());
    page(vec![it])
}

#[test]
fn item_url_must_be_an_absolute_http_url() {
    for ok in [
        "https://github.com/octo/repo/issues/1",
        "http://example.com",
        "https://example.com:8443/a?b=c#d",
    ] {
        with_url(ok).validate().unwrap();
    }
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "/relative/path",
        "relative/path",
        "",
        "github.com/octo/repo",
        "https://",
        "mailto:a@example.com",
        "data:text/html,hi",
        "https://exa mple.com/",
        "https://example.com/a b",
        " https://example.com/",
        "https://example.com/\n",
    ] {
        assert_eq!(
            with_url(bad).validate().unwrap_err(),
            ContractError::BadUrl("1".into()),
            "{bad:?}"
        );
    }
}

#[test]
fn item_url_and_version_at_most_4_kib() {
    let base = "https://example.com/";
    let longest = format!("{base}{}", "p".repeat(4096 - base.len()));
    assert_eq!(longest.len(), 4096);
    with_url(&longest).validate().unwrap();
    assert_eq!(
        with_url(&format!("{longest}p")).validate().unwrap_err(),
        ContractError::UrlTooLong("1".into())
    );

    let mut it = item("1");
    it.version = Some("v".repeat(4096));
    page(vec![it.clone()]).validate().unwrap();
    it.version = Some("v".repeat(4097));
    assert_eq!(
        page(vec![it]).validate().unwrap_err(),
        ContractError::VersionTooLong("1".into())
    );
}

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    v: T,
) -> serde_json::Value {
    let json = serde_json::to_value(&v).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back, v);
    json
}

fn request() -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Post,
        url: "https://api.github.com/x".into(),
        headers: vec![("accept".into(), "json".into())],
        body: Some(vec![1, 2, 3]),
        auth: Some(Auth::Bearer {
            secret: "token".into(),
        }),
    }
}

#[test]
fn envelopes_round_trip() {
    let sync = SyncRequest {
        resource: "issue".into(),
        cursor: None,
        settings: json!({}),
    };
    let mut full = item("1");
    full.updated_at = Some(chrono::Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap());
    full.fields = json!({"a": 1});
    round_trip(PluginCall::Sync {
        call: 7,
        request: sync,
    });
    round_trip(PluginReply::Sync(Ok(page(vec![full]))));
    round_trip(PluginReply::Sync(Err(PluginError::RateLimited {
        retry_after_secs: 5,
        message: "slow".into(),
    })));
    round_trip(PluginReply::Panicked {
        message: "boom".into(),
    });
    round_trip(PluginReply::BadCall {
        message: "bad".into(),
    });
    round_trip(HostCall::Log {
        call: 1,
        level: LogLevel::Warn,
        message: "m".into(),
    });
    round_trip(HostReply::Done);
    round_trip(HostReply::BadCall {
        message: "bad".into(),
    });
    round_trip(HostReply::Http(Err(HttpError::Timeout)));

    let call = round_trip(HostCall::Http {
        call: 2,
        request: request(),
    });
    assert_eq!(call["type"], "http");
    assert_eq!(call["request"]["body"], "AQID");

    let resp = HttpResponse {
        status: 200,
        headers: vec![],
        body: vec![1, 2, 3],
    };
    let reply = round_trip(HostReply::Http(Ok(resp)));
    assert_eq!(reply["type"], "http");
    assert_eq!(reply["Ok"]["body"], "AQID");
}

#[test]
fn header_lookup_is_case_insensitive() {
    let resp = HttpResponse {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "a".into()),
            ("link".into(), "next".into()),
            ("Link".into(), "later".into()),
        ],
        body: vec![],
    };
    assert_eq!(resp.header("LINK"), Some("next"));
    assert_eq!(resp.header("missing"), None);
}

#[test]
fn http_error_display() {
    assert_eq!(
        HttpError::Denied {
            reason: "host not allowed".into()
        }
        .to_string(),
        "denied: host not allowed"
    );
    assert_eq!(
        HttpError::SecretMissing {
            key: "token".into()
        }
        .to_string(),
        "secret missing: token"
    );
}

#[test]
fn the_crates_in_the_contract_are_named_through_it() {
    // A plugin that names these types through the protocol crate always has
    // the versions the contract was built with, whatever it depends on itself.
    use smithagents_plugin_protocol::{chrono, semver, serde_json};

    let item = Item {
        id: "a".into(),
        title: "A".into(),
        url: None,
        updated_at: Some(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        version: None,
        fields: serde_json::json!({}),
    };
    let page = SyncPage {
        upserts: vec![item],
        deletes: vec![],
        next_cursor: None,
        done: true,
        snapshot_complete: false,
    };
    assert_eq!(page.validate(), Ok(()));
    let manifest = smithagents_plugin_protocol::Manifest::parse(
        "id = \"t\"\nname = \"T\"\nversion = \"1.2.3\"\ncontract = 1\nhost = \"in-process\"\n",
    )
    .unwrap();
    assert_eq!(manifest.version, semver::Version::new(1, 2, 3));
}
