//! Read-only queries backing the agent-facing tools.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::model::Confidence;
use crate::resolve::{sym_from_row, Resolver, SymRow, SYM_COLS, SYM_COLS_S};

pub fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub fn def_json(s: &SymRow) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(s.qualname));
    m.insert("kind".into(), json!(s.kind));
    m.insert("file".into(), json!(s.file));
    m.insert("line".into(), json!(s.start_line));
    m.insert("end_line".into(), json!(s.end_line));
    m.insert("signature".into(), json!(s.signature));
    if let Some(d) = &s.doc {
        m.insert("doc".into(), json!(d));
    }
    Value::Object(m)
}

// ---------------------------------------------------------------- skeleton

pub fn skeleton(conn: &Connection, file: &str, max_depth: Option<u32>) -> Result<String> {
    let info: Option<(String, i64)> = conn
        .query_row("SELECT lang,lines FROM files WHERE path=?1", [file], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((lang, lines)) = info else {
        let base = file.rsplit('/').next().unwrap_or(file);
        let mut st = conn.prepare("SELECT path FROM files WHERE path LIKE ?1 ESCAPE '\\' LIMIT 5")?;
        let like = format!("%{}", like_escape(base));
        let similar: Vec<String> = st
            .query_map([like], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        if similar.is_empty() {
            bail!("file not indexed: {file} (unsupported language, ignored by .gitignore, or too large)");
        }
        bail!("file not indexed: {file}. Did you mean: {}", similar.join(", "));
    };
    let mut st = conn.prepare(
        "SELECT start_line,depth,signature,doc FROM symbols WHERE file=?1 ORDER BY id",
    )?;
    let rows: Vec<(u32, u32, String, Option<String>)> = st
        .query_map([file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let total = rows.len();
    let mut out = format!("# {file} · {lang} · {lines} lines · {total} symbols\n");
    for (line, depth, sig, doc) in rows {
        if let Some(m) = max_depth {
            if depth > m {
                continue;
            }
        }
        let indent = "  ".repeat(depth as usize);
        let mut first = true;
        for l in sig.lines() {
            if first {
                out.push_str(&format!("{line:>5}| {indent}{l}"));
                first = false;
            } else {
                out.push_str(&format!("\n     | {indent}{l}"));
            }
        }
        if let Some(d) = doc {
            out.push_str(&format!("  — {d}"));
        }
        out.push('\n');
    }
    Ok(out)
}

// ------------------------------------------------------------ workspace map

pub fn workspace_map(
    conn: &Connection,
    path: Option<&str>,
    depth: usize,
    max_chars: usize,
) -> Result<String> {
    let prefix = path
        .map(|p| p.trim_matches('/').to_string())
        .filter(|p| !p.is_empty());
    let total_files: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let total_syms: i64 = conn.query_row("SELECT count(*) FROM symbols", [], |r| r.get(0))?;

    let mut st = conn.prepare(
        "SELECT f.path, f.lang, s.name FROM files f
         LEFT JOIN symbols s ON s.file=f.path AND s.depth=0 AND s.exported=1 AND s.kind<>'impl'
         ORDER BY f.path, s.start_line",
    )?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
    })?;
    let mut files: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for r in rows {
        let (p, lang, name) = r?;
        if let Some(pre) = &prefix {
            if p != *pre && !p.starts_with(&format!("{pre}/")) {
                continue;
            }
        }
        let e = files.entry(p).or_insert((lang, Vec::new()));
        if let Some(n) = name {
            if !e.1.contains(&n) {
                e.1.push(n);
            }
        }
    }

    let base_depth = prefix.as_ref().map_or(0, |p| p.split('/').count());
    let mut deep_counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in files.keys() {
        let comps: Vec<&str> = p.split('/').collect();
        let dir_comps = comps.len() - 1;
        if dir_comps > base_depth + depth {
            let key = comps[..base_depth + depth].join("/");
            *deep_counts.entry(key).or_insert(0) += 1;
        }
    }

    let mut out = format!(
        "# workspace map · {total_files} files · {total_syms} symbols{}\n",
        prefix.as_ref().map_or(String::new(), |p| format!(" · under {p}/"))
    );
    let mut printed: Vec<String> = Vec::new();
    let mut deep_done: BTreeMap<String, bool> = BTreeMap::new();
    let mut truncated = false;
    for (p, (lang, names)) in &files {
        let comps: Vec<&str> = p.split('/').collect();
        let dir: Vec<String> = comps[..comps.len() - 1].iter().map(|s| s.to_string()).collect();
        let limit = base_depth + depth;
        let shown_dir: Vec<String> = dir.iter().take(limit).cloned().collect();
        let mut common = 0;
        while common < printed.len() && common < shown_dir.len() && printed[common] == shown_dir[common] {
            common += 1;
        }
        let mut chunk = String::new();
        for (i, d) in shown_dir.iter().enumerate().skip(common) {
            chunk.push_str(&format!("{}{}/\n", "  ".repeat(i), d));
        }
        printed = shown_dir.clone();
        if dir.len() > limit {
            let key = shown_dir.join("/");
            if !deep_done.contains_key(&key) {
                deep_done.insert(key.clone(), true);
                let n = deep_counts.get(&key).copied().unwrap_or(0);
                chunk.push_str(&format!("{}… {n} files in deeper directories\n", "  ".repeat(shown_dir.len())));
            }
        } else {
            let fname = comps[comps.len() - 1];
            let mut shown: Vec<&str> = names.iter().take(8).map(|s| s.as_str()).collect();
            let more = names.len().saturating_sub(8);
            let tail = if more > 0 { format!(", +{more}") } else { String::new() };
            if shown.is_empty() {
                shown.push("");
            }
            chunk.push_str(&format!(
                "{}{} [{}] {}{}\n",
                "  ".repeat(shown_dir.len()),
                fname,
                short_lang(lang),
                shown.join(", "),
                tail
            ));
        }
        if out.len() + chunk.len() > max_chars {
            truncated = true;
            break;
        }
        out.push_str(&chunk);
    }
    if truncated {
        out.push_str("… map truncated; pass `path` to zoom into a subtree or lower `depth`\n");
    }
    Ok(out)
}

fn short_lang(l: &str) -> &str {
    match l {
        "python" => "py",
        "typescript" => "ts",
        "javascript" => "js",
        "rust" => "rs",
        other => other,
    }
}

// ------------------------------------------------------------------- search

pub fn search_symbols(conn: &Connection, query: &str, kind: Option<&str>, limit: usize) -> Result<String> {
    let q = query.trim().to_lowercase().replace("::", ".");
    if q.is_empty() {
        bail!("query must not be empty");
    }
    let tokens: Vec<String> = q
        .split(|c: char| c.is_whitespace() || c == '_' || c == '.' || c == '-')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    let mut sql = format!("SELECT {SYM_COLS} FROM symbols WHERE kind<>'impl'");
    let mut args: Vec<String> = Vec::new();
    for t in &tokens {
        args.push(format!("%{}%", like_escape(t)));
        sql.push_str(&format!(" AND lower(qualname) LIKE ?{} ESCAPE '\\'", args.len()));
    }
    if let Some(k) = kind {
        args.push(k.to_string());
        sql.push_str(&format!(" AND kind=?{}", args.len()));
    }
    sql.push_str(" LIMIT 500");
    let mut st = conn.prepare(&sql)?;
    let rows: Vec<SymRow> = st
        .query_map(params_from_iter(args.iter()), sym_from_row)?
        .collect::<rusqlite::Result<_>>()?;

    let compact: String = q.chars().filter(|c| c.is_alphanumeric()).collect();
    let mut scored: Vec<(i32, SymRow)> = rows
        .into_iter()
        .map(|s| {
            let n = s.name.to_lowercase();
            let nc: String = n.chars().filter(|c| c.is_alphanumeric()).collect();
            let mut score = if n == q || nc == compact {
                100
            } else if nc.starts_with(&compact) {
                80
            } else if nc.contains(&compact) {
                60
            } else {
                40
            };
            if s.exported {
                score += 5;
            }
            score -= (s.depth as i32).min(5);
            (score, s)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.file.cmp(&b.1.file)).then(a.1.start_line.cmp(&b.1.start_line)));
    let total = scored.len();
    let results: Vec<Value> = scored.into_iter().take(limit).map(|(_, s)| def_json(&s)).collect();
    Ok(serde_json::to_string(&json!({ "total_matches": total, "results": results }))?)
}

// ---------------------------------------------------------------- query_graph

#[derive(Default)]
pub struct GraphFilter<'a> {
    pub kind: Option<&'a str>,
    pub name: Option<&'a str>,
    pub path_prefix: Option<&'a str>,
    pub calls: Option<&'a str>,
    pub called_by: Option<&'a str>,
    pub exported: Option<bool>,
    pub limit: usize,
}

/// Name-level structural query. `calls` / `called_by` match call-site names, not
/// resolved targets; use `trace_symbol` when exact resolution matters.
pub fn query_graph(conn: &Connection, f: &GraphFilter<'_>) -> Result<String> {
    let mut sql = format!("SELECT DISTINCT {SYM_COLS_S} FROM symbols s WHERE s.kind<>'impl'");
    let mut args: Vec<String> = Vec::new();
    if let Some(k) = f.kind {
        args.push(k.to_string());
        sql.push_str(&format!(" AND s.kind=?{}", args.len()));
    }
    if let Some(n) = f.name {
        args.push(format!("%{}%", like_escape(&n.to_lowercase())));
        sql.push_str(&format!(" AND lower(s.qualname) LIKE ?{} ESCAPE '\\'", args.len()));
    }
    if let Some(p) = f.path_prefix {
        args.push(format!("{}%", like_escape(p.trim_start_matches("./"))));
        sql.push_str(&format!(" AND s.file LIKE ?{} ESCAPE '\\'", args.len()));
    }
    if let Some(c) = f.calls {
        args.push(c.to_string());
        sql.push_str(&format!(
            " AND s.id IN (SELECT r.enclosing_id FROM refs r WHERE r.name=?{} AND r.enclosing_id IS NOT NULL)",
            args.len()
        ));
    }
    if let Some(c) = f.called_by {
        args.push(c.to_string());
        sql.push_str(&format!(
            " AND s.name IN (SELECT r.name FROM refs r JOIN symbols c ON c.id=r.enclosing_id WHERE c.name=?{})",
            args.len()
        ));
    }
    if let Some(e) = f.exported {
        sql.push_str(if e { " AND s.exported=1" } else { " AND s.exported=0" });
    }
    let limit = f.limit.clamp(1, 200);
    sql.push_str(&format!(" ORDER BY s.file, s.start_line LIMIT {limit}"));
    let mut st = conn.prepare(&sql)?;
    let rows: Vec<SymRow> = st
        .query_map(params_from_iter(args.iter()), sym_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let results: Vec<Value> = rows.iter().map(def_json).collect();
    Ok(serde_json::to_string(&json!({
        "count": results.len(),
        "note": "name-level match; use trace_symbol for resolved, confidence-tagged edges",
        "results": results
    }))?)
}

// ------------------------------------------------------------------- trace

pub fn find_defs(conn: &Connection, name: &str, file: Option<&str>) -> Result<Vec<SymRow>> {
    let clean = name.trim();
    let norm = clean.replace("::", ".");
    let leaf = clean
        .rsplit("::")
        .next()
        .unwrap_or(clean)
        .rsplit('.')
        .next()
        .unwrap_or(clean);

    let mut sql = format!(
        "SELECT {SYM_COLS} FROM symbols
         WHERE (name=?1 OR qualname=?1 OR qualname=?2 OR name=?3 OR qualname LIKE ?4 ESCAPE '\\') AND kind<>'impl'"
    );
    let mut args: Vec<String> = vec![
        clean.to_string(),
        norm.clone(),
        leaf.to_string(),
        format!("%.{}", like_escape(leaf)),
    ];
    if let Some(f) = file {
        args.push(format!("%{}", like_escape(f.trim_start_matches("./"))));
        sql.push_str(&format!(" AND file LIKE ?{} ESCAPE '\\'", args.len()));
    }
    sql.push_str(" ORDER BY exported DESC, file, start_line LIMIT 50");
    let mut st = conn.prepare(&sql)?;
    let mut rows: Vec<SymRow> = st
        .query_map(params_from_iter(args.iter()), sym_from_row)?
        .collect::<rusqlite::Result<_>>()?;

    // If exact lookup yields no matches, do a resilient substring fallback:
    if rows.is_empty() {
        let mut fallback_sql = format!(
            "SELECT {SYM_COLS} FROM symbols
             WHERE (lower(name) LIKE ?1 ESCAPE '\\' OR lower(qualname) LIKE ?1 ESCAPE '\\') AND kind<>'impl'"
        );
        let mut fb_args: Vec<String> = vec![format!("%{}%", like_escape(&leaf.to_lowercase()))];
        if let Some(f) = file {
            fb_args.push(format!("%{}", like_escape(f.trim_start_matches("./"))));
            fallback_sql.push_str(&format!(" AND file LIKE ?{} ESCAPE '\\'", fb_args.len()));
        }
        fallback_sql.push_str(" ORDER BY exported DESC, file, start_line LIMIT 20");
        let mut fb_st = conn.prepare(&fallback_sql)?;
        rows = fb_st
            .query_map(params_from_iter(fb_args.iter()), sym_from_row)?
            .collect::<rusqlite::Result<_>>()?;
    }

    Ok(rows)
}

pub fn trace_symbol(
    conn: &Connection,
    name: &str,
    file: Option<&str>,
    direction: &str,
    limit: usize,
) -> Result<String> {
    let defs = find_defs(conn, name, file)?;
    if defs.is_empty() {
        bail!("no symbol named '{name}' found; use search_symbols for fuzzy lookup");
    }
    let resolver = Resolver::new(conn, vec![])?;
    let limit = limit.clamp(1, 500);
    let mut matches = Vec::new();
    for d in defs.iter().take(3) {
        let mut obj = Map::new();
        obj.insert("definition".into(), def_json(d));
        if direction != "callees" {
            let (hits, truncated) = resolver.callers(d)?;
            let (mut ex, mut pr, mut he) = (0, 0, 0);
            for h in &hits {
                match h.confidence {
                    Confidence::Exact => ex += 1,
                    Confidence::Probable => pr += 1,
                    Confidence::Heuristic => he += 1,
                }
            }
            let list: Vec<Value> = hits
                .iter()
                .take(limit)
                .map(|h| {
                    json!({
                        "file": h.file, "line": h.line, "usage": h.usage,
                        "in": h.enclosing, "confidence": h.confidence.as_str()
                    })
                })
                .collect();
            obj.insert(
                "callers_summary".into(),
                json!({ "total": hits.len(), "exact": ex, "probable": pr, "heuristic": he,
                        "candidates_truncated": truncated }),
            );
            obj.insert("callers".into(), Value::Array(list));
        }
        if direction != "callers" {
            let callees = resolver.callees(d)?;
            let list: Vec<Value> = callees
                .iter()
                .take(limit)
                .map(|c| {
                    json!({
                        "name": c.name, "line": c.line,
                        "targets": c.targets.iter().map(|(s, conf)| json!({
                            "symbol": s.qualname, "file": s.file, "line": s.start_line,
                            "confidence": conf.as_str()
                        })).collect::<Vec<_>>()
                    })
                })
                .collect();
            obj.insert("callees".into(), Value::Array(list));
        }
        matches.push(Value::Object(obj));
    }
    let mut res = Map::new();
    res.insert("matches".into(), Value::Array(matches));
    if defs.len() > 3 {
        res.insert(
            "other_matches".into(),
            Value::Array(defs[3..].iter().map(def_json).collect()),
        );
        res.insert(
            "hint".into(),
            json!("several definitions match; pass file_path to focus on one"),
        );
    }
    Ok(serde_json::to_string(&Value::Object(res)).map_err(|e| anyhow!(e))?)
}

// =====================================================================
// Read-less tools: file context, symbol source, change journal
// =====================================================================

use std::collections::HashSet;
use std::path::Path;

use crate::resolve::ImportRow;
use crate::store::{events_since, EventRow};

const MAX_SOURCE_LINES: usize = 400;

pub fn event_json(e: &EventRow) -> Value {
    json!({
        "seq": e.seq,
        "ts": e.ts,
        "file": e.file,
        "kind": e.kind,
        "added": e.added,
        "removed": e.removed,
        "signature_changed": e.changed,
    })
}

/// Raw journal. With `since` = None returns the most recent `limit` events.
pub fn workspace_changes(
    conn: &Connection,
    since: Option<i64>,
    file: Option<&str>,
    limit: usize,
) -> Result<String> {
    let limit = limit.clamp(1, 200);
    let mut events = match since {
        Some(s) => events_since(conn, s, file, limit, false)?,
        None => {
            let mut v = events_since(conn, 0, file, limit, true)?;
            v.reverse();
            v
        }
    };
    events.truncate(limit);
    let latest = crate::store::max_seq(conn)?;
    Ok(serde_json::to_string(&json!({
        "latest_seq": latest,
        "events": events.iter().map(event_json).collect::<Vec<_>>(),
        "hint": "pass since=<latest_seq> next time to see only newer changes"
    }))?)
}

/// Exact source lines of one symbol (not the whole file).
pub fn symbol_source(
    conn: &Connection,
    root: &Path,
    name: &str,
    file: Option<&str>,
    context: usize,
) -> Result<String> {
    let defs = find_defs(conn, name, file)?;
    if defs.is_empty() {
        bail!("no symbol named '{name}' found; use search_symbols for fuzzy lookup");
    }
    let context = context.min(20);
    let mut out = String::new();
    for d in defs.iter().take(3) {
        let text = std::fs::read_to_string(root.join(&d.file))
            .map_err(|e| anyhow!("cannot read {}: {e}", d.file))?;
        let lines: Vec<&str> = text.lines().collect();
        let start = (d.start_line as usize).saturating_sub(1 + context).min(lines.len());
        let mut end = ((d.end_line as usize) + context).min(lines.len());
        let mut truncated = false;
        if end > start && end - start > MAX_SOURCE_LINES {
            end = start + MAX_SOURCE_LINES;
            truncated = true;
        }
        out.push_str(&format!(
            "// {}:{}-{} · {} {}\n",
            d.file, d.start_line, d.end_line, d.kind, d.qualname
        ));
        for (i, l) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:>5}| {}\n", start + i + 1, l));
        }
        if truncated {
            out.push_str("// … truncated; use get_file_skeleton to pick a smaller symbol\n");
        }
        out.push('\n');
    }
    if defs.len() > 3 {
        out.push_str(&format!("// {} more matches; pass file_path to focus\n", defs.len() - 3));
    }
    Ok(out)
}

fn file_stem_key(file: &str) -> String {
    let (dir, fname) = file.rsplit_once('/').unwrap_or(("", file));
    let stem = fname.rsplit_once('.').map_or(fname, |x| x.0);
    if matches!(stem, "__init__" | "mod" | "index") {
        dir.rsplit('/').next().unwrap_or(stem).to_string()
    } else {
        stem.to_string()
    }
}

/// Everything about a file in one call: outline, what it imports, who depends on
/// it, and what changed in it recently.
pub fn file_context(conn: &Connection, file: &str) -> Result<String> {
    let sk = skeleton(conn, file, None)?;
    let resolver = Resolver::new(conn, vec![])?;

    let mut st = conn.prepare(
        "SELECT local,module,original,wildcard,line FROM imports WHERE file=?1 ORDER BY line",
    )?;
    let imps: Vec<ImportRow> = st
        .query_map([file], |r| {
            Ok(ImportRow {
                local: r.get(0)?,
                module: r.get(1)?,
                original: r.get(2)?,
                wildcard: r.get(3)?,
                line: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let imports: Vec<Value> = imps
        .iter()
        .take(60)
        .map(|imp| {
            let files = resolver.import_files(file, imp);
            json!({
                "module": imp.module,
                "name": if imp.wildcard { "*".to_string() } else { imp.original.clone().unwrap_or_else(|| imp.local.clone()) },
                "line": imp.line,
                "internal": if files.is_empty() { Value::Null } else { json!(files.iter().collect::<Vec<_>>()) },
            })
        })
        .collect();

    // Dependents: files whose imports resolve to this file.
    let mut nst = conn.prepare(
        "SELECT DISTINCT name FROM symbols WHERE file=?1 AND depth=0 AND exported=1 AND kind<>'impl' LIMIT 100",
    )?;
    let names: Vec<String> = nst
        .query_map([file], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut args: Vec<String> = vec![file.to_string(), format!("%{}%", like_escape(&file_stem_key(file)))];
    let mut sql = String::from(
        "SELECT file,local,module,original,wildcard,line FROM imports WHERE file<>?1 AND (module LIKE ?2 ESCAPE '\\'",
    );
    if !names.is_empty() {
        let ph: Vec<String> = (0..names.len()).map(|i| format!("?{}", i + 3)).collect();
        sql.push_str(&format!(" OR original IN ({})", ph.join(",")));
        args.extend(names.iter().cloned());
    }
    sql.push_str(") LIMIT 5000");
    let mut dst = conn.prepare(&sql)?;
    let cand: Vec<(String, ImportRow)> = dst
        .query_map(params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                ImportRow {
                    local: r.get(1)?,
                    module: r.get(2)?,
                    original: r.get(3)?,
                    wildcard: r.get(4)?,
                    line: r.get(5)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut dependents: BTreeMap<String, u32> = BTreeMap::new();
    let mut checked: HashSet<(String, String)> = HashSet::new();
    for (f, imp) in &cand {
        if !checked.insert((f.clone(), imp.module.clone())) {
            continue;
        }
        if resolver.import_files(f, imp).iter().any(|x| x == file) {
            let e = dependents.entry(f.clone()).or_insert(imp.line);
            *e = (*e).min(imp.line);
        }
    }
    let total = dependents.len();
    let dep_list: Vec<Value> = dependents
        .iter()
        .take(40)
        .map(|(f, l)| json!({ "file": f, "line": l }))
        .collect();

    let recent: Vec<Value> = events_since(conn, 0, Some(file), 5, true)?
        .iter()
        .map(event_json)
        .collect();

    Ok(serde_json::to_string(&json!({
        "file": file,
        "skeleton": sk,
        "imports": imports,
        "dependents": { "total": total, "files": dep_list },
        "recent_changes": recent,
    }))?)
}

// ----------------------------------------------------------- affected_tests

pub fn is_test_file(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    let file_name = p.rsplit('/').next().unwrap_or(&p);

    if p.starts_with("tests/")
        || p.contains("/tests/")
        || p.starts_with("test/")
        || p.contains("/test/")
        || p.contains("/__tests__/")
        || p.starts_with("spec/")
        || p.contains("/spec/")
    {
        return true;
    }

    file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_test.go")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".test.tsx")
        || file_name.ends_with(".spec.tsx")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".spec.js")
        || file_name.ends_with(".test.jsx")
        || file_name.ends_with(".spec.jsx")
        || file_name.ends_with("test.java")
        || file_name.ends_with("tests.java")
        || file_name.ends_with("testcase.java")
        || file_name.ends_with("test.cs")
        || file_name.ends_with("tests.cs")
        || file_name.ends_with("test.php")
        || file_name.ends_with("_spec.rb")
        || file_name.ends_with("_test.rb")
}

pub fn suggested_test_command(test_file: &str, test_symbol: Option<&str>) -> String {
    let p = test_file.replace('\\', "/");
    let ext = p.rsplit('.').next().unwrap_or("");
    let stem = p.rsplit('/').next().unwrap_or(&p).trim_end_matches(&format!(".{ext}"));

    match ext {
        "py" => {
            if let Some(sym) = test_symbol {
                format!("pytest {p} -k {sym}")
            } else {
                format!("pytest {p}")
            }
        }
        "rs" => {
            if let Some(sym) = test_symbol {
                if p.starts_with("tests/") {
                    format!("cargo test --test {stem} {sym}")
                } else {
                    format!("cargo test {sym}")
                }
            } else if p.starts_with("tests/") {
                format!("cargo test --test {stem}")
            } else {
                "cargo test".to_string()
            }
        }
        "go" => {
            let dir = p.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
            if let Some(sym) = test_symbol {
                format!("go test ./{dir} -run {sym}")
            } else {
                format!("go test ./{dir}")
            }
        }
        "ts" | "tsx" | "js" | "jsx" => {
            if let Some(sym) = test_symbol {
                format!("npm test -- {p} -t {sym}")
            } else {
                format!("npm test -- {p}")
            }
        }
        "java" => {
            if let Some(sym) = test_symbol {
                format!("mvn test -Dtest={stem}#{sym}")
            } else {
                format!("mvn test -Dtest={stem}")
            }
        }
        "cs" => {
            if let Some(sym) = test_symbol {
                format!("dotnet test --filter FullyQualifiedName~{sym}")
            } else {
                "dotnet test".to_string()
            }
        }
        "php" => {
            if let Some(sym) = test_symbol {
                format!("vendor/bin/phpunit {p} --filter {sym}")
            } else {
                format!("vendor/bin/phpunit {p}")
            }
        }
        "rb" => {
            if let Some(sym) = test_symbol {
                format!("bundle exec rspec {p} -e {sym}")
            } else {
                format!("bundle exec rspec {p}")
            }
        }
        _ => format!("test {p}"),
    }
}

pub fn affected_tests(
    conn: &Connection,
    file_path: Option<&str>,
    symbol_name: Option<&str>,
) -> Result<String> {
    if file_path.is_none() && symbol_name.is_none() {
        bail!("at least one of file_path or symbol_name must be provided");
    }

    let resolver = Resolver::new(conn, vec![])?;
    let mut affected_files = HashSet::new();
    let mut test_symbols = Vec::new();
    let mut suggested_cmds = HashSet::new();

    let mut defs = Vec::new();
    if let Some(sym) = symbol_name {
        defs = find_defs(conn, sym, file_path)?;
    } else if let Some(f) = file_path {
        let mut st = conn.prepare(&format!(
            "SELECT {SYM_COLS} FROM symbols WHERE file=?1 AND kind<>'impl'"
        ))?;
        defs = st.query_map([f], sym_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    }

    for def in &defs {
        let (callers, _) = resolver.callers(def)?;
        for h in callers {
            let is_tf = is_test_file(&h.file);
            let is_tsym = h.enclosing.as_ref().map_or(false, |e| {
                let lower = e.to_lowercase();
                lower.starts_with("test") || lower.contains("test")
            });

            if is_tf || is_tsym {
                affected_files.insert(h.file.clone());
                let enc_name = h.enclosing.clone().unwrap_or_else(|| "test".to_string());
                suggested_cmds.insert(suggested_test_command(&h.file, Some(&enc_name)));
                test_symbols.push(json!({
                    "test_file": h.file,
                    "test_symbol": enc_name,
                    "line": h.line,
                    "target_symbol": def.qualname,
                    "confidence": h.confidence.as_str()
                }));
            }
        }
    }

    if let Some(f) = file_path {
        let mut ist = conn.prepare(
            "SELECT file, line FROM imports WHERE original LIKE ?1 OR module LIKE ?1",
        )?;
        let stem = f.rsplit('/').next().unwrap_or(f);
        let like_pat = format!("%{}%", like_escape(stem));
        let imp_rows = ist.query_map([like_pat], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        for row in imp_rows.flatten() {
            let (imp_file, line) = row;
            if imp_file != f && is_test_file(&imp_file) {
                if affected_files.insert(imp_file.clone()) {
                    suggested_cmds.insert(suggested_test_command(&imp_file, None));
                    test_symbols.push(json!({
                        "test_file": imp_file,
                        "test_symbol": "<import>",
                        "line": line,
                        "target_symbol": f,
                        "confidence": "probable"
                    }));
                }
            }
        }
    }

    let mut files_vec: Vec<String> = affected_files.into_iter().collect();
    files_vec.sort();

    let mut cmds_vec: Vec<String> = suggested_cmds.into_iter().collect();
    cmds_vec.sort();

    Ok(serde_json::to_string(&json!({
        "target": {
            "file": file_path,
            "symbol": symbol_name
        },
        "affected_test_files": files_vec,
        "affected_test_symbols": test_symbols,
        "suggested_commands": cmds_vec,
        "summary": format!("Found {} affected test files and {} test callers.", files_vec.len(), test_symbols.len())
    }))?)
}

// ------------------------------------------------------------- find_dead_code

pub fn find_dead_code(
    conn: &Connection,
    path_prefix: Option<&str>,
    limit: usize,
) -> Result<String> {
    let mut sql = format!(
        "SELECT {SYM_COLS} FROM symbols s
         WHERE s.kind IN ('function', 'method', 'class', 'struct', 'enum', 'interface', 'trait')
           AND s.depth <= 1"
    );
    let mut args: Vec<String> = Vec::new();
    if let Some(p) = path_prefix {
        args.push(format!("{}%", like_escape(p.trim_start_matches("./"))));
        sql.push_str(&format!(" AND s.file LIKE ?{} ESCAPE '\\'", args.len()));
    }
    sql.push_str(" ORDER BY s.exported ASC, s.file, s.start_line");

    let mut st = conn.prepare(&sql)?;
    let candidates: Vec<SymRow> = st
        .query_map(params_from_iter(args.iter()), sym_from_row)?
        .collect::<rusqlite::Result<_>>()?;

    let mut ref_st = conn.prepare("SELECT count(*) FROM refs WHERE name=?1")?;
    let mut imp_st = conn.prepare("SELECT count(*) FROM imports WHERE original=?1 OR local=?1")?;

    let ignored_names: &[&str] = &[
        "main", "init", "run", "real_main", "new", "default", "handler", "execute",
        "activate", "deactivate", "setup", "teardown", "start", "stop", "close",
        "dispose", "from", "into", "as_ref", "clone", "to_string", "fmt",
    ];

    let mut dead = Vec::new();
    for s in candidates {
        if is_test_file(&s.file) {
            continue;
        }
        let lower = s.name.to_lowercase();
        if ignored_names.contains(&lower.as_str())
            || lower.starts_with("test_")
            || lower.starts_with("test")
            || lower.starts_with("__")
            || lower.starts_with("on_")
            || lower.starts_with("handle_")
        {
            continue;
        }

        let ref_count: i64 = ref_st.query_row([&s.name], |r| r.get(0))?;
        if ref_count > 0 {
            continue;
        }
        let imp_count: i64 = imp_st.query_row([&s.name], |r| r.get(0))?;
        if imp_count > 0 {
            continue;
        }

        let confidence = if s.exported {
            Confidence::Heuristic
        } else {
            Confidence::Probable
        };

        dead.push(json!({
            "name": s.qualname,
            "file": s.file,
            "line": s.start_line,
            "kind": s.kind,
            "signature": s.signature,
            "exported": s.exported,
            "confidence": confidence.as_str()
        }));

        if dead.len() >= limit.clamp(1, 200) {
            break;
        }
    }

    Ok(serde_json::to_string(&json!({
        "count": dead.len(),
        "candidates": dead,
        "note": "Probable indicates internal unreferenced symbols; Heuristic indicates unreferenced exported symbols."
    }))?)
}
