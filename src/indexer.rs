//! Incremental, parallel workspace indexing with a change journal.
//!
//! A scan compares (mtime, size) with the stored state, then confirms real
//! content changes by blake3 hash before re-parsing. Every re-parsed file is
//! diffed against its previous symbols and recorded as an event so the agent can
//! be told exactly what happened ("verify_token gained a parameter").

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};

use crate::extract;
use crate::lang::Lang;
use crate::model::{collapse, cap, FileData, SymbolRec};
use crate::resolve::join_norm;
use crate::store::{
    load_old_symbols, load_ts_aliases, prune_events, record_event, replace_ts_aliases, write_file,
    OldSym, Store, TsAlias,
};

#[derive(Debug, Clone)]
pub struct Options {
    pub max_file_bytes: u64,
    /// Ignore stored mtime/size and re-hash every file.
    pub force: bool,
}

impl Default for Options {
    fn default() -> Self {
        let max = std::env::var("BLAST_MAX_FILE_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_000_000);
        Options { max_file_bytes: max, force: false }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub scanned: usize,
    pub parsed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped_large: usize,
    pub errors: usize,
}

const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", "vendor", "dist", "build", "__pycache__", ".git", ".venv", "venv",
    ".tox", ".next", "site-packages", "bin", "obj", ".gradle", ".idea",
];

const MAX_LIST: usize = 20;

struct Entry {
    rel: String,
    abs: PathBuf,
    lang: Lang,
    mtime: i64,
    size: u64,
}

enum Outcome {
    Parsed(FileData, Option<String>),
    Touch { path: String, mtime: i64, size: i64 },
}

pub fn init_thread_pool() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rayon::ThreadPoolBuilder::new()
            .stack_size(32 * 1024 * 1024)
            .build_global();
    });
}

fn scan(root: &Path, opts: &Options, stats: &mut IndexStats) -> (Vec<Entry>, Vec<PathBuf>) {
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|e| {
            let is_dir = e.file_type().map_or(false, |t| t.is_dir());
            if !is_dir {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&&*name)
        })
        .build();
    let mut out = Vec::new();
    let mut configs = Vec::new();
    for dent in walker {
        let Ok(dent) = dent else { continue };
        if !dent.file_type().map_or(false, |t| t.is_file()) {
            continue;
        }
        let fname = dent.file_name().to_string_lossy().to_string();
        if fname == "tsconfig.json" || fname == "jsconfig.json" {
            configs.push(dent.path().to_path_buf());
            continue;
        }
        let Ok(relp) = dent.path().strip_prefix(root) else { continue };
        let rel = relp.to_string_lossy().replace('\\', "/");
        let Some(lang) = Lang::from_path(&rel) else { continue };
        let Ok(meta) = dent.metadata() else { continue };
        if meta.len() > opts.max_file_bytes {
            stats.skipped_large += 1;
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64) * 1_000_000_000 + d.subsec_nanos() as i64)
            .unwrap_or(0);
        out.push(Entry { rel, abs: dent.path().to_path_buf(), lang, mtime, size: meta.len() });
    }
    (out, configs)
}

fn process(e: &Entry, old_hash: Option<&str>) -> Outcome {
    let empty = |hash: String| FileData {
        path: e.rel.clone(),
        lang: e.lang.name().to_string(),
        hash,
        mtime: e.mtime,
        size: e.size as i64,
        lines: 0,
        symbols: vec![],
        refs: vec![],
        imports: vec![],
    };
    let bytes = match std::fs::read(&e.abs) {
        Ok(b) => b,
        Err(err) => {
            return Outcome::Parsed(empty(String::new()), Some(format!("read {}: {err}", e.rel)));
        }
    };
    let hash = blake3::hash(&bytes).to_hex().to_string();
    if old_hash == Some(hash.as_str()) {
        return Outcome::Touch { path: e.rel.clone(), mtime: e.mtime, size: e.size as i64 };
    }
    let parsed = catch_unwind(AssertUnwindSafe(|| extract::parse(e.lang, &bytes)));
    match parsed {
        Ok(Ok(p)) => Outcome::Parsed(
            FileData {
                path: e.rel.clone(),
                lang: e.lang.name().to_string(),
                hash,
                mtime: e.mtime,
                size: e.size as i64,
                lines: p.lines,
                symbols: p.symbols,
                refs: p.refs,
                imports: p.imports,
            },
            None,
        ),
        Ok(Err(err)) => Outcome::Parsed(empty(hash), Some(format!("parse {}: {err}", e.rel))),
        Err(_) => Outcome::Parsed(empty(hash), Some(format!("parser panicked on {}", e.rel))),
    }
}

// ------------------------------------------------------------------ journal

fn diff_symbols(old: &[OldSym], new: &[SymbolRec]) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let oldm: HashMap<&str, &OldSym> = old
        .iter()
        .filter(|o| o.kind != "impl")
        .map(|o| (o.qualname.as_str(), o))
        .collect();
    let newm: HashMap<&str, &SymbolRec> = new
        .iter()
        .filter(|s| s.kind != "impl")
        .map(|s| (s.qualname.as_str(), s))
        .collect();
    let mut added: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut changed: Vec<Value> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for s in new.iter().filter(|s| s.kind != "impl") {
        if !seen.insert(s.qualname.as_str()) {
            continue;
        }
        match oldm.get(s.qualname.as_str()) {
            None => added.push(s.qualname.clone()),
            Some(o) => {
                if collapse(&o.signature) != collapse(&s.signature) {
                    changed.push(json!({
                        "name": s.qualname,
                        "old": cap(&collapse(&o.signature), 140),
                        "new": cap(&collapse(&s.signature), 140),
                    }));
                }
            }
        }
    }
    let mut seen_old: HashSet<&str> = HashSet::new();
    for o in old.iter().filter(|o| o.kind != "impl") {
        if !seen_old.insert(o.qualname.as_str()) {
            continue;
        }
        if !newm.contains_key(o.qualname.as_str()) {
            removed.push(o.qualname.clone());
        }
    }
    added.truncate(MAX_LIST);
    removed.truncate(MAX_LIST);
    changed.truncate(MAX_LIST);
    (added, removed, changed)
}

// ------------------------------------------------------------ tsconfig paths

fn strip_jsonc(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // trailing commas
    let t: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(t.len());
    let mut j = 0;
    in_str = false;
    while j < t.len() {
        let c = t[j];
        if in_str {
            res.push(c);
            if c == '\\' && j + 1 < t.len() {
                res.push(t[j + 1]);
                j += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            j += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            res.push(c);
            j += 1;
            continue;
        }
        if c == ',' {
            let mut k = j + 1;
            while k < t.len() && t[k].is_whitespace() {
                k += 1;
            }
            if k < t.len() && (t[k] == '}' || t[k] == ']') {
                j += 1;
                continue;
            }
        }
        res.push(c);
        j += 1;
    }
    res
}

fn collect_ts_aliases(root: &Path, configs: &[PathBuf]) -> Vec<TsAlias> {
    let mut out = Vec::new();
    for c in configs {
        let Ok(text) = std::fs::read_to_string(c) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&strip_jsonc(&text)) else { continue };
        let dir_rel = c
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let co = &v["compilerOptions"];
        let base_url = co["baseUrl"].as_str();
        let Some(base) = join_norm(&dir_rel, base_url.unwrap_or(".")) else { continue };
        if base_url.is_some() {
            let target = if base.is_empty() { "*".to_string() } else { format!("{base}/*") };
            out.push(TsAlias { dir: dir_rel.clone(), pattern: "*".to_string(), target });
        }
        if let Some(paths) = co["paths"].as_object() {
            for (pat, targets) in paths {
                for t in targets.as_array().into_iter().flatten() {
                    if let Some(t) = t.as_str() {
                        if let Some(tp) = join_norm(&base, t) {
                            out.push(TsAlias { dir: dir_rel.clone(), pattern: pat.clone(), target: tp });
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// -------------------------------------------------------------------- index

pub fn index_workspace(store: &mut Store, root: &Path, opts: &Options) -> Result<IndexStats> {
    init_thread_pool();
    let mut stats = IndexStats::default();
    let (entries, configs) = scan(root, opts, &mut stats);
    stats.scanned = entries.len();
    let existing = store.file_states()?;
    // The very first index would flood the journal with "file_added"; skip it.
    let track = !existing.is_empty();

    let seen: HashSet<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
    let removed: Vec<&String> = existing.keys().filter(|k| !seen.contains(k.as_str())).collect();
    if !removed.is_empty() {
        let tx = store.conn.transaction()?;
        for p in &removed {
            if track {
                if let Some(old) = load_old_symbols(&tx, p)? {
                    let names: Vec<String> = old
                        .iter()
                        .filter(|o| o.kind != "impl")
                        .map(|o| o.qualname.clone())
                        .take(MAX_LIST)
                        .collect();
                    record_event(&tx, p, "file_removed", &[], &names, &[])?;
                }
            }
            tx.execute("DELETE FROM files WHERE path=?1", [p.as_str()])?;
        }
        prune_events(&tx)?;
        tx.commit()?;
        stats.removed = removed.len();
    }

    let todo: Vec<&Entry> = entries
        .iter()
        .filter(|e| {
            opts.force
                || match existing.get(&e.rel) {
                    Some((m, s, _)) => *m != e.mtime || *s != e.size as i64,
                    None => true,
                }
        })
        .collect();
    stats.unchanged = entries.len() - todo.len();

    for chunk in todo.chunks(256) {
        let results: Vec<Outcome> = chunk
            .par_iter()
            .map(|e| process(e, existing.get(&e.rel).map(|x| x.2.as_str())))
            .collect();
        let tx = store.conn.transaction()?;
        for r in results {
            match r {
                Outcome::Parsed(fd, err) => {
                    if track && err.is_none() {
                        match load_old_symbols(&tx, &fd.path)? {
                            None => {
                                let names: Vec<String> = fd
                                    .symbols
                                    .iter()
                                    .filter(|s| s.kind != "impl" && s.depth == 0)
                                    .map(|s| s.qualname.clone())
                                    .take(MAX_LIST)
                                    .collect();
                                record_event(&tx, &fd.path, "file_added", &names, &[], &[])?;
                            }
                            Some(old) => {
                                let (a, rm, ch) = diff_symbols(&old, &fd.symbols);
                                record_event(&tx, &fd.path, "file_modified", &a, &rm, &ch)?;
                            }
                        }
                    }
                    write_file(&tx, &fd)?;
                    stats.parsed += 1;
                    if let Some(msg) = err {
                        stats.errors += 1;
                        eprintln!("blast: warning: {msg}");
                    }
                }
                Outcome::Touch { path, mtime, size } => {
                    tx.execute(
                        "UPDATE files SET mtime=?2, size=?3 WHERE path=?1",
                        rusqlite::params![path, mtime, size],
                    )?;
                    stats.unchanged += 1;
                }
            }
        }
        prune_events(&tx)?;
        tx.commit()?;
    }

    let aliases = collect_ts_aliases(root, &configs);
    if load_ts_aliases(&store.conn)? != aliases {
        let tx = store.conn.transaction()?;
        replace_ts_aliases(&tx, &aliases)?;
        tx.commit()?;
    }
    Ok(stats)
}
