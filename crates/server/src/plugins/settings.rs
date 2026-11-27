//! A plugin's settings as the daemon keeps them: the stored value is what the
//! user gave; the plugin receives it checked against the manifest, with the
//! declared defaults filled in.

use serde_json::{Map, Value};
use smithagents_core::plugin::SettingField;
use smithagents_plugin_protocol::Manifest;

use super::manager::clean_message;

/// The settings a new plugin starts with: each declared default.
pub(crate) fn defaults(manifest: &Manifest) -> Value {
    let defaults: Map<String, Value> = manifest
        .settings
        .iter()
        .filter_map(|s| Some((s.key.clone(), s.default.clone()?)))
        .collect();
    Value::Object(defaults)
}

/// What the plugin receives for `stored`, or why `stored` is not valid. The
/// reason is `SettingsError`'s sentence, which names a setting key at most.
pub(crate) fn effective(manifest: &Manifest, stored: &Value) -> Result<Value, String> {
    manifest
        .validate_settings(stored)
        .map_err(|e| clean_message(&e.to_string()))
}

/// The declared settings, for the plugin's status.
pub(crate) fn fields(manifest: &Manifest) -> Vec<SettingField> {
    manifest
        .settings
        .iter()
        .map(|s| SettingField {
            key: s.key.clone(),
            label: s.label.clone(),
            kind: s.kind.name().into(),
            required: s.required,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn manifest() -> Manifest {
        Manifest::parse(
            "id = \"probe\"\nname = \"Probe\"\nversion = \"0.1.0\"\ncontract = 1\n\
             host = \"in-process\"\n\
             [[settings]]\nkey = \"org\"\nlabel = \"Org\"\nkind = \"text\"\ndefault = \"acme\"\n\
             [[settings]]\nkey = \"repos\"\nlabel = \"Repos\"\nkind = \"list\"\nrequired = true\n\
             [[settings]]\nkey = \"archived\"\nlabel = \"Archived\"\nkind = \"bool\"\n",
        )
        .unwrap()
    }

    #[test]
    fn defaults_hold_only_declared_defaults() {
        assert_eq!(defaults(&manifest()), json!({"org": "acme"}));
    }

    #[test]
    fn effective_settings_fill_defaults_and_explain_refusals() {
        let m = manifest();
        assert_eq!(
            effective(&m, &json!({"repos": ["a/b"]})),
            Ok(json!({"org": "acme", "repos": ["a/b"]}))
        );
        assert_eq!(
            effective(&m, &json!({})),
            Err("missing required setting \"repos\"".into())
        );
    }

    #[test]
    fn fields_name_each_kind() {
        let kinds: Vec<_> = fields(&manifest())
            .into_iter()
            .map(|f| (f.key, f.kind, f.required))
            .collect();
        assert_eq!(
            kinds,
            [
                ("org".into(), "text".into(), false),
                ("repos".into(), "list".into(), true),
                ("archived".into(), "bool".into(), false),
            ]
        );
    }
}
