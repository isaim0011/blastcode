//! Query-time resolution of references to definitions.
//!
//! Resolution order (highest to lowest confidence):
//! 1. same-file scope / import pointing at the defining file  -> Exact
//! 2. unique match by name across the repo                    -> Probable
//! 3. several candidates sharing a name                       -> Heuristic
//!
//! Imports are followed through re-export barrels (`export * from`, Python
//! `__init__` re-imports, Rust `pub use`). TypeScript path aliases come from
//! tsconfig/jsconfig. Calls into libraries (imports that resolve to no indexed
//! file) are deliberately left unresolved instead of being matched by name.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::Result;
use rusqlite::{params, Connection, Row};

use crate::lang::{Family, Lang};
use crate::model::{arity, Confidence, Param};
use crate::store::{load_ts_aliases, TsAlias};

pub const SYM_COLS: &str =
    "id,file,name,qualname,kind,signature,doc,start_line,end_line,depth,parent_id,exported,params,has_self";
pub const SYM_COLS_S: &str =
    "s.id,s.file,s.name,s.qualname,s.kind,s.signature,s.doc,s.start_line,s.end_line,s.depth,s.parent_id,s.exported,s.params,s.has_self";

const MAX_CANDIDATES: usize = 5000;
const MAX_HEURISTIC: usize = 20;
const MAX_REEXPORT_DEPTH: u8 = 4;
const U_KINDS: [&str; 3] = ["function", "class", "struct"];
const Q_KINDS: [&str; 2] = ["function", "method"];
const T_KINDS: [&str; 6] = ["class", "struct", "interface", "enum", "trait", "type"];

#[derive(Debug, Clone)]
pub struct SymRow {
    pub id: i64,
    pub file: String,
    pub name: String,
    pub qualname: String,
    pub kind: String,
    pub signature: String,
    pub doc: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub depth: u32,
    pub parent_id: Option<i64>,
    pub exported: bool,
    pub params: Option<Vec<Param>>,
    pub has_self: bool,
}

pub fn sym_from_row(r: &Row<'_>) -> rusqlite::Result<SymRow> {
    let params: Option<String> = r.get(12)?;
    Ok(SymRow {
        id: r.get(0)?,
        file: r.get(1)?,
        name: r.get(2)?,
        qualname: r.get(3)?,
        kind: r.get(4)?,
        signature: r.get(5)?,
        doc: r.get(6)?,
        start_line: r.get(7)?,
        end_line: r.get(8)?,
        depth: r.get(9)?,
        parent_id: r.get(10)?,
        exported: r.get(11)?,
        params: params.and_then(|s| serde_json::from_str::<Vec<Param>>(&s).ok()),
        has_self: r.get(13)?,
    })
}

#[derive(Debug, Clone)]
pub struct ImportRow {
    pub local: String,
    pub module: String,
    pub original: Option<String>,
    pub wildcard: bool,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct RefHit {
    pub file: String,
    pub line: u32,
    /// "call", "type" or "import"
    pub usage: &'static str,
    /// ref kind ("call" / "path_call" / "type"); empty for imports
    pub kind: String,
    pub qualifier: Option<String>,
    pub arg_count: Option<u32>,
    pub kwargs: Option<String>,
    pub enclosing: Option<String>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone)]
pub struct CalleeHit {
    pub name: String,
    pub line: u32,
    pub targets: Vec<(SymRow, Confidence)>,
}

struct RawRef {
    file: String,
    name: String,
    qualifier: Option<String>,
    kind: String,
    line: u32,
    arg_count: Option<u32>,
    enclosing: Option<String>,
    kwargs: Option<String>,
}

// ------------------------------------------------------------ file index

fn split_dir(p: &str) -> (&str, &str) {
    match p.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", p),
    }
}

fn has_suffix(path: &str, suffix: &str) -> bool {
    path == suffix || path.ends_with(&format!("/{suffix}"))
}

/// Join `rel` onto `base`, resolving `.` and `..`. `None` if it escapes the root.
pub fn join_norm(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base.is_empty() { vec![] } else { base.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

const TS_EXTS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];
const C_SOURCE_EXTS: [&str; 4] = [".c", ".cc", ".cpp", ".cxx"];

pub struct FileIndex {
    pub files: HashSet<String>,
    stems: HashMap<String, Vec<String>>,
    dirs: HashMap<String, Vec<String>>,
}

impl FileIndex {
    pub fn new(paths: Vec<String>) -> Self {
        let mut files = HashSet::with_capacity(paths.len());
        let mut stems: HashMap<String, Vec<String>> = HashMap::new();
        let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
        for p in paths {
            let (dir, fname) = split_dir(&p);
            let stem = fname.rsplit_once('.').map(|x| x.0).unwrap_or(fname);
            let key = if matches!(stem, "__init__" | "mod" | "index") {
                split_dir(dir).1.to_string()
            } else {
                stem.to_string()
            };
            let dir = dir.to_string();
            stems.entry(key).or_default().push(p.clone());
            dirs.entry(dir).or_default().push(p.clone());
            files.insert(p);
        }
        FileIndex { files, stems, dirs }
    }

    fn by_stem_suffix(&self, key: &str, suffixes: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = self.stems.get(key) {
            for f in c {
                if suffixes.iter().any(|s| has_suffix(f, s)) {
                    out.push(f.clone());
                }
            }
        }
        out
    }

    fn try_ts(&self, base: &str) -> Option<String> {
        let stripped = base
            .trim_end_matches(".js")
            .trim_end_matches(".jsx")
            .trim_end_matches(".mjs")
            .to_string();
        let mut cands = vec![base.to_string()];
        for stem in [base, stripped.as_str()] {
            for e in TS_EXTS {
                cands.push(format!("{stem}.{e}"));
                cands.push(format!("{stem}/index.{e}"));
            }
        }
        cands.into_iter().find(|c| self.files.contains(c))
    }

    fn ts_module(&self, from: &str, spec: &str, aliases: &[TsAlias]) -> Vec<String> {
        if spec.starts_with('.') {
            let Some(base) = join_norm(split_dir(from).0, spec) else { return vec![] };
            return self.try_ts(&base).into_iter().collect();
        }
        for a in aliases {
            if !(a.dir.is_empty() || from.starts_with(&format!("{}/", a.dir))) {
                continue;
            }
            let rest: Option<&str> = if let Some(prefix) = a.pattern.strip_suffix('*') {
                spec.strip_prefix(prefix)
            } else if a.pattern == spec {
                Some("")
            } else {
                None
            };
            let Some(rest) = rest else { continue };
            let base = if a.target.contains('*') {
                a.target.replacen('*', rest, 1)
            } else {
                a.target.clone()
            };
            if let Some(f) = self.try_ts(&base) {
                return vec![f];
            }
        }
        vec![]
    }

    fn py_module(&self, from: &str, spec: &str) -> Vec<String> {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = &spec[dots..];
        let rel = rest.replace('.', "/");
        if dots > 0 {
            let mut dir = split_dir(from).0.to_string();
            for _ in 1..dots {
                dir = split_dir(&dir).0.to_string();
            }
            let path = match (dir.is_empty(), rel.is_empty()) {
                (true, _) => rel,
                (false, true) => dir,
                (false, false) => format!("{dir}/{rel}"),
            };
            let mut out = Vec::new();
            if path.is_empty() {
                let c = "__init__.py".to_string();
                if self.files.contains(&c) {
                    out.push(c);
                }
            } else {
                for c in [format!("{path}.py"), format!("{path}.pyi"), format!("{path}/__init__.py")] {
                    if self.files.contains(&c) {
                        out.push(c);
                    }
                }
            }
            out
        } else {
            let key = rel.rsplit('/').next().unwrap_or("").to_string();
            self.by_stem_suffix(
                &key,
                &[format!("{rel}.py"), format!("{rel}.pyi"), format!("{rel}/__init__.py")],
            )
        }
    }

    fn rs_module(&self, spec: &str) -> Vec<String> {
        let mut segs: Vec<&str> = spec.split("::").filter(|s| !s.is_empty()).collect();
        while matches!(segs.first(), Some(&"crate") | Some(&"self") | Some(&"super")) {
            segs.remove(0);
        }
        if segs.is_empty() || matches!(segs[0], "std" | "core" | "alloc") {
            return vec![];
        }
        let key = segs[segs.len() - 1];
        for k in 0..segs.len() {
            let rel = segs[k..].join("/");
            let hits = self.by_stem_suffix(key, &[format!("{rel}.rs"), format!("{rel}/mod.rs")]);
            if !hits.is_empty() {
                return hits;
            }
        }
        vec![]
    }

    fn dir_files_with_suffix(&self, suffix: &str, ext: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (d, files) in &self.dirs {
            if has_suffix(d, suffix) {
                out.extend(files.iter().filter(|f| f.ends_with(ext)).cloned());
            }
        }
        out
    }

    fn go_module(&self, spec: &str) -> Vec<String> {
        let segs: Vec<&str> = spec.split('/').filter(|s| !s.is_empty()).collect();
        for k in 0..segs.len() {
            let suffix = segs[k..].join("/");
            let out = self.dir_files_with_suffix(&suffix, ".go");
            if !out.is_empty() {
                return out;
            }
        }
        vec![]
    }

    fn java_module(&self, spec: &str, wildcard: bool) -> Vec<String> {
        let rel = spec.replace('.', "/");
        if wildcard {
            return self.dir_files_with_suffix(&rel, ".java");
        }
        let key = rel.rsplit('/').next().unwrap_or("").to_string();
        self.by_stem_suffix(&key, &[format!("{rel}.java")])
    }

    fn php_module(&self, spec: &str) -> Vec<String> {
        let segs: Vec<&str> = spec.split('\\').filter(|s| !s.is_empty()).collect();
        let Some(key) = segs.last() else { return vec![] };
        for k in 0..segs.len() {
            let rel = segs[k..].join("/");
            let hits = self.by_stem_suffix(key, &[format!("{rel}.php")]);
            if !hits.is_empty() {
                return hits;
            }
        }
        vec![]
    }

    fn php_include(&self, from: &str, spec: &str) -> Vec<String> {
        if let Some(p) = join_norm(split_dir(from).0, spec) {
            if self.files.contains(&p) {
                return vec![p];
            }
        }
        let rel = spec.trim_start_matches("./").trim_start_matches('/').to_string();
        let stem = split_dir(&rel).1.rsplit_once('.').map_or(split_dir(&rel).1, |x| x.0).to_string();
        self.by_stem_suffix(&stem, &[rel])
    }

    fn ruby_module(&self, from: &str, spec: &str) -> Vec<String> {
        let spec_rb = if spec.ends_with(".rb") { spec.to_string() } else { format!("{spec}.rb") };
        if spec.starts_with('.') {
            if let Some(p) = join_norm(split_dir(from).0, &spec_rb) {
                if self.files.contains(&p) {
                    return vec![p];
                }
            }
            return vec![];
        }
        let stem = split_dir(&spec_rb).1.trim_end_matches(".rb").to_string();
        self.by_stem_suffix(&stem, &[spec_rb])
    }

    /// `#include "x.h"`: the header plus same-stem implementation files beside it.
    fn c_include(&self, from: &str, spec: &str) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        if let Some(p) = join_norm(split_dir(from).0, spec) {
            if self.files.contains(&p) {
                found.push(p);
            }
        }
        if found.is_empty() {
            let rel = spec.trim_start_matches("./").to_string();
            let fname = split_dir(&rel).1;
            let stem = fname.rsplit_once('.').map_or(fname, |x| x.0).to_string();
            found = self.by_stem_suffix(&stem, &[rel]);
        }
        let mut out = found.clone();
        for h in &found {
            let (dir, fname) = split_dir(h);
            let stem = fname.rsplit_once('.').map_or(fname, |x| x.0);
            if let Some(cands) = self.stems.get(stem) {
                for c in cands {
                    if split_dir(c).0 == dir && C_SOURCE_EXTS.iter().any(|e| c.ends_with(e)) {
                        out.push(c.clone());
                    }
                }
            }
        }
        out
    }
}

// -------------------------------------------------------------- resolver

pub struct Resolver<'a> {
    conn: &'a Connection,
    ix: FileIndex,
    aliases: Vec<TsAlias>,
    overlay: Vec<SymRow>,
    by_name_cache: RefCell<HashMap<String, Rc<Vec<SymRow>>>>,
    imports_cache: RefCell<HashMap<String, Rc<Vec<ImportRow>>>>,
    files_cache: RefCell<HashMap<(String, String, Option<String>), Rc<Vec<String>>>>,
}

fn rank_global(cands: Vec<&SymRow>, unique_conf: Confidence) -> Vec<(SymRow, Confidence)> {
    match cands.len() {
        0 => vec![],
        1 => vec![(cands[0].clone(), unique_conf)],
        n if n <= MAX_HEURISTIC => cands
            .into_iter()
            .map(|s| (s.clone(), Confidence::Heuristic))
            .collect(),
        _ => vec![],
    }
}

fn narrow_by_type<'s>(hits: Vec<&'s SymRow>, ty: &str) -> Vec<&'s SymRow> {
    if !ty.chars().next().map_or(false, |c| c.is_uppercase()) {
        return hits;
    }
    let narrowed: Vec<&SymRow> = hits
        .iter()
        .copied()
        .filter(|s| s.qualname.rsplit('.').nth(1) == Some(ty))
        .collect();
    if narrowed.is_empty() {
        hits
    } else {
        narrowed
    }
}

fn exact_all(v: Vec<SymRow>) -> Vec<(SymRow, Confidence)> {
    v.into_iter().take(5).map(|s| (s, Confidence::Exact)).collect()
}

impl<'a> Resolver<'a> {
    /// `overlay` rows replace stored rows with the same (file, qualname); used by
    /// impact analysis to resolve against the *previous* version of a symbol.
    pub fn new(conn: &'a Connection, overlay: Vec<SymRow>) -> Result<Self> {
        let mut st = conn.prepare("SELECT path FROM files")?;
        let paths: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Resolver {
            conn,
            ix: FileIndex::new(paths),
            aliases: load_ts_aliases(conn)?,
            overlay,
            by_name_cache: RefCell::new(HashMap::new()),
            imports_cache: RefCell::new(HashMap::new()),
            files_cache: RefCell::new(HashMap::new()),
        })
    }

    fn by_name(&self, name: &str) -> Result<Rc<Vec<SymRow>>> {
        if let Some(v) = self.by_name_cache.borrow().get(name) {
            return Ok(v.clone());
        }
        let sql = format!("SELECT {SYM_COLS} FROM symbols WHERE name=?1 AND kind<>'impl' LIMIT {MAX_CANDIDATES}");
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows: Vec<SymRow> = st
            .query_map([name], sym_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        let mut rows: Vec<SymRow> = rows
            .into_iter()
            .filter(|r| !self.overlay.iter().any(|o| o.file == r.file && o.qualname == r.qualname))
            .collect();
        rows.extend(self.overlay.iter().filter(|o| o.name == name).cloned());
        let rc = Rc::new(rows);
        self.by_name_cache.borrow_mut().insert(name.to_string(), rc.clone());
        Ok(rc)
    }

    fn imports_of(&self, file: &str) -> Result<Rc<Vec<ImportRow>>> {
        if let Some(v) = self.imports_cache.borrow().get(file) {
            return Ok(v.clone());
        }
        let mut st = self
            .conn
            .prepare_cached("SELECT local,module,original,wildcard,line FROM imports WHERE file=?1")?;
        let rows: Vec<ImportRow> = st
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
        let rc = Rc::new(rows);
        self.imports_cache.borrow_mut().insert(file.to_string(), rc.clone());
        Ok(rc)
    }

    /// Indexed files an import statement refers to (empty = external/unresolvable).
    pub fn import_files(&self, from: &str, imp: &ImportRow) -> Rc<Vec<String>> {
        let key = (from.to_string(), imp.module.clone(), imp.original.clone());
        if let Some(v) = self.files_cache.borrow().get(&key) {
            return v.clone();
        }
        let fam = Lang::from_path(from).map(|l| l.family());
        let mut v: Vec<String> = match fam {
            Some(Family::Python) => {
                let mut v = self.ix.py_module(from, &imp.module);
                if let (Some(o), false) = (imp.original.as_deref(), imp.wildcard) {
                    let sub = if imp.module.ends_with('.') {
                        format!("{}{}", imp.module, o)
                    } else {
                        format!("{}.{}", imp.module, o)
                    };
                    v.extend(self.ix.py_module(from, &sub));
                }
                v
            }
            Some(Family::Ts) => self.ix.ts_module(from, &imp.module, &self.aliases),
            Some(Family::Rust) => {
                let mut v = self.ix.rs_module(&imp.module);
                if v.is_empty() && imp.original.is_some() {
                    if let Some((parent, _)) = imp.module.rsplit_once("::") {
                        v = self.ix.rs_module(parent);
                    }
                }
                v
            }
            Some(Family::Go) => self.ix.go_module(&imp.module),
            Some(Family::Java) => {
                let mut v = self.ix.java_module(&imp.module, imp.wildcard);
                if v.is_empty() && !imp.wildcard {
                    if let Some((p, _)) = imp.module.rsplit_once('.') {
                        v = self.ix.java_module(p, false);
                    }
                }
                v
            }
            Some(Family::CSharp) => vec![],
            Some(Family::C) => self.ix.c_include(from, &imp.module),
            Some(Family::Php) => {
                if imp.module.contains('\\') || !imp.wildcard {
                    self.ix.php_module(&imp.module)
                } else {
                    self.ix.php_include(from, &imp.module)
                }
            }
            Some(Family::Ruby) => self.ix.ruby_module(from, &imp.module),
            None => vec![],
        };
        v.sort();
        v.dedup();
        let rc = Rc::new(v);
        self.files_cache.borrow_mut().insert(key, rc.clone());
        rc
    }

    /// Find `name` among `files`, following re-exports (barrels, `__init__`, `pub use`).
    fn lookup_in_files(&self, files: &[String], name: &str, kinds: &[&str], depth: u8) -> Result<Vec<SymRow>> {
        let cands = self.by_name(name)?;
        let direct: Vec<SymRow> = cands
            .iter()
            .filter(|s| files.contains(&s.file) && kinds.contains(&s.kind.as_str()))
            .cloned()
            .collect();
        if !direct.is_empty() || depth >= MAX_REEXPORT_DEPTH {
            return Ok(direct);
        }
        let mut out = Vec::new();
        for f in files {
            let imps = self.imports_of(f)?;
            for imp in imps.iter() {
                if imp.wildcard {
                    let fs = self.import_files(f, imp);
                    if !fs.is_empty() {
                        out.extend(self.lookup_in_files(&fs, name, kinds, depth + 1)?);
                    }
                } else if imp.local == name {
                    let orig = match imp.original.as_deref() {
                        Some("default") | None => name,
                        Some(o) => o,
                    };
                    let fs = self.import_files(f, imp);
                    if !fs.is_empty() {
                        out.extend(self.lookup_in_files(&fs, orig, kinds, depth + 1)?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Resolve one reference. `rkind` is the ref kind ("call", "path_call" or "type").
    pub fn resolve(
        &self,
        file: &str,
        name: &str,
        qualifier: Option<&str>,
        rkind: &str,
    ) -> Result<Vec<(SymRow, Confidence)>> {
        let fam = Lang::from_path(file).map(|l| l.family());
        let is_type = rkind == "type";
        match qualifier {
            None => self.resolve_plain(file, name, fam, is_type),
            Some(q) => self.resolve_qualified(file, name, q, fam, is_type),
        }
    }

    fn resolve_plain(
        &self,
        file: &str,
        name: &str,
        fam: Option<Family>,
        is_type: bool,
    ) -> Result<Vec<(SymRow, Confidence)>> {
        let cands = self.by_name(name)?;
        let kinds: &[&str] = if is_type { &T_KINDS } else { &U_KINDS };
        let ok = |s: &&SymRow| kinds.contains(&s.kind.as_str());

        // Same file. Languages with implicit `this` may call sibling methods unqualified.
        let implicit_this = !is_type
            && matches!(fam, Some(Family::Java) | Some(Family::CSharp) | Some(Family::C) | Some(Family::Ruby));
        let same: Vec<&SymRow> = cands
            .iter()
            .filter(|s| s.file == file)
            .filter(|s| ok(s) || (implicit_this && s.kind == "method"))
            .collect();
        if !same.is_empty() {
            return Ok(same.into_iter().take(5).map(|s| (s.clone(), Confidence::Exact)).collect());
        }
        if matches!(fam, Some(Family::Go) | Some(Family::Java)) {
            let dir = split_dir(file).0;
            let pkg: Vec<&SymRow> = cands
                .iter()
                .filter(|s| {
                    split_dir(&s.file).0 == dir
                        && Lang::from_path(&s.file).map(|l| l.family()) == fam
                })
                .filter(ok)
                .collect();
            if !pkg.is_empty() {
                return Ok(pkg.into_iter().take(5).map(|s| (s.clone(), Confidence::Exact)).collect());
            }
        }

        let imps = self.imports_of(file)?;
        for imp in imps.iter().filter(|i| !i.wildcard && i.local == name) {
            let files = self.import_files(file, imp);
            let orig = match imp.original.as_deref() {
                Some("default") | None => name,
                Some(o) => o,
            };
            let hits = self.lookup_in_files(&files, orig, kinds, 0)?;
            if !hits.is_empty() {
                return Ok(exact_all(hits));
            }
            let ocands = self.by_name(orig)?;
            let g: Vec<&SymRow> = ocands.iter().filter(ok).collect();
            return Ok(rank_global(g, Confidence::Probable));
        }

        let wildcard_conf = if fam == Some(Family::C) { Confidence::Exact } else { Confidence::Probable };
        for imp in imps.iter().filter(|i| i.wildcard) {
            let files = self.import_files(file, imp);
            if files.is_empty() {
                continue;
            }
            let hits = self.lookup_in_files(&files, name, kinds, 0)?;
            if !hits.is_empty() {
                return Ok(hits.into_iter().take(5).map(|s| (s, wildcard_conf)).collect());
            }
        }

        let g: Vec<&SymRow> = cands.iter().filter(ok).collect();
        Ok(rank_global(g, Confidence::Probable))
    }

    fn resolve_qualified(
        &self,
        file: &str,
        name: &str,
        q: &str,
        fam: Option<Family>,
        is_type: bool,
    ) -> Result<Vec<(SymRow, Confidence)>> {
        let cands = self.by_name(name)?;
        let kinds: &[&str] = if is_type { &T_KINDS } else { &Q_KINDS };
        let ok = |s: &&SymRow| kinds.contains(&s.kind.as_str());
        let ty = q.rsplit(|c| c == '.' || c == ':' || c == '\\').next().unwrap_or(q);

        if !is_type && matches!(q, "self" | "this" | "cls" | "Self" | "super" | "parent") {
            let m: Vec<&SymRow> = cands
                .iter()
                .filter(|s| s.file == file && s.kind == "method")
                .collect();
            if !m.is_empty() {
                let conf = if m.len() == 1 { Confidence::Exact } else { Confidence::Probable };
                return Ok(m.into_iter().take(5).map(|s| (s.clone(), conf)).collect());
            }
        }

        let first = q.split(|c| c == '.' || c == ':' || c == '\\').next().unwrap_or(q);
        let imps = self.imports_of(file)?;
        for imp in imps.iter().filter(|i| !i.wildcard && (i.local == q || i.local == first)) {
            let files = self.import_files(file, imp);
            let hits: Vec<SymRow> = if is_type {
                self.lookup_in_files(&files, name, kinds, 0)?
            } else {
                let direct: Vec<SymRow> = cands
                    .iter()
                    .filter(|s| files.contains(&s.file) && kinds.contains(&s.kind.as_str()))
                    .cloned()
                    .collect();
                if !direct.is_empty() {
                    direct
                } else if let Some(o) = imp.original.as_deref().filter(|o| *o != "default") {
                    // `Foo.method()` where `Foo` was imported through a barrel.
                    let cls = self.lookup_in_files(&files, o, &T_KINDS, 0)?;
                    let cfiles: Vec<String> = cls.iter().map(|s| s.file.clone()).collect();
                    cands
                        .iter()
                        .filter(|s| cfiles.contains(&s.file) && kinds.contains(&s.kind.as_str()))
                        .cloned()
                        .collect()
                } else {
                    vec![]
                }
            };
            if !hits.is_empty() {
                let refs: Vec<&SymRow> = hits.iter().collect();
                let narrowed = narrow_by_type(refs, ty);
                return Ok(narrowed
                    .into_iter()
                    .take(5)
                    .map(|s| (s.clone(), Confidence::Exact))
                    .collect());
            }
            if files.is_empty() {
                // Library call: not part of this codebase.
                return Ok(vec![]);
            }
        }

        if fam == Some(Family::Rust) && !is_type {
            let files = self.ix.rs_module(q);
            if !files.is_empty() {
                let hits: Vec<&SymRow> = cands
                    .iter()
                    .filter(|s| files.contains(&s.file))
                    .filter(ok)
                    .collect();
                if !hits.is_empty() {
                    return Ok(hits.into_iter().take(5).map(|s| (s.clone(), Confidence::Exact)).collect());
                }
            }
        }

        let prefix = format!("{ty}.");
        let local: Vec<&SymRow> = cands
            .iter()
            .filter(|s| s.file == file && (is_type || s.qualname.starts_with(&prefix)))
            .filter(ok)
            .collect();
        if !local.is_empty() {
            return Ok(local.into_iter().take(5).map(|s| (s.clone(), Confidence::Exact)).collect());
        }

        if is_type {
            let g: Vec<&SymRow> = cands.iter().filter(ok).collect();
            return Ok(rank_global(g, Confidence::Probable));
        }

        // Unknown receiver: only methods are plausible targets.
        let methods: Vec<&SymRow> = cands.iter().filter(|s| s.kind == "method").collect();
        let typed = narrow_by_type(methods.clone(), ty);
        let narrowed = typed.len() < methods.len() || (typed.len() == 1 && methods.len() == 1);
        let conf = if q == "?" && !narrowed { Confidence::Heuristic } else { Confidence::Probable };
        Ok(rank_global(typed, conf))
    }

    // ------------------------------------------------------- graph queries

    pub fn callers(&self, target: &SymRow) -> Result<(Vec<RefHit>, bool)> {
        const RAW_SQL: &str = "SELECT r.file,r.name,r.qualifier,r.kind,r.line,r.arg_count,s.qualname,r.kwargs
             FROM refs r LEFT JOIN symbols s ON s.id=r.enclosing_id";
        let mut raw: Vec<RawRef> = Vec::new();
        {
            let mut st = self
                .conn
                .prepare_cached(&format!("{RAW_SQL} WHERE r.name=?1 LIMIT ?2"))?;
            let rows = st.query_map(params![target.name, MAX_CANDIDATES as i64 + 1], map_raw)?;
            for r in rows {
                raw.push(r?);
            }
        }
        let aliases: Vec<(String, String)> = {
            let mut st = self.conn.prepare_cached(
                "SELECT file,local FROM imports WHERE original=?1 AND local<>original AND wildcard=0 LIMIT 500",
            )?;
            let rows = st.query_map([&target.name], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (file, local) in aliases {
            let mut st = self
                .conn
                .prepare_cached(&format!("{RAW_SQL} WHERE r.file=?1 AND r.name=?2 LIMIT 500"))?;
            let rows = st.query_map(params![file, local], map_raw)?;
            for r in rows {
                raw.push(r?);
            }
        }
        let truncated = raw.len() > MAX_CANDIDATES;
        raw.truncate(MAX_CANDIDATES);

        let overloadable = Lang::from_path(&target.file).map_or(false, |l| l.overloadable());
        let mut hits: Vec<RefHit> = Vec::new();
        for r in raw {
            let res = self.resolve(&r.file, &r.name, r.qualifier.as_deref(), &r.kind)?;
            let Some((_, conf)) = res
                .iter()
                .find(|(s, _)| s.file == target.file && s.qualname == target.qualname)
            else {
                continue;
            };
            // Overloads share a qualname; keep only the one whose arity accepts the call.
            if overloadable && r.kind != "type" {
                if let (Some(ps), Some(n)) = (&target.params, r.arg_count) {
                    let (req, max) = arity(ps);
                    let n = n as usize;
                    if n < req || max.map_or(false, |m| n > m) {
                        continue;
                    }
                }
            }
            hits.push(RefHit {
                file: r.file,
                line: r.line,
                usage: if r.kind == "type" { "type" } else { "call" },
                kind: r.kind,
                qualifier: r.qualifier,
                arg_count: r.arg_count,
                kwargs: r.kwargs,
                enclosing: r.enclosing,
                confidence: *conf,
            });
        }

        // Import statements (and re-exports) that name the symbol.
        let imports: Vec<(String, ImportRow)> = {
            let mut st = self.conn.prepare_cached(
                "SELECT file,local,module,original,wildcard,line FROM imports WHERE original=?1 AND wildcard=0 LIMIT 2000",
            )?;
            let rows = st.query_map([&target.name], |r| {
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
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (file, imp) in imports {
            let files = self.import_files(&file, &imp);
            if files.contains(&target.file) {
                hits.push(RefHit {
                    file,
                    line: imp.line,
                    usage: "import",
                    kind: String::new(),
                    qualifier: None,
                    arg_count: None,
                    kwargs: None,
                    enclosing: None,
                    confidence: Confidence::Exact,
                });
            }
        }

        hits.sort_by(|a, b| {
            a.confidence
                .cmp(&b.confidence)
                .then(a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        });
        hits.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.usage == b.usage);
        Ok((hits, truncated))
    }

    pub fn callees(&self, sym: &SymRow) -> Result<Vec<CalleeHit>> {
        let mut st = self.conn.prepare_cached(
            "SELECT name,qualifier,kind,line FROM refs
             WHERE kind<>'type' AND enclosing_id IN
                 (SELECT id FROM symbols WHERE file=?1 AND start_line>=?2 AND end_line<=?3)
             ORDER BY line LIMIT 1000",
        )?;
        let rows: Vec<(String, Option<String>, String, u32)> = st
            .query_map(params![sym.file, sym.start_line, sym.end_line], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
        let mut out = Vec::new();
        for (name, qual, kind, line) in rows {
            if !seen.insert((name.clone(), qual.clone())) {
                continue;
            }
            let mut targets = self.resolve(&sym.file, &name, qual.as_deref(), &kind)?;
            targets.truncate(3);
            if targets.is_empty() {
                continue;
            }
            out.push(CalleeHit { name, line, targets });
        }
        Ok(out)
    }
}

fn map_raw(r: &Row<'_>) -> rusqlite::Result<RawRef> {
    Ok(RawRef {
        file: r.get(0)?,
        name: r.get(1)?,
        qualifier: r.get(2)?,
        kind: r.get(3)?,
        line: r.get(4)?,
        arg_count: r.get(5)?,
        enclosing: r.get(6)?,
        kwargs: r.get(7)?,
    })
}
