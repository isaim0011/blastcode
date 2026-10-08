//! High-speed multi-threaded workspace regex and token search.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize)]
pub struct GrepMatch {
    pub file: String,
    pub line: usize,
    pub content: String,
}

pub fn grep_workspace(
    root: &Path,
    pattern: &str,
    path_filter: Option<&str>,
    case_sensitive: bool,
    limit: usize,
) -> Result<String> {
    if pattern.is_empty() {
        return Err(anyhow!("pattern must not be empty"));
    }

    let re = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| anyhow!("invalid regex pattern: {e}"))?;

    let limit = limit.clamp(1, 1000);
    let match_count = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true);

    if let Some(pf) = path_filter {
        let pf = pf.trim();
        if !pf.is_empty() {
            let mut ov = ignore::overrides::OverrideBuilder::new(root);
            if ov.add(pf).is_ok() {
                if let Ok(built) = ov.build() {
                    walker.overrides(built);
                }
            }
        }
    }

    walker.build_parallel().run(|| {
        let re = re.clone();
        let match_count = Arc::clone(&match_count);
        let results = Arc::clone(&results);
        let root = root.to_path_buf();

        Box::new(move |entry| {
            if match_count.load(Ordering::Relaxed) >= limit {
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();
            // Skip large files (> 5MB)
            if let Ok(meta) = entry.metadata() {
                if meta.len() > 5 * 1024 * 1024 {
                    return ignore::WalkState::Continue;
                }
            }

            let Ok(content) = std::fs::read_to_string(path) else {
                return ignore::WalkState::Continue; // Skip binary or unreadable file
            };

            let rel_path = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            for (idx, line) in content.lines().enumerate() {
                if match_count.load(Ordering::Relaxed) >= limit {
                    return ignore::WalkState::Quit;
                }
                if re.is_match(line) {
                    let cur = match_count.fetch_add(1, Ordering::Relaxed);
                    if cur < limit {
                        let mut res = results.lock().unwrap();
                        res.push(GrepMatch {
                            file: rel_path.clone(),
                            line: idx + 1,
                            content: line.trim_end().to_string(),
                        });
                    }
                }
            }

            ignore::WalkState::Continue
        })
    });

    let mut final_results = results.lock().unwrap().clone();
    final_results.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(serde_json::to_string(&json!({
        "pattern": pattern,
        "total_matches": final_results.len(),
        "limit": limit,
        "matches": final_results
    }))?)
}
