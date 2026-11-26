use std::collections::HashSet;
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
}

impl SecretDecl {
    /// `^[a-z][a-z0-9-]{0,63}$` with no `--` and no trailing `-`.
    pub fn is_valid_key(key: &str) -> bool {
        key.len() <= 64 && is_dashed_name(key)
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub secrets: Vec<SecretDecl>,
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
    #[serde(default)]
    pub resources: Vec<ResourceDecl>,
}

fn line_suffix(line: &Option<usize>) -> String {
    line.map(|n| format!(" (line {n})")).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
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
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
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
    /// Parse and check a `plugin.toml`.
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
        for s in m.secrets.iter_mut() {
            if !SecretDecl::is_valid_key(&s.key) {
                return Err(ManifestError::BadSecretKey(s.key.clone()));
            }
            if !seen.insert(s.key.clone()) {
                return Err(ManifestError::DuplicateSecret(s.key.clone()));
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
            if !(60..=86_400).contains(&r.interval_secs) {
                return Err(ManifestError::BadInterval {
                    kind: r.kind.clone(),
                    secs: r.interval_secs,
                });
            }
        }
        Ok(m)
    }

    /// Check user-supplied settings against the declarations and fill in defaults.
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
