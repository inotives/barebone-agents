//! Generic local-first artifact pusher (EP-00015 Decision A2).
//!
//! Walks a configurable set of (local glob → AKW path prefix) mappings, hashes
//! each file, and pushes new/changed files to AKW via `memory_create` /
//! `memory_update`. The manifest at `data/.akw_push_manifest.json` records
//! `local_path → {sha256, last_pushed_at, akw_path}` so subsequent cycles only
//! send diffs.
//!
//! Used by:
//! - The background pusher tokio task spawned in `main.rs`.
//! - `barebone-agent akw push | status` CLI verbs (`cmd_akw.rs`).
//! - `barebone-agent prefs pull` (writes a manifest entry to skip the
//!   immediate push-back of just-pulled content).
//! - `barebone-agent prefs promote` (drops a manifest entry when the local
//!   draft file is moved to the active pool).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::tools::akw_client::{AkwClient, AkwError};

/// One watched directory → AKW path prefix mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedMapping {
    /// Local directory (recursive). Files matching `*.md` underneath are watched.
    pub local_dir: PathBuf,
    /// AKW path prefix. The remote path is `prefix + relative_path_from_local_dir`.
    pub akw_path_prefix: String,
    /// Human-readable label for logs and `akw status` output.
    pub label: String,
}

/// One manifest entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub sha256: String,
    pub last_pushed_at: String,
    pub akw_path: String,
}

/// `data/.akw_push_manifest.json` — tracks what's been pushed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Map: relative-from-cwd local path string → entry.
    pub entries: BTreeMap<String, ManifestEntry>,
}

impl Manifest {
    /// Load from disk; missing file → empty manifest.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                warn!(error = %e, path = %path.display(), "manifest parse failed; treating as empty");
                Manifest::default()
            }),
            Err(_) => Manifest::default(),
        }
    }

    /// Persist atomically (write to tmp, rename).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create manifest parent dir: {}", e))?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize manifest: {}", e))?;
        std::fs::write(&tmp, json)
            .map_err(|e| format!("failed to write manifest tmp: {}", e))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("failed to rename manifest: {}", e))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&ManifestEntry> {
        self.entries.get(key)
    }

    pub fn upsert(&mut self, key: String, entry: ManifestEntry) {
        self.entries.insert(key, entry);
    }

    pub fn remove(&mut self, key: &str) -> Option<ManifestEntry> {
        self.entries.remove(key)
    }
}

/// What kind of operation a diff produced for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAction {
    /// File is new — call `memory_create`.
    Create,
    /// File changed since last push — call `memory_update`.
    Update,
}

/// One push operation.
#[derive(Debug, Clone)]
pub struct PushOp {
    pub local_path: PathBuf,
    pub local_path_str: String,
    pub akw_path: String,
    pub sha256: String,
    pub action: PushAction,
    /// For logs.
    pub mapping_label: String,
}

/// Per-cycle summary.
#[derive(Debug, Default, Clone)]
pub struct PushReport {
    pub created: usize,
    pub updated: usize,
    pub failed: usize,
    pub failure_messages: Vec<String>,
}

impl PushReport {
    pub fn total(&self) -> usize {
        self.created + self.updated + self.failed
    }
}

/// Per-mapping status info for `akw status` output.
#[derive(Debug, Clone)]
pub struct MappingStatus {
    pub label: String,
    pub local_dir: PathBuf,
    pub file_count: usize,
    pub dirty_count: usize,
    pub never_pushed: usize,
}

/// Defaults shipped with v1.
///
/// **AKW path constraint** (live-verified during EP-00015 Phase 8): the AKW
/// MCP server's `memory_create` / `memory_update` reject any path under
/// curated tiers (`2_knowledges/`, `3_intelligences/`, `0_configs/`,
/// `1_drafts/_archived/`). Only `1_drafts/...` paths are agent-writable.
///
/// Consequence: active preferences cannot be backed up directly to
/// `2_knowledges/preferences/`. Instead, the pusher writes them to
/// `1_drafts/preferences-active/<slug>.md` and the curator promotes to
/// `2_knowledges/preferences/` via filesystem when reviewed. This preserves
/// the local-first contract — the agent never depends on AKW for selection —
/// while respecting AKW's tier write boundary.
pub fn default_mappings() -> Vec<WatchedMapping> {
    vec![
        // Active preferences — agent-promoted (manual save / `prefs promote`)
        // back up to `1_drafts/preferences-active/`. Curator promotes to
        // `2_knowledges/preferences/` via filesystem.
        WatchedMapping {
            local_dir: PathBuf::from("agents/_preferences"),
            akw_path_prefix: "1_drafts/preferences-active/".into(),
            label: "active_prefs".into(),
        },
        // Pending preferences (reflection-generated, awaiting review).
        WatchedMapping {
            local_dir: PathBuf::from("data/drafts/2_knowledges/preferences"),
            akw_path_prefix: "1_drafts/preferences-pending/".into(),
            label: "pending_prefs".into(),
        },
        // Research drafts (task output).
        WatchedMapping {
            local_dir: PathBuf::from("data/drafts/2_researches"),
            akw_path_prefix: "1_drafts/2_researches/".into(),
            label: "research_drafts".into(),
        },
        // Session summaries (Discord/CLI conversations).
        WatchedMapping {
            local_dir: PathBuf::from("data/drafts/sessions"),
            akw_path_prefix: "1_drafts/sessions/".into(),
            label: "session_drafts".into(),
        },
        // Ad-hoc note drafts (forward-compatible — no v1 producer).
        WatchedMapping {
            local_dir: PathBuf::from("data/drafts/notes"),
            akw_path_prefix: "1_drafts/notes/".into(),
            label: "note_drafts".into(),
        },
    ]
}

/// Default manifest path relative to cwd.
pub fn default_manifest_path() -> PathBuf {
    PathBuf::from("data/.akw_push_manifest.json")
}

/// Hash a file's contents (lowercase hex SHA-256).
pub fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Walk `local_dir` recursively, returning all `*.md` paths.
fn walk_md(local_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !local_dir.exists() {
        return out;
    }
    walk_md_inner(local_dir, &mut out);
    out.sort();
    out
}

fn walk_md_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_md_inner(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
            // Skip dot-prefixed sentinel files (e.g. `.template.md`).
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            out.push(p);
        }
    }
}

/// For one file: derive (local_path_str, akw_path).
fn derive_paths(file: &Path, mapping: &WatchedMapping, root_dir: &Path) -> Option<(String, String)> {
    let rel_to_local = file.strip_prefix(&mapping.local_dir).ok()?;
    let rel_to_root = file.strip_prefix(root_dir).unwrap_or(file);
    let local_path_str = rel_to_root.to_string_lossy().to_string();
    let mut akw_path = mapping.akw_path_prefix.clone();
    akw_path.push_str(&rel_to_local.to_string_lossy());
    Some((local_path_str, akw_path))
}

/// Compute the diff for a single mapping. Returns ops to push.
pub fn compute_diffs_for_mapping(
    mapping: &WatchedMapping,
    manifest: &Manifest,
    root_dir: &Path,
) -> Vec<PushOp> {
    let mut ops = Vec::new();
    for file in walk_md(&mapping.local_dir) {
        let Some((local_path_str, akw_path)) = derive_paths(&file, mapping, root_dir) else {
            continue;
        };
        let hash = match hash_file(&file) {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, path = %file.display(), "skipping unreadable file");
                continue;
            }
        };

        let action = match manifest.get(&local_path_str) {
            Some(entry) if entry.sha256 == hash => continue, // unchanged
            Some(_) => PushAction::Update,
            None => PushAction::Create,
        };
        ops.push(PushOp {
            local_path: file,
            local_path_str,
            akw_path,
            sha256: hash,
            action,
            mapping_label: mapping.label.clone(),
        });
    }
    ops
}

/// Compute diffs across all mappings.
pub fn compute_diffs(
    mappings: &[WatchedMapping],
    manifest: &Manifest,
    root_dir: &Path,
) -> Vec<PushOp> {
    let mut all = Vec::new();
    for m in mappings {
        all.extend(compute_diffs_for_mapping(m, manifest, root_dir));
    }
    all
}

/// Status report for `akw status`.
pub fn status(
    mappings: &[WatchedMapping],
    manifest: &Manifest,
    root_dir: &Path,
) -> Vec<MappingStatus> {
    mappings
        .iter()
        .map(|m| {
            let files = walk_md(&m.local_dir);
            let mut dirty = 0;
            let mut never = 0;
            for f in &files {
                let Some((local_path_str, _)) = derive_paths(f, m, root_dir) else {
                    continue;
                };
                let hash = hash_file(f).unwrap_or_default();
                match manifest.get(&local_path_str) {
                    None => never += 1,
                    Some(entry) if entry.sha256 != hash => dirty += 1,
                    _ => {}
                }
            }
            MappingStatus {
                label: m.label.clone(),
                local_dir: m.local_dir.clone(),
                file_count: files.len(),
                dirty_count: dirty,
                never_pushed: never,
            }
        })
        .collect()
}

fn now_iso() -> String {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read the local file and push via AKW. Returns the AKW response or an error.
async fn push_one(
    client: &AkwClient,
    op: &PushOp,
) -> Result<(), String> {
    let body = std::fs::read_to_string(&op.local_path)
        .map_err(|e| format!("read {}: {}", op.local_path.display(), e))?;

    let result = match op.action {
        PushAction::Create => {
            let r = client
                .memory_create(&op.akw_path, &body)
                .await;
            if let Err(e) = &r {
                // Detect "path already exists" → fall back to update.
                let msg = e.to_string().to_lowercase();
                if msg.contains("already exists") || msg.contains("path exists") {
                    debug!(akw_path = %op.akw_path, "memory_create reports exists; falling back to memory_update");
                    return client
                        .memory_update(&op.akw_path, &body)
                        .await
                        .map_err(|e| e.to_string());
                }
            }
            r.map_err(|e| e.to_string())
        }
        PushAction::Update => {
            client
                .memory_update(&op.akw_path, &body)
                .await
                .map_err(|e| e.to_string())
        }
    };
    result
}

/// Run one pusher cycle.
pub async fn push_cycle(
    client: &AkwClient,
    mappings: &[WatchedMapping],
    manifest_path: &Path,
    root_dir: &Path,
) -> PushReport {
    let mut manifest = Manifest::load(manifest_path);
    let ops = compute_diffs(mappings, &manifest, root_dir);
    let mut report = PushReport::default();

    if ops.is_empty() {
        debug!("pusher: no diffs to push");
        return report;
    }

    info!(diffs = ops.len(), "pusher cycle starting");

    for op in &ops {
        match push_one(client, op).await {
            Ok(_) => {
                manifest.upsert(
                    op.local_path_str.clone(),
                    ManifestEntry {
                        sha256: op.sha256.clone(),
                        last_pushed_at: now_iso(),
                        akw_path: op.akw_path.clone(),
                    },
                );
                match op.action {
                    PushAction::Create => report.created += 1,
                    PushAction::Update => report.updated += 1,
                }
                debug!(
                    label = %op.mapping_label,
                    local = %op.local_path_str,
                    akw = %op.akw_path,
                    action = ?op.action,
                    "push ok"
                );
            }
            Err(e) => {
                report.failed += 1;
                let msg = format!("[{}] {}: {}", op.mapping_label, op.local_path_str, e);
                warn!("push failed: {}", msg);
                report.failure_messages.push(msg);
            }
        }
    }

    if let Err(e) = manifest.save(manifest_path) {
        warn!(error = %e, "failed to save push manifest");
    }

    info!(
        created = report.created,
        updated = report.updated,
        failed = report.failed,
        "pusher cycle complete"
    );
    report
}

/// Convenience helper: write a manifest entry for a file just pulled from AKW.
/// This prevents the next pusher cycle from immediately pushing the same
/// content back. Idempotent — overwrites any existing entry.
pub fn record_pulled_file(
    manifest_path: &Path,
    local_path_str: &str,
    sha256: &str,
    akw_path: &str,
) -> Result<(), String> {
    let mut manifest = Manifest::load(manifest_path);
    manifest.upsert(
        local_path_str.to_string(),
        ManifestEntry {
            sha256: sha256.to_string(),
            last_pushed_at: now_iso(),
            akw_path: akw_path.to_string(),
        },
    );
    manifest.save(manifest_path)
}

/// Convenience helper: drop a manifest entry. Used by `prefs promote` when
/// the local source file is moved to the active pool.
pub fn drop_manifest_entry(manifest_path: &Path, local_path_str: &str) -> Result<(), String> {
    let mut manifest = Manifest::load(manifest_path);
    if manifest.remove(local_path_str).is_none() {
        return Ok(());
    }
    manifest.save(manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_md(dir: &Path, slug: &str, content: &str) -> PathBuf {
        let p = dir.join(format!("{}.md", slug));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn hash_file_deterministic() {
        let tmp = TempDir::new().unwrap();
        let p = write_md(tmp.path(), "x", "hello");
        let a = hash_file(&p).unwrap();
        let b = hash_file(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn hash_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let p = write_md(tmp.path(), "x", "hello");
        let a = hash_file(&p).unwrap();
        std::fs::write(&p, "different").unwrap();
        let b = hash_file(&p).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn manifest_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mp = tmp.path().join(".manifest.json");
        let mut m = Manifest::default();
        m.upsert(
            "a.md".into(),
            ManifestEntry {
                sha256: "deadbeef".into(),
                last_pushed_at: "2026-01-01T00:00:00Z".into(),
                akw_path: "x/a.md".into(),
            },
        );
        m.save(&mp).unwrap();
        let loaded = Manifest::load(&mp);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.get("a.md").unwrap().sha256, "deadbeef");
    }

    #[test]
    fn manifest_load_missing_returns_empty() {
        let m = Manifest::load(Path::new("/nonexistent/manifest.json"));
        assert!(m.entries.is_empty());
    }

    #[test]
    fn walk_md_finds_md_only() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("d");
        std::fs::create_dir_all(&dir).unwrap();
        write_md(&dir, "a", "alpha");
        write_md(&dir, "b", "beta");
        std::fs::write(dir.join("c.txt"), "ignore").unwrap();
        // Dot-prefixed sentinel — must be skipped.
        std::fs::write(dir.join(".template.md"), "tpl").unwrap();

        let files = walk_md(&dir);
        assert_eq!(files.len(), 2);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"a.md".to_string()));
        assert!(names.contains(&"b.md".to_string()));
    }

    #[test]
    fn compute_diffs_create_for_new_file() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write_md(&local, "alpha", "content");

        let mapping = WatchedMapping {
            local_dir: local.clone(),
            akw_path_prefix: "out/".into(),
            label: "test".into(),
        };
        let manifest = Manifest::default();
        let ops = compute_diffs_for_mapping(&mapping, &manifest, tmp.path());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].action, PushAction::Create);
        assert_eq!(ops[0].akw_path, "out/alpha.md");
    }

    #[test]
    fn compute_diffs_update_for_changed() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let p = write_md(&local, "x", "v1");

        let mapping = WatchedMapping {
            local_dir: local.clone(),
            akw_path_prefix: "out/".into(),
            label: "test".into(),
        };

        // Manifest already has this path with an old hash.
        let mut manifest = Manifest::default();
        let local_path_str = p.strip_prefix(tmp.path()).unwrap().to_string_lossy().to_string();
        manifest.upsert(
            local_path_str,
            ManifestEntry {
                sha256: "stale".into(),
                last_pushed_at: "old".into(),
                akw_path: "out/x.md".into(),
            },
        );

        let ops = compute_diffs_for_mapping(&mapping, &manifest, tmp.path());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].action, PushAction::Update);
    }

    #[test]
    fn compute_diffs_skips_unchanged() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let p = write_md(&local, "x", "stable");

        let mapping = WatchedMapping {
            local_dir: local.clone(),
            akw_path_prefix: "out/".into(),
            label: "test".into(),
        };

        let hash = hash_file(&p).unwrap();
        let local_path_str = p.strip_prefix(tmp.path()).unwrap().to_string_lossy().to_string();
        let mut manifest = Manifest::default();
        manifest.upsert(
            local_path_str,
            ManifestEntry {
                sha256: hash,
                last_pushed_at: "old".into(),
                akw_path: "out/x.md".into(),
            },
        );

        let ops = compute_diffs_for_mapping(&mapping, &manifest, tmp.path());
        assert!(ops.is_empty());
    }

    #[test]
    fn record_pulled_file_writes_entry() {
        let tmp = TempDir::new().unwrap();
        let mp = tmp.path().join(".manifest.json");
        record_pulled_file(&mp, "agents/_preferences/foo.md", "abc", "2_knowledges/preferences/foo.md").unwrap();
        let m = Manifest::load(&mp);
        assert_eq!(m.get("agents/_preferences/foo.md").unwrap().sha256, "abc");
    }

    #[test]
    fn drop_manifest_entry_removes() {
        let tmp = TempDir::new().unwrap();
        let mp = tmp.path().join(".manifest.json");
        record_pulled_file(&mp, "a.md", "x", "akw/a.md").unwrap();
        drop_manifest_entry(&mp, "a.md").unwrap();
        let m = Manifest::load(&mp);
        assert!(m.get("a.md").is_none());
    }

    #[test]
    fn drop_manifest_entry_missing_is_noop() {
        let tmp = TempDir::new().unwrap();
        let mp = tmp.path().join(".manifest.json");
        // No file exists yet.
        drop_manifest_entry(&mp, "missing.md").unwrap();
    }

    #[test]
    fn status_reports_counts() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("p");
        std::fs::create_dir_all(&local).unwrap();
        write_md(&local, "a", "v1"); // never pushed
        let pb = write_md(&local, "b", "stable"); // unchanged
        let pc = write_md(&local, "c", "old"); // dirty (manifest hash will be stale)

        let mapping = WatchedMapping {
            local_dir: local.clone(),
            akw_path_prefix: "out/".into(),
            label: "L".into(),
        };
        let mut manifest = Manifest::default();
        // b unchanged
        manifest.upsert(
            pb.strip_prefix(tmp.path()).unwrap().to_string_lossy().to_string(),
            ManifestEntry {
                sha256: hash_file(&pb).unwrap(),
                last_pushed_at: "x".into(),
                akw_path: "out/b.md".into(),
            },
        );
        // c stale
        manifest.upsert(
            pc.strip_prefix(tmp.path()).unwrap().to_string_lossy().to_string(),
            ManifestEntry {
                sha256: "oldhash".into(),
                last_pushed_at: "x".into(),
                akw_path: "out/c.md".into(),
            },
        );

        let st = status(&[mapping], &manifest, tmp.path());
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].file_count, 3);
        assert_eq!(st[0].never_pushed, 1);
        assert_eq!(st[0].dirty_count, 1);
    }

    #[test]
    fn default_mappings_has_five_entries() {
        let m = default_mappings();
        assert_eq!(m.len(), 5);
        assert!(m.iter().any(|x| x.label == "active_prefs"));
        assert!(m.iter().any(|x| x.label == "pending_prefs"));
        assert!(m.iter().any(|x| x.label == "research_drafts"));
        assert!(m.iter().any(|x| x.label == "session_drafts"));
        assert!(m.iter().any(|x| x.label == "note_drafts"));
    }
}
