// SPDX-License-Identifier: AGPL-3.0-only
//
// Read-only filesystem endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// Matches the existing Cyberdriver contract for:
//   GET /computer/fs/list?path=...
//   GET /computer/fs/read?path=...

use super::framing::RequestMeta;
use hbb_common::{
    anyhow::{bail, Context, Result},
    base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, env, fs, io::Read, path::PathBuf};

const MAX_READ_BYTES: u64 = 100 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 20_000;
const MAX_WRITE_BYTES: usize = 100 * 1024 * 1024;

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

#[derive(Debug, Deserialize)]
struct FsWriteRequest {
    path: String,
    content: String,
    #[serde(default = "default_write_mode")]
    mode: String,
}

#[derive(Debug, Serialize)]
struct FsWriteResponse {
    path: String,
    size: u64,
    modified: Option<u64>,
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
        if items.len() >= MAX_LIST_ENTRIES {
            bail!("directory has too many entries");
        }
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

    let file = fs::File::open(&safe_path).context("failed to open file")?;
    let mut content = Vec::new();
    file.take(MAX_READ_BYTES + 1)
        .read_to_end(&mut content)
        .context("failed to read file")?;
    if content.len() as u64 > MAX_READ_BYTES {
        bail!("file too large (>100MB)");
    }

    Ok(serde_json::to_vec(&FsReadResponse {
        path: safe_path.display().to_string(),
        content: BASE64_STANDARD.encode(&content),
        size: content.len(),
    })?)
}

pub fn write(body: &[u8]) -> Result<Vec<u8>> {
    let request: FsWriteRequest = parse_json(body)?;
    if request.path.trim().is_empty() {
        bail!("missing 'path' field");
    }

    let mode = request.mode.trim().to_ascii_lowercase();
    if mode != "write" && mode != "append" {
        bail!(
            "invalid 'mode' value {:?}; expected 'write' or 'append'",
            request.mode
        );
    }

    if estimated_decoded_len(&request.content) > MAX_WRITE_BYTES {
        bail!("decoded content too large (>100MB)");
    }
    let file_data = BASE64_STANDARD
        .decode(request.content.as_bytes())
        .context("invalid base64 content")?;
    if file_data.len() > MAX_WRITE_BYTES {
        bail!("decoded content too large (>100MB)");
    }

    let target_path = resolve_write_path(&request.path)?;
    let parent = target_path
        .parent()
        .context("target path does not have a parent directory")?;
    fs::create_dir_all(parent).context("failed to create parent directories")?;

    if mode == "append" {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_path)
            .context("failed to open file for append")?;
        file.write_all(&file_data)
            .context("failed to append file contents")?;
        file.sync_all().ok();
    } else {
        atomic_write(&target_path, &file_data)?;
    }

    let metadata = fs::metadata(&target_path).context("failed to stat written file")?;
    Ok(serde_json::to_vec(&FsWriteResponse {
        path: target_path.display().to_string(),
        size: metadata.len(),
        modified: metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
    })?)
}

fn resolve_path(raw: &str) -> Result<PathBuf> {
    let expanded = expand_home(raw);
    let path = PathBuf::from(expanded);
    path.canonicalize().context("failed to resolve path")
}

fn resolve_write_path(raw: &str) -> Result<PathBuf> {
    let expanded = expand_home(raw);
    let raw_path = PathBuf::from(expanded);

    let path = if !raw_path.is_absolute()
        && raw_path
            .parent()
            .map(|p| p.as_os_str().is_empty() || p == std::path::Path::new("."))
            .unwrap_or(true)
    {
        let home = home_dir().context("could not resolve home directory for bare filename")?;
        home.join("CyberdeskTransfers").join(&raw_path)
    } else {
        raw_path
    };

    Ok(canonicalize_best_effort(path))
}

fn canonicalize_best_effort(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
        if let Ok(canonical_parent) = parent.canonicalize() {
            return canonical_parent.join(file_name);
        }
    }
    path
}

fn atomic_write(target_path: &PathBuf, data: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let parent = target_path
        .parent()
        .context("target path does not have a parent directory")?;
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("target file name is not valid UTF-8")?;
    let temp_path = parent.join(format!(
        ".{file_name}.cyberdriver-tmp-{}",
        uuid::Uuid::new_v4()
    ));

    {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .context("failed to create temporary file")?;
        file.write_all(data)
            .context("failed to write temporary file")?;
        file.sync_all().ok();
    }

    match fs::rename(&temp_path, target_path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            Err(err).context("failed to move temporary file into place")
        }
    }
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

fn parse_json<T: for<'de> serde::Deserialize<'de>>(body: &[u8]) -> Result<T> {
    if body.is_empty() {
        bail!("missing JSON request body");
    }
    Ok(serde_json::from_slice(body).context("invalid JSON request body")?)
}

fn estimated_decoded_len(encoded: &str) -> usize {
    (encoded.len() / 4 + 1) * 3
}

fn default_write_mode() -> String {
    "write".to_string()
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
