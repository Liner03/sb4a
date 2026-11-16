// SPDX-License-Identifier: GPL-3.0-or-later
//! Give stateful endpoints a directory that survives a config switch.
//!
//! A `tailscale` endpoint keeps its node identity and keys in
//! `state_directory`, which sing-box defaults to the relative path `tailscale`
//! and resolves against the process working directory. The supervisor starts
//! the core inside `generations/<revision>/run`, a directory rebuilt on every
//! `config apply`. Left alone, every config switch would mint a new node:
//! a fresh interactive login each time, with the old node lingering in the
//! tailnet as a duplicate.
//!
//! So each such endpoint is pinned under the module data root, keyed by tag.

use crate::error::Code;
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Rewrite `state_directory` of every `tailscale` endpoint in place, creating
/// the directory. An absolute path is left alone: the user may have pointed it
/// at their own volume on purpose.
pub fn prepare(data_root: &Path, config: &mut Value) -> Result<(), Code> {
    let Some(endpoints) = config.get_mut("endpoints").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut rewrote = false;
    for endpoint in endpoints.iter_mut() {
        let Some(endpoint) = endpoint.as_object_mut() else {
            continue;
        };
        if endpoint.get("type").and_then(Value::as_str) != Some("tailscale") {
            continue;
        }
        if endpoint
            .get("state_directory")
            .and_then(Value::as_str)
            .is_some_and(|dir| Path::new(dir).is_absolute())
        {
            continue;
        }
        let tag = endpoint
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or("tailscale");
        let dir = data_root.join("endpoints").join(tag);
        private_dir(&dir)?;
        endpoint.insert(
            "state_directory".into(),
            Value::String(dir.to_string_lossy().into_owned()),
        );
        rewrote = true;
    }
    if rewrote && !has_dns_servers(config) {
        return Err(crate::error::DNS_REQUIRED);
    }
    Ok(())
}

/// A tailscale endpoint resolves its control plane through sing-box's DNS
/// router. Android has no `/etc/resolv.conf`, so a config without `dns.servers`
/// falls back to 127.0.0.1:53, where nothing listens — every lookup fails with
/// `connection refused`. Reject it at import time instead of retrying for
/// minutes on device.
fn has_dns_servers(config: &Value) -> bool {
    config
        .get("dns")
        .and_then(|dns| dns.get("servers"))
        .and_then(Value::as_array)
        .is_some_and(|servers| !servers.is_empty())
}

/// 0700: this directory holds the node's private key.
fn private_dir(path: &Path) -> Result<(), Code> {
    fs::create_dir_all(path).map_err(|_| crate::error::STATE_WRITE_FAILED)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // create_dir_all leaves parents at the default mode; the keys live
        // under them, so tighten the ones we create.
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| crate::error::STATE_WRITE_FAILED)?;
        if let Some(parent) = path.parent().filter(|dir| dir.ends_with("endpoints")) {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| crate::error::STATE_WRITE_FAILED)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sb4a-endpoint-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn tailscale_state_lands_under_the_data_root() {
        let root = temp_root("a");
        let mut config = json!({
            "dns": {"servers": [{"tag": "d", "type": "udp", "server": "223.5.5.5"}]},
            "endpoints": [{"type": "tailscale", "tag": "ts", "auth_key": "k"}]
        });
        prepare(&root, &mut config).unwrap();
        let state = config["endpoints"][0]["state_directory"].as_str().unwrap();
        assert!(state.starts_with(root.to_str().unwrap()));
        assert!(state.ends_with("endpoints/ts"));
        assert!(Path::new(state).is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn absolute_paths_are_respected() {
        let root = temp_root("b");
        let mut config = json!({
            "dns": {"servers": [{"tag": "d", "type": "udp", "server": "223.5.5.5"}]},
            "endpoints": [{"type": "tailscale", "tag": "ts", "state_directory": "/sdcard/ts"}]
        });
        prepare(&root, &mut config).unwrap();
        assert_eq!(config["endpoints"][0]["state_directory"], "/sdcard/ts");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn other_endpoints_are_untouched() {
        let root = temp_root("c");
        let mut config = json!({
            "endpoints": [{"type": "wireguard", "tag": "wg", "private_key": "x"}]
        });
        prepare(&root, &mut config).unwrap();
        assert!(config["endpoints"][0].get("state_directory").is_none());
    }

    #[test]
    fn tailscale_without_dns_is_rejected() {
        // Android has no /etc/resolv.conf, so sing-box would fall back to
        // 127.0.0.1:53 and every lookup would fail. Catch it at import time.
        let root = temp_root("e");
        let mut config = json!({
            "endpoints": [{"type": "tailscale", "tag": "ts"}]
        });
        assert_eq!(prepare(&root, &mut config).unwrap_err().0, "dns_required");
    }

    #[test]
    fn wireguard_without_dns_is_fine() {
        // Only tailscale resolves through the DNS router at startup.
        let root = temp_root("f");
        let mut config = json!({
            "endpoints": [{"type": "wireguard", "tag": "wg"}]
        });
        prepare(&root, &mut config).unwrap();
    }

    #[test]
    fn config_without_endpoints_is_fine() {
        let root = temp_root("d");
        let mut config = json!({"outbounds": []});
        prepare(&root, &mut config).unwrap();
    }
}
