use std::path::{Path, PathBuf};

use crate::cli::{RoleCommand, SkillCommand};
use crate::tools::akw_client::{AkwClient, AkwError, FetchedDoc, Kind, SearchHit};

const SEARCH_LIMIT: usize = 5;

pub async fn run_skill(root_dir: &Path, cmd: SkillCommand) -> Result<(), String> {
    match cmd {
        SkillCommand::Search { query, agent } => {
            run_search(root_dir, Kind::Skill, &query, agent.as_deref()).await
        }
        SkillCommand::Pull {
            slug,
            force,
            rename,
            agent,
        } => run_pull(root_dir, Kind::Skill, &slug, force, rename.as_deref(), agent.as_deref())
            .await,
        SkillCommand::List => run_list(root_dir, Kind::Skill),
    }
}

pub async fn run_role(root_dir: &Path, cmd: RoleCommand) -> Result<(), String> {
    match cmd {
        RoleCommand::Search { query, agent } => {
            run_search(root_dir, Kind::Role, &query, agent.as_deref()).await
        }
        RoleCommand::Pull {
            slug,
            force,
            rename,
            agent,
        } => run_pull(root_dir, Kind::Role, &slug, force, rename.as_deref(), agent.as_deref())
            .await,
        RoleCommand::List => run_list(root_dir, Kind::Role),
    }
}

// ---------- search ----------

async fn run_search(
    root_dir: &Path,
    kind: Kind,
    query: &str,
    agent_override: Option<&str>,
) -> Result<(), String> {
    let client = AkwClient::connect(root_dir, agent_override)
        .await
        .map_err(|e| e.to_string())?;
    println!("Using akw config from {}", client.source_path());

    let hits = client.search(kind, query, SEARCH_LIMIT).await;
    client.shutdown().await;

    let hits = hits.map_err(|e| e.to_string())?;
    if hits.is_empty() {
        println!("(no {} matches for {:?})", kind.label(), query);
        return Ok(());
    }

    println!("Top {} matches for {:?}:", kind.label(), query);
    for (i, hit) in hits.iter().enumerate() {
        print_hit(i + 1, hit);
    }
    Ok(())
}

fn print_hit(rank: usize, hit: &SearchHit) {
    println!("  {}. {} ({}) — score {:.2}", rank, hit.slug, hit.path, hit.score);
    if let Some(desc) = &hit.description {
        println!("     {}", desc);
    }
}

// ---------- pull ----------

async fn run_pull(
    root_dir: &Path,
    kind: Kind,
    slug_or_path: &str,
    force: bool,
    rename: Option<&str>,
    agent_override: Option<&str>,
) -> Result<(), String> {
    let client = AkwClient::connect(root_dir, agent_override)
        .await
        .map_err(|e| e.to_string())?;
    println!("Using akw config from {}", client.source_path());

    let fetch = client.get(kind, slug_or_path).await;
    client.shutdown().await;

    let doc = fetch.map_err(|e| e.to_string())?;
    let written = write_pulled_doc(root_dir, kind, &doc, force, rename)?;
    let action = if force { "Overwrote" } else { "Wrote" };
    println!(
        "{} {} from AKW path {} ({} bytes)",
        action,
        display_relative(root_dir, &written.path),
        doc.path,
        written.bytes_written
    );
    Ok(())
}

/// Apply a pull's local-write logic: derive target path, enforce collision
/// policy, normalize content (skills only), write to disk.
pub fn write_pulled_doc(
    root_dir: &Path,
    kind: Kind,
    doc: &FetchedDoc,
    force: bool,
    rename: Option<&str>,
) -> Result<WrittenFile, String> {
    let final_slug = rename
        .map(String::from)
        .unwrap_or_else(|| doc.slug.clone());
    let target = local_target_path(root_dir, kind, &final_slug);

    if target.exists() && !force {
        return Err(format!(
            "File exists at {}. Use --force to overwrite or --rename <new_slug> to write under a different name.",
            display_relative(root_dir, &target)
        ));
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }

    let to_write = match kind {
        Kind::Skill => normalize_skill_frontmatter(doc),
        Kind::Role => doc.raw.clone(),
    };

    std::fs::write(&target, &to_write)
        .map_err(|e| format!("Failed to write {}: {}", target.display(), e))?;

    Ok(WrittenFile {
        path: target,
        bytes_written: to_write.len(),
    })
}

#[derive(Debug)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub bytes_written: usize,
}

fn local_target_path(root_dir: &Path, kind: Kind, slug: &str) -> PathBuf {
    let dir = match kind {
        Kind::Skill => "_skills",
        Kind::Role => "_roles",
    };
    root_dir.join("agents").join(dir).join(format!("{}.md", slug))
}

fn display_relative(root_dir: &Path, path: &Path) -> String {
    path.strip_prefix(root_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// If the fetched skill's frontmatter has `tags:` but no `keywords:`, insert a
/// synthesized `keywords:` block (copy of `tags`). Pass-through otherwise.
///
/// Line-based to preserve original formatting; we don't round-trip through
/// `serde_yaml` because that loses comments and reorders keys.
pub fn normalize_skill_frontmatter(doc: &FetchedDoc) -> String {
    let raw = &doc.raw;

    let Some(rest) = raw.strip_prefix("---\n") else {
        return raw.clone();
    };
    let Some(end) = rest.find("\n---\n") else {
        return raw.clone();
    };

    let fm = &rest[..end];
    let body_start = end + "\n---\n".len();
    let body = &rest[body_start..];

    if has_top_level_key(fm, "keywords") {
        return raw.clone();
    }

    let tags = match doc
        .frontmatter
        .as_ref()
        .and_then(|v| v.get("tags").or_else(|| v.get("trigger_tags")))
    {
        Some(t) => t,
        None => return raw.clone(),
    };

    let keywords_block = match render_keywords_from_tags(tags) {
        Some(s) => s,
        None => return raw.clone(),
    };

    let mut out = String::with_capacity(raw.len() + keywords_block.len() + 16);
    out.push_str("---\n");
    out.push_str(fm);
    if !fm.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&keywords_block);
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// True when `fm` (frontmatter without fences) has a top-level `<key>:` line.
fn has_top_level_key(fm: &str, key: &str) -> bool {
    let needle = format!("{}:", key);
    fm.lines()
        .any(|l| !l.starts_with(' ') && !l.starts_with('\t') && l.starts_with(&needle))
}

fn render_keywords_from_tags(tags: &serde_yaml::Value) -> Option<String> {
    let items: Vec<String> = match tags {
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_yaml::Value::String(s) => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect(),
        _ => return None,
    };

    if items.is_empty() {
        return None;
    }

    let mut out = String::from("keywords:\n");
    for item in items {
        out.push_str("  - ");
        out.push_str(&item);
        out.push('\n');
    }
    Some(out)
}

// ---------- list ----------

fn run_list(root_dir: &Path, kind: Kind) -> Result<(), String> {
    let dir = match kind {
        Kind::Skill => root_dir.join("agents").join("_skills"),
        Kind::Role => root_dir.join("agents").join("_roles"),
    };

    if !dir.exists() {
        println!("(no local {}s; {} does not exist)", kind.label(), dir.display());
        return Ok(());
    }

    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    let it = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?;
    for entry in it.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let description = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| extract_description(&raw));
        entries.push((slug, description));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    if entries.is_empty() {
        println!("(no local {}s in {})", kind.label(), dir.display());
        return Ok(());
    }

    println!("Local {}s ({}):", kind.label(), entries.len());
    for (slug, desc) in &entries {
        match desc {
            Some(d) => println!("  - {} — {}", slug, d),
            None => println!("  - {}", slug),
        }
    }
    Ok(())
}

fn extract_description(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let fm = &rest[..end];
    let value: serde_yaml::Value = serde_yaml::from_str(fm).ok()?;
    value
        .get("description")
        .or_else(|| value.get("title"))
        .and_then(|d| d.as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::akw_client::FetchedDoc;

    fn doc_from(raw: &str) -> FetchedDoc {
        let (fm, body) = crate::tools::akw_client::split_frontmatter(raw);
        FetchedDoc {
            slug: "x".to_string(),
            path: "p/x/SKILL.md".to_string(),
            frontmatter: fm,
            body,
            raw: raw.to_string(),
        }
    }

    #[test]
    fn normalize_passthrough_when_keywords_present() {
        let raw = "---\nname: foo\nkeywords:\n  - a\n  - b\n---\n\nbody\n";
        let doc = doc_from(raw);
        assert_eq!(normalize_skill_frontmatter(&doc), raw);
    }

    #[test]
    fn normalize_synthesizes_from_tags_seq() {
        let raw = "---\nname: foo\ntags:\n  - alpha\n  - beta\n---\n\nbody\n";
        let doc = doc_from(raw);
        let out = normalize_skill_frontmatter(&doc);
        assert!(out.contains("keywords:\n  - alpha\n  - beta\n"));
        assert!(out.contains("body"));
        assert!(out.starts_with("---\n"));
    }

    #[test]
    fn normalize_synthesizes_from_tags_string() {
        let raw = "---\nname: foo\ntags: alpha, beta gamma\n---\n\nbody\n";
        let doc = doc_from(raw);
        let out = normalize_skill_frontmatter(&doc);
        assert!(out.contains("keywords:\n  - alpha\n  - beta\n  - gamma\n"));
    }

    #[test]
    fn normalize_synthesizes_from_trigger_tags() {
        let raw = "---\nname: foo\ntrigger_tags: [alpha, beta]\n---\n\nbody\n";
        let doc = doc_from(raw);
        let out = normalize_skill_frontmatter(&doc);
        assert!(out.contains("keywords:\n  - alpha\n  - beta\n"));
    }

    #[test]
    fn normalize_passthrough_when_no_tags_or_keywords() {
        let raw = "---\nname: foo\n---\n\nbody\n";
        let doc = doc_from(raw);
        assert_eq!(normalize_skill_frontmatter(&doc), raw);
    }

    #[test]
    fn normalize_passthrough_when_no_frontmatter() {
        let raw = "# heading\n\nbody only\n";
        let doc = doc_from(raw);
        assert_eq!(normalize_skill_frontmatter(&doc), raw);
    }

    #[test]
    fn local_target_paths() {
        let root = Path::new("/repo");
        assert_eq!(
            local_target_path(root, Kind::Skill, "incident_commander"),
            Path::new("/repo/agents/_skills/incident_commander.md")
        );
        assert_eq!(
            local_target_path(root, Kind::Role, "sre"),
            Path::new("/repo/agents/_roles/sre.md")
        );
    }

    fn skill_doc(slug: &str, raw: &str) -> FetchedDoc {
        let (fm, body) = crate::tools::akw_client::split_frontmatter(raw);
        FetchedDoc {
            slug: slug.to_string(),
            path: format!("3_intelligences/skills/x/{}/SKILL.md", slug),
            frontmatter: fm,
            body,
            raw: raw.to_string(),
        }
    }

    #[test]
    fn write_pulled_doc_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let doc = skill_doc(
            "incident_commander",
            "---\nname: incident_commander\nkeywords:\n  - alerts\n---\n\nbody\n",
        );
        let written = write_pulled_doc(tmp.path(), Kind::Skill, &doc, false, None).unwrap();
        assert!(written.path.exists());
        assert!(written
            .path
            .ends_with("agents/_skills/incident_commander.md"));
        let on_disk = std::fs::read_to_string(&written.path).unwrap();
        assert!(on_disk.contains("name: incident_commander"));
    }

    #[test]
    fn write_pulled_doc_refuses_collision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let doc = skill_doc("foo", "---\nname: foo\nkeywords:\n  - x\n---\n\nbody\n");
        write_pulled_doc(tmp.path(), Kind::Skill, &doc, false, None).unwrap();

        let err = write_pulled_doc(tmp.path(), Kind::Skill, &doc, false, None).unwrap_err();
        assert!(err.contains("File exists"));
        assert!(err.contains("--force"));
        assert!(err.contains("--rename"));
    }

    #[test]
    fn write_pulled_doc_force_overwrites() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = skill_doc("foo", "---\nname: foo\nkeywords:\n  - a\n---\n\nfirst\n");
        write_pulled_doc(tmp.path(), Kind::Skill, &first, false, None).unwrap();

        let second = skill_doc(
            "foo",
            "---\nname: foo\nkeywords:\n  - b\n---\n\nsecond\n",
        );
        let written = write_pulled_doc(tmp.path(), Kind::Skill, &second, true, None).unwrap();
        let on_disk = std::fs::read_to_string(&written.path).unwrap();
        assert!(on_disk.contains("second"));
        assert!(!on_disk.contains("first"));
    }

    #[test]
    fn write_pulled_doc_rename_disambiguates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let doc = skill_doc("foo", "---\nname: foo\nkeywords:\n  - x\n---\n\nbody\n");
        write_pulled_doc(tmp.path(), Kind::Skill, &doc, false, None).unwrap();

        let written =
            write_pulled_doc(tmp.path(), Kind::Skill, &doc, false, Some("foo_alt")).unwrap();
        assert!(written.path.ends_with("agents/_skills/foo_alt.md"));
        assert!(written.path.exists());
    }

    #[test]
    fn write_pulled_doc_role_passthrough() {
        let tmp = tempfile::TempDir::new().unwrap();
        let raw = "# Coder\n\nYou are a software engineer.\n";
        let doc = FetchedDoc {
            slug: "coder".to_string(),
            path: "3_intelligences/agents/engineering/coder.md".to_string(),
            frontmatter: None,
            body: raw.to_string(),
            raw: raw.to_string(),
        };
        let written = write_pulled_doc(tmp.path(), Kind::Role, &doc, false, None).unwrap();
        assert!(written.path.ends_with("agents/_roles/coder.md"));
        let on_disk = std::fs::read_to_string(&written.path).unwrap();
        assert_eq!(on_disk, raw);
    }

    #[test]
    fn has_top_level_key_works() {
        let fm = "name: foo\nkeywords:\n  - x\n";
        assert!(has_top_level_key(fm, "name"));
        assert!(has_top_level_key(fm, "keywords"));
        assert!(!has_top_level_key(fm, "tags"));
        // Indented `keywords` does not count.
        let fm2 = "name: foo\nnested:\n  keywords:\n    - x\n";
        assert!(!has_top_level_key(fm2, "keywords"));
    }
}
