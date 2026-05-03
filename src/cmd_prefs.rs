//! `barebone-agent prefs list | pull | promote` CLI verbs (EP-00015 Decision A).

use std::path::{Path, PathBuf};

use crate::akw_pusher::{default_manifest_path, drop_manifest_entry, hash_file, record_pulled_file};
use crate::cli::PrefsCommand;
use crate::tools::akw_client::AkwClient;

const PREFS_PULL_AKW_PREFIX: &str = "2_knowledges/preferences/";
const DRAFT_AKW_PREFIX: &str = "1_drafts/2_knowledges/preferences/";

pub async fn run(root_dir: &Path, cmd: PrefsCommand) -> Result<(), String> {
    match cmd {
        PrefsCommand::List => run_list(root_dir),
        PrefsCommand::Pull {
            slug,
            force,
            rename,
            agent,
        } => run_pull(root_dir, &slug, force, rename.as_deref(), agent.as_deref()).await,
        PrefsCommand::Promote { slug, agent } => {
            run_promote(root_dir, &slug, agent.as_deref()).await
        }
    }
}

// ---------- list ----------

fn run_list(root_dir: &Path) -> Result<(), String> {
    let active_dir = root_dir.join("agents/_preferences");
    let pending_dir = root_dir.join("data/drafts/2_knowledges/preferences");

    let active = list_dir(&active_dir);
    let pending = list_dir(&pending_dir);

    println!("Active preferences ({}): {}", active.len(), active_dir.display());
    if active.is_empty() {
        println!("  (none)");
    } else {
        for entry in &active {
            print_pref_entry(entry);
        }
    }

    println!();
    println!("Pending preferences ({}): {}", pending.len(), pending_dir.display());
    if pending.is_empty() {
        println!("  (none — reflection drafts land here; promote with `prefs promote <slug>`)");
    } else {
        for entry in &pending {
            print_pref_entry(entry);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PrefEntry {
    slug: String,
    scope: Option<String>,
    summary: Option<String>,
}

fn list_dir(dir: &Path) -> Vec<PrefEntry> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let it = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for entry in it.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let slug = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.starts_with('.') => s.to_string(),
            _ => continue,
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (scope, summary) = parse_scope_summary(&raw);
        out.push(PrefEntry { slug, scope, summary });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

fn parse_scope_summary(raw: &str) -> (Option<String>, Option<String>) {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return (None, None);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (None, None);
    };
    let fm = &rest[..end];
    let value: serde_yaml::Value = match serde_yaml::from_str(fm) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let scope = value
        .get("scope")
        .and_then(|v| v.as_str())
        .map(String::from);
    let summary = value
        .get("summary")
        .and_then(|v| v.as_str())
        .map(String::from);
    (scope, summary)
}

fn print_pref_entry(entry: &PrefEntry) {
    let scope = entry.scope.as_deref().unwrap_or("?");
    match &entry.summary {
        Some(s) => println!("  - {} (scope: {}) — {}", entry.slug, scope, s),
        None => println!("  - {} (scope: {})", entry.slug, scope),
    }
}

// ---------- pull ----------

async fn run_pull(
    root_dir: &Path,
    slug: &str,
    force: bool,
    rename: Option<&str>,
    agent_override: Option<&str>,
) -> Result<(), String> {
    let final_slug = rename.unwrap_or(slug).to_string();
    let target = root_dir.join(format!("agents/_preferences/{}.md", final_slug));

    if target.exists() && !force {
        return Err(format!(
            "File exists at {}. Use --force to overwrite or --rename <new_slug> to write under a different name.",
            display_relative(root_dir, &target)
        ));
    }

    let client = AkwClient::connect(root_dir, agent_override)
        .await
        .map_err(|e| e.to_string())?;
    println!("Using akw config from {}", client.source_path());

    let akw_path = if slug.contains('/') {
        slug.to_string()
    } else {
        format!("{}{}.md", PREFS_PULL_AKW_PREFIX, slug)
    };

    let content = client
        .memory_read(&akw_path)
        .await
        .map_err(|e| e.to_string());
    client.shutdown().await;
    let content = content?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    std::fs::write(&target, &content)
        .map_err(|e| format!("failed to write {}: {}", target.display(), e))?;

    // Per Decision A2 / EP-00015 Q7-fix: write a manifest entry so the next
    // pusher cycle doesn't re-push the same content back to AKW.
    let local_path_str = display_relative(root_dir, &target);
    let sha = hash_file(&target)?;
    let manifest_path = root_dir.join(default_manifest_path());
    if let Err(e) = record_pulled_file(&manifest_path, &local_path_str, &sha, &akw_path) {
        eprintln!("warning: failed to update push manifest: {}", e);
    }

    let action = if force { "Overwrote" } else { "Wrote" };
    println!(
        "{} {} from AKW path {} ({} bytes)",
        action,
        local_path_str,
        akw_path,
        content.len()
    );
    Ok(())
}

// ---------- promote ----------

async fn run_promote(
    root_dir: &Path,
    slug: &str,
    agent_override: Option<&str>,
) -> Result<(), String> {
    let pending_dir = root_dir.join("data/drafts/2_knowledges/preferences");
    let active_dir = root_dir.join("agents/_preferences");

    // Locate the source file. `slug` may be a bare slug (no extension) or a
    // full filename. Try both.
    let candidate_a = pending_dir.join(format!("{}.md", slug));
    let candidate_b = pending_dir.join(slug);
    let source = if candidate_a.exists() {
        candidate_a
    } else if candidate_b.exists() {
        candidate_b
    } else {
        return Err(format!(
            "Pending preference '{}' not found in {}",
            slug,
            pending_dir.display()
        ));
    };

    let final_slug = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(slug)
        .to_string();
    let target = active_dir.join(format!("{}.md", final_slug));

    if target.exists() {
        return Err(format!(
            "Active preference already exists at {}. Move or delete it first.",
            display_relative(root_dir, &target)
        ));
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }

    // Move source → target.
    std::fs::rename(&source, &target).or_else(|_| {
        // Cross-device rename can fail; fall back to copy + remove.
        std::fs::copy(&source, &target)
            .map_err(|e| format!("failed to copy {}: {}", source.display(), e))?;
        std::fs::remove_file(&source).map_err(|e| {
            format!(
                "copied to active pool but failed to remove pending source {}: {}",
                source.display(),
                e
            )
        })
    })?;

    // Drop the manifest entry for the pending path (file moved/gone).
    let manifest_path = root_dir.join(default_manifest_path());
    let pending_local_str = display_relative(root_dir, &source);
    if let Err(e) = drop_manifest_entry(&manifest_path, &pending_local_str) {
        eprintln!("warning: failed to drop pending manifest entry: {}", e);
    }

    println!(
        "Promoted {} → {}",
        display_relative(root_dir, &source),
        display_relative(root_dir, &target)
    );

    // Best-effort delete of the AKW draft (Decision A / Q8 resolution).
    let draft_akw_path = format!("{}{}.md", DRAFT_AKW_PREFIX, final_slug);
    match AkwClient::connect(root_dir, agent_override).await {
        Ok(client) => {
            let result = client.memory_delete(&draft_akw_path).await;
            client.shutdown().await;
            match result {
                Ok(()) => println!("Deleted AKW draft at {}", draft_akw_path),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.to_lowercase().contains("not found")
                        || msg.contains("404")
                        || msg.to_lowercase().contains("does not exist")
                    {
                        // The draft was never pushed. Harmless.
                        eprintln!("(note: AKW draft at {} was not present — nothing to delete)", draft_akw_path);
                    } else {
                        eprintln!(
                            "warning: failed to delete AKW draft at {}: {} (orphan accepted)",
                            draft_akw_path, msg
                        );
                    }
                }
            }
        }
        Err(e) => {
            eprintln!(
                "warning: AKW MCP not reachable; AKW draft at {} left as orphan ({})",
                draft_akw_path, e
            );
        }
    }

    Ok(())
}

fn display_relative(root_dir: &Path, path: &Path) -> String {
    path.strip_prefix(root_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

#[allow(dead_code)]
fn _ensure_pathbuf(_: &PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_scope_summary_extracts_fields() {
        let raw = "---\nscope: git\nsummary: Be concise\n---\n\nbody";
        let (scope, summary) = parse_scope_summary(raw);
        assert_eq!(scope.as_deref(), Some("git"));
        assert_eq!(summary.as_deref(), Some("Be concise"));
    }

    #[test]
    fn parse_scope_summary_no_frontmatter() {
        let (scope, summary) = parse_scope_summary("# bare body");
        assert!(scope.is_none());
        assert!(summary.is_none());
    }

    #[test]
    fn list_dir_skips_dotfiles() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.md"), "---\nscope: x\n---\n\nbody").unwrap();
        std::fs::write(dir.join(".template.md"), "---\nscope: y\n---\n\ntpl").unwrap();
        let entries = list_dir(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].slug, "real");
    }

    #[test]
    fn list_dir_missing_returns_empty() {
        let entries = list_dir(Path::new("/nonexistent/_preferences"));
        assert!(entries.is_empty());
    }
}
