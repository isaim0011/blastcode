//! The engine: owns the index, keeps it fresh, journals what changed, and
//! dispatches tool calls. Both the CLI and the MCP server go through it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::impact;
use crate::indexer::{index_workspace, IndexStats, Options};
use crate::query::{self, GraphFilter};
use crate::store::{events_since, max_seq, EventRow, Store};

const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const WATCH_INTERVAL: Duration = Duration::from_millis(1500);
const MAX_OUTPUT_BYTES: usize = 48_000;
const DIGEST_MAX_LINES: usize = 12;
const DIGEST_MAX_CHARS: usize = 2_400;

pub const TOOL_NAMES: &[&str] = &[
    "get_workspace_map",
    "get_file_context",
    "get_file_skeleton",
    "get_symbol_source",
    "search_symbols",
    "trace_symbol",
    "get_impact_radius",
    "query_graph",
    "get_workspace_changes",
];

pub struct Engine {
    pub root: PathBuf,
    db_path: PathBuf,
    store: Store,
    opts: Options,
    last_refresh: Instant,
    indexing: Arc<AtomicBool>,
    watching: bool,
    /// Highest journal sequence number already reported to the agent.
    cursor: i64,
}

fn arg_str<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty())
}
fn arg_u64(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64())
}
fn arg_i64(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}
fn arg_bool(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(|x| x.as_bool())
}

fn short(s: &str, n: usize) -> String {
    crate::model::cap(&crate::model::collapse(s), n)
}

fn list(items: &[String], max: usize) -> String {
    let mut shown: Vec<&str> = items.iter().take(max).map(|s| s.as_str()).collect();
    if shown.is_empty() {
        return String::new();
    }
    let more = items.len().saturating_sub(max);
    let mut s = shown.join(", ");
    if more > 0 {
        s.push_str(&format!(" +{more}"));
    }
    shown.clear();
    s
}

/// One human/LLM-readable line per journal event.
pub fn format_event(e: &EventRow) -> String {
    match e.kind.as_str() {
        "file_added" => {
            let syms = list(&e.added, 6);
            if syms.is_empty() {
                format!("+ {} added", e.file)
            } else {
                format!("+ {} added · {syms}", e.file)
            }
        }
        "file_removed" => {
            let syms = list(&e.removed, 6);
            if syms.is_empty() {
                format!("- {} deleted", e.file)
            } else {
                format!("- {} deleted · had {syms}", e.file)
            }
        }
        _ => {
            let mut parts: Vec<String> = Vec::new();
            if !e.changed.is_empty() {
                let items: Vec<String> = e
                    .changed
                    .iter()
                    .take(3)
                    .map(|c| {
                        format!(
                            "{} → {}",
                            short(c["old"].as_str().unwrap_or(""), 70),
                            short(c["new"].as_str().unwrap_or(""), 70)
                        )
                    })
                    .collect();
                let more = e.changed.len().saturating_sub(3);
                let tail = if more > 0 { format!(" +{more}") } else { String::new() };
                parts.push(format!("signature changed: {}{tail}", items.join("; ")));
            }
            if !e.removed.is_empty() {
                parts.push(format!("removed: {}", list(&e.removed, 6)));
            }
            if !e.added.is_empty() {
                parts.push(format!("added: {}", list(&e.added, 6)));
            }
            if parts.is_empty() {
                parts.push("internal edit, no signature changes".to_string());
            }
            format!("~ {} — {}", e.file, parts.join(" | "))
        }
    }
}

impl Engine {
    pub fn open(root: &Path, db: Option<PathBuf>) -> Result<Engine> {
        let root = root
            .canonicalize()
            .map_err(|e| anyhow!("cannot open workspace root {}: {e}", root.display()))?;
        let db_path = db.unwrap_or_else(|| root.join(".blastradius").join("index.db"));
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
            if dir.file_name().map_or(false, |n| n == ".blastradius") {
                let gi = dir.join(".gitignore");
                if !gi.exists() {
                    let _ = std::fs::write(gi, "*\n");
                }
            }
        }
        let store = Store::open(&db_path)?;
        let cursor = max_seq(&store.conn)?;
        Ok(Engine {
            root,
            db_path,
            store,
            opts: Options::default(),
            last_refresh: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            indexing: Arc::new(AtomicBool::new(false)),
            watching: false,
            cursor,
        })
    }

    pub fn has_tool(name: &str) -> bool {
        TOOL_NAMES.contains(&name)
    }

    /// Synchronous incremental index (used by the CLI).
    pub fn index_now(&mut self, force: bool) -> Result<IndexStats> {
        let mut opts = self.opts.clone();
        opts.force = force;
        let stats = index_workspace(&mut self.store, &self.root, &opts)?;
        self.last_refresh = Instant::now();
        Ok(stats)
    }

    /// Keep the index current from a background thread for the life of the process.
    /// Used by the MCP server so the handshake is never blocked and changes are
    /// journaled even while the agent is idle.
    pub fn spawn_watcher(&mut self) {
        let (db, root, opts, flag) = (
            self.db_path.clone(),
            self.root.clone(),
            self.opts.clone(),
            self.indexing.clone(),
        );
        self.watching = true;
        flag.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let mut store = match Store::open(&db) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("blast: watcher could not open index: {e:#}");
                    flag.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let mut first = true;
            loop {
                match index_workspace(&mut store, &root, &opts) {
                    Ok(st) => {
                        if first {
                            eprintln!(
                                "blast: indexed {} files ({} parsed, {} unchanged, {} removed)",
                                st.scanned, st.parsed, st.unchanged, st.removed
                            );
                        }
                    }
                    Err(e) => eprintln!("blast: watcher index error: {e:#}"),
                }
                if first {
                    flag.store(false, Ordering::SeqCst);
                    first = false;
                }
                std::thread::sleep(WATCH_INTERVAL);
            }
        });
    }

    /// Cheap staleness check before each query when no watcher is running.
    fn refresh(&mut self) {
        if self.watching
            || self.indexing.load(Ordering::SeqCst)
            || self.last_refresh.elapsed() < REFRESH_INTERVAL
        {
            return;
        }
        if let Err(e) = index_workspace(&mut self.store, &self.root, &self.opts) {
            eprintln!("blast: refresh failed: {e:#}");
        }
        self.last_refresh = Instant::now();
    }

    pub fn norm_rel(&self, p: &str) -> Result<String> {
        let p = p.replace('\\', "/");
        let path = Path::new(&p);
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| anyhow!("path is outside the workspace root ({})", self.root.display()))?
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            p.trim_start_matches("./").to_string()
        };
        if rel.split('/').any(|s| s == "..") {
            bail!("path must not contain '..'");
        }
        Ok(rel)
    }

    pub fn stats(&self) -> Result<Value> {
        let c = &self.store.conn;
        let n = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
        Ok(json!({
            "root": self.root.display().to_string(),
            "db": self.db_path.display().to_string(),
            "files": n("SELECT count(*) FROM files")?,
            "symbols": n("SELECT count(*) FROM symbols")?,
            "references": n("SELECT count(*) FROM refs")?,
            "imports": n("SELECT count(*) FROM imports")?,
            "journal_events": n("SELECT count(*) FROM events")?,
            "indexing_in_progress": self.indexing.load(Ordering::SeqCst)
        }))
    }

    /// Journal events not yet reported to the agent, formatted for reading.
    /// Advances the cursor, so each change is reported exactly once.
    pub fn take_digest(&mut self) -> Result<Option<String>> {
        let evs = events_since(&self.store.conn, self.cursor, None, 200, false)?;
        let Some(last) = evs.last() else { return Ok(None) };
        self.cursor = last.seq;
        let lines: Vec<String> = evs.iter().map(format_event).collect();
        let mut out = String::from("[blast] workspace changes since your last call:\n");
        for l in lines.iter().take(DIGEST_MAX_LINES) {
            out.push_str(l);
            out.push('\n');
        }
        if lines.len() > DIGEST_MAX_LINES {
            out.push_str(&format!(
                "… +{} more (call get_workspace_changes)\n",
                lines.len() - DIGEST_MAX_LINES
            ));
        }
        if evs.iter().any(|e| !e.changed.is_empty() || !e.removed.is_empty()) {
            out.push_str("Run get_impact_radius on changed files to see affected callers.\n");
        }
        if out.len() > DIGEST_MAX_CHARS {
            let mut end = DIGEST_MAX_CHARS;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("…\n");
        }
        Ok(Some(out))
    }

    /// Tool call for agents: the result plus an optional digest of what changed
    /// in the workspace since the previous call.
    pub fn call_with_digest(&mut self, name: &str, args: &Value) -> Result<(Option<String>, String)> {
        let body = self.call(name, args)?;
        // The journal tool already reports raw events; do not report them twice.
        let digest = if name == "get_workspace_changes" {
            self.cursor = max_seq(&self.store.conn)?;
            None
        } else {
            self.take_digest()?
        };
        Ok((digest, body))
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        self.refresh();
        let conn = &self.store.conn;
        let body = match name {
            "get_workspace_map" => {
                let path = arg_str(args, "path").map(|p| self.norm_rel(p)).transpose()?;
                query::workspace_map(
                    conn,
                    path.as_deref(),
                    arg_u64(args, "depth").unwrap_or(3).clamp(1, 12) as usize,
                    arg_u64(args, "max_chars").unwrap_or(12_000).clamp(1_000, 40_000) as usize,
                )?
            }
            "get_file_context" => {
                let f = arg_str(args, "file_path")
                    .ok_or_else(|| anyhow!("missing required argument: file_path"))?;
                query::file_context(conn, &self.norm_rel(f)?)?
            }
            "get_file_skeleton" => {
                let f = arg_str(args, "file_path")
                    .ok_or_else(|| anyhow!("missing required argument: file_path"))?;
                let rel = self.norm_rel(f)?;
                query::skeleton(conn, &rel, arg_u64(args, "max_depth").map(|d| d as u32))?
            }
            "get_symbol_source" => {
                let s = arg_str(args, "symbol_name")
                    .ok_or_else(|| anyhow!("missing required argument: symbol_name"))?;
                let file = arg_str(args, "file_path").map(|p| self.norm_rel(p)).transpose()?;
                query::symbol_source(
                    conn,
                    &self.root,
                    s,
                    file.as_deref(),
                    arg_u64(args, "context_lines").unwrap_or(0) as usize,
                )?
            }
            "search_symbols" => {
                let q = arg_str(args, "query").ok_or_else(|| anyhow!("missing required argument: query"))?;
                query::search_symbols(
                    conn,
                    q,
                    arg_str(args, "kind"),
                    arg_u64(args, "limit").unwrap_or(20).clamp(1, 100) as usize,
                )?
            }
            "trace_symbol" => {
                let s = arg_str(args, "symbol_name")
                    .ok_or_else(|| anyhow!("missing required argument: symbol_name"))?;
                let file = arg_str(args, "file_path").map(|p| self.norm_rel(p)).transpose()?;
                let dir = arg_str(args, "direction").unwrap_or("both");
                if !matches!(dir, "both" | "callers" | "callees") {
                    bail!("direction must be one of: both, callers, callees");
                }
                query::trace_symbol(
                    conn,
                    s,
                    file.as_deref(),
                    dir,
                    arg_u64(args, "limit").unwrap_or(50) as usize,
                )?
            }
            "get_impact_radius" => {
                let f = arg_str(args, "file_path")
                    .ok_or_else(|| anyhow!("missing required argument: file_path"))?;
                let rel = self.norm_rel(f)?;
                impact::impact_report(conn, &self.root, &rel, args.get("new_source").and_then(|v| v.as_str()))?
            }
            "query_graph" => {
                let f = GraphFilter {
                    kind: arg_str(args, "kind"),
                    name: arg_str(args, "name"),
                    path_prefix: arg_str(args, "path_prefix"),
                    calls: arg_str(args, "calls"),
                    called_by: arg_str(args, "called_by"),
                    exported: arg_bool(args, "exported"),
                    limit: arg_u64(args, "limit").unwrap_or(50) as usize,
                };
                query::query_graph(conn, &f)?
            }
            "get_workspace_changes" => {
                let file = arg_str(args, "file_path").map(|p| self.norm_rel(p)).transpose()?;
                query::workspace_changes(
                    conn,
                    arg_i64(args, "since"),
                    file.as_deref(),
                    arg_u64(args, "limit").unwrap_or(30) as usize,
                )?
            }
            other => bail!("unknown tool: {other}"),
        };
        let mut body = body;
        if body.len() > MAX_OUTPUT_BYTES {
            let mut end = MAX_OUTPUT_BYTES;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            body.push_str("\n[output truncated]");
        }
        if self.indexing.load(Ordering::SeqCst) {
            body = format!("[note: initial indexing still running; results may be incomplete]\n{body}");
        }
        Ok(body)
    }

    /// Journal events newer than the CLI watcher's cursor; advances the cursor.
    pub fn poll_events(&mut self) -> Result<Vec<EventRow>> {
        let evs = events_since(&self.store.conn, self.cursor, None, 500, false)?;
        if let Some(l) = evs.last() {
            self.cursor = l.seq;
        }
        Ok(evs)
    }
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "get_workspace_map",
            "description": "High-level directory tree annotated with each file's primary exported symbols. Call this FIRST to orient yourself in an unfamiliar repo instead of listing and reading files. Zoom into a subtree with `path`.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "Subdirectory to zoom into (relative to repo root)." },
                "depth": { "type": "integer", "description": "Directory levels to expand (default 3)." },
                "max_chars": { "type": "integer", "description": "Output budget in characters (default 12000)." }
            }}
        },
        {
            "name": "get_file_context",
            "description": "Everything about one file in a single call: its skeleton, what it imports (and which imports are internal files), which files depend on it, and its recent changes. Use this instead of reading the file or grepping for importers.",
            "inputSchema": { "type": "object", "properties": {
                "file_path": { "type": "string" }
            }, "required": ["file_path"] }
        },
        {
            "name": "get_file_skeleton",
            "description": "Compact outline of a file: classes, functions, methods with signatures, decorators and first doc line, with line numbers. Bodies are omitted (typically 90%+ fewer tokens than reading the file).",
            "inputSchema": { "type": "object", "properties": {
                "file_path": { "type": "string" },
                "max_depth": { "type": "integer", "description": "Max nesting depth to show (0 = top level only)." }
            }, "required": ["file_path"] }
        },
        {
            "name": "get_symbol_source",
            "description": "Exact source lines of ONE function/class/method with line numbers, instead of reading the whole file. Use after get_file_skeleton or search_symbols once you know which symbol you need.",
            "inputSchema": { "type": "object", "properties": {
                "symbol_name": { "type": "string", "description": "Name or qualified name, e.g. 'verify_token' or 'UserService.getUserById'." },
                "file_path": { "type": "string", "description": "Disambiguate when several symbols share the name." },
                "context_lines": { "type": "integer", "description": "Extra lines before/after (default 0, max 20)." }
            }, "required": ["symbol_name"] }
        },
        {
            "name": "search_symbols",
            "description": "Fuzzy lookup of symbols by name when you do not know the exact name or location. Multi-word queries match all words (e.g. 'user service'). Returns definitions with file and line.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "kind": { "type": "string", "description": "Filter: function, method, class, struct, interface, enum, trait, type." },
                "limit": { "type": "integer" }
            }, "required": ["query"] }
        },
        {
            "name": "trace_symbol",
            "description": "Definition plus all inbound callers, type usages and importers, and outbound callees of a symbol, with exact file:line. Every edge carries a confidence: exact (resolved via scope/imports), probable (unique name match), heuristic (several candidates). Prefer this over grep for 'who uses X' and 'what does X call'.",
            "inputSchema": { "type": "object", "properties": {
                "symbol_name": { "type": "string", "description": "Name or qualified name, e.g. 'verify_token' or 'UserService.getUserById'." },
                "file_path": { "type": "string", "description": "Disambiguate when several symbols share the name." },
                "direction": { "type": "string", "enum": ["both", "callers", "callees"] },
                "limit": { "type": "integer" }
            }, "required": ["symbol_name"] }
        },
        {
            "name": "get_impact_radius",
            "description": "Impact analysis for an edit. Without `new_source`, compares the working-tree file with git HEAD (use AFTER editing). With `new_source`, compares proposed full file content with what is on disk (use BEFORE writing). Reports removed or re-signatured symbols and the call sites they affect. 'breaking' is only reported when provable; otherwise 'review'.",
            "inputSchema": { "type": "object", "properties": {
                "file_path": { "type": "string" },
                "new_source": { "type": "string", "description": "Complete proposed content of the file (optional)." }
            }, "required": ["file_path"] }
        },
        {
            "name": "query_graph",
            "description": "Structural filter over the symbol index without grepping: by kind, name substring, path prefix, exported flag, symbols that call a given name (`calls`) or are called by a given symbol (`called_by`). Name-level matching; use trace_symbol for resolved edges.",
            "inputSchema": { "type": "object", "properties": {
                "kind": { "type": "string" },
                "name": { "type": "string" },
                "path_prefix": { "type": "string" },
                "calls": { "type": "string", "description": "Only symbols whose body calls this name." },
                "called_by": { "type": "string", "description": "Only symbols called from a symbol with this name." },
                "exported": { "type": "boolean" },
                "limit": { "type": "integer" }
            }}
        },
        {
            "name": "get_workspace_changes",
            "description": "Journal of what changed in the workspace (files added/removed/edited, symbols added/removed, signatures changed), maintained by a background watcher. Normally you do not need to call this: recent changes are attached to every tool response automatically. Use it to look further back or to filter by file.",
            "inputSchema": { "type": "object", "properties": {
                "since": { "type": "integer", "description": "Only events with seq greater than this." },
                "file_path": { "type": "string" },
                "limit": { "type": "integer" }
            }}
        }
    ])
}
