// SPDX-License-Identifier: AGPL-3.0
//
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct CacheMeta {
    manifest_path: String,
    manifest_mtime: u64,
    generated_at_secs: u64,
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache").join("cargo-callgraph")
}

fn ensure_dir() -> std::io::Result<PathBuf> {
    let dir = cache_dir();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn manifest_hash(manifest: &Path) -> String {
    let canonical = manifest.canonicalize().unwrap_or_else(|_| manifest.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&canonical, &mut hasher);
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

fn graph_path(manifest: &Path) -> PathBuf {
    cache_dir().join(format!("{}.json", manifest_hash(manifest)))
}

fn meta_path(manifest: &Path) -> PathBuf {
    cache_dir().join(format!("{}.meta.json", manifest_hash(manifest)))
}

fn file_mtime(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;
    let epoch = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(epoch.as_secs())
}

/// Check whether a cached graph for this manifest is still valid.
pub fn is_valid(manifest: &Path) -> bool {
    let meta_path = meta_path(manifest);
    if !meta_path.exists() {
        return false;
    }
    let Ok(meta_str) = fs::read_to_string(&meta_path) else {
        return false;
    };
    let Ok(meta): Result<CacheMeta, _> = serde_json::from_str(&meta_str) else {
        return false;
    };
    let Some(current_mtime) = file_mtime(manifest) else {
        return false;
    };
    meta.manifest_mtime == current_mtime
}

/// Read a cached graph JSON string for this manifest.
pub fn read_graph(manifest: &Path) -> Option<String> {
    let path = graph_path(manifest);
    fs::read_to_string(&path).ok()
}

/// Write a generated graph and its metadata to the cache.
pub fn write(manifest: &Path, graph_json: &str) -> std::io::Result<()> {
    ensure_dir()?;
    fs::write(graph_path(manifest), graph_json)?;

    let mtime = file_mtime(manifest).unwrap_or(0);
    let meta = CacheMeta {
        manifest_path: manifest.to_string_lossy().to_string(),
        manifest_mtime: mtime,
        generated_at_secs: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    fs::write(meta_path(manifest), serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}
