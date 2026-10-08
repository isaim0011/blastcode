//! Git-aware repository intelligence: commit history co-change coupling.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use serde_json::{json, Value};

/// Analyze Git commit history to find files that frequently change together with `file_path`.
pub fn co_changed_files(
    root: &Path,
    file_path: &str,
    commit_depth: usize,
    limit: usize,
) -> Result<String> {
    let norm_target = file_path.replace('\\', "/").trim_start_matches("./").to_string();

    // 1. Query git log across recent commits
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("log")
        .arg("-n")
        .arg(commit_depth.to_string())
        .arg("--name-only")
        .arg("--format=commit:%H")
        .output();

    let Ok(out) = output else {
        return Ok(serde_json::to_string(&json!({
            "file": norm_target,
            "commits_analyzed": 0,
            "co_changed_files": [],
            "note": "git executable not found or not in PATH"
        }))?);
    };

    if !out.status.success() {
        return Ok(serde_json::to_string(&json!({
            "file": norm_target,
            "commits_analyzed": 0,
            "co_changed_files": [],
            "note": "not a git repository or no git history available"
        }))?);
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut current_files: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("commit:") {
            if !current_files.is_empty() {
                commits.push(current_files);
                current_files = Vec::new();
            }
        } else {
            current_files.push(trimmed.replace('\\', "/"));
        }
    }
    if !current_files.is_empty() {
        commits.push(current_files);
    }

    // 2. Filter commits that touched the target file
    let mut target_commits_count = 0;
    let mut co_occurrence: HashMap<String, usize> = HashMap::new();

    for files in &commits {
        if files.iter().any(|f| f == &norm_target || f.ends_with(&format!("/{norm_target}"))) {
            target_commits_count += 1;
            for f in files {
                if f != &norm_target && !f.ends_with(&format!("/{norm_target}")) {
                    *co_occurrence.entry(f.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    // If target not found in recent repo commits, try specific log for this file
    if target_commits_count == 0 {
        let specific = Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("log")
            .arg("-n")
            .arg(commit_depth.to_string())
            .arg("--name-only")
            .arg("--format=commit:%H")
            .arg("--")
            .arg(&norm_target)
            .output();

        if let Ok(sp_out) = specific {
            if sp_out.status.success() {
                let sp_text = String::from_utf8_lossy(&sp_out.stdout);
                let sp_hashes: Vec<String> = sp_text
                    .lines()
                    .filter(|l| l.starts_with("commit:"))
                    .map(|l| l.trim_start_matches("commit:").trim().to_string())
                    .collect();

                if !sp_hashes.is_empty() {
                    target_commits_count = sp_hashes.len();
                    // Inspect those specific commits
                    let mut cmd = Command::new("git");
                    cmd.arg("-C").arg(root).arg("show").arg("--name-only").arg("--format=commit:%H");
                    for h in sp_hashes.iter().take(25) {
                        cmd.arg(h);
                    }
                    if let Ok(show_out) = cmd.output() {
                        let show_text = String::from_utf8_lossy(&show_out.stdout);
                        let mut sh_files: Vec<String> = Vec::new();
                        for line in show_text.lines() {
                            let tr = line.trim();
                            if tr.is_empty() {
                                continue;
                            }
                            if tr.starts_with("commit:") {
                                for f in &sh_files {
                                    if f != &norm_target && !f.ends_with(&format!("/{norm_target}")) {
                                        *co_occurrence.entry(f.clone()).or_insert(0) += 1;
                                    }
                                }
                                sh_files.clear();
                            } else {
                                sh_files.push(tr.replace('\\', "/"));
                            }
                        }
                    }
                }
            }
        }
    }

    let mut scored: Vec<(String, usize, f64)> = co_occurrence
        .into_iter()
        .map(|(f, count)| {
            let pct = if target_commits_count > 0 {
                (count as f64 / target_commits_count as f64) * 100.0
            } else {
                0.0
            };
            (f, count, (pct * 10.0).round() / 10.0)
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let list: Vec<Value> = scored
        .into_iter()
        .take(limit.clamp(1, 100))
        .map(|(f, count, pct)| {
            json!({
                "file": f,
                "co_change_count": count,
                "co_change_pct": pct
            })
        })
        .collect();

    Ok(serde_json::to_string(&json!({
        "file": norm_target,
        "commits_analyzed": target_commits_count,
        "co_changed_files": list
    }))?)
}
