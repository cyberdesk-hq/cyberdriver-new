// SPDX-License-Identifier: AGPL-3.0-only
//
// Read-only filesystem endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// Matches the existing Cyberdriver contract for:
//   GET /computer/fs/list?path=...
//   GET /computer/fs/read?path=...

use super::framing::RequestMeta;
use hbb_common::anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashMap, env, fs, path::PathBuf};

const MAX_READ_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Serialize)]
struct FsListEntry {
    name: String,
    path: String,
    is_dir: bool,
    is_file: bool,
    size: Option<u64>,
    modified: Option<u64>,
}

#[derive(Debug, Serialize)]
struct FsListResponse {
    path: String,
    items: Vec<FsListEntry>,
}

#[derive(Debug, Serialize)]
struct FsReadResponse {
    path: String,
    content: String,
    size: usize,
}

pub fn list(meta: &RequestMeta) -> Result<Vec<u8>> {
    let path = path_param(meta)?.unwrap_or_else(|| ".".to_string());
    let safe_path = resolve_path(&path)?;
    if !safe_path.exists() {
        bail!("directory not found");
    }
    if !safe_path.is_dir() {
        bail!("path is not a directory");
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(&safe_path).context("failed to list directory")? {
        let entry = entry.context("failed to read directory entry")?;
        let entry_path = entry.path();
        let metadata = entry.metadata().ok();
        items.push(FsListEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry_path.display().to_string(),
            is_dir: metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            is_file: metadata.as_ref().map(|m| m.is_file()).unwrap_or(false),
            size: metadata.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            modified: metadata
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        });
    }

    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(serde_json::to_vec(&FsListResponse {
        path: safe_path.display().to_string(),
        items,
    })?)
}

pub fn read(meta: &RequestMeta) -> Result<Vec<u8>> {
    let path = path_param(meta)?.context("missing required 'path' query parameter")?;
    let safe_path = resolve_path(&path)?;
    if !safe_path.exists() {
        bail!("file not found");
    }
    if !safe_path.is_file() {
        bail!("path is not a file");
    }

    let metadata = fs::metadata(&safe_path).context("failed to stat file")?;
    if metadata.len() > MAX_READ_BYTES {
        bail!("file too large (>100MB)");
    }

    let content = fs::read(&safe_path).context("failed to read file")?;
    Ok(serde_json::to_vec(&FsReadResponse {
        path: safe_path.display().to_string(),
        content: hbb_common::base64::encode(&content),
        size: content.len(),
    })?)
}

fn resolve_path(raw: &str) -> Result<PathBuf> {
    let expanded = expand_home(raw);
    let path = PathBuf::from(expanded);
    Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

fn expand_home(raw: &str) -> String {
    if raw == "~" {
        if let Some(home) = home_dir() {
            return home.display().to_string();
        }
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    raw.to_string()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn path_param(meta: &RequestMeta) -> Result<Option<String>> {
    let params = query_params(meta);
    Ok(params.get("path").cloned())
}

fn query_params(meta: &RequestMeta) -> HashMap<String, String> {
    let mut params = HashMap::new();

    if let Some((_, query)) = meta.path.split_once('?') {
        params.extend(url::form_urlencoded::parse(query.as_bytes()).into_owned());
    }

    match &meta.query {
        Value::String(raw) => {
            params.extend(url::form_urlencoded::parse(raw.as_bytes()).into_owned());
        }
        Value::Object(map) => {
            for (key, value) in map {
                if let Some(value) = value_to_string(value) {
                    params.insert(key.clone(), value);
                }
            }
        }
        _ => {}
    }

    params
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
