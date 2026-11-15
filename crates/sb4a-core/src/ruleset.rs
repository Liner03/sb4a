// SPDX-License-Identifier: GPL-3.0-or-later
//! Geo ruleset provisioning.
//!
//! The module ships no geo database. Rulesets are fetched on demand as
//! sing-box `.srs` binaries (zlib members behind a `SRS\x02` header) and cached
//! under `<assets>/srs/`, so a config referencing `geosite:cn` resolves without
//! a multi-megabyte v2ray database.

use crate::atomic;
use crate::error::{self, Code};
use crate::runtime::Runtime;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MIRROR: &str = "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/geo";
pub const ASN_MIRROR: &str = "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/asn";
pub const SRS_MAGIC: &[u8; 4] = b"SRS\x02";

/// `geosite:cn` / `geoip:cn` / `asn:AS13335` / `srs/geosite-cn.srs`
pub fn parse_alias(text: &str) -> Option<(&'static str, String)> {
    let text = text.rsplit('/').next().unwrap_or(text);
    let text = text.strip_suffix(".srs").unwrap_or(text);
    let (family, rest) = if let Some(rest) = text.strip_prefix("geosite:") {
        ("geosite", rest)
    } else if let Some(rest) = text.strip_prefix("geoip:") {
        ("geoip", rest)
    } else if let Some(rest) = text.strip_prefix("asn:") {
        ("asn", rest)
    } else if let Some(rest) = text.strip_prefix("geosite-") {
        ("geosite", rest)
    } else if let Some(rest) = text.strip_prefix("geoip-") {
        ("geoip", rest)
    } else if let Some(rest) = text.strip_prefix("asn-") {
        ("asn", rest)
    } else {
        return None;
    };
    let code = rest;
    if code.is_empty()
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@!_-".contains(&b))
    {
        return None;
    }
    Some((family, code.to_string()))
}

pub fn cached_path(assets: &Path, family: &str, code: &str) -> PathBuf {
    assets.join("srs").join(format!("{family}-{code}.srs"))
}

pub fn remote_url(family: &str, code: &str) -> String {
    // `asn` lives beside `geo/`, not inside it.
    if family == "asn" {
        return format!("{ASN_MIRROR}/{code}.srs");
    }
    format!("{MIRROR}/{family}/{code}.srs")
}

/// Only `https://` is accepted: a ruleset URL comes from a config the user may
/// have copied from anywhere, and plain HTTP or `file://` would let it read
/// local state or be swapped in transit.
pub fn validate_url(url: &str) -> Result<(), Code> {
    let rest = url
        .strip_prefix("https://")
        .ok_or(error::RULESET_URL_INVALID)?;
    if rest.is_empty() || rest.contains(' ') || rest.contains('\t') {
        return Err(error::RULESET_URL_INVALID);
    }
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() || host.starts_with(':') || host.contains('@') {
        return Err(error::RULESET_URL_INVALID);
    }
    Ok(())
}

pub fn is_srs(bytes: &[u8]) -> bool {
    bytes.len() > 8 && &bytes[0..4] == SRS_MAGIC
}

/// Download `family/code` unless already cached, returning the cached path.
pub fn ensure(assets: &Path, family: &str, code: &str) -> Result<PathBuf, Code> {
    ensure_from(assets, family, code, None)
}

/// Same as [`ensure`], but `url` overrides the default mirror. Used for
/// self-hosted or third-party ruleset sources.
pub fn ensure_from(
    assets: &Path,
    family: &str,
    code: &str,
    url: Option<&str>,
) -> Result<PathBuf, Code> {
    let path = cached_path(assets, family, code);
    if path.is_file() {
        return Ok(path);
    }
    let source = match url {
        Some(url) => {
            validate_url(url)?;
            url.to_string()
        }
        None => remote_url(family, code),
    };
    let bytes = fetch(&source)?;
    if !is_srs(&bytes) {
        return Err(error::RULESET_CORRUPT);
    }
    if let Some(parent) = path.parent() {
        atomic::private_dir(parent).map_err(|_| error::STATE_WRITE_FAILED)?;
    }
    atomic::write(&path, &bytes, 0o600).map_err(|_| error::STATE_WRITE_FAILED)?;
    Ok(path)
}

/// Point every `type: "local"` ruleset at a real `.srs` on disk, fetching
/// aliases like `geosite:cn` or `srs/geosite-cn.srs` on demand.
pub fn prepare(assets: &Path, config: &mut serde_json::Value) -> Result<(), Code> {
    let mut resolved: BTreeMap<String, PathBuf> = BTreeMap::new();
    prepare_into(assets, config, &mut resolved)
}

fn prepare_into(
    assets: &Path,
    value: &mut serde_json::Value,
    resolved: &mut BTreeMap<String, PathBuf>,
) -> Result<(), Code> {
    match value {
        serde_json::Value::Object(map) => {
            let is_local = map.get("type").and_then(|v| v.as_str()) == Some("local");
            let text = map
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|_| is_local)
                .map(str::to_string);
            // A config may name its own source, e.g.
            //   {"type": "local", "path": "geosite:cn", "url": "https://host/cn.srs"}
            // so a private mirror or a pinned copy works without patching the module.
            let custom = map
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|_| is_local)
                .map(str::to_string);
            if let Some(text) = text {
                // `url` is a module-side hint; sing-box would reject it as an
                // unknown field. Drop it even when the ruleset is already cached
                // and no download happens, otherwise a second start breaks.
                if custom.is_some() {
                    map.remove("url");
                }
                if !Path::new(&text).is_file() && !assets.join(&text).is_file() {
                    if let Some((family, code)) = parse_alias(&text) {
                        let path = match resolved.get(&text) {
                            Some(path) => path.clone(),
                            None => {
                                let path = ensure_from(assets, family, &code, custom.as_deref())?;
                                resolved.insert(text.clone(), path.clone());
                                path
                            }
                        };
                        map.insert(
                            "path".into(),
                            serde_json::Value::String(path.to_string_lossy().into_owned()),
                        );
                        // A rewritten alias always points at a .srs, which sing-box
                        // only parses when the format is declared.
                        map.insert("format".into(), serde_json::Value::String("binary".into()));
                    }
                }
            }
            for child in map.values_mut() {
                prepare_into(assets, child, resolved)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                prepare_into(assets, item, resolved)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Fetch through the OS trust store; refuses oversized bodies.
pub fn fetch(url: &str) -> Result<Vec<u8>, Code> {
    let curl = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "60",
            "--max-filesize",
            "8388608",
            url,
        ])
        .output()
        .map_err(|_| error::RULESET_FETCH_FAILED)?;
    if !curl.status.success() {
        return Err(error::RULESET_FETCH_FAILED);
    }
    Ok(curl.stdout)
}

/// Re-fetch every ruleset the running generation references.
pub fn update_all(runtime: &Runtime) -> Result<(), Code> {
    let (revision, snapshot) = runtime.current()?;
    let Some(mut snapshot) = snapshot else {
        return Ok(());
    };
    let assets = runtime.path(&["generations", &revision, "assets"]);
    let _ = std::fs::remove_dir_all(assets.join("srs"));
    prepare(&assets, &mut snapshot.config)
}

/// Download one ruleset into the active generation without touching the config.
/// `family` is `geosite`, `geoip` or `asn`; `code` is the file stem, e.g. `cn`
/// or `category-ai-!cn@ads`.
pub fn download(runtime: &Runtime, family: &str, code: &str) -> Result<PathBuf, Code> {
    download_from(runtime, family, code, None)
}

/// `url = Some(..)` fetches from a custom source instead of the default mirror.
pub fn download_from(
    runtime: &Runtime,
    family: &str,
    code: &str,
    url: Option<&str>,
) -> Result<PathBuf, Code> {
    let family = match family {
        "geosite" | "geoip" | "asn" => family,
        _ => return Err(error::RULESET_UNKNOWN),
    };
    if code.is_empty()
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@!_-.".contains(&b))
    {
        return Err(error::RULESET_UNKNOWN);
    }
    let (revision, snapshot) = runtime.current()?;
    if snapshot.is_none() {
        return Err(error::CONFIG_MISSING);
    }
    let assets = runtime.path(&["generations", &revision, "assets"]);
    ensure_from(&assets, family, code, url)
}

/// List the cached rulesets as `family/code`, for `sb4a ruleset list`.
pub fn list_cached(runtime: &Runtime) -> Vec<String> {
    let (revision, _) = match runtime.current() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let dir = runtime.path(&["generations", &revision, "assets", "srs"]);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".srs")?;
            let (family, code) = stem.split_once('-')?;
            Some(format!("{family}/{code}"))
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases() {
        assert_eq!(parse_alias("asn:AS13335"), Some(("asn", "AS13335".into())));
        assert_eq!(parse_alias("asn-AS13335"), Some(("asn", "AS13335".into())));
        assert_eq!(parse_alias("geosite:cn"), Some(("geosite", "cn".into())));
        assert_eq!(parse_alias("geoip:cn"), Some(("geoip", "cn".into())));
        assert_eq!(
            parse_alias("geosite:category-ai-!cn@ads.srs"),
            Some(("geosite", "category-ai-!cn@ads".into()))
        );
        assert_eq!(
            parse_alias("srs/geosite-cn.srs"),
            Some(("geosite", "cn".into()))
        );
        assert_eq!(parse_alias("geoip-cn"), Some(("geoip", "cn".into())));
        assert_eq!(parse_alias("geosite:"), None);
        assert_eq!(parse_alias("/etc/passwd"), None);
    }

    #[test]
    fn builds_paths_and_urls() {
        let base = Path::new("/data/adb/sb4a/assets");
        assert_eq!(
            cached_path(base, "geosite", "cn"),
            Path::new("/data/adb/sb4a/assets/srs/geosite-cn.srs")
        );
        assert!(remote_url("geoip", "cn").ends_with("/geoip/cn.srs"));
    }

    fn temp_assets() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "sb4a-srs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn rewrites_alias_to_cached_path() {
        let assets = temp_assets();
        let cached = cached_path(&assets, "geosite", "cn");
        atomic::private_dir(cached.parent().unwrap()).unwrap();
        atomic::write(&cached, b"SRS\x02zzzz", 0o600).unwrap();
        let mut config = serde_json::json!({"route": {"rule_set": [
            {"type": "local", "tag": "cn", "path": "geosite:cn"},
            {"type": "local", "tag": "keep", "path": "/abs/other.srs"}
        ]}});
        prepare(&assets, &mut config).unwrap();
        assert_eq!(
            config["route"]["rule_set"][0]["path"],
            cached.to_string_lossy().as_ref()
        );
        assert_eq!(config["route"]["rule_set"][0]["format"], "binary");
        assert_eq!(config["route"]["rule_set"][1]["path"], "/abs/other.srs");
        let _ = std::fs::remove_dir_all(&assets);
    }

    #[test]
    fn rejects_unknown_family_and_unsafe_code() {
        let base = std::env::temp_dir().join(format!("sb4a-rs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let runtime = Runtime::new(base.join("data"), base.join("module"));
        std::fs::create_dir_all(runtime.module_dir()).unwrap();
        std::fs::write(runtime.module_dir().join("module.prop"), "id=sb4a").unwrap();
        assert_eq!(
            download(&runtime, "bogus", "cn").unwrap_err(),
            error::RULESET_UNKNOWN
        );
        assert_eq!(
            download(&runtime, "geosite", "../passwd").unwrap_err(),
            error::RULESET_UNKNOWN
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validates_custom_urls() {
        assert!(validate_url("https://example.com/cn.srs").is_ok());
        assert!(validate_url("https://example.com:8443/a/b.srs").is_ok());
        // Plain HTTP and local schemes could be tampered with or read local data.
        assert_eq!(
            validate_url("http://example.com/cn.srs").unwrap_err(),
            error::RULESET_URL_INVALID
        );
        assert_eq!(
            validate_url("file:///etc/passwd").unwrap_err(),
            error::RULESET_URL_INVALID
        );
        assert_eq!(
            validate_url("https://user@host/x.srs").unwrap_err(),
            error::RULESET_URL_INVALID
        );
        assert_eq!(
            validate_url("https://host x/x.srs").unwrap_err(),
            error::RULESET_URL_INVALID
        );
        assert_eq!(validate_url("").unwrap_err(), error::RULESET_URL_INVALID);
    }

    #[test]
    fn config_url_hint_is_consumed_not_forwarded() {
        let assets = temp_assets();
        let cached = cached_path(&assets, "geosite", "cn");
        atomic::private_dir(cached.parent().unwrap()).unwrap();
        atomic::write(&cached, b"SRS\x02zzzz", 0o600).unwrap();
        // Cache hit: the file already exists, so nothing is downloaded, but the
        // hint must still be stripped or sing-box rejects the config.
        let mut config = serde_json::json!({"route": {"rule_set": [
            {"type": "local", "tag": "cn", "path": "geosite:cn",
             "url": "https://mirror.example/cn.srs"}
        ]}});
        prepare(&assets, &mut config).unwrap();
        let rule = &config["route"]["rule_set"][0];
        assert_eq!(rule["path"], cached.to_string_lossy().as_ref());
        assert_eq!(rule["format"], "binary");
        // sing-box would reject the hint, so it must not survive.
        assert!(rule.get("url").is_none());
        let _ = std::fs::remove_dir_all(&assets);
    }

    #[test]
    fn asn_family_uses_its_own_mirror() {
        assert!(remote_url("asn", "AS13335").ends_with("/asn/AS13335.srs"));
        assert!(remote_url("geosite", "cn").ends_with("/geo/geosite/cn.srs"));
    }

    #[test]
    fn detects_srs_header() {
        assert!(!is_srs(b"SRS"));
        assert!(!is_srs(b"SRS\x01zzzzzz"));
        assert!(is_srs(b"SRS\x02zzzzzz"));
    }
}
