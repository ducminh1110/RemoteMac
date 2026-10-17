//! Versioned migrations of runtime settings. They only ever add what a new core expects and the
//! machine's file does not have yet (`set_default`); a value the operator set is never
//! replaced, and a file that does not exist yet is left for Wizard 2 to write.
//!
//! ```json
//! { "description": "updates are checked daily",
//!   "ops": [ { "op": "set_default", "file": "relay.toml", "table": "updates",
//!              "key": "check", "value": "daily" } ] }
//! ```

use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    SetDefault {
        file: String,
        #[serde(default)]
        table: Option<String>,
        key: String,
        value: serde_json::Value,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub description: String,
    pub ops: Vec<Step>,
}

fn simple_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Migration {
    pub fn parse(bytes: &[u8]) -> Result<Migration, String> {
        let m: Migration = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        for Step::SetDefault {
            file,
            table,
            key,
            value,
        } in &m.ops
        {
            if !crate::policy::MIGRATABLE.contains(&file.as_str()) {
                return Err(format!("{file} is not a file migrations may change"));
            }
            if !simple_key(key) || table.as_deref().is_some_and(|t| !simple_key(t)) {
                return Err(format!("{key}: not a plain setting name"));
            }
            if !(value.is_string() || value.is_boolean() || value.is_i64()) {
                return Err(format!("{key}: only text, true/false and whole numbers"));
            }
        }
        Ok(m)
    }

    /// Apply to the runtime folder; the names of the files changed.
    pub fn apply(&self, runtime: &Path) -> Result<Vec<String>, String> {
        let mut changed = Vec::new();
        for Step::SetDefault {
            file,
            table,
            key,
            value,
        } in &self.ops
        {
            let path = runtime.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut doc: toml_edit::DocumentMut = text
                .parse()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let item = match table {
                Some(t) => {
                    if doc.get(t).is_none() {
                        doc[t.as_str()] = toml_edit::table();
                    }
                    let Some(tab) = doc[t.as_str()].as_table_like_mut() else {
                        return Err(format!("{}: [{t}] is not a table", path.display()));
                    };
                    if tab.contains_key(key) {
                        continue;
                    }
                    tab.insert(key, toml_value(value));
                    true
                }
                None => {
                    if doc.contains_key(key) {
                        continue;
                    }
                    doc[key.as_str()] = toml_value(value);
                    true
                }
            };
            if item {
                write_atomic(&path, doc.to_string().as_bytes())?;
                if !changed.contains(file) {
                    changed.push(file.clone());
                }
            }
        }
        Ok(changed)
    }
}

fn toml_value(v: &serde_json::Value) -> toml_edit::Item {
    match v {
        serde_json::Value::Bool(b) => toml_edit::value(*b),
        serde_json::Value::Number(n) => toml_edit::value(n.as_i64().unwrap_or(0)),
        serde_json::Value::String(s) => toml_edit::value(s.as_str()),
        _ => toml_edit::Item::None,
    }
}

/// Write a file through a temporary one next to it and a rename, keeping its permissions.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("taro-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(m) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_added_never_replaced() {
        let d = std::env::temp_dir().join(format!("taro-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("relay.toml"),
            "# mine\nport = 7470\n\n[updates]\nmode = \"manual\"\n",
        )
        .unwrap();
        let m = Migration::parse(br#"{"description":"t","ops":[
            {"op":"set_default","file":"relay.toml","table":"updates","key":"mode","value":"auto"},
            {"op":"set_default","file":"relay.toml","table":"updates","key":"check","value":"daily"},
            {"op":"set_default","file":"relay.toml","key":"udp","value":true}]}"#)
        .unwrap();
        assert_eq!(m.apply(&d).unwrap(), vec!["relay.toml".to_string()]);
        let t = std::fs::read_to_string(d.join("relay.toml")).unwrap();
        assert!(
            t.contains("# mine")
                && t.contains("mode = \"manual\"")
                && t.contains("check = \"daily\"")
                && t.contains("udp = true"),
            "{t}"
        );
        // again: nothing to do
        assert!(m.apply(&d).unwrap().is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn only_runtime_files_and_plain_values() {
        assert!(Migration::parse(br#"{"description":"t","ops":[{"op":"set_default","file":"provider.json","key":"a","value":1}]}"#).is_err());
        assert!(Migration::parse(br#"{"description":"t","ops":[{"op":"set_default","file":"relay.toml","key":"a.b","value":1}]}"#).is_err());
        assert!(Migration::parse(br#"{"description":"t","ops":[{"op":"replace","file":"relay.toml","key":"a","value":1}]}"#).is_err());
    }
}
