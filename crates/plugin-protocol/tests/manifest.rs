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
            help: None,
            hosts: vec!["api.github.com".into()],
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
    for id in ["GitHub", "a", &"a".repeat(33), "git--hub", "github-"] {
        let text = SAMPLE.replace("id = \"github\"", &format!("id = \"{id}\""));
        assert!(matches!(err(&text), ManifestError::Toml(_)), "{id}");
    }
    assert!("ab".parse::<PluginId>().is_ok());
    assert!("a".repeat(32).parse::<PluginId>().is_ok());
    assert!("a".parse::<PluginId>().is_err());
}

#[test]
fn plugin_id_rules() {
    for ok in ["ab", "a-b", "a1", "github-api", "a-1-b", &"a".repeat(32)] {
        assert!(ok.parse::<PluginId>().is_ok(), "{ok:?}");
    }
    for bad in [
        "a",
        "a--b",
        "ab-",
        "-ab",
        "1ab",
        "Ab",
        "a_b",
        "a.b",
        &"a".repeat(33),
    ] {
        assert!(bad.parse::<PluginId>().is_err(), "{bad:?}");
    }
}

#[test]
fn secret_key_rules() {
    let long = "k".repeat(65);
    for key in [
        "Token", "1token", "to--ken", "token-", "-token", "to_ken", "to.ken", "", &long,
    ] {
        let text = SAMPLE.replace("key = \"token\"", &format!("key = \"{key}\""));
        assert_eq!(
            err(&text),
            ManifestError::BadSecretKey(key.into()),
            "{key:?}"
        );
    }
    let longest = "k".repeat(64);
    for key in ["t", "api-key", "k2", "a-b-c", &longest] {
        let text = SAMPLE.replace("key = \"token\"", &format!("key = \"{key}\""));
        assert_eq!(Manifest::parse(&text).unwrap().secrets[0].key, key);
    }
}

#[test]
fn setting_keys_and_resource_kinds() {
    let long = "k".repeat(65);
    for name in ["Repos", "1repos", "re pos", "re.pos", "_repos", "", &long] {
        let text = SAMPLE.replace("key = \"repositories\"", &format!("key = \"{name}\""));
        assert_eq!(
            err(&text),
            ManifestError::BadSettingKey(name.into()),
            "{name:?}"
        );
        let text = SAMPLE.replace("kind = \"issue\"", &format!("kind = \"{name}\""));
        assert_eq!(
            err(&text),
            ManifestError::BadResourceKind(name.into()),
            "{name:?}"
        );
    }
    let longest = "k".repeat(64);
    for name in ["r", "pull_request", "pull-request", "k9", "a__b", &longest] {
        let text = SAMPLE.replace("key = \"repositories\"", &format!("key = \"{name}\""));
        Manifest::parse(&text).unwrap();
        let text = SAMPLE.replace("kind = \"issue\"", &format!("kind = \"{name}\""));
        Manifest::parse(&text).unwrap();
    }
}

#[test]
fn a_secret_without_hosts_is_bound_to_the_only_host() {
    let m = Manifest::parse(SAMPLE).unwrap();
    assert_eq!(m.secrets[0].hosts, vec!["api.github.com"]);
}

#[test]
fn a_secret_must_name_its_hosts_unless_there_is_exactly_one() {
    let two = SAMPLE.replace(
        r#"hosts = ["api.github.com"]"#,
        r#"hosts = ["api.github.com", "uploads.github.com"]"#,
    );
    assert_eq!(
        err(&two),
        ManifestError::SecretHostsRequired("token".into())
    );
    let none = SAMPLE.replace("hosts = [\"api.github.com\"]\n", "");
    assert_eq!(
        err(&none),
        ManifestError::SecretHostsRequired("token".into())
    );
    let empty = two.replace(
        "label = \"Personal access token\"",
        "label = \"Personal access token\"\nhosts = []",
    );
    assert_eq!(
        err(&empty),
        ManifestError::SecretHostsRequired("token".into())
    );

    let bound = two.replace(
        "label = \"Personal access token\"",
        "label = \"Personal access token\"\nhosts = [\"API.github.com\"]",
    );
    let m = Manifest::parse(&bound).unwrap();
    assert_eq!(m.secrets[0].hosts, vec!["api.github.com"]);
}

#[test]
fn a_secret_host_must_be_a_manifest_host() {
    let stray = SAMPLE.replace(
        "label = \"Personal access token\"",
        "label = \"Personal access token\"\nhosts = [\"api.github.com\", \"evil.example\"]",
    );
    assert_eq!(
        err(&stray),
        ManifestError::SecretHostNotDeclared {
            key: "token".into(),
            host: "evil.example".into()
        }
    );
    let malformed = SAMPLE.replace(
        "label = \"Personal access token\"",
        "label = \"Personal access token\"\nhosts = [\"*.github.com\"]",
    );
    assert_eq!(
        err(&malformed),
        ManifestError::BadHost("*.github.com".into())
    );
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
        // No dot, or a last label that does not start with a letter: names a
        // resolver or URL parser may read as an address.
        "localhost",
        "0x7f.0x0.0x0.0x1",
        "127.1",
        "a.0x1",
        "a.1b",
        "a..com",
        "-a.com",
        "a-.com",
        "a.com-",
        "a_b.com",
    ] {
        let text = SAMPLE.replace("api.github.com", h);
        assert_eq!(err(&text), ManifestError::BadHost(h.to_string()), "{h:?}");
    }
}

#[test]
fn good_hosts() {
    for h in [
        "api.github.com",
        "fixture.invalid",
        "a.b",
        "x1.example",
        "a-b.c-d.org",
        "1password.com",
        "example.xn--p1ai",
    ] {
        let text = SAMPLE.replace("api.github.com", h);
        assert_eq!(Manifest::parse(&text).unwrap().hosts, vec![h], "{h:?}");
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
