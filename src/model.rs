use serde::{Deserialize, Serialize};

/// How much a resolved edge can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Resolved through same-file scope or an import that points at the defining file.
    Exact,
    /// Unique match by name across the repo, or import resolved only by name.
    Probable,
    /// Several candidates share the name; receiver type unknown.
    Heuristic,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Probable => "probable",
            Confidence::Heuristic => "heuristic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub optional: bool,
    pub variadic: bool,
    pub kw_only: bool,
}

/// (minimum, maximum) number of arguments a call may pass. `None` max = unbounded.
pub fn arity(params: &[Param]) -> (usize, Option<usize>) {
    let required = params.iter().filter(|p| !p.optional && !p.variadic).count();
    let unbounded = params.iter().any(|p| p.variadic || p.name.starts_with("**"));
    let max = if unbounded { None } else { Some(params.len()) };
    (required, max)
}

#[derive(Debug, Clone)]
pub struct SymbolRec {
    pub name: String,
    pub qualname: String,
    pub kind: String,
    pub signature: String,
    pub doc: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub depth: u32,
    pub parent: Option<usize>,
    pub exported: bool,
    pub params: Option<Vec<Param>>,
    pub has_self: bool,
}

#[derive(Debug, Clone)]
pub struct RefRec {
    pub name: String,
    pub qualifier: Option<String>,
    /// "call", "path_call" (Rust `Type::f(..)` style) or "type" (type/inheritance reference).
    pub kind: &'static str,
    pub line: u32,
    pub arg_count: Option<u32>,
    pub enclosing: Option<usize>,
    /// Python keyword-argument names used at the call site (comma separated).
    pub kwargs: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImportRec {
    pub local: String,
    pub module: String,
    pub original: Option<String>,
    pub wildcard: bool,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct FileData {
    pub path: String,
    pub lang: String,
    pub hash: String,
    pub mtime: i64,
    pub size: i64,
    pub lines: u32,
    pub symbols: Vec<SymbolRec>,
    pub refs: Vec<RefRec>,
    pub imports: Vec<ImportRec>,
}

pub fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
