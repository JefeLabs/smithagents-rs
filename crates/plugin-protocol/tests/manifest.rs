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
            from_tool: None,
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
        ManifestError::Toml { .. }
    ));
    let nested = with("", "[[secrets]]\nkey = \"t\"\nlabel = \"T\"\nbogus = 1\n");
    assert!(matches!(err(&nested), ManifestError::Toml { .. }));
}

#[test]
fn bad_ids() {
    for id in ["GitHub", "a", &"a".repeat(33), "git--hub", "github-"] {
        let text = SAMPLE.replace("id = \"github\"", &format!("id = \"{id}\""));
        assert!(matches!(err(&text), ManifestError::Toml { .. }), "{id}");
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

// ------------------------------------------------------------- shown text

/// The error for `field` holding text that cannot be shown as it is.
fn bad_text(field: &str, max: usize) -> ManifestError {
    ManifestError::BadText {
        field: field.into(),
        max,
    }
}

#[test]
fn names_labels_and_help_are_plain_short_text() {
    let long = "x".repeat(129);
    let long_help = "x".repeat(513);
    // (the text replaced in TOOLS_SAMPLE, the field the error names, its cap)
    let fields = [
        ("name = \"GitHub\"", "name = \"{}\"", "name", 128),
        (
            "label = \"GitHub CLI\"",
            "label = \"{}\"",
            "tool \"gh\" label",
            128,
        ),
        (
            "label = \"GitHub sign-in\"",
            "label = \"{}\"",
            "secret \"token\" label",
            128,
        ),
        (
            "help = \"Run: gh auth login\"",
            "help = \"{}\"",
            "secret \"token\" help",
            512,
        ),
    ];
    for (from, to, field, max) in fields {
        let too_long = if max == 512 { &long_help } else { &long };
        for bad in [
            "",
            "   ",
            too_long.as_str(),
            "two\\nlines",
            "tab\\there",
            "esc \\u001b[31m",
            "bidi \\u202e",
            "sep \\u2028",
        ] {
            let text = TOOLS_SAMPLE.replace(from, &to.replace("{}", bad));
            assert_eq!(err(&text), bad_text(field, max), "{field}: {bad:?}");
        }
        // The cap itself is fine, counted in characters, not bytes.
        let at_cap = "é".repeat(max);
        let text = TOOLS_SAMPLE.replace(from, &to.replace("{}", &at_cap));
        Manifest::parse(&text).unwrap();
    }

    let setting = SAMPLE.replace("label = \"Repositories (owner/name)\"", "label = \"a\\rb\"");
    assert_eq!(
        err(&setting),
        bad_text("setting \"repositories\" label", 128)
    );
    let resource = SAMPLE.replace("label = \"Issues\"", "label = \"\"");
    assert_eq!(err(&resource), bad_text("resource \"issue\" label", 128));
}

#[test]
fn a_bad_text_error_never_quotes_the_text() {
    let text = TOOLS_SAMPLE.replace("name = \"GitHub\"", "name = \"SECRET-ish\\u0007\"");
    let e = err(&text);
    assert!(!e.to_string().contains("SECRET-ish"), "{e}");
    assert!(!format!("{e:?}").contains("SECRET-ish"), "{e:?}");
}

// ------------------------------------------------------------------ tools

const TOOLS_SAMPLE: &str = r#"
id = "github"
name = "GitHub"
version = "0.1.0"
contract = 1
host = "dylib"
entry = "libsmithagents_plugin_github.dylib"
hosts = ["api.github.com"]

[[tools]]
name = "gh"
label = "GitHub CLI"
min_version = "2.40.0"
install_url = "https://cli.github.com"
install = { mise = "gh", brew = "gh" }

[[secrets]]
key = "token"
label = "GitHub sign-in"
help = "Run: gh auth login"
from_tool = { tool = "gh", args = ["auth", "token"] }
"#;

#[test]
fn the_tools_sample_parses() {
    let m = Manifest::parse(TOOLS_SAMPLE).unwrap();
    assert_eq!(
        m.tools,
        vec![ToolDecl {
            name: "gh".into(),
            label: "GitHub CLI".into(),
            min_version: Some(semver::Version::new(2, 40, 0)),
            // Stored in its normal form.
            install_url: Some("https://cli.github.com/".into()),
            install: [("brew", "gh"), ("mise", "gh")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }]
    );
    assert_eq!(
        m.secrets[0].from_tool,
        Some(ToolSource {
            tool: "gh".into(),
            args: vec!["auth".into(), "token".into()],
        })
    );
    assert_eq!(m.secrets[0].help.as_deref(), Some("Run: gh auth login"));
}

#[test]
fn tools_are_optional_and_so_are_their_details() {
    let m = Manifest::parse(SAMPLE).unwrap();
    assert!(m.tools.is_empty());
    assert_eq!(m.secrets[0].from_tool, None);

    let bare = with("", "[[tools]]\nname = \"jq\"\nlabel = \"jq\"\n");
    let m = Manifest::parse(&bare).unwrap();
    assert_eq!(m.tools[0].min_version, None);
    assert_eq!(m.tools[0].install_url, None);
    assert!(m.tools[0].install.is_empty());

    let no_args = TOOLS_SAMPLE.replace(", args = [\"auth\", \"token\"]", "");
    let m = Manifest::parse(&no_args).unwrap();
    assert_eq!(
        m.secrets[0].from_tool.as_ref().unwrap().args,
        Vec::<String>::new()
    );
}

#[test]
fn from_tool_must_name_a_declared_tool() {
    let text = TOOLS_SAMPLE.replace("tool = \"gh\"", "tool = \"hub\"");
    assert_eq!(
        err(&text),
        ManifestError::UnknownTool {
            key: "token".into(),
            tool: "hub".into()
        }
    );
    let undeclared = TOOLS_SAMPLE.replace("[[tools]]\nname = \"gh\"", "[[tools]]\nname = \"jq\"");
    assert!(matches!(
        err(&undeclared),
        ManifestError::UnknownTool { .. }
    ));
}

#[test]
fn a_tool_name_is_a_bare_executable_name() {
    let long = "g".repeat(33);
    for name in [
        "bin/gh",
        "/usr/bin/gh",
        "../gh",
        "gh.exe",
        "GH",
        "1gh",
        "-gh",
        "g h",
        "",
        &long,
    ] {
        let text = TOOLS_SAMPLE
            .replace("name = \"gh\"", &format!("name = {name:?}"))
            .replace("tool = \"gh\"", &format!("tool = {name:?}"));
        assert_eq!(
            err(&text),
            ManifestError::BadToolName(name.into()),
            "{name:?}"
        );
    }
    let longest = "g".repeat(32);
    for name in ["g", "gh", "my_tool-2", "a-", "a__b", &longest] {
        let text = TOOLS_SAMPLE
            .replace("name = \"gh\"", &format!("name = {name:?}"))
            .replace("tool = \"gh\"", &format!("tool = {name:?}"));
        assert_eq!(Manifest::parse(&text).unwrap().tools[0].name, name);
    }
}

#[test]
fn tool_names_are_unique() {
    let text = format!("{TOOLS_SAMPLE}\n[[tools]]\nname = \"gh\"\nlabel = \"Again\"\n");
    assert_eq!(err(&text), ManifestError::DuplicateTool("gh".into()));
}

#[test]
fn a_bad_min_version_is_refused() {
    for v in ["2.40", "latest", "v2.40.0", ""] {
        let text =
            TOOLS_SAMPLE.replace("min_version = \"2.40.0\"", &format!("min_version = {v:?}"));
        assert!(matches!(err(&text), ManifestError::Toml { .. }), "{v:?}");
    }
}

#[test]
fn install_specs_are_plain_package_names() {
    let long = "a".repeat(129);
    for spec in [
        "-g",
        "--global",
        "gh; rm -rf ~",
        "gh rm",
        " gh",
        "gh\n",
        "$(id)",
        "`id`",
        "",
        "@gh",
        &long,
    ] {
        for installer in ["mise", "brew"] {
            let text = TOOLS_SAMPLE.replace(
                "install = { mise = \"gh\", brew = \"gh\" }",
                &format!("install = {{ {installer} = {spec:?} }}"),
            );
            let e = err(&text);
            assert_eq!(
                e,
                ManifestError::BadInstall("gh".into()),
                "{installer} {spec:?}"
            );
            // The refused value is not repeated: it is file text.
            if !spec.is_empty() {
                assert!(!e.to_string().contains(spec), "{e}");
            }
        }
    }
    let longest = "a".repeat(128);
    for spec in [
        "gh",
        "github-cli@2.40.1",
        "aqua:cli/cli",
        "cargo:ripgrep@latest",
        "npm:prettier@3",
        "GitHub.cli",
        "A1._:/@-",
        &longest,
    ] {
        let text = TOOLS_SAMPLE.replace(
            "install = { mise = \"gh\", brew = \"gh\" }",
            &format!("install = {{ mise = {spec:?} }}"),
        );
        assert_eq!(
            Manifest::parse(&text).unwrap().tools[0].install["mise"],
            spec,
            "{spec:?}"
        );
    }
    for installer in ["Brew", "1brew", "br ew", "\"\""] {
        let text = TOOLS_SAMPLE.replace("brew = \"gh\"", &format!("{installer} = \"gh\""));
        assert!(
            matches!(
                Manifest::parse(&text),
                Err(ManifestError::BadInstall(_) | ManifestError::Toml { .. })
            ),
            "{installer}"
        );
    }
}

#[test]
fn an_install_url_is_an_https_url() {
    for url in [
        "http://cli.github.com",
        "javascript:alert(1)",
        "cli.github.com",
        "https://",
        "file:///etc/passwd",
        "",
        // Whitespace or control characters anywhere, even where a URL
        // parser would strip or encode them (TOML escapes, so the value
        // reaches the manifest check rather than the TOML parser).
        " https://cli.github.com",
        "https://cli.github.com/a b",
        "https://cli.github.com/\\t",
        "https://cli.github.com/\\u007F",
        "https://cli.github.com/\\n",
        "https://cli.github.com\\r",
    ] {
        let text = TOOLS_SAMPLE.replace("https://cli.github.com", url);
        assert_eq!(
            err(&text),
            ManifestError::BadInstallUrl("gh".into()),
            "{url:?}"
        );
    }
    let long = format!("https://cli.github.com/{}", "a".repeat(2048));
    let text = TOOLS_SAMPLE.replace("https://cli.github.com", &long);
    assert_eq!(err(&text), ManifestError::BadInstallUrl("gh".into()));
    for (given, stored) in [
        (
            "https://cli.github.com/manual/installation",
            "https://cli.github.com/manual/installation",
        ),
        ("HTTPS://CLI.GitHub.com", "https://cli.github.com/"),
        (
            "https://cli.github.com/a/../b?x=1",
            "https://cli.github.com/b?x=1",
        ),
    ] {
        let ok = TOOLS_SAMPLE.replace("https://cli.github.com", given);
        assert_eq!(
            Manifest::parse(&ok).unwrap().tools[0]
                .install_url
                .as_deref(),
            Some(stored),
            "{given:?}"
        );
    }
}

#[test]
fn unknown_keys_are_rejected_in_tools_and_from_tool() {
    let tool = TOOLS_SAMPLE.replace(
        "label = \"GitHub CLI\"",
        "label = \"GitHub CLI\"\npath = \"/bin/gh\"",
    );
    assert!(matches!(err(&tool), ManifestError::Toml { .. }));
    let source = TOOLS_SAMPLE.replace("tool = \"gh\",", "tool = \"gh\", shell = true,");
    assert!(matches!(err(&source), ManifestError::Toml { .. }));
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
