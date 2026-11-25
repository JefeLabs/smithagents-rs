use raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};
use smithagents_center_host::*;
use smithagents_theme::state::ThemeState;
use smithagents_theme::tokens::Appearance;
use std::process::Command;

fn theme() -> ThemeState {
    ThemeState {
        theme: "smith-dark".into(),
        appearance: Appearance::Dark,
        text_scale: 1.25,
        font: "system".into(),
    }
}

fn spec(agent: &str) -> TabSpec {
    TabSpec {
        id: TabId(1),
        kind: TabKind::Chat {
            agent_id: agent.into(),
        },
        title: "Chat".into(),
    }
}

const ORIGIN: &str = "smithagents://app";

/// The rule from web/src/host.ts: /^http:\/\/(127\.0\.0\.1|localhost):\d{1,5}$/ and port 1..65535.
fn page_validator_accepts(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let Some((host, port)) = rest.rsplit_once(':') else {
        return false;
    };
    (host == "127.0.0.1" || host == "localhost")
        && !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && matches!(port.parse::<u32>(), Ok(1..=65535))
}

fn base_url_of(boot: &PageBoot) -> String {
    let marker = "window.__SMITH_HOST__=";
    let i = boot.host_script.find(marker).unwrap() + marker.len();
    let json = boot.host_script[i..].trim_end_matches(';');
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v["baseUrl"].as_str().unwrap().to_string()
}

#[test]
fn boot_contains_theme_json_and_keeps_token_out_of_url() {
    let t = theme();
    let b = page_boot(&spec("a1"), 4000, "sekrit-token", &t, ORIGIN).unwrap();
    assert_eq!(b.page_url, "smithagents://app/index.html");
    assert!(!b.page_url.contains("sekrit-token"));
    assert!(b.host_script.contains("sekrit-token"));
    let theme_json = serde_json::to_string(&t).unwrap();
    assert!(b.host_script.starts_with(&format!(
        "window.__SMITH_THEME__={theme_json};window.__SMITH_HOST__="
    )));
    let dev = page_boot(&spec("a1"), 4000, "t", &t, "http://localhost:5173/").unwrap();
    assert_eq!(dev.page_url, "http://localhost:5173/index.html");
}

#[test]
fn base_url_satisfies_the_page_validator() {
    for port in [1u16, 80, 4000, 5173, 65535] {
        let b = page_boot(&spec("a"), port, "t", &theme(), ORIGIN).unwrap();
        let base = base_url_of(&b);
        assert_eq!(base, format!("http://127.0.0.1:{port}"));
        assert!(page_validator_accepts(&base), "{base}");
    }
    assert!(!page_validator_accepts("http://127.0.0.1:0"));
    assert!(!page_validator_accepts("http://127.0.0.1:4000/"));
}

#[test]
fn port_zero_is_rejected() {
    let r = page_boot(&spec("a"), 0, "t", &theme(), ORIGIN);
    assert!(matches!(r, Err(HostError::InvalidPort)));
}

#[test]
fn debug_and_redacted_never_show_token() {
    let b = page_boot(&spec("a"), 4000, "sekrit-token", &theme(), ORIGIN).unwrap();
    assert!(!format!("{b:?}").contains("sekrit-token"));
    let r = b.redacted_host_script();
    assert!(!r.contains("sekrit-token"));
    // `<` is script-escaped, so the marker reads `\u003credacted>` in source.
    assert!(r.contains("\\u003credacted>"));
    assert_eq!(r.replace("\\u003credacted>", "sekrit-token"), b.host_script);
}

fn eval_in_node(script: &str) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boot.js");
    std::fs::write(&path, script).unwrap();
    let runner = r#"const fs=require("fs");const window={};
new Function("window",fs.readFileSync(process.argv[1],"utf8"))(window);
process.stdout.write(JSON.stringify({host:window.__SMITH_HOST__,theme:window.__SMITH_THEME__}));"#;
    let out = Command::new("node")
        .arg("-e")
        .arg(runner)
        .arg(&path)
        .output()
        .expect("node must be installed to run this test");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn hostile_values_round_trip_through_javascript() {
    let hostile = [
        "a\"b\\c</script><!--x\u{2028}y\u{2029}z'`${1}",
        "</script>",
        "\u{2028}",
        "plain",
    ];
    for agent in hostile {
        let token = format!("tok{agent}");
        let mut t = theme();
        t.font = agent.into();
        let b = page_boot(&spec(agent), 4000, &token, &t, ORIGIN).unwrap();
        assert!(!b.host_script.to_lowercase().contains("</script"));
        assert!(!b.host_script.contains('\u{2028}'));
        assert!(!b.host_script.contains('\u{2029}'));
        let v = eval_in_node(&b.host_script);
        assert_eq!(v["host"]["agentId"], agent);
        assert_eq!(v["host"]["token"], token.as_str());
        assert_eq!(v["host"]["baseUrl"], "http://127.0.0.1:4000");
        assert_eq!(v["theme"]["font"], agent);
        assert_eq!(v["theme"]["text_scale"], 1.25);
    }
}

#[test]
fn navigation_policy() {
    let page = "smithagents://app/index.html";
    assert!(navigation_allowed(page, "smithagents://app/index.html"));
    assert!(navigation_allowed(
        page,
        "smithagents://app/index.html?x=1#frag"
    ));
    for bad in [
        "smithagents://app/other.html",
        "smithagents://app/",
        "smithagents://evil/index.html",
        "smithagents://app:99/index.html",
        "https://example.com",
        "https://app/index.html",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,hi",
        "blob:smithagents://app/uuid",
        "about:blank",
        "//app/index.html",
        "/index.html",
        "index.html",
        "",
        "smithagents://app\\@evil/index.html",
        "smithagents://user@app/index.html",
    ] {
        assert!(!navigation_allowed(page, bad), "{bad}");
    }
    let dev = "http://localhost:5173/index.html";
    assert!(navigation_allowed(
        dev,
        "http://localhost:5173/index.html?a=b"
    ));
    assert!(!navigation_allowed(dev, "http://localhost:5174/index.html"));
    assert!(!navigation_allowed(dev, "http://127.0.0.1:5173/index.html"));
    assert!(!navigation_allowed(
        page,
        "http://localhost:5173/index.html"
    ));
    assert!(!navigation_allowed("not a url", "not a url"));
}

#[test]
fn external_targets() {
    assert_eq!(
        external_target("https://example.com/a?b=1").as_deref(),
        Some("https://example.com/a?b=1")
    );
    assert_eq!(
        external_target("http://example.com").as_deref(),
        Some("http://example.com/")
    );
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,x",
        "smithagents://app/index.html",
        "https://user:pw@example.com/",
        "https://user@example.com/",
        "mailto:a@b.c",
        "relative/path",
        "",
    ] {
        assert_eq!(external_target(bad), None, "{bad}");
    }
}

struct Win;
impl HasWindowHandle for Win {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

#[test]
fn trait_is_object_safe() {
    let mut h: Box<dyn CenterTabHost> = Box::new(RecordingHost::default());
    h.close(TabId(9));
}

#[test]
fn recording_host_records_sequence_with_redaction() {
    let mut h = RecordingHost::default();
    h.attach(&Win).unwrap();
    let s = spec("a1");
    let b = page_boot(&s, 4000, "sekrit-token", &theme(), ORIGIN).unwrap();
    h.open(s.clone(), b.clone()).unwrap();
    h.show(TabId(1));
    h.set_bounds(PixelRect {
        x: 1.0,
        y: 2.0,
        w: 3.0,
        h: 4.0,
        scale: 2.0,
    });
    h.push_theme(&theme());
    h.open_external("https://example.com/");
    h.close(TabId(1));
    assert_eq!(
        h.calls(),
        &[
            HostCall::Attach,
            HostCall::Open { spec: s, boot: b },
            HostCall::Show(TabId(1)),
            HostCall::SetBounds(PixelRect {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
                scale: 2.0
            }),
            HostCall::PushTheme(theme()),
            HostCall::OpenExternal("https://example.com/".into()),
            HostCall::Close(TabId(1)),
        ]
    );
    assert!(!format!("{:?}", h.calls()).contains("sekrit-token"));
    assert_eq!(h.opened_ids(), vec![TabId(1)]);
}
