use serde_json::json;
use smithagents_plugin_protocol::*;

const SAMPLE: &str = r#"
id = "github"
name = "GitHub"
version = "0.1.0"
contract = 1
host = "dylib"
entry = "libsmithagents_plugin_github.dylib"
hosts = ["api.github.com"]

[[secrets]]
key = "token"
label = "Personal access token"

[[settings]]
key = "repositories"
label = "Repositories (owner/name)"
kind = "list"
required = true

[[resources]]
kind = "issue"
label = "Issues"
interval_secs = 300
"#;

fn with(extra_top: &str, tail: &str) -> String {
    format!(
        "id = \"github\"\nname = \"GitHub\"\nversion = \"0.1.0\"\ncontract = 1\nhost = \"in-process\"\n{extra_top}\n{tail}"
    )
}

fn err(text: &str) -> ManifestError {
    Manifest::parse(text).unwrap_err()
}

#[test]
fn sample_parses() {
    let m = Manifest::parse(SAMPLE).unwrap();
    assert_eq!(m.id.to_string(), "github");
    assert_eq!(m.name, "GitHub");
    assert_eq!(m.version, semver::Version::new(0, 1, 0));
    assert_eq!(m.contract, 1);
    assert_eq!(m.host, HostMode::Dylib);
    assert_eq!(
        m.entry.as_deref(),
        Some("libsmithagents_plugin_github.dylib")
    );
    assert_eq!(m.hosts, vec!["api.github.com"]);
    assert_eq!(
        m.secrets,
        vec![SecretDecl {
            key: "token".into(),
            label: "Personal access token".into(),
            help: None
        }]
    );
    assert_eq!(m.settings.len(), 1);
    assert_eq!(m.settings[0].kind, SettingKind::List);
    assert!(m.settings[0].required);
    assert_eq!(
        m.resources,
        vec![ResourceDecl {
            kind: "issue".into(),
            label: "Issues".into(),
            interval_secs: 300
        }]
    );
}

#[test]
fn unknown_keys_are_rejected_at_every_level() {
    assert!(matches!(
        err(&with("bogus = 1", "")),
        ManifestError::Toml(_)
    ));
    let nested = with("", "[[secrets]]\nkey = \"t\"\nlabel = \"T\"\nbogus = 1\n");
    assert!(matches!(err(&nested), ManifestError::Toml(_)));
}

#[test]
fn bad_ids() {
    for id in ["GitHub", "a", &"a".repeat(33)] {
        let text = SAMPLE.replace("id = \"github\"", &format!("id = \"{id}\""));
        assert!(matches!(err(&text), ManifestError::Toml(_)), "{id}");
    }
    assert!("ab".parse::<PluginId>().is_ok());
    assert!("a".repeat(32).parse::<PluginId>().is_ok());
    assert!("a".parse::<PluginId>().is_err());
}

#[test]
fn contract_must_be_supported() {
    let zero = SAMPLE.replace("contract = 1", "contract = 0");
    assert_eq!(err(&zero), ManifestError::UnsupportedContract(0));
    let two = SAMPLE.replace("contract = 1", "contract = 2");
    assert_eq!(err(&two), ManifestError::UnsupportedContract(2));
}

#[test]
fn entry_rules() {
    let no_entry = SAMPLE.replace("entry = \"libsmithagents_plugin_github.dylib\"\n", "");
    assert_eq!(
        err(&no_entry),
        ManifestError::EntryRequired(HostMode::Dylib)
    );
    let wasm = no_entry.replace("host = \"dylib\"", "host = \"wasm\"");
    assert_eq!(err(&wasm), ManifestError::EntryRequired(HostMode::Wasm));
    let slash = SAMPLE.replace("libsmithagents_plugin_github.dylib", "sub/lib.dylib");
    assert!(matches!(err(&slash), ManifestError::BadEntry(_)));
    let dots = SAMPLE.replace("libsmithagents_plugin_github.dylib", "..");
    assert!(matches!(err(&dots), ManifestError::BadEntry(_)));
}

#[test]
fn bad_hosts() {
    for h in [
        "https://api.github.com",
        "api.github.com:443",
        "*.github.com",
        "127.0.0.1",
        "[::1]",
        "api.github.com.",
        "",
    ] {
        let text = SAMPLE.replace("api.github.com", h);
        assert_eq!(err(&text), ManifestError::BadHost(h.to_string()), "{h:?}");
    }
}

#[test]
fn hosts_are_lower_cased() {
    let text = SAMPLE.replace("api.github.com", "API.GitHub.com");
    assert_eq!(
        Manifest::parse(&text).unwrap().hosts,
        vec!["api.github.com"]
    );
}

#[test]
fn duplicates() {
    let secrets = format!("{SAMPLE}\n[[secrets]]\nkey = \"token\"\nlabel = \"Again\"\n");
    assert_eq!(
        err(&secrets),
        ManifestError::DuplicateSecret("token".into())
    );
    let settings = format!(
        "{SAMPLE}\n[[settings]]\nkey = \"repositories\"\nlabel = \"Again\"\nkind = \"bool\"\n"
    );
    assert_eq!(
        err(&settings),
        ManifestError::DuplicateSetting("repositories".into())
    );
    let resources = format!(
        "{SAMPLE}\n[[resources]]\nkind = \"issue\"\nlabel = \"Again\"\ninterval_secs = 60\n"
    );
    assert_eq!(
        err(&resources),
        ManifestError::DuplicateResource("issue".into())
    );
}

#[test]
fn interval_bounds() {
    for secs in [59, 86_401] {
        let text = SAMPLE.replace("interval_secs = 300", &format!("interval_secs = {secs}"));
        assert_eq!(
            err(&text),
            ManifestError::BadInterval {
                kind: "issue".into(),
                secs
            }
        );
    }
    for secs in [60, 86_400] {
        let text = SAMPLE.replace("interval_secs = 300", &format!("interval_secs = {secs}"));
        Manifest::parse(&text).unwrap();
    }
}

#[test]
fn default_of_wrong_kind_is_rejected() {
    let text = SAMPLE.replace(
        "required = true",
        "required = true\ndefault = \"owner/name\"",
    );
    assert_eq!(err(&text), ManifestError::BadDefault("repositories".into()));
    let ok = SAMPLE.replace("required = true", "required = true\ndefault = [\"a/b\"]");
    Manifest::parse(&ok).unwrap();
}

fn settings_manifest() -> Manifest {
    let text = with(
        "",
        r#"
[[settings]]
key = "repositories"
label = "Repos"
kind = "list"
required = true

[[settings]]
key = "title"
label = "Title"
kind = "text"
default = "hello"

[[settings]]
key = "archived"
label = "Archived"
kind = "bool"
"#,
    );
    Manifest::parse(&text).unwrap()
}

#[test]
fn settings_fill_defaults() {
    let m = settings_manifest();
    let out = m
        .validate_settings(&json!({"repositories": ["a/b"]}))
        .unwrap();
    assert_eq!(out, json!({"repositories": ["a/b"], "title": "hello"}));
    let out = m
        .validate_settings(&json!({"repositories": [], "title": "x", "archived": true}))
        .unwrap();
    assert_eq!(
        out,
        json!({"repositories": [], "title": "x", "archived": true})
    );
}

#[test]
fn settings_errors() {
    let m = settings_manifest();
    assert_eq!(
        m.validate_settings(&json!({})).unwrap_err(),
        SettingsError::Missing("repositories".into())
    );
    assert_eq!(
        m.validate_settings(&json!({"repositories": "a/b"}))
            .unwrap_err(),
        SettingsError::WrongKind {
            key: "repositories".into(),
            expected: SettingKind::List
        }
    );
    assert_eq!(
        m.validate_settings(&json!({"repositories": [1]}))
            .unwrap_err(),
        SettingsError::WrongKind {
            key: "repositories".into(),
            expected: SettingKind::List
        }
    );
    assert_eq!(
        m.validate_settings(&json!({"repositories": [], "archived": "yes"}))
            .unwrap_err(),
        SettingsError::WrongKind {
            key: "archived".into(),
            expected: SettingKind::Bool
        }
    );
    assert_eq!(
        m.validate_settings(&json!({"repositories": [], "extra": 1}))
            .unwrap_err(),
        SettingsError::Unknown("extra".into())
    );
    assert_eq!(
        m.validate_settings(&json!([])).unwrap_err(),
        SettingsError::NotAnObject
    );
}
