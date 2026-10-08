//! Blast-radius analysis.
//!
//! Two modes:
//! * `new_source` given: compare that proposed content against the file on disk
//!   (pre-edit "what if").
//! * otherwise: compare the working-tree file against `git HEAD` (post-edit check).
//!
//! Severity is deliberately conservative. `breaking` is only reported when the
//! breakage is provable: the symbol was removed, or a call site resolved with
//! `exact` confidence passes an argument count the new signature cannot accept.
//! Everything else that may be affected is `review`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::extract;
use crate::lang::Lang;
use crate::model::*;
use crate::resolve::{RefHit, Resolver, SymRow};

const MAX_ITEMS_PER_CHANGE: usize = 25;

fn git_head(root: &Path, rel: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("show")
        .arg(format!("HEAD:./{rel}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn target_row(rel: &str, o: &SymbolRec, n: i64) -> SymRow {
    SymRow {
        id: -(n + 1),
        file: rel.to_string(),
        name: o.name.clone(),
        qualname: o.qualname.clone(),
        kind: o.kind.clone(),
        signature: o.signature.clone(),
        doc: o.doc.clone(),
        start_line: o.start_line,
        end_line: o.end_line,
        depth: o.depth,
        parent_id: None,
        exported: o.exported,
        params: o.params.clone(),
        has_self: o.has_self,
    }
}

fn sig_differs(o: &SymbolRec, n: &SymbolRec) -> bool {
    collapse(&o.signature) != collapse(&n.signature) || o.params != n.params || o.has_self != n.has_self
}

fn arity_violation(new: &SymbolRec, hit: &RefHit) -> Option<String> {
    let params = new.params.as_ref()?;
    if let Some(kw) = &hit.kwargs {
        if !params.iter().any(|p| p.name.starts_with("**")) {
            for k in kw.split(',') {
                if !params.iter().any(|p| p.name == k) {
                    return Some(format!(
                        "call uses keyword argument '{k}' which the new signature no longer has"
                    ));
                }
            }
        }
    }
    let n = hit.arg_count? as usize;
    let (req, max) = arity(params);
    let adj = if new.has_self && hit.kind == "path_call" { 1 } else { 0 };
    if n < req + adj {
        return Some(format!(
            "call passes {n} argument(s) but the new signature requires at least {}",
            req + adj
        ));
    }
    if let Some(m) = max {
        if n > m + adj {
            return Some(format!(
                "call passes {n} argument(s) but the new signature accepts at most {}",
                m + adj
            ));
        }
    }
    None
}

pub fn impact_report(
    conn: &Connection,
    root: &Path,
    rel: &str,
    new_source: Option<&str>,
) -> Result<String> {
    let lang = Lang::from_path(rel).ok_or_else(|| anyhow!("unsupported file type: {rel}"))?;
    let disk = std::fs::read_to_string(root.join(rel)).ok();
    let (old_text, new_text, mode) = match new_source {
        Some(ns) => (disk.clone(), ns.to_string(), "proposed_vs_disk"),
        None => (git_head(root, rel), disk.clone().unwrap_or_default(), "working_tree_vs_git_head"),
    };
    let Some(old_text) = old_text else {
        return Ok(json!({
            "file": rel,
            "mode": mode,
            "summary": { "breaking": 0, "review": 0, "changed_symbols": 0 },
            "note": "No baseline to compare against: the file is new/untracked or this is not a git repository. \
                     Pass `new_source` to compare a proposed edit against what is on disk."
        })
        .to_string());
    };

    let old_p = extract::parse(lang, old_text.as_bytes())?;
    let new_p = extract::parse(lang, new_text.as_bytes())?;

    // In overloadable languages (Java, C#, C/C++) methods sharing a name are distinct
    // symbols, so identity includes the parameter count.
    let overl = lang.overloadable();
    let key_of = |s: &SymbolRec| -> String {
        if overl {
            format!("{}#{}", s.qualname, s.params.as_ref().map_or(0, |p| p.len()))
        } else {
            s.qualname.clone()
        }
    };
    let mut new_by: HashMap<String, &SymbolRec> = HashMap::new();
    for s in new_p.symbols.iter().filter(|s| s.kind != "impl") {
        new_by.entry(key_of(s)).or_insert(s);
    }
    let old_keys: HashSet<String> = old_p.symbols.iter().map(|s| key_of(s)).collect();
    let added: Vec<&SymbolRec> = new_p
        .symbols
        .iter()
        .filter(|s| s.kind != "impl" && !old_keys.contains(&key_of(s)))
        .collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut changes: Vec<(&SymbolRec, Option<&SymbolRec>)> = Vec::new();
    for o in old_p.symbols.iter().filter(|s| s.kind != "impl") {
        let k = key_of(o);
        if !seen.insert(k.clone()) {
            continue;
        }
        match new_by.get(&k) {
            None => changes.push((o, None)),
            Some(n) if sig_differs(o, n) => changes.push((o, Some(*n))),
            Some(_) => {}
        }
    }

    let overlay: Vec<SymRow> = changes
        .iter()
        .enumerate()
        .map(|(i, (o, _))| target_row(rel, o, i as i64))
        .collect();
    let resolver = Resolver::new(conn, overlay.clone())?;

    let (mut total_breaking, mut total_review) = (0usize, 0usize);
    let mut change_json: Vec<Value> = Vec::new();
    let mut alerts: Vec<String> = Vec::new();

    for (i, (old, new)) in changes.iter().enumerate() {
        let target = &overlay[i];
        let callable = old.params.is_some();
        let (hits, truncated) = if new.is_none() || callable {
            resolver.callers(target)?
        } else {
            (Vec::new(), false)
        };

        let mut items: Vec<Value> = Vec::new();
        let (mut breaking, mut review) = (0usize, 0usize);
        let mut breaking_sites: Vec<String> = Vec::new();
        for h in &hits {
            let (severity, reason): (&str, String) = match new {
                None => {
                    if h.confidence == Confidence::Exact {
                        ("breaking", "symbol was removed".to_string())
                    } else {
                        ("review", "symbol was removed; match is not certain".to_string())
                    }
                }
                Some(n) => {
                    if h.usage != "call" {
                        continue;
                    }
                    match arity_violation(n, h) {
                        Some(r) if h.confidence == Confidence::Exact => ("breaking", r),
                        Some(r) => ("review", format!("{r} (match confidence: {})", h.confidence.as_str())),
                        None => ("review", "signature changed; verify argument order and types".to_string()),
                    }
                }
            };
            if severity == "breaking" {
                breaking += 1;
                if breaking_sites.len() < 8 {
                    breaking_sites.push(format!("{}:{}", h.file, h.line));
                }
            } else {
                review += 1;
            }
            if items.len() < MAX_ITEMS_PER_CHANGE {
                items.push(json!({
                    "file": h.file, "line": h.line, "usage": h.usage,
                    "in": h.enclosing, "severity": severity,
                    "confidence": h.confidence.as_str(), "reason": reason
                }));
            }
        }
        total_breaking += breaking;
        total_review += review;

        let verb = if new.is_none() { "removed" } else { "changed signature" };
        if breaking > 0 {
            alerts.push(format!(
                "You {verb} `{}` in {rel}. This breaks {}{}.",
                old.qualname,
                breaking_sites.join(", "),
                if breaking > breaking_sites.len() { " and more" } else { "" }
            ));
        }

        let mut obj = json!({
            "symbol": old.qualname,
            "kind": old.kind,
            "change": if new.is_none() { "removed" } else { "signature_changed" },
            "old_signature": old.signature,
            "impacted_total": breaking + review,
            "impacted": items,
            "candidates_truncated": truncated
        });
        if let Some(n) = new {
            obj["new_signature"] = json!(n.signature);
        } else {
            let same_shape: Vec<&&SymbolRec> = added
                .iter()
                .filter(|a| a.kind == old.kind && a.params == old.params && a.has_self == old.has_self)
                .collect();
            if same_shape.len() == 1 {
                obj["possibly_renamed_to"] = json!(same_shape[0].qualname);
            }
        }
        change_json.push(obj);
    }

    let alert = if !alerts.is_empty() {
        format!("{} Update those call sites next.", alerts.join(" "))
    } else if total_review > 0 {
        format!("No provable breakage, but {total_review} call site(s) should be reviewed.")
    } else if changes.is_empty() {
        "No public-surface changes detected (no symbol added/removed/re-signatured).".to_string()
    } else {
        "Symbols changed but no dependents were found in the index.".to_string()
    };

    Ok(serde_json::to_string(&json!({
        "file": rel,
        "mode": mode,
        "summary": {
            "breaking": total_breaking,
            "review": total_review,
            "changed_symbols": changes.len(),
            "added_symbols": added.len()
        },
        "agent_alert": alert,
        "changes": change_json,
        "legend": "breaking = provable (removed symbol or exact-resolution arity mismatch); review = may be affected, verify manually"
    }))?)
}
