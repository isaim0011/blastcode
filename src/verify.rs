//! Pre-flight AST validation and call-site arity checks for proposed edits.

use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::json;
use tree_sitter::{Node, Parser};

use crate::extract;
use crate::lang::Lang;
use crate::model::arity;

#[derive(Debug, serde::Serialize)]
pub struct SyntaxError {
    pub line: u32,
    pub col: u32,
    pub message: String,
    pub snippet: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ArityMismatch {
    pub symbol: String,
    pub call_line: u32,
    pub passed_args: u32,
    pub expected_min: usize,
    pub expected_max: Option<usize>,
    pub defined_in: String,
    pub message: String,
}

fn collect_syntax_errors(node: Node, src: &[u8], errors: &mut Vec<SyntaxError>, max: usize) {
    if errors.len() >= max {
        return;
    }
    if node.is_error() || node.is_missing() {
        let start = node.start_position();
        let line = (start.row + 1) as u32;
        let col = (start.column + 1) as u32;
        let bytes = &src[node.start_byte()..node.end_byte()];
        let snippet = String::from_utf8_lossy(bytes).trim().to_string();
        let msg = if node.is_missing() {
            format!("missing expected token '{}'", node.kind())
        } else {
            "syntax error".to_string()
        };
        errors.push(SyntaxError {
            line,
            col,
            message: msg,
            snippet: if snippet.is_empty() { None } else { Some(snippet) },
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_syntax_errors(child, src, errors, max);
    }
}

pub fn verify_patch(
    conn: &Connection,
    _root: &Path,
    file_path: &str,
    content: &str,
) -> Result<String> {
    let norm_path = file_path.replace('\\', "/").trim_start_matches("./").to_string();
    let Some(lang) = Lang::from_path(&norm_path) else {
        return Ok(serde_json::to_string(&json!({
            "file": norm_path,
            "valid": true,
            "note": "file extension not in supported 10-language AST grammar set; skipping AST verification"
        }))?);
    };

    let src = content.as_bytes();
    let mut parser = Parser::new();
    parser
        .set_language(&lang.ts_language())
        .map_err(|e| anyhow!("loading grammar: {e}"))?;

    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("tree-sitter parser failed to produce AST"))?;

    // 1. Syntax Error Extraction
    let mut syntax_errors = Vec::new();
    collect_syntax_errors(tree.root_node(), src, &mut syntax_errors, 10);

    // 2. Symbol & Reference Extraction
    let parsed_res = extract::parse(lang, src);
    let mut arity_mismatches = Vec::new();
    let mut symbols_count = 0;
    let mut calls_count = 0;

    if let Ok(parsed) = parsed_res {
        symbols_count = parsed.symbols.len();
        calls_count = parsed.refs.len();

        let mut st_qual = conn.prepare(
            "SELECT file, qualname, params, has_self FROM symbols
             WHERE (qualname=?1 OR qualname LIKE ?2) AND kind IN ('function', 'method', 'constructor') LIMIT 5",
        )?;
        let mut st_bare = conn.prepare(
            "SELECT file, name, params, has_self FROM symbols
             WHERE name=?1 AND depth<=1 AND kind IN ('function', 'method', 'constructor') LIMIT 5",
        )?;

        for r in &parsed.refs {
            if r.kind != "call" && r.kind != "path_call" {
                continue;
            }
            let Some(passed) = r.arg_count else {
                continue;
            };

            let rows: Vec<(String, String, Option<String>, bool)> = if let Some(ref q) = r.qualifier {
                let exact_qual = format!("{q}.{}", r.name);
                let suffix_qual = format!("%.{q}.{}", r.name);
                let mapped = st_qual.query_map([&exact_qual, &suffix_qual], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                let mapped = st_bare.query_map([&r.name], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for item in rows {
                let (def_file, sym_name, params_json, _has_self) = item;
                let Some(pj) = params_json else {
                    continue;
                };
                let Ok(params) = serde_json::from_str::<Vec<crate::model::Param>>(&pj) else {
                    continue;
                };
                if params.is_empty() {
                    continue;
                }
                let (min, max) = arity(&params);
                let passed_usize = passed as usize;
                if passed_usize < min || max.map_or(false, |m| passed_usize > m) {
                    let max_str = max.map_or("∞".to_string(), |m| m.to_string());
                    arity_mismatches.push(ArityMismatch {
                        symbol: sym_name.clone(),
                        call_line: r.line,
                        passed_args: passed,
                        expected_min: min,
                        expected_max: max,
                        defined_in: def_file.clone(),
                        message: format!(
                            "Call to '{sym_name}' on line {} passes {passed} argument(s), but definition in '{def_file}' expects {min}..{max_str}",
                            r.line
                        ),
                    });
                    break;
                }
            }
        }
    }

    let is_valid = syntax_errors.is_empty() && arity_mismatches.is_empty();
    let summary = if is_valid {
        format!("Patch is clean: valid syntax ({symbols_count} symbols, {calls_count} calls), zero arity mismatches.")
    } else {
        format!(
            "Issues found: {} syntax errors, {} arity mismatches.",
            syntax_errors.len(),
            arity_mismatches.len()
        )
    };

    Ok(serde_json::to_string(&json!({
        "file": norm_path,
        "valid": is_valid,
        "syntax_errors": syntax_errors,
        "arity_mismatches": arity_mismatches,
        "symbols_extracted": symbols_count,
        "calls_extracted": calls_count,
        "summary": summary
    }))?)
}
