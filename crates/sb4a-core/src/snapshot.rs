// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable snapshot: config + resources, no host paths.

use crate::error::{self, Code};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path};

pub const MAX_SNAPSHOT_BYTES: u64 = 192 << 20;
pub const MAX_CONFIG_BYTES: usize = 8 << 20;
pub const MAX_FILES: usize = 512;
pub const MAX_PLUGINS: usize = 32;
pub const MAX_FILE_BYTES: usize = 64 << 20;
pub const MAX_FILES_BYTES: usize = 128 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plugin {
    pub kind: String,
    pub binary: String,
    pub config: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub certificate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i32,
    #[serde(rename = "expectedRevision", default)]
    pub expected_revision: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub files: BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    pub plugins: Vec<Plugin>,
    #[serde(rename = "performancePriority", default)]
    pub performance_priority: bool,
    #[serde(rename = "autoStart", default)]
    pub auto_start: bool,
    #[serde(rename = "profileId")]
    pub profile_id: i64,
    #[serde(rename = "profileName")]
    pub profile_name: String,
}

impl Snapshot {
    pub fn config_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.config).unwrap_or_default()
    }
}

/// Accept either the module envelope or a bare sing-box `config.json`.
/// A bare config is wrapped with `profile_id = 1`, which satisfies the
/// `profile_id > 0` rule without inventing an identity the caller did not give.
pub fn decode_any(bytes: &[u8]) -> Result<Snapshot, Code> {
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(error::SNAPSHOT_TOO_LARGE);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| error::SNAPSHOT_INVALID)?;
    if value.get("config").is_some() {
        return decode(bytes);
    }
    if !value.is_object() {
        return Err(error::CONFIG_INVALID);
    }
    let snapshot = Snapshot {
        schema_version: crate::state::PROTOCOL_VERSION,
        expected_revision: String::new(),
        config: value,
        files: Default::default(),
        plugins: Vec::new(),
        performance_priority: false,
        auto_start: false,
        profile_id: 1,
        profile_name: "imported".into(),
    };
    check(&snapshot)?;
    Ok(snapshot)
}

pub fn decode(bytes: &[u8]) -> Result<Snapshot, Code> {
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(error::SNAPSHOT_TOO_LARGE);
    }
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| error::SNAPSHOT_INVALID)?;
    check(&snapshot)?;
    Ok(snapshot)
}

pub fn is_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn safe_relative(name: &str) -> bool {
    if name.len() > 181 || name.contains('\\') || Path::new(name).is_absolute() {
        return false;
    }
    if name.starts_with('/')
        || name.contains("../")
        || name.contains("/../")
        || name.ends_with("/..")
    {
        return false;
    }
    let mut first = true;
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                let ok = text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@!+".contains(&b));
                if !ok {
                    return false;
                }
                first = false;
            }
            _ => return false,
        }
    }
    !first
}

pub fn check(snapshot: &Snapshot) -> Result<(), Code> {
    if snapshot.schema_version != crate::state::PROTOCOL_VERSION
        || snapshot.profile_id <= 0
        || snapshot.profile_name.len() > 512
    {
        return Err(error::SNAPSHOT_INVALID);
    }
    if !snapshot.expected_revision.is_empty() && !is_revision(&snapshot.expected_revision) {
        return Err(error::REVISION_INVALID);
    }
    let config_bytes = snapshot.config_bytes();
    if config_bytes.len() > MAX_CONFIG_BYTES || !snapshot.config.is_object() {
        return Err(error::SNAPSHOT_INVALID);
    }
    if snapshot.files.len() > MAX_FILES || snapshot.plugins.len() > MAX_PLUGINS {
        return Err(error::TOO_MANY_FILES);
    }
    let mut total = 0usize;
    for (name, data) in &snapshot.files {
        if !safe_relative(name)
            || !(name.starts_with("assets/")
                || name.starts_with("files/")
                || name.starts_with("plugins/"))
        {
            return Err(error::UNSAFE_PATH);
        }
        if data.len() > MAX_FILE_BYTES {
            return Err(error::FILE_TOO_LARGE);
        }
        total += data.len();
        if total > MAX_FILES_BYTES {
            return Err(error::FILES_TOO_LARGE);
        }
    }
    for plugin in &snapshot.plugins {
        if !matches!(
            plugin.kind.as_str(),
            "trojan-go" | "mieru" | "naive" | "hysteria"
        ) {
            return Err(error::PLUGIN_UNSUPPORTED);
        }
        let binary = snapshot
            .files
            .get(&plugin.binary)
            .ok_or(error::PLUGIN_INVALID)?;
        if !plugin.binary.starts_with("plugins/")
            || binary.len() < 64
            || &binary[0..4] != b"\x7fELF"
            || binary[4] != 2
            || binary[5] != 1
            || u16::from_le_bytes([binary[18], binary[19]]) != 183
        {
            return Err(error::PLUGIN_INVALID);
        }
        if !plugin.config.starts_with("files/") || !snapshot.files.contains_key(&plugin.config) {
            return Err(error::PLUGIN_CONFIG_MISSING);
        }
        if !plugin.certificate.is_empty()
            && (!plugin.certificate.starts_with("files/")
                || !snapshot.files.contains_key(&plugin.certificate))
        {
            return Err(error::PLUGIN_CERT_MISSING);
        }
    }
    check_paths(&snapshot.config, &snapshot.files, "")
}

fn is_geo_alias(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("geoip:")
        .or_else(|| text.strip_prefix("geosite:"))?;
    let family = if text.starts_with("geoip:") {
        "geoip"
    } else {
        "geosite"
    };
    let code = rest.strip_suffix(".srs").unwrap_or(rest);
    if !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@!_-".contains(&b))
    {
        Some((family, code))
    } else {
        None
    }
}

fn check_path_text(text: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Code> {
    if text.contains("/data/user/")
        || text.contains("/data/data/")
        || text.contains("/storage/emulated/")
    {
        return Err(error::APP_PATH_NOT_PORTABLE);
    }
    let name = text.strip_prefix("../").ok_or(error::UNSAFE_CONFIG_PATH)?;
    if !safe_relative(name) {
        return Err(error::UNSAFE_CONFIG_PATH);
    }
    if !files.contains_key(name) {
        return Err(error::CONFIG_RESOURCE_MISSING);
    }
    Ok(())
}

fn check_paths(
    value: &serde_json::Value,
    files: &BTreeMap<String, Vec<u8>>,
    parent: &str,
) -> Result<(), Code> {
    match value {
        serde_json::Value::Object(map) => {
            let kind = map.get("type").and_then(|v| v.as_str()).unwrap_or("");
            for (key, child) in map {
                if let Some(text) = child.as_str() {
                    let is_path = key.ends_with("_path")
                        || key == "external_ui"
                        || (key == "path" && (kind == "local" || parent == "cache_file"))
                        || (key == "output" && parent == "log");
                    if is_path {
                        // `geosite:cn` style aliases are fetched on demand when the
                        // core starts, so the snapshot carries no file for them.
                        if key == "path" && kind == "local" && is_geo_alias(text).is_some() {
                            continue;
                        }
                        if text == "../files/yacd" {
                            if !files.contains_key("files/yacd/index.html") {
                                return Err(error::CONFIG_RESOURCE_MISSING);
                            }
                        } else if !text.is_empty()
                            && text != "cache.db"
                            && !(parent == "log" && (text == "stdout" || text == "stderr"))
                        {
                            check_path_text(text, files)?;
                        }
                    }
                }
                check_paths(child, files, key)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                check_paths(item, files, parent)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Snapshot {
        Snapshot {
            schema_version: 1,
            expected_revision: String::new(),
            config: serde_json::json!({"log": {"level": "warn"}}),
            files: BTreeMap::new(),
            plugins: Vec::new(),
            performance_priority: false,
            auto_start: false,
            profile_id: 1,
            profile_name: "test".into(),
        }
    }

    #[test]
    fn accepts_minimal() {
        assert!(check(&minimal()).is_ok());
    }

    #[test]
    fn rejects_app_paths() {
        let mut snapshot = minimal();
        snapshot.config = serde_json::json!({
            "log": {"output": "/storage/emulated/0/x.log"}
        });
        assert_eq!(check(&snapshot).unwrap_err(), error::APP_PATH_NOT_PORTABLE);
    }

    #[test]
    fn rejects_escape() {
        let mut snapshot = minimal();
        snapshot.config = serde_json::json!({
            "dns": {"servers": [{"type": "local", "path": "../../etc/passwd"}]}
        });
        assert_eq!(check(&snapshot).unwrap_err(), error::UNSAFE_CONFIG_PATH);
    }

    #[test]
    fn geo_alias_needs_no_shipped_file() {
        let mut snapshot = minimal();
        snapshot.config = serde_json::json!({
            "route": {"rule_set": [{"type": "local", "tag": "cn", "path": "geosite:cn"}]}
        });
        assert!(check(&snapshot).is_ok());
    }

    #[test]
    fn missing_relative_resource_fails() {
        let mut snapshot = minimal();
        snapshot.config = serde_json::json!({
            "route": {"rule_set": [{"type": "local", "tag": "x", "path": "../files/missing.json"}]}
        });
        assert_eq!(
            check(&snapshot).unwrap_err(),
            error::CONFIG_RESOURCE_MISSING
        );
    }

    #[test]
    fn decode_any_wraps_a_bare_config() {
        let raw = br#"{"log":{"level":"warn"},"outbounds":[{"type":"direct","tag":"direct"}]}"#;
        let snapshot = decode_any(raw).unwrap();
        assert_eq!(snapshot.profile_id, 1);
        assert_eq!(snapshot.profile_name, "imported");
        assert!(snapshot.config.get("outbounds").is_some());
    }

    #[test]
    fn decode_any_keeps_an_explicit_envelope() {
        let raw = br#"{"schemaVersion":1,"config":{"log":{"level":"warn"}},"profileId":7,"profileName":"mine"}"#;
        let snapshot = decode_any(raw).unwrap();
        assert_eq!(snapshot.profile_id, 7);
        assert_eq!(snapshot.profile_name, "mine");
    }

    #[test]
    fn rejects_unknown_fields() {
        let raw = br#"{"schemaVersion":1,"config":{},"profileId":1,"profileName":"x","extra":1}"#;
        assert_eq!(decode(raw).unwrap_err(), error::SNAPSHOT_INVALID);
    }
}
