use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{CONTRACT_VERSION, MIN_CONTRACT_VERSION};

/// A plugin's id: `^[a-z][a-z0-9-]{1,31}$` with no `--` and no trailing `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluginId(String);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("invalid plugin id {0:?}: use a lower-case letter, then 1 to 31 lower-case letters, digits or single '-', not ending in '-'")]
pub struct PluginIdError(String);

impl FromStr for PluginId {
    type Err = PluginIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if (2..=32).contains(&s.len()) && is_dashed_name(s) {
            Ok(PluginId(s.to_string()))
        } else {
            Err(PluginIdError(s.to_string()))
        }
    }
}

impl TryFrom<String> for PluginId {
    type Error = PluginIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<PluginId> for String {
    fn from(id: PluginId) -> String {
        id.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PluginId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMode {
    #[serde(rename = "dylib")]
    Dylib,
    #[serde(rename = "wasm")]
    Wasm,
    #[serde(rename = "process")]
    Process,
    #[serde(rename = "in-process")]
    InProcess,
}

impl fmt::Display for HostMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HostMode::Dylib => "dylib",
            HostMode::Wasm => "wasm",
            HostMode::Process => "process",
            HostMode::InProcess => "in-process",
        })
    }
}

/// `^[a-z][a-z0-9-]*$` with no `--` and no trailing `-`. Upper-cased with `-`
/// as `_`, such names never contain `__` nor end in `_`, so
/// `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>` names one plugin and key only.
fn is_dashed_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_lowercase)
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.contains("--")
        && !s.ends_with('-')
}

/// `^[a-z][a-z0-9_-]{0,63}$`: setting keys and resource kinds, which later
/// appear in URLs and in the store.
fn is_plain_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() <= 64
        && b.first().is_some_and(u8::is_ascii_lowercase)
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// An installer's name in a tool's `install` table, such as `mise` or `brew`.
fn is_installer(s: &str) -> bool {
    s.len() <= 32 && is_plain_name(s)
}

/// The most characters of the plugin's `name` and of any `label`.
const MAX_LABEL_CHARS: usize = 128;
/// The most characters of a secret's `help`.
const MAX_HELP_CHARS: usize = 512;

/// Text the daemon shows as the manifest gives it: not blank, at most `max`
/// characters, and no control character, line or paragraph separator or
/// bidi mark (which could forge lines or reorder what a reader sees).
fn is_shown_text(s: &str, max: usize) -> bool {
    let unsafe_char = |c: char| {
        c.is_control()
            || matches!(
                c,
                '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    };
    !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(unsafe_char)
}

/// `Ok` if `text` may be shown; else the error naming `field`.
fn check_text(text: &str, max: usize, field: impl FnOnce() -> String) -> Result<(), ManifestError> {
    if is_shown_text(text, max) {
        Ok(())
    } else {
        Err(ManifestError::BadText {
            field: field(),
            max,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDecl {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
    /// The hosts this secret may be sent to, each one of the manifest's
    /// `hosts`. May be left out when the manifest declares exactly one host;
    /// after [`Manifest::parse`] it is never empty.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// A declared tool that prints the value, used when neither
    /// `secrets.toml` nor the environment provides one. Honoured only for
    /// built-in plugins; the daemon runs the tool, never the plugin.
    #[serde(default)]
    pub from_tool: Option<ToolSource>,
}

impl SecretDecl {
    /// `^[a-z][a-z0-9-]{0,63}$` with no `--` and no trailing `-`.
    pub fn is_valid_key(key: &str) -> bool {
        key.len() <= 64 && is_dashed_name(key)
    }
}

/// `<tool> <args>`, run by the daemon without a shell; the first line it
/// prints is the secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSource {
    /// The `name` of one of the manifest's `[[tools]]`.
    pub tool: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// A command-line program the plugin depends on. Honoured only for built-in
/// plugins; the daemon runs it, never the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDecl {
    /// The executable's name, never a path: `^[a-z][a-z0-9_-]{0,31}$`.
    pub name: String,
    pub label: String,
    /// Compared with the first version `<name> --version` prints.
    #[serde(default)]
    pub min_version: Option<semver::Version>,
    /// An `https` page that explains how to install the tool; after
    /// [`Manifest::parse`], in its normal URL form.
    #[serde(default)]
    pub install_url: Option<String>,
    /// Installer name to package spec, such as `mise = "gh"`. The daemon runs
    /// only `mise`, and only when asked to; the rest are hints for the user.
    #[serde(default)]
    pub install: BTreeMap<String, String>,
}

impl ToolDecl {
    /// `^[a-z][a-z0-9_-]{0,31}$`: a bare executable name, which can never
    /// name a path.
    pub fn is_valid_name(name: &str) -> bool {
        name.len() <= 32 && is_plain_name(name)
    }

    /// `^[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,127}$`: a package name, possibly with
    /// a backend prefix and a version, that becomes a process argument (so
    /// it never starts with `-`) and may be shown as a command to copy.
    pub fn is_valid_spec(spec: &str) -> bool {
        let b = spec.as_bytes();
        b.len() <= 128
            && b.first().is_some_and(u8::is_ascii_alphanumeric)
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:/@-".contains(c))
    }
}

/// The longest `install_url` a manifest may give.
const MAX_INSTALL_URL_BYTES: usize = 2048;

/// `raw` in its normal form (`Url::as_str`), if it is an `https` URL with a
/// host and no whitespace or control character anywhere (a URL parser would
/// quietly strip or encode them), at most 2048 bytes once normalised.
fn install_url(raw: &str) -> Option<String> {
    if raw
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let usable = url.scheme() == "https"
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.as_str().len() <= MAX_INSTALL_URL_BYTES;
    usable.then(|| url.as_str().to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Text,
    List,
    Bool,
}

impl SettingKind {
    fn accepts(self, v: &Value) -> bool {
        match self {
            SettingKind::Text => v.is_string(),
            SettingKind::List => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            SettingKind::Bool => v.is_boolean(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingDecl {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDecl {
    pub kind: String,
    pub label: String,
    /// 60 to 86 400.
    pub interval_secs: u32,
}

/// A plugin's `plugin.toml`, parsed and checked. Only [`Manifest::parse`]
/// builds one, and a later build may read more from the file: fields can be
/// added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Manifest {
    pub id: PluginId,
    pub name: String,
    pub version: semver::Version,
    pub contract: u32,
    pub host: HostMode,
    /// File name inside the plugin directory; required for dylib and wasm.
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub secrets: Vec<SecretDecl>,
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
    #[serde(default)]
    pub resources: Vec<ResourceDecl>,
}

fn line_suffix(line: &Option<usize>) -> String {
    line.map(|n| format!(" (line {n})")).unwrap_or_default()
}

/// Why a `plugin.toml` was refused.
///
/// A later build may refuse a manifest for a reason this one does not have:
/// the set of variants can grow.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The text is not a manifest. Deliberately carries no message from the
    /// TOML parser: it quotes the offending source line, which may hold a
    /// secret, and this error reaches logs and API responses.
    #[error("plugin.toml is not valid TOML for a plugin manifest{}", line_suffix(.line))]
    Toml {
        /// 1-based line of the problem, when the parser reports one.
        line: Option<usize>,
    },
    #[error("unsupported contract {0} (supported: {min} to {max})", min = MIN_CONTRACT_VERSION, max = CONTRACT_VERSION)]
    UnsupportedContract(u32),
    #[error("host mode {0} needs an entry file")]
    EntryRequired(HostMode),
    #[error("entry {0:?} must be a plain file name")]
    BadEntry(String),
    #[error("host {0:?} is not a plain DNS host name")]
    BadHost(String),
    #[error("secret key {0:?}: use a lower-case letter, then up to 63 lower-case letters, digits or single '-', not ending in '-'")]
    BadSecretKey(String),
    #[error("secret {0:?} must list its hosts: the manifest does not declare exactly one host")]
    SecretHostsRequired(String),
    #[error("secret {key:?}: host {host:?} is not one of the manifest's hosts")]
    SecretHostNotDeclared { key: String, host: String },
    #[error("setting key {0:?}: use a lower-case letter, then up to 63 lower-case letters, digits, '-' or '_'")]
    BadSettingKey(String),
    #[error("resource kind {0:?}: use a lower-case letter, then up to 63 lower-case letters, digits, '-' or '_'")]
    BadResourceKind(String),
    #[error("duplicate secret key {0:?}")]
    DuplicateSecret(String),
    #[error("duplicate setting key {0:?}")]
    DuplicateSetting(String),
    #[error("duplicate resource kind {0:?}")]
    DuplicateResource(String),
    #[error("resource {kind:?}: interval_secs {secs} is outside 60 to 86400")]
    BadInterval { kind: String, secs: u32 },
    #[error("setting {0:?}: default does not match its kind")]
    BadDefault(String),
    #[error("tool name {0:?}: use a lower-case letter, then up to 31 lower-case letters, digits, '-' or '_'")]
    BadToolName(String),
    #[error("duplicate tool name {0:?}")]
    DuplicateTool(String),
    /// Names the tool only: the refused entry is file text.
    #[error(
        "tool {0:?}: an install entry is not a lower-case installer name with a plain package name"
    )]
    BadInstall(String),
    /// Names the tool only: the refused address is file text.
    #[error("tool {0:?}: install_url must be an https address of at most 2048 bytes")]
    BadInstallUrl(String),
    #[error("secret {key:?}: from_tool names {tool:?}, which is not one of the manifest's tools")]
    UnknownTool { key: String, tool: String },
    /// Names the field only (with keys already checked): the refused text is
    /// file text.
    #[error("{field} must be 1 to {max} characters with no control characters")]
    BadText { field: String, max: usize },
}

/// Why a settings value does not fit a manifest's declarations. The set of
/// variants can grow.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    #[error("settings must be an object")]
    NotAnObject,
    #[error("missing required setting {0:?}")]
    Missing(String),
    #[error("setting {key:?} must be of kind {expected:?}")]
    WrongKind { key: String, expected: SettingKind },
    #[error("unknown setting {0:?}")]
    Unknown(String),
}

/// A plain DNS name, lower-cased: at least two labels of `[a-z0-9-]` that do
/// not start or end with `-`, the last starting with a letter. That rules out
/// `localhost` and every spelling a resolver or URL parser may read as an
/// address (`127.1`, `0x7f.0x0.0x0.0x1`, `a.0x1`).
fn normalise_host(raw: &str) -> Result<String, ManifestError> {
    let host = raw.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let last_starts_with_letter = labels
        .last()
        .and_then(|l| l.bytes().next())
        .is_some_and(|b| b.is_ascii_lowercase());
    if host.len() > 253
        || labels.len() < 2
        || !labels.iter().all(label_ok)
        || !last_starts_with_letter
    {
        return Err(ManifestError::BadHost(raw.to_string()));
    }
    Ok(host)
}

impl Manifest {
    /// Parses and checks a `plugin.toml`.
    ///
    /// # Errors
    ///
    /// [`ManifestError::Toml`] for text that is not a manifest (with the
    /// line, never the text: a manifest can hold what should not be shown),
    /// and one variant per rule the manifest breaks: a contract version this
    /// build does not load, a missing or unsafe entry file name, a host,
    /// key, kind or tool name that is not valid, a duplicate, an interval
    /// outside 60 to 86400 seconds, a default that does not match its kind.
    pub fn parse(toml_text: &str) -> Result<Manifest, ManifestError> {
        let mut m: Manifest = toml::from_str(toml_text).map_err(|e| ManifestError::Toml {
            line: e.span().map(|span| {
                toml_text[..span.start.min(toml_text.len())]
                    .matches('\n')
                    .count()
                    + 1
            }),
        })?;
        if !(MIN_CONTRACT_VERSION..=CONTRACT_VERSION).contains(&m.contract) {
            return Err(ManifestError::UnsupportedContract(m.contract));
        }
        check_text(&m.name, MAX_LABEL_CHARS, || "name".into())?;
        match &m.entry {
            None if matches!(m.host, HostMode::Dylib | HostMode::Wasm) => {
                return Err(ManifestError::EntryRequired(m.host));
            }
            Some(e)
                if e.is_empty()
                    || e == ".."
                    || e == "."
                    || e.contains(['/', '\\'])
                    || e.contains("..") =>
            {
                return Err(ManifestError::BadEntry(e.clone()));
            }
            _ => {}
        }
        m.hosts = m
            .hosts
            .iter()
            .map(|h| normalise_host(h))
            .collect::<Result<_, _>>()?;

        let mut seen = HashSet::new();
        for t in m.tools.iter_mut() {
            if !ToolDecl::is_valid_name(&t.name) {
                return Err(ManifestError::BadToolName(t.name.clone()));
            }
            if !seen.insert(t.name.clone()) {
                return Err(ManifestError::DuplicateTool(t.name.clone()));
            }
            check_text(&t.label, MAX_LABEL_CHARS, || {
                format!("tool {:?} label", t.name)
            })?;
            // Every entry, not only `mise`: a hint may be shown as a command
            // for the user to copy.
            if !t
                .install
                .iter()
                .all(|(installer, spec)| is_installer(installer) && ToolDecl::is_valid_spec(spec))
            {
                return Err(ManifestError::BadInstall(t.name.clone()));
            }
            if let Some(raw) = &t.install_url {
                let normal =
                    install_url(raw).ok_or_else(|| ManifestError::BadInstallUrl(t.name.clone()))?;
                t.install_url = Some(normal);
            }
        }

        let mut seen = HashSet::new();
        for s in m.secrets.iter_mut() {
            if !SecretDecl::is_valid_key(&s.key) {
                return Err(ManifestError::BadSecretKey(s.key.clone()));
            }
            if !seen.insert(s.key.clone()) {
                return Err(ManifestError::DuplicateSecret(s.key.clone()));
            }
            check_text(&s.label, MAX_LABEL_CHARS, || {
                format!("secret {:?} label", s.key)
            })?;
            if let Some(help) = &s.help {
                check_text(help, MAX_HELP_CHARS, || format!("secret {:?} help", s.key))?;
            }
            if let Some(source) = &s.from_tool {
                if !m.tools.iter().any(|t| t.name == source.tool) {
                    return Err(ManifestError::UnknownTool {
                        key: s.key.clone(),
                        tool: source.tool.clone(),
                    });
                }
            }
            if s.hosts.is_empty() {
                // Bound to the only host, or the manifest must say where it may go.
                match m.hosts.as_slice() {
                    [only] => s.hosts = vec![only.clone()],
                    _ => return Err(ManifestError::SecretHostsRequired(s.key.clone())),
                }
            } else {
                s.hosts = s
                    .hosts
                    .iter()
                    .map(|h| normalise_host(h))
                    .collect::<Result<_, _>>()?;
                if let Some(host) = s.hosts.iter().find(|h| !m.hosts.contains(h)) {
                    return Err(ManifestError::SecretHostNotDeclared {
                        key: s.key.clone(),
                        host: host.clone(),
                    });
                }
            }
        }
        let mut seen = HashSet::new();
        for s in &m.settings {
            if !is_plain_name(&s.key) {
                return Err(ManifestError::BadSettingKey(s.key.clone()));
            }
            if !seen.insert(s.key.as_str()) {
                return Err(ManifestError::DuplicateSetting(s.key.clone()));
            }
            check_text(&s.label, MAX_LABEL_CHARS, || {
                format!("setting {:?} label", s.key)
            })?;
            if s.default.as_ref().is_some_and(|d| !s.kind.accepts(d)) {
                return Err(ManifestError::BadDefault(s.key.clone()));
            }
        }
        let mut seen = HashSet::new();
        for r in &m.resources {
            if !is_plain_name(&r.kind) {
                return Err(ManifestError::BadResourceKind(r.kind.clone()));
            }
            if !seen.insert(r.kind.as_str()) {
                return Err(ManifestError::DuplicateResource(r.kind.clone()));
            }
            check_text(&r.label, MAX_LABEL_CHARS, || {
                format!("resource {:?} label", r.kind)
            })?;
            if !(60..=86_400).contains(&r.interval_secs) {
                return Err(ManifestError::BadInterval {
                    kind: r.kind.clone(),
                    secs: r.interval_secs,
                });
            }
        }
        Ok(m)
    }

    /// Checks user-supplied settings against the declarations and returns
    /// them with the defaults filled in.
    ///
    /// # Errors
    ///
    /// [`SettingsError`] when `value` is not an object, lacks a required
    /// setting, holds one of the wrong kind, or holds one the manifest does
    /// not declare.
    pub fn validate_settings(&self, value: &Value) -> Result<Value, SettingsError> {
        let given = value.as_object().ok_or(SettingsError::NotAnObject)?;
        if let Some(key) = given
            .keys()
            .find(|k| !self.settings.iter().any(|s| &s.key == *k))
        {
            return Err(SettingsError::Unknown(key.clone()));
        }
        let mut out = Map::new();
        for decl in &self.settings {
            match given.get(&decl.key).or(decl.default.as_ref()) {
                Some(v) if decl.kind.accepts(v) => {
                    out.insert(decl.key.clone(), v.clone());
                }
                Some(_) => {
                    return Err(SettingsError::WrongKind {
                        key: decl.key.clone(),
                        expected: decl.kind,
                    });
                }
                None if decl.required => return Err(SettingsError::Missing(decl.key.clone())),
                None => {}
            }
        }
        Ok(Value::Object(out))
    }
}
