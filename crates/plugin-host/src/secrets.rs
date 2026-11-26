//! Where secret values come from. A plugin names a secret; only the host sees its value.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use smithagents_plugin_protocol::{PluginId, SecretDecl};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// A secret value. Its `Debug` output is `Secret(..)`; it has no `Display` and no `Serialize`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    /// The value itself, for building a request header. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Why a secret could not be resolved. No variant carries a secret value or a command's output.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("cannot read {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("{path} is writable by group or others; restrict it with `chmod 600`")]
    Permissions { path: PathBuf },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("environment variable {name} is not valid UTF-8")]
    NotUnicode { name: String },
    #[error("secret command {program:?} could not start: {message}")]
    Spawn { program: String, message: String },
    #[error("secret command {program:?} did not finish within 10 s")]
    CommandTimeout { program: String },
    #[error("secret command failed: {status}")]
    Command { status: ExitStatus },
}

/// Resolves a plugin's secret by key.
#[async_trait]
pub trait SecretSource: Send + Sync {
    /// `Ok(None)` when nothing provides the secret.
    async fn get(&self, plugin: &PluginId, key: &str) -> Result<Option<Secret>, SecretError>;
}

/// Fixed secrets keyed by `(plugin id, key)`. For tests.
pub struct StaticSecrets(pub HashMap<(String, String), String>);

#[async_trait]
impl SecretSource for StaticSecrets {
    async fn get(&self, plugin: &PluginId, key: &str) -> Result<Option<Secret>, SecretError> {
        let found = self.0.get(&(plugin.to_string(), key.to_string()));
        Ok(found.map(|v| Secret(v.clone())))
    }
}

/// Where one configured secret comes from. The file holds pointers, never values.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Env(String),
    Command(Vec<String>),
}

type Key = (String, String);

/// Secrets from `<home>/secrets.toml`, then from `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>`.
///
/// The file has one table per plugin id, each key either `{ env = "NAME" }` or
/// `{ command = ["prog", "arg"] }`. A key listed in the file is resolved only
/// from its entry; the environment fallback applies to keys the file does not
/// list. Resolved values are cached in memory for five minutes.
///
/// `Default` has no entries: only the environment fallback.
#[derive(Default)]
pub struct ConfiguredSecrets {
    entries: HashMap<Key, Entry>,
    cache: Mutex<HashMap<Key, (Instant, Secret)>>,
}

impl ConfiguredSecrets {
    /// Reads `<home>/secrets.toml`. A missing file means no entries; a file that
    /// group or others can write is refused.
    pub fn load(home: &Path) -> Result<ConfiguredSecrets, SecretError> {
        let path = home.join("secrets.toml");
        let entries = match read_private(&path)? {
            Some(text) => parse(&text).map_err(|message| SecretError::Parse {
                path: path.clone(),
                message,
            })?,
            None => HashMap::new(),
        };
        Ok(ConfiguredSecrets {
            entries,
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn cached(&self, key: &Key) -> Option<Secret> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(key) {
            Some((at, secret)) if at.elapsed() < CACHE_TTL => Some(secret.clone()),
            Some(_) => {
                cache.remove(key);
                None
            }
            None => None,
        }
    }

    fn remember(&self, key: Key, secret: &Secret) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(key, (Instant::now(), secret.clone()));
    }
}

#[async_trait]
impl SecretSource for ConfiguredSecrets {
    async fn get(&self, plugin: &PluginId, key: &str) -> Result<Option<Secret>, SecretError> {
        let id = (plugin.to_string(), key.to_string());
        if let Some(secret) = self.cached(&id) {
            return Ok(Some(secret));
        }
        let resolved = match self.entries.get(&id) {
            Some(Entry::Env(name)) => from_env(name)?,
            Some(Entry::Command(argv)) => from_command(argv).await?,
            None => match env_var_name(plugin, key) {
                Some(name) => from_env(&name)?,
                None => None,
            },
        };
        if let Some(secret) = &resolved {
            self.remember(id, secret);
        }
        Ok(resolved)
    }
}

/// `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>`, upper case with `-` as `_`. Neither
/// a plugin id nor a valid secret key can contain `--` or end in `-`, so the
/// double underscore separates them unambiguously: no two (plugin, key) pairs
/// share a name. A key outside the manifest rules has no fallback.
fn env_var_name(plugin: &PluginId, key: &str) -> Option<String> {
    if !SecretDecl::is_valid_key(key) {
        return None;
    }
    let upper = |s: &str| s.to_ascii_uppercase().replace('-', "_");
    Some(format!(
        "SMITHAGENTS_SECRET_{}__{}",
        upper(plugin.as_ref()),
        upper(key)
    ))
}

/// An empty value counts as unset.
fn from_env(name: &str) -> Result<Option<Secret>, SecretError> {
    match std::env::var(name) {
        Ok(v) if v.is_empty() => Ok(None),
        Ok(v) => Ok(Some(Secret(v))),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(SecretError::NotUnicode {
            name: name.to_string(),
        }),
    }
}

/// Runs `argv` without a shell, stdin closed and stderr discarded; the first line
/// of stdout, trimmed, is the secret. Empty output counts as unset.
async fn from_command(argv: &[String]) -> Result<Option<Secret>, SecretError> {
    let program = argv[0].clone();
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| SecretError::Spawn {
        program: program.clone(),
        message: e.to_string(),
    })?;
    let output = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(SecretError::Spawn {
                program,
                message: e.to_string(),
            })
        }
        Err(_) => return Err(SecretError::CommandTimeout { program }),
    };
    if !output.status.success() {
        return Err(SecretError::Command {
            status: output.status,
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or("").trim();
    Ok((!first.is_empty()).then(|| Secret(first.to_string())))
}

/// The file's text, or `None` when it does not exist. Checks the mode on the
/// open handle so the check and the read see the same file.
fn read_private(path: &Path) -> Result<Option<String>, SecretError> {
    let io = |e: std::io::Error| SecretError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file.metadata().map_err(io)?.permissions().mode();
        if mode & 0o022 != 0 {
            return Err(SecretError::Permissions {
                path: path.to_path_buf(),
            });
        }
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(io)?;
    Ok(Some(text))
}

/// Parses the file. Messages name tables, keys and lines, never values: a
/// person may have pasted a token where a pointer belongs, and neither the
/// TOML parser's message nor its source excerpt may repeat it.
fn parse(text: &str) -> Result<HashMap<Key, Entry>, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| match e.span() {
        Some(span) => {
            let before = &text.as_bytes()[..span.start.min(text.len())];
            let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
            format!("not valid TOML (line {line})")
        }
        None => "not valid TOML".to_string(),
    })?;
    let shape = "must be { env = \"NAME\" } or { command = [\"program\", \"arg\", ...] }";
    let mut entries = HashMap::new();
    for (plugin, keys) in &table {
        let id: PluginId = plugin
            .parse()
            .map_err(|_| format!("[{plugin}] is not a plugin id"))?;
        let keys = keys
            .as_table()
            .ok_or_else(|| format!("{plugin} must be a table of secrets"))?;
        for (key, value) in keys {
            let bad = || format!("{plugin}.{key} {shape}");
            let fields = value.as_table().ok_or_else(bad)?;
            let entry = match (fields.len(), fields.get("env"), fields.get("command")) {
                (1, Some(toml::Value::String(name)), None) if !name.is_empty() => {
                    Entry::Env(name.clone())
                }
                (1, None, Some(toml::Value::Array(items))) if !items.is_empty() => Entry::Command(
                    items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<_>>()
                        .ok_or_else(bad)?,
                ),
                _ => return Err(bad()),
            };
            entries.insert((id.to_string(), key.clone()), entry);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_name_separates_plugin_and_key_with_a_double_underscore() {
        let id = |s: &str| -> PluginId { s.parse().unwrap() };
        assert_eq!(
            env_var_name(&id("my-plug"), "api-key").as_deref(),
            Some("SMITHAGENTS_SECRET_MY_PLUG__API_KEY")
        );
        assert_ne!(
            env_var_name(&id("github"), "api-token"),
            env_var_name(&id("github-api"), "token")
        );
        for bad in ["a.b", "a b", "API", "a--b", "a-", "", "a_b"] {
            assert_eq!(env_var_name(&id("my-plug"), bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parse_reads_both_entry_forms() {
        let entries = parse(
            "[gh]\ntoken = { command = [\"gh\", \"auth\", \"token\"] }\nother = { env = \"X\" }\n",
        )
        .unwrap();
        let key = |k: &str| ("gh".to_string(), k.to_string());
        assert_eq!(
            entries[&key("token")],
            Entry::Command(vec!["gh".into(), "auth".into(), "token".into()])
        );
        assert_eq!(entries[&key("other")], Entry::Env("X".into()));
    }

    #[test]
    fn a_parse_error_names_the_line() {
        let err = parse("[gh]\n\ntoken = \"oops\n").unwrap_err();
        assert!(err.contains("line 3"), "{err}");
        assert!(!err.contains("oops"), "{err}");
    }
}
