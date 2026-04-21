use std::path::Path;
use tracing::{info, warn};

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
}
