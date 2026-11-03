//! Portable companion manifests. Payload formats and descriptors remain opaque.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use stp_client::assets::new_asset_id;
use stp_client::{error::Result, protocol::Asset, session::Session};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn manifest_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.stimma-sidecars.json", path.display()))
}
fn valid_bytes(entry: &Value, bytes: &[u8]) -> bool {
    entry
        .get("sha256")
        .and_then(Value::as_str)
        .is_none_or(|h| h == digest(bytes))
        && entry
            .get("byte_size")
            .and_then(Value::as_u64)
            .is_none_or(|n| n == bytes.len() as u64)
}
fn unavailable(entry: &mut Value, reason: &str) {
    if let Some(obj) = entry.as_object_mut() {
        obj.remove("asset_id");
        obj.insert("status".into(), json!("unavailable"));
        obj.insert("unavailable_reason".into(), json!(reason));
    }
}

/// Missing companions decline individually; the already uploaded render stays usable.
pub async fn upload(session: &mut Session, primary: &Path, leaf: &Value) -> Vec<Value> {
    if session
        .provider
        .capabilities
        .extra
        .get("media_sidecars")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return vec![];
    }
    let accepts = leaf.get("x-accept-sidecars").and_then(Value::as_array);
    let Some(accepts) = accepts else {
        return vec![];
    };
    let manifest = std::fs::read(manifest_path(primary))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(manifest) = manifest else {
        return vec![];
    };
    let Some(entries) = manifest.get("sidecars").and_then(Value::as_array) else {
        return vec![];
    };
    let primary_valid = std::fs::read(primary).ok().is_some_and(|b| {
        manifest.get("primary_sha256").and_then(Value::as_str) == Some(digest(&b).as_str())
    });
    let parent = primary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .ok();
    let mut out = Vec::new();
    for original in entries {
        if !accepts.iter().any(|a| {
            a.get("format") == original.get("format")
                && a.get("format_version") == original.get("format_version")
        }) {
            continue;
        }
        let mut entry = original.clone();
        if !primary_valid {
            unavailable(&mut entry, "integrity_failed");
            out.push(entry);
            continue;
        }
        let file = original.get("file").and_then(Value::as_str);
        let resolved = parent
            .as_ref()
            .and_then(|p| file.map(|f| p.join(f)))
            .and_then(|p| p.canonicalize().ok())
            .filter(|p| parent.as_ref().is_some_and(|base| p.starts_with(base)));
        let bytes = resolved.and_then(|p| std::fs::read(p).ok());
        match bytes {
            None => unavailable(&mut entry, "local_missing"),
            Some(bytes) if !valid_bytes(&entry, &bytes) => {
                unavailable(&mut entry, "integrity_failed")
            }
            Some(bytes) => {
                let id = new_asset_id("bin");
                match session.asset_store.put(&id, bytes).await {
                    Ok(()) => {
                        entry["asset_id"] = json!(id);
                        entry["status"] = json!("available");
                    }
                    Err(_) => unavailable(&mut entry, "upload_failed"),
                }
            }
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.remove("file");
        }
        out.push(entry);
    }
    out
}

pub async fn download(
    session: &mut Session,
    asset: &Asset,
    primary: &Path,
    keep_remote: bool,
) -> Result<()> {
    if asset.sidecars.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::new();
    let name = primary.file_name().unwrap_or_default().to_string_lossy();
    let dir_name = format!("{name}.sidecars");
    let dir = primary
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&dir_name);
    let mut downloaded = Vec::new();
    for (index, original) in asset.sidecars.iter().enumerate() {
        let mut entry = original.clone();
        if let Some(id) = original.get("asset_id").and_then(Value::as_str) {
            match session.asset_store.get(id).await {
                Ok(bytes) if valid_bytes(&entry, &bytes) => {
                    let file = format!("state_{}.bin", index + 1);
                    if std::fs::create_dir_all(&dir)
                        .and_then(|()| std::fs::write(dir.join(&file), &bytes))
                        .is_ok()
                    {
                        entry["file"] = json!(format!("{dir_name}/{file}"));
                        entry["sha256"] = json!(digest(&bytes));
                        entry["byte_size"] = json!(bytes.len());
                        downloaded.push(id.to_string());
                        if let Some(obj) = entry.as_object_mut() {
                            obj.remove("asset_id");
                        }
                    } else {
                        unavailable(&mut entry, "local_write_failed");
                    }
                }
                Ok(_) => unavailable(&mut entry, "integrity_failed"),
                Err(_) => unavailable(&mut entry, "provider_asset_missing"),
            }
        }
        entries.push(entry);
    }
    let bytes = std::fs::read(primary)?;
    let manifest = json!({"version": 1, "primary_sha256": digest(&bytes), "sidecars": entries});
    let path = manifest_path(primary);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::rename(tmp, path)?;
    if !keep_remote {
        for id in downloaded {
            let _ = session.asset_store.delete(&id).await;
        }
    }
    Ok(())
}

pub fn media_schema(schema: &Value, path: &str) -> Value {
    if !path.starts_with('/') {
        let prop = &schema["properties"][path];
        return if prop.get("x-accept-sidecars").is_some() {
            prop.clone()
        } else {
            prop["items"].clone()
        };
    }
    let mut leaf = schema;
    for key in path.trim_start_matches('/').split('/') {
        if leaf.get("type").and_then(Value::as_str) == Some("array") {
            leaf = &leaf["items"];
        } else {
            leaf = &leaf["properties"][key.replace("~1", "/").replace("~0", "~")];
        }
    }
    leaf.clone()
}

pub fn nested_media(
    value: &Value,
    schema: &Value,
    pointer: &str,
    out: &mut Vec<(String, String, bool)>,
) {
    if schema.get("x-media").is_some() {
        if let Some(file) = value.as_str().filter(|s| Path::new(s).is_file()) {
            let root = pointer
                .trim_start_matches('/')
                .split('/')
                .next()
                .unwrap_or("");
            if !out.iter().any(|(p, f, _)| p == root && f == file) {
                out.push((pointer.into(), file.into(), false));
            }
        }
    }
    if let Some(values) = value.as_array() {
        for (i, item) in values.iter().enumerate() {
            nested_media(item, &schema["items"], &format!("{pointer}/{i}"), out);
        }
    } else if let Some(values) = value.as_object() {
        for (key, item) in values {
            nested_media(
                item,
                &schema["properties"][key],
                &format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1")),
                out,
            );
        }
    }
}
