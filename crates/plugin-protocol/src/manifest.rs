use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{CONTRACT_VERSION, MIN_CONTRACT_VERSION};

/// A plugin's id: `^[a-z][a-z0-9-]{1,31}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluginId(String);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("invalid plugin id {0:?}: use a lower-case letter, then 1 to 31 lower-case letters, digits or '-'")]
pub struct PluginIdError(String);

impl FromStr for PluginId {
    type Err = PluginIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        let ok = (2..=32).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes[1..]
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if ok {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDecl {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
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

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest: {0}")]
    Toml(String),
    #[error("unsupported contract {0} (supported: {min} to {max})", min = MIN_CONTRACT_VERSION, max = CONTRACT_VERSION)]
    UnsupportedContract(u32),
    #[error("host mode {0} needs an entry file")]
    EntryRequired(HostMode),
    #[error("entry {0:?} must be a plain file name")]
    BadEntry(String),
    #[error("host {0:?} is not a plain DNS host name")]
    BadHost(String),
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

fn normalise_host(raw: &str) -> Result<String, ManifestError> {
    let bad = || ManifestError::BadHost(raw.to_string());
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
    // A dotted-quad (or any all-digit tail) is an IP literal, not a name.
    let ip_like = labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()));
    if host.len() > 253 || !labels.iter().all(label_ok) || ip_like {
        return Err(bad());
    }
    Ok(host)
}

impl Manifest {
    /// Parse and check a `plugin.toml`.
    pub fn parse(toml_text: &str) -> Result<Manifest, ManifestError> {
        let mut m: Manifest =
            toml::from_str(toml_text).map_err(|e| ManifestError::Toml(e.to_string()))?;
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
        for s in &m.secrets {
            if !seen.insert(s.key.as_str()) {
                return Err(ManifestError::DuplicateSecret(s.key.clone()));
            }
        }
        let mut seen = HashSet::new();
        for s in &m.settings {
            if !seen.insert(s.key.as_str()) {
                return Err(ManifestError::DuplicateSetting(s.key.clone()));
            }
            if s.default.as_ref().is_some_and(|d| !s.kind.accepts(d)) {
                return Err(ManifestError::BadDefault(s.key.clone()));
            }
        }
        let mut seen = HashSet::new();
        for r in &m.resources {
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
