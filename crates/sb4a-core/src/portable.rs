// SPDX-License-Identifier: GPL-3.0-or-later
//! Turn a desktop sing-box config into one a root module can actually run.
//!
//! A GUI for desktop emits fields that make no sense on Android: a Clash API on
//! a loopback port, a `cache.db` next to the binary, an IPv6 TUN address, and
//! rule sets pointing at `../rulesets/*.srs` files that only exist on that
//! machine. Left alone those either fail to load or silently route nothing, so
//! the conversion is explicit and reported rather than implicit.

use crate::error::{self, Code};
use serde_json::{Map, Value};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub removed: Vec<String>,
    pub normalised: Vec<String>,
    /// Rule sets that pointed at files only the desktop machine has.
    pub dropped: Vec<String>,
    pub rulesets: usize,
}

/// Rewrite `config` in place into a portable shape.
pub fn make_portable(config: &mut Value) -> Result<Report, Code> {
    let Some(root) = config.as_object_mut() else {
        return Err(error::CONFIG_INVALID);
    };
    let mut report = Report::default();

    for key in ["$schema", "http_clients"] {
        if root.remove(key).is_some() {
            report.removed.push(key.to_string());
        }
    }
    // The Clash API binds a loopback port and `cache_file` writes next to the
    // binary; neither is meaningful (or safe) in a module.
    if root.remove("experimental").is_some() {
        report.removed.push("experimental".into());
    }

    // Keep logs bounded and timestamp-free: the supervisor owns rotation.
    root.insert(
        "log".into(),
        serde_json::json!({"level": "warn", "timestamp": false}),
    );

    if let Some(inbounds) = root.get_mut("inbounds").and_then(Value::as_array_mut) {
        for inbound in inbounds {
            let Some(inbound) = inbound.as_object_mut() else {
                continue;
            };
            match inbound.get("type").and_then(Value::as_str) {
                Some("tun") => {
                    // IPv6 on the tun address breaks connectivity on carriers
                    // that hand out an unreachable v6 prefix; strict_route
                    // fights Android's own routing rules.
                    inbound.insert("address".into(), serde_json::json!(["172.19.0.1/30"]));
                    inbound.insert("mtu".into(), serde_json::json!(1500));
                    inbound.insert("strict_route".into(), Value::Bool(false));
                    inbound
                        .entry("stack")
                        .or_insert_with(|| Value::String("system".into()));
                    inbound.remove("interface_name");
                    report.normalised.push("tun".into());
                }
                Some("mixed") | Some("socks") | Some("http") => {
                    inbound.insert("listen".into(), Value::String("127.0.0.1".into()));
                    report.normalised.push("loopback inbound".into());
                }
                _ => {}
            }
        }
    }

    // `find_process` needs platform connection-owner lookup, which a bare root
    // module does not provide; leaving it on makes every rule that is not
    // domain/ip based silently never match.
    if root
        .get_mut("route")
        .and_then(Value::as_object_mut)
        .and_then(|route| route.remove("find_process"))
        .is_some()
    {
        report.removed.push("route.find_process".into());
    }

    let mut dropped_tags: Vec<String> = Vec::new();
    if let Some(rule_sets) = root
        .get_mut("route")
        .and_then(|route| route.get_mut("rule_set"))
        .and_then(Value::as_array_mut)
    {
        let mut kept: Vec<Value> = Vec::with_capacity(rule_sets.len());
        for mut rule_set in std::mem::take(rule_sets) {
            let Some(map) = rule_set.as_object_mut() else {
                continue;
            };
            let alias = match map.get("type").and_then(Value::as_str) {
                Some("remote") => {
                    family_code_from_url(map.get("url").and_then(Value::as_str).unwrap_or(""))
                }
                _ => family_code_from_path(map.get("path").and_then(Value::as_str).unwrap_or("")),
            };
            let tag = map
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match alias {
                Some((family, code)) => {
                    map.clear();
                    map.insert("type".into(), Value::String("local".into()));
                    map.insert("tag".into(), Value::String(tag));
                    map.insert("path".into(), Value::String(format!("{family}:{code}")));
                    report.rulesets += 1;
                    kept.push(rule_set);
                }
                // A local set that is not a geo alias points at a file only the
                // machine that produced the config has. A dangling reference
                // fails validation, so drop it and report instead.
                None if map.get("type").and_then(Value::as_str) != Some("remote") => {
                    if !tag.is_empty() {
                        report.dropped.push(tag.clone());
                        dropped_tags.push(tag);
                    }
                }
                // A remote set whose URL we cannot map is fetched by the core.
                None => kept.push(rule_set),
            }
        }
        *rule_sets = kept;
    }

    if !dropped_tags.is_empty() {
        if let Some(rules) = root
            .get_mut("route")
            .and_then(|route| route.get_mut("rules"))
            .and_then(Value::as_array_mut)
        {
            rules.retain_mut(|rule| {
                let Some(map) = rule.as_object_mut() else {
                    return true;
                };
                let Some(reference) = map.get_mut("rule_set") else {
                    return true;
                };
                match reference {
                    Value::String(tag) => !dropped_tags.contains(tag),
                    Value::Array(tags) => {
                        tags.retain(|tag| {
                            tag.as_str()
                                .map(|t| !dropped_tags.contains(&t.to_string()))
                                .unwrap_or(true)
                        });
                        // A rule whose only condition was the dropped set can
                        // never match, so removing it changes nothing.
                        !tags.is_empty()
                    }
                    _ => true,
                }
            });
        }
    }

    Ok(report)
}

/// `.../geo/geosite/cn.json` -> ("geosite", "cn")
fn family_code_from_url(url: &str) -> Option<(&'static str, String)> {
    let tail = url.split("/geo/").nth(1)?;
    let mut parts = tail.split('/');
    let family = match parts.next()? {
        "geosite" => "geosite",
        "geoip" => "geoip",
        _ => return None,
    };
    let code = strip_suffix(parts.next()?)?;
    Some((family, code))
}

/// `../rulesets/geosite_cn.srs` -> ("geosite", "cn")
fn family_code_from_path(path: &str) -> Option<(&'static str, String)> {
    let name = path.rsplit('/').next()?;
    let stem = name
        .strip_suffix(".srs")
        .or_else(|| name.strip_suffix(".json"))?;
    let (family, rest) = stem.split_once(['_', '-'])?;
    let family = match family {
        "geosite" => "geosite",
        "geoip" => "geoip",
        _ => return None,
    };
    Some((family, rest.to_string()))
}

fn strip_suffix(name: &str) -> Option<String> {
    Some(
        name.strip_suffix(".srs")
            .or_else(|| name.strip_suffix(".json"))?
            .to_string(),
    )
}

/// Remove fields that exist only because a desktop GUI wrote them.
pub fn strip_unknown_experimental(config: &mut Value) {
    if let Some(root) = config.as_object_mut() {
        if let Some(Value::Object(experimental)) = root.get_mut("experimental") {
            for key in ["clash_api", "cache_file", "v2ray_api"] {
                experimental.remove(key);
            }
            if experimental.is_empty() {
                root.remove("experimental");
            }
        }
    }
    let _ = Map::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> Value {
        serde_json::json!({
            "$schema": "x", "http_clients": [],
            "log": {"disabled": false, "level": "debug", "output": "a.log"},
            "experimental": {"clash_api": {"external_controller": "127.0.0.1:20123"}},
            "inbounds": [
                {"type": "mixed", "tag": "mixed-in", "listen": "192.168.1.5", "listen_port": 20122},
                {"type": "tun", "tag": "tun-in", "address": ["172.18.0.1/30", "fc00::/18"],
                 "mtu": 0, "strict_route": true, "interface_name": "utun5"}
            ],
            "outbounds": [{"type": "direct", "tag": "direct"}],
            "route": {
                "final": "direct",
                "rule_set": [
                    {"type": "remote", "tag": "cn", "format": "source",
                     "url": "https://x/geo/geosite/cn.json"},
                    {"type": "local", "tag": "cnip", "format": "binary",
                     "path": "../rulesets/geoip_cn.srs"},
                    {"type": "local", "tag": "custom", "format": "source",
                     "path": "../rulesets/direct.json"}
                ],
                "rules": [
                    {"rule_set": "cn", "outbound": "direct"},
                    {"rule_set": ["cnip", "custom"], "outbound": "proxy"},
                    {"rule_set": "custom", "outbound": "direct"}
                ]
            }
        })
    }

    #[test]
    fn rewrites_desktop_config() {
        let mut config = desktop();
        let report = make_portable(&mut config).unwrap();
        assert!(report.removed.contains(&"experimental".to_string()));
        assert_eq!(report.rulesets, 2);
        // The file-backed custom set has no portable equivalent.
        assert_eq!(report.dropped, vec!["custom".to_string()]);
        assert_eq!(config["route"]["rule_set"].as_array().unwrap().len(), 2);
        // Rules lose the reference; one that had nothing else is removed.
        let rules = config["route"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1]["rule_set"], serde_json::json!(["cnip"]));

        assert_eq!(config["log"]["level"], "warn");
        assert_eq!(
            config["inbounds"][1]["address"],
            serde_json::json!(["172.19.0.1/30"])
        );
        assert_eq!(config["inbounds"][1]["mtu"], 1500);
        assert_eq!(config["inbounds"][1]["strict_route"], false);
        assert_eq!(config["inbounds"][1]["stack"], "system");
        assert!(config["inbounds"][1].get("interface_name").is_none());
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");

        let rule_sets = &config["route"]["rule_set"];
        assert_eq!(rule_sets[0]["type"], "local");
        assert_eq!(rule_sets[0]["path"], "geosite:cn");
        assert_eq!(rule_sets[0]["tag"], "cn");
        assert_eq!(rule_sets[1]["path"], "geoip:cn");
    }

    #[test]
    fn rejects_non_object() {
        let mut value = serde_json::json!([1, 2, 3]);
        assert_eq!(
            make_portable(&mut value).unwrap_err(),
            error::CONFIG_INVALID
        );
    }

    #[test]
    fn drops_file_backed_rule_sets() {
        // A local set that is not a geo alias points at a file the phone does
        // not have; keeping it would fail validation with `config_resource_missing`.
        let mut config = serde_json::json!({
            "route": {
                "rule_set": [{"type": "local", "tag": "keep", "path": "../files/x.json"}],
                "rules": [{"rule_set": "keep", "outbound": "direct"}]
            }
        });
        let report = make_portable(&mut config).unwrap();
        assert_eq!(report.dropped, vec!["keep".to_string()]);
        assert!(config["route"]["rule_set"].as_array().unwrap().is_empty());
        assert!(config["route"]["rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn keeps_unmappable_remote_rule_sets() {
        // The core downloads remote sets itself, so an unknown URL is still usable.
        let mut config = serde_json::json!({
            "route": {"rule_set": [{"type": "remote", "tag": "r", "format": "source",
                                    "url": "https://example.com/custom.json"}]}
        });
        let report = make_portable(&mut config).unwrap();
        assert!(report.dropped.is_empty());
        assert_eq!(config["route"]["rule_set"][0]["tag"], "r");
    }
}
