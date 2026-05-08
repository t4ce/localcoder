/*!
 * GlobTool — S03
 *
 * Corresponds to: src/tools/GlobTool/GlobTool.ts
 *
 * Fast file pattern matching tool. Returns matching file paths sorted by
 * modification time (newest first), capped at 100 results.
 */

use crate::tools::Tool;
use anyhow::{Result, anyhow};
use glob::Pattern;
use serde_json::{Value, json};
use std::env;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use tokio::fs;

/// Maximum number of results before truncation.
const MAX_RESULTS: usize = 100;

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Fast file pattern matching tool. Supports glob patterns like \"**/*.js\" or \"src/**/*.ts\". \
         Returns matching file paths sorted by modification time. Use this to find files by name patterns."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match files against (e.g. \"**/*.ts\", \"src/**/*.rs\")"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in. If not specified, the current working directory is used."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow!("Glob: missing required field 'pattern'"))?;

        let base_dir = match input["path"].as_str() {
            Some(p) if !p.is_empty() => {
                let path = PathBuf::from(p);
                let metadata = fs::metadata(&path)
                    .await
                    .map_err(|_| anyhow!("Glob: directory does not exist: {}", p))?;
                if !metadata.is_dir() {
                    return Err(anyhow!("Glob: path is not a directory: {}", p));
                }
                path
            }
            _ => PathBuf::from(env::current_dir()?),
        };

        // Build full glob pattern: base_dir/pattern
        let full_pattern = if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            base_dir.join(pattern)
        };
        let matcher = Pattern::new(
            full_pattern
                .to_str()
                .ok_or_else(|| anyhow!("Glob: invalid path encoding"))?,
        )
        .map_err(|e| anyhow!("Glob: invalid glob pattern: {}", e))?;
        let search_root = static_search_root(&full_pattern);

        let mut entries = collect_matching_files(search_root, &matcher).await?;

        if entries.is_empty() {
            return Ok("No files found".to_string());
        }

        // Sort by modification time (newest first)
        entries.sort_by(|a, b| {
            b.modified.cmp(&a.modified) // newest first
        });

        let truncated = entries.len() > MAX_RESULTS;
        entries.truncate(MAX_RESULTS);

        // Convert to relative paths where possible
        let cwd = env::current_dir().unwrap_or_default();
        let filenames: Vec<String> = entries
            .iter()
            .map(|entry| {
                if let Ok(rel) = entry.path.strip_prefix(&cwd) {
                    rel.to_str()
                        .unwrap_or(entry.path.to_str().unwrap_or("?"))
                        .to_string()
                } else {
                    entry.path.to_str().unwrap_or("?").to_string()
                }
            })
            .collect();

        let mut output = filenames.join("\n");
        if truncated {
            output.push_str(
                "\n(Results are truncated. Consider using a more specific path or pattern.)",
            );
        }

        Ok(output)
    }
}

struct MatchedFile {
    path: PathBuf,
    modified: Option<SystemTime>,
}

fn static_search_root(pattern: &Path) -> PathBuf {
    let mut root = PathBuf::new();

    for component in pattern.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => root.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => root.push(component.as_os_str()),
            Component::Normal(part) => {
                if has_glob_magic(part.to_string_lossy().as_ref()) {
                    break;
                }
                root.push(part);
            }
        }
    }

    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    }
}

fn has_glob_magic(component: &str) -> bool {
    component
        .bytes()
        .any(|b| matches!(b, b'*' | b'?' | b'[' | b']'))
}

async fn collect_matching_files(root: PathBuf, pattern: &Pattern) -> Result<Vec<MatchedFile>> {
    let mut out = Vec::new();
    let Ok(root_metadata) = fs::metadata(&root).await else {
        return Ok(out);
    };

    if root_metadata.is_file() {
        if pattern.matches_path(&root) {
            out.push(MatchedFile {
                path: root,
                modified: root_metadata.modified().ok(),
            });
        }
        return Ok(out);
    }

    if !root_metadata.is_dir() {
        return Ok(out);
    }

    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = fs::read_dir(&dir).await else {
            continue;
        };

        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => break,
            };
            let path = entry.path();
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };

            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() && pattern.matches_path(&path) {
                out.push(MatchedFile {
                    path,
                    modified: metadata.modified().ok(),
                });
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "{}", content).unwrap();
        path
    }

    #[tokio::test]
    async fn glob_finds_matching_files() {
        let dir = TempDir::new().unwrap();
        create_file(&dir, "a.rs", "fn a() {}");
        create_file(&dir, "b.ts", "const b = 1;");
        create_file(&dir, "c.rs", "fn c() {}");

        let path_str = dir.path().to_str().unwrap().to_string();
        let result = GlobTool
            .execute(json!({"pattern": "**/*.rs", "path": path_str}))
            .await
            .unwrap();

        assert!(result.contains("a.rs"));
        assert!(result.contains("c.rs"));
        assert!(!result.contains("b.ts"));
    }

    #[tokio::test]
    async fn glob_no_matches_returns_not_found() {
        let dir = TempDir::new().unwrap();
        create_file(&dir, "a.txt", "hello");

        let path_str = dir.path().to_str().unwrap().to_string();
        let result = GlobTool
            .execute(json!({"pattern": "**/*.py", "path": path_str}))
            .await
            .unwrap();

        assert_eq!(result, "No files found");
    }

    #[tokio::test]
    async fn glob_errors_on_nonexistent_directory() {
        let result = GlobTool
            .execute(json!({"pattern": "*.rs", "path": "/tmp/__nonexistent_localcoder_glob__"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn glob_missing_pattern_errors() {
        let result = GlobTool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn glob_truncates_at_max_results() {
        let dir = TempDir::new().unwrap();
        for i in 0..110 {
            create_file(&dir, &format!("f{:03}.txt", i), "x");
        }

        let path_str = dir.path().to_str().unwrap().to_string();
        let result = GlobTool
            .execute(json!({"pattern": "*.txt", "path": path_str}))
            .await
            .unwrap();

        assert!(result.contains("truncated"));
        let count = result.lines().filter(|l| !l.contains("truncated")).count();
        assert_eq!(count, MAX_RESULTS);
    }
}
