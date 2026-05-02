use std::collections::HashSet;
use std::path::Path;
use tracing::{debug, info, warn};

/// Core skills loaded from `config/skills/*.md` at startup.
#[derive(Debug, Clone)]
pub struct CoreSkills {
    /// Concatenated content of all core skill files.
    pub content: String,
    /// Number of skills loaded.
    pub count: usize,
    /// Estimated token count (len/4).
    pub token_estimate: u32,
}

impl CoreSkills {
    /// Load all `.md` files from the skills directory.
    pub fn load(skills_dir: &Path) -> Self {
        let mut parts = Vec::new();

        if !skills_dir.exists() {
            warn!(path = %skills_dir.display(), "skills directory not found, no core skills loaded");
            return Self {
                content: String::new(),
                count: 0,
                token_estimate: 0,
            };
        }

        let mut entries: Vec<_> = match std::fs::read_dir(skills_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map_or(false, |ext| ext == "md")
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "failed to read skills directory");
                return Self {
                    content: String::new(),
                    count: 0,
                    token_estimate: 0,
                };
            }
        };

        // Sort for deterministic order
        entries.sort_by_key(|e| e.file_name());

        for entry in &entries {
            match std::fs::read_to_string(entry.path()) {
                Ok(content) => {
                    parts.push(content.trim().to_string());
                }
                Err(e) => {
                    warn!(
                        file = %entry.path().display(),
                        error = %e,
                        "failed to read skill file"
                    );
                }
            }
        }

        let count = parts.len();
        let content = parts.join("\n\n");
        let token_estimate = (content.len() / 4) as u32;

        info!(
            count,
            token_estimate,
            "core skills loaded"
        );

        Self {
            content,
            count,
            token_estimate,
        }
    }

    /// Format core skills for injection into the system prompt.
    pub fn format_for_prompt(&self) -> String {
        if self.content.is_empty() {
            return String::new();
        }
        format!("## Core Skills\n\n{}", self.content)
    }
}

// ---------- Equipped skills (task-matched, hot-reloaded) ----------

/// One file from the local skills pool (`agents/_skills/<slug>.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquippedSkill {
    pub slug: String,
    pub keywords: Vec<String>,
    pub description: Option<String>,
    pub body: String,
    /// Estimated tokens for the body content (len/4).
    pub token_estimate: u32,
}

/// Stopwords excluded from message tokenization. Kept tiny on purpose —
/// the goal is to drop function-words that appear in every message, not
/// to do real NLP. Skill bodies / keywords are NOT stopword-filtered, so
/// a skill keyword like "for" still matches if the user actually typed "for".
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "or", "the", "to", "of", "in", "on", "at", "by", "for",
    "is", "are", "was", "were", "be", "been", "being", "do", "does", "did",
    "has", "have", "had", "i", "you", "we", "they", "it", "this", "that",
    "with", "from", "as", "but", "if", "then", "so", "not",
];

fn tokenize_message(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Read every `*.md` under `pool_dir`, parse frontmatter if present.
/// Missing dir → empty pool, no error.
pub fn load_equipped_pool(pool_dir: &Path) -> Vec<EquippedSkill> {
    if !pool_dir.exists() {
        debug!(path = %pool_dir.display(), "equipped skills pool dir not found; empty pool");
        return Vec::new();
    }

    let entries = match std::fs::read_dir(pool_dir) {
        Ok(it) => it,
        Err(e) => {
            warn!(error = %e, path = %pool_dir.display(), "failed to read equipped skills dir");
            return Vec::new();
        }
    };

    let mut pool = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "skipping skill file");
                continue;
            }
        };

        pool.push(parse_skill(&slug, &raw));
    }

    pool.sort_by(|a, b| a.slug.cmp(&b.slug));
    debug!(count = pool.len(), "equipped skills pool loaded");
    pool
}

fn parse_skill(slug: &str, raw: &str) -> EquippedSkill {
    // Frontmatter: file starts with `---\n`, body begins after the next `---\n`.
    let (frontmatter, body) = if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let fm = &rest[..end];
            let body_start = end + "\n---\n".len();
            (Some(fm), rest[body_start..].trim_start_matches('\n').to_string())
        } else {
            (None, raw.to_string())
        }
    } else {
        (None, raw.to_string())
    };

    let mut keywords: Vec<String> = Vec::new();
    let mut description: Option<String> = None;

    if let Some(fm) = frontmatter {
        match serde_yaml::from_str::<serde_yaml::Value>(fm) {
            Ok(value) => {
                if let Some(kw) = value.get("keywords") {
                    if let Some(arr) = kw.as_sequence() {
                        keywords = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                            .collect();
                    }
                }
                if let Some(desc) = value.get("description").and_then(|d| d.as_str()) {
                    description = Some(desc.to_string());
                }
            }
            Err(e) => {
                warn!(slug = %slug, error = %e, "skill frontmatter parse failed; treating whole file as body");
            }
        }
    }

    let token_estimate = (body.len() / 4) as u32;
    EquippedSkill {
        slug: slug.to_string(),
        keywords,
        description,
        body,
        token_estimate,
    }
}

/// Score a skill against a tokenized message. Hits = unique tokens that appear
/// in either the skill's keywords or its body. Bodies are tokenized lazily.
fn score_skill(skill: &EquippedSkill, message_tokens: &HashSet<String>) -> u32 {
    let mut skill_tokens: HashSet<String> = skill.keywords.iter().cloned().collect();
    for word in skill.body.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        skill_tokens.insert(word.to_lowercase());
    }
    message_tokens.intersection(&skill_tokens).count() as u32
}

/// Pick skills relevant to `message` from `pool`. Greedy by score, file granularity.
///
/// - `min_hits`: drop skills with fewer than this many distinct token matches.
/// - `token_budget`: stop adding skills once cumulative `token_estimate` would exceed this.
///
/// Ties broken by slug (asc) for determinism. Returns owned clones so the caller
/// can move them into the system prompt without holding a borrow on the pool.
pub fn select_equipped_skills(
    pool: &[EquippedSkill],
    message: &str,
    min_hits: u32,
    token_budget: u32,
) -> Vec<EquippedSkill> {
    let message_tokens = tokenize_message(message);
    if message_tokens.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(u32, &EquippedSkill)> = pool
        .iter()
        .map(|s| (score_skill(s, &message_tokens), s))
        .filter(|(hits, _)| *hits >= min_hits)
        .collect();

    // Sort by hits desc, then slug asc — stable ordering for tie-breaks.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));

    let mut chosen: Vec<EquippedSkill> = Vec::new();
    let mut used: u32 = 0;
    for (_, skill) in scored {
        if used + skill.token_estimate > token_budget {
            continue; // skip this one but keep trying smaller ones below
        }
        used += skill.token_estimate;
        chosen.push(skill.clone());
    }
    chosen
}

/// Format the chosen skills as a system-prompt section. Empty input → empty string.
pub fn format_equipped_skills(skills: &[EquippedSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let bodies: Vec<String> = skills.iter().map(|s| s.body.trim().to_string()).collect();
    format!("## Equipped Skills\n\n{}", bodies.join("\n\n---\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_core_skills() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        std::fs::write(skills_dir.join("alpha.md"), "# Alpha\nAlpha content").unwrap();
        std::fs::write(skills_dir.join("beta.md"), "# Beta\nBeta content").unwrap();

        let skills = CoreSkills::load(&skills_dir);
        assert_eq!(skills.count, 2);
        assert!(skills.content.contains("Alpha content"));
        assert!(skills.content.contains("Beta content"));
        assert!(skills.token_estimate > 0);
    }

    #[test]
    fn test_load_sorted_order() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        std::fs::write(skills_dir.join("z_last.md"), "LAST").unwrap();
        std::fs::write(skills_dir.join("a_first.md"), "FIRST").unwrap();

        let skills = CoreSkills::load(&skills_dir);
        let first_pos = skills.content.find("FIRST").unwrap();
        let last_pos = skills.content.find("LAST").unwrap();
        assert!(first_pos < last_pos);
    }

    #[test]
    fn test_load_ignores_non_md() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        std::fs::write(skills_dir.join("skill.md"), "good").unwrap();
        std::fs::write(skills_dir.join("notes.txt"), "ignored").unwrap();
        std::fs::write(skills_dir.join("data.json"), "ignored").unwrap();

        let skills = CoreSkills::load(&skills_dir);
        assert_eq!(skills.count, 1);
        assert!(skills.content.contains("good"));
        assert!(!skills.content.contains("ignored"));
    }

    #[test]
    fn test_load_missing_directory() {
        let skills = CoreSkills::load(Path::new("/nonexistent/skills"));
        assert_eq!(skills.count, 0);
        assert!(skills.content.is_empty());
    }

    #[test]
    fn test_format_for_prompt() {
        let skills = CoreSkills {
            content: "Some skill content".to_string(),
            count: 1,
            token_estimate: 5,
        };
        let formatted = skills.format_for_prompt();
        assert!(formatted.starts_with("## Core Skills"));
        assert!(formatted.contains("Some skill content"));
    }

    #[test]
    fn test_format_for_prompt_empty() {
        let skills = CoreSkills {
            content: String::new(),
            count: 0,
            token_estimate: 0,
        };
        assert!(skills.format_for_prompt().is_empty());
    }

    #[test]
    fn test_token_estimate() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // 400 chars → ~100 tokens
        let content = "a".repeat(400);
        std::fs::write(skills_dir.join("test.md"), &content).unwrap();

        let skills = CoreSkills::load(&skills_dir);
        assert_eq!(skills.token_estimate, 100);
    }

    // ---------- equipped skills tests ----------

    fn write_skill(dir: &Path, slug: &str, content: &str) {
        std::fs::write(dir.join(format!("{}.md", slug)), content).unwrap();
    }

    #[test]
    fn test_load_equipped_pool_missing_dir() {
        let pool = load_equipped_pool(Path::new("/nonexistent/_skills"));
        assert!(pool.is_empty());
    }

    #[test]
    fn test_parse_skill_with_frontmatter() {
        let raw = "---\nname: research_pipeline\nkeywords: [research, market, macro]\ndescription: Research workflow\n---\n\n# Research Pipeline\nBody here.";
        let skill = parse_skill("research_pipeline", raw);
        assert_eq!(skill.slug, "research_pipeline");
        assert_eq!(skill.keywords, vec!["research", "market", "macro"]);
        assert_eq!(skill.description.as_deref(), Some("Research workflow"));
        assert!(skill.body.starts_with("# Research Pipeline"));
        assert!(!skill.body.contains("---"));
    }

    #[test]
    fn test_parse_skill_no_frontmatter() {
        let raw = "# Bare\nNo frontmatter here.";
        let skill = parse_skill("bare", raw);
        assert_eq!(skill.slug, "bare");
        assert!(skill.keywords.is_empty());
        assert_eq!(skill.body, raw);
    }

    #[test]
    fn test_parse_skill_malformed_frontmatter_treats_as_body() {
        // "---" but no closing fence → treat whole file as body.
        let raw = "---\nname: broken\nthis never closes";
        let skill = parse_skill("broken", raw);
        assert_eq!(skill.body, raw);
        assert!(skill.keywords.is_empty());
    }

    #[test]
    fn test_select_filters_by_min_hits() {
        let pool = vec![
            EquippedSkill {
                slug: "research".into(),
                keywords: vec!["research".into(), "market".into(), "macro".into()],
                description: None,
                body: "Research body".into(),
                token_estimate: 3,
            },
            EquippedSkill {
                slug: "unrelated".into(),
                keywords: vec!["zzz".into()],
                description: None,
                body: "Nothing matches".into(),
                token_estimate: 3,
            },
        ];
        let picked = select_equipped_skills(&pool, "research the crypto market and macro economy", 2, 4000);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "research");
    }

    #[test]
    fn test_select_respects_token_budget() {
        // Two skills both eligible by hits, but only the first fits the budget.
        let pool = vec![
            EquippedSkill {
                slug: "alpha".into(),
                keywords: vec!["foo".into(), "bar".into()],
                description: None,
                body: "x".repeat(400), // ~100 tokens
                token_estimate: 100,
            },
            EquippedSkill {
                slug: "beta".into(),
                keywords: vec!["foo".into(), "bar".into()],
                description: None,
                body: "y".repeat(400),
                token_estimate: 100,
            },
        ];
        let picked = select_equipped_skills(&pool, "foo bar baz", 2, 100);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "alpha");
    }

    #[test]
    fn test_select_tie_breaks_by_slug() {
        let pool = vec![
            EquippedSkill {
                slug: "zeta".into(),
                keywords: vec!["foo".into(), "bar".into()],
                description: None,
                body: "z".into(),
                token_estimate: 1,
            },
            EquippedSkill {
                slug: "alpha".into(),
                keywords: vec!["foo".into(), "bar".into()],
                description: None,
                body: "a".into(),
                token_estimate: 1,
            },
        ];
        let picked = select_equipped_skills(&pool, "foo bar", 2, 4000);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].slug, "alpha"); // tie broken by slug asc
        assert_eq!(picked[1].slug, "zeta");
    }

    #[test]
    fn test_select_body_words_count_as_keywords() {
        // No frontmatter keywords — body terms should still match.
        let pool = vec![EquippedSkill {
            slug: "body_only".into(),
            keywords: Vec::new(),
            description: None,
            body: "When debugging stack traces look at the panic message".into(),
            token_estimate: 12,
        }];
        let picked = select_equipped_skills(&pool, "I'm debugging a panic in stack trace", 2, 4000);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "body_only");
    }

    #[test]
    fn test_select_empty_message_returns_nothing() {
        let pool = vec![EquippedSkill {
            slug: "x".into(),
            keywords: vec!["foo".into()],
            description: None,
            body: "".into(),
            token_estimate: 0,
        }];
        let picked = select_equipped_skills(&pool, "", 1, 4000);
        assert!(picked.is_empty());
    }

    #[test]
    fn test_load_equipped_pool_end_to_end() {
        let dir = TempDir::new().unwrap();
        let pool_dir = dir.path().join("_skills");
        std::fs::create_dir_all(&pool_dir).unwrap();

        write_skill(
            &pool_dir,
            "research_pipeline",
            "---\nname: research_pipeline\nkeywords: [research, market, macro]\n---\n\n# Research\nbody",
        );
        write_skill(&pool_dir, "code_review", "---\nname: code_review\nkeywords: [review, diff, pull request]\n---\n\nReview body");
        write_skill(&pool_dir, "notes", "no frontmatter, just a body about debugging");

        let pool = load_equipped_pool(&pool_dir);
        assert_eq!(pool.len(), 3);
        // Sorted by slug
        assert_eq!(pool[0].slug, "code_review");
        assert_eq!(pool[1].slug, "notes");
        assert_eq!(pool[2].slug, "research_pipeline");

        let picked = select_equipped_skills(&pool, "research the crypto market and macro economy", 2, 4000);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "research_pipeline");
    }

    #[test]
    fn test_format_equipped_skills() {
        let skills = vec![
            EquippedSkill {
                slug: "a".into(),
                keywords: Vec::new(),
                description: None,
                body: "First skill body.".into(),
                token_estimate: 5,
            },
            EquippedSkill {
                slug: "b".into(),
                keywords: Vec::new(),
                description: None,
                body: "Second skill body.".into(),
                token_estimate: 5,
            },
        ];
        let formatted = format_equipped_skills(&skills);
        assert!(formatted.starts_with("## Equipped Skills"));
        assert!(formatted.contains("First skill body."));
        assert!(formatted.contains("Second skill body."));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_format_equipped_skills_empty() {
        assert!(format_equipped_skills(&[]).is_empty());
    }
}
