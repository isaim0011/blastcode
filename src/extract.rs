//! Tree-sitter based extraction of symbols, call references and imports.
//!
//! One generic walker drives per-language visitors. A visitor returns
//! `Some(index)` when the node declared a symbol (children are then nested
//! under it) and `None` otherwise.

use anyhow::{anyhow, Result};
use tree_sitter::{Node, Parser};

use crate::lang::{Family, Lang};
use crate::model::*;

pub struct Parsed {
    pub symbols: Vec<SymbolRec>,
    pub refs: Vec<RefRec>,
    pub imports: Vec<ImportRec>,
    pub lines: u32,
}

/// Receivers that are always JS/TS globals; calls on them are never project code.
const JS_GLOBALS: &[&str] = &[
    "console", "Math", "JSON", "Object", "Array", "Promise", "Number", "String", "Date", "Reflect",
    "Symbol", "process",
];

pub fn parse(lang: Lang, src: &[u8]) -> Result<Parsed> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.ts_language())
        .map_err(|e| anyhow!("loading grammar: {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("parser returned no tree"))?;
    let mut ex = Ex {
        src,
        lang,
        out: Parsed {
            symbols: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            lines: 0,
        },
    };
    ex.walk(tree.root_node(), None, 0);
    let mut out = ex.out;
    out.lines = if src.is_empty() {
        0
    } else {
        let nl = src.iter().filter(|b| **b == b'\n').count() as u32;
        if src.last() == Some(&b'\n') {
            nl
        } else {
            nl + 1
        }
    };
    Ok(out)
}

struct Ex<'a> {
    src: &'a [u8],
    lang: Lang,
    out: Parsed,
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| cap(l, 140))
}

impl<'a> Ex<'a> {
    // ---------------------------------------------------------------- helpers

    fn t(&self, n: Node<'_>) -> &'a str {
        n.utf8_text(self.src).unwrap_or("")
    }

    fn line(n: Node<'_>) -> u32 {
        n.start_position().row as u32 + 1
    }

    /// Text of a receiver expression, or "?" if it is not a simple name/path.
    fn qual(&self, n: Option<Node<'_>>) -> String {
        match n {
            Some(n) => {
                let t = self.t(n);
                if t.len() <= 80 && !t.contains(|c: char| c == '(' || c == '[' || c.is_whitespace())
                {
                    t.to_string()
                } else {
                    "?".to_string()
                }
            }
            None => "?".to_string(),
        }
    }

    fn sig(&self, node: Node<'_>, body: Option<Node<'_>>) -> String {
        let start = node.start_byte();
        let end = body.map_or(node.end_byte(), |b| b.start_byte()).max(start);
        let raw = String::from_utf8_lossy(&self.src[start..end]);
        let c = collapse(&raw);
        if body.is_none() {
            cap(&c, 200)
        } else {
            c
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        name: &str,
        kind: &str,
        node: Node<'_>,
        sig: String,
        doc: Option<String>,
        parent: Option<usize>,
        exported: bool,
        params: Option<Vec<Param>>,
        has_self: bool,
    ) -> usize {
        let (qualname, depth) = match parent {
            Some(p) => (
                format!("{}.{}", self.out.symbols[p].qualname, name),
                self.out.symbols[p].depth + 1,
            ),
            None => (name.to_string(), 0),
        };
        self.out.symbols.push(SymbolRec {
            name: name.to_string(),
            qualname,
            kind: kind.to_string(),
            signature: cap(&sig, 400),
            doc,
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            depth,
            parent,
            exported,
            params,
            has_self,
        });
        self.out.symbols.len() - 1
    }

    fn add_ref(
        &mut self,
        name: &str,
        qualifier: Option<String>,
        kind: &'static str,
        node: Node<'_>,
        arg_count: Option<u32>,
        enclosing: Option<usize>,
    ) {
        if name.is_empty() || name.len() > 100 {
            return;
        }
        self.out.refs.push(RefRec {
            name: name.to_string(),
            qualifier,
            kind,
            line: Self::line(node),
            arg_count,
            enclosing,
            kwargs: None,
        });
    }

    fn add_import(
        &mut self,
        local: &str,
        module: &str,
        original: Option<&str>,
        wildcard: bool,
        line: u32,
    ) {
        self.out.imports.push(ImportRec {
            local: local.to_string(),
            module: module.to_string(),
            original: original.map(|s| s.to_string()),
            wildcard,
            line,
        });
    }

    fn count_args(&self, args: Option<Node<'_>>, splat_kinds: &[&str]) -> Option<u32> {
        let a = args?;
        if a.kind() == "generator_expression" {
            return Some(1);
        }
        if a.kind() != "arguments" && a.kind() != "argument_list" {
            return None;
        }
        let mut n = 0u32;
        let mut c = a.walk();
        for ch in a.named_children(&mut c) {
            let k = ch.kind();
            if k.contains("comment") {
                continue;
            }
            if splat_kinds.contains(&k) {
                return None;
            }
            n += 1;
        }
        Some(n)
    }

    /// Doc comment immediately preceding `node` (first line only).
    fn leading_doc(&self, node: Node<'_>) -> Option<String> {
        let mut first = None;
        let mut cur = node.prev_sibling();
        let mut expect_row = node.start_position().row;
        while let Some(p) = cur {
            if p.kind() == "attribute_item" {
                expect_row = p.start_position().row;
                cur = p.prev_sibling();
                continue;
            }
            if p.kind().contains("comment") && p.end_position().row + 1 >= expect_row {
                first = Some(p);
                expect_row = p.start_position().row;
                cur = p.prev_sibling();
            } else {
                break;
            }
        }
        let c = first?;
        let raw = self.t(c);
        let line = raw
            .lines()
            .map(|l| {
                l.trim()
                    .trim_start_matches("///")
                    .trim_start_matches("//!")
                    .trim_start_matches("//")
                    .trim_start_matches("/**")
                    .trim_start_matches("/*")
                    .trim_start_matches('*')
                    .trim_start_matches('#')
                    .trim()
                    .trim_end_matches("*/")
                    .trim()
            })
            .find(|l| !l.is_empty())?;
        Some(cap(line, 140))
    }

    // ---------------------------------------------------------------- walker

    fn walk(&mut self, node: Node<'_>, parent: Option<usize>, depth: usize) {
        if depth > 400 {
            return;
        }
        let declared = match self.lang.family() {
            Family::Python => self.py(node, parent),
            Family::Ts => self.ts(node, parent),
            Family::Rust => self.rs(node, parent),
            Family::Go => self.go(node, parent),
            Family::Java => self.java(node, parent),
            Family::CSharp => self.csharp(node, parent),
            Family::C => self.c_like(node, parent),
            Family::Php => self.php(node, parent),
            Family::Ruby => self.ruby(node, parent),
        };
        let np = declared.or(parent);
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            self.walk(ch, np, depth + 1);
        }
    }

    // ---------------------------------------------------------------- Python

    fn py(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_definition" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let in_class = parent.map_or(false, |p| self.out.symbols[p].kind == "class");
                let mut params = self.py_params(node.child_by_field_name("parameters"));
                let mut has_self = false;
                if in_class {
                    if let Some(f) = params.first() {
                        if f.name == "self" || f.name == "cls" {
                            has_self = true;
                            params.remove(0);
                        }
                    }
                }
                let body = node.child_by_field_name("body");
                let mut sig = String::new();
                if let Some(p) = node.parent() {
                    if p.kind() == "decorated_definition" {
                        let mut c = p.walk();
                        for ch in p.children(&mut c) {
                            if ch.kind() == "decorator" {
                                sig.push_str(&collapse(self.t(ch)));
                                sig.push('\n');
                            }
                        }
                    }
                }
                let head = self.sig(node, body);
                sig.push_str(head.trim_end_matches(':').trim_end());
                let doc = body.and_then(|b| self.py_doc(b));
                let exported = !name.starts_with('_') || (name.starts_with("__") && name.ends_with("__"));
                let kind = if in_class { "method" } else { "function" };
                Some(self.add(&name, kind, node, sig, doc, parent, exported, Some(params), has_self))
            }
            "class_definition" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let body = node.child_by_field_name("body");
                let mut sig = String::new();
                if let Some(p) = node.parent() {
                    if p.kind() == "decorated_definition" {
                        let mut c = p.walk();
                        for ch in p.children(&mut c) {
                            if ch.kind() == "decorator" {
                                sig.push_str(&collapse(self.t(ch)));
                                sig.push('\n');
                            }
                        }
                    }
                }
                let head = self.sig(node, body);
                sig.push_str(head.trim_end_matches(':').trim_end());
                let doc = body.and_then(|b| self.py_doc(b));
                let exported = !name.starts_with('_');
                let idx = self.add(&name, "class", node, sig, doc, parent, exported, None, false);
                self.py_bases(node, parent);
                Some(idx)
            }
            "type" => {
                self.py_type_refs(node, parent);
                None
            }
            "call" => {
                self.py_call(node, parent);
                None
            }
            "decorator" => {
                if let Some(e) = node.named_child(0) {
                    match e.kind() {
                        "identifier" => {
                            let n = self.t(e).to_string();
                            self.add_ref(&n, None, "call", node, None, parent);
                        }
                        "attribute" => {
                            if let Some(a) = e.child_by_field_name("attribute") {
                                let n = self.t(a).to_string();
                                let q = self.qual(e.child_by_field_name("object"));
                                self.add_ref(&n, Some(q), "call", node, None, parent);
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            "import_statement" | "import_from_statement" => {
                self.py_import(node);
                None
            }
            _ => None,
        }
    }

    fn py_doc(&self, body: Node<'_>) -> Option<String> {
        let first = body.named_child(0)?;
        if first.kind() != "expression_statement" {
            return None;
        }
        let s = first.named_child(0)?;
        if s.kind() != "string" {
            return None;
        }
        let raw = self.t(s);
        let trimmed = raw
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_matches(|c| c == '"' || c == '\'');
        first_line(trimmed)
    }

    fn py_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut kw_only = false;
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "identifier" => out.push(Param {
                    name: self.t(ch).to_string(),
                    optional: false,
                    variadic: false,
                    kw_only,
                }),
                "default_parameter" | "typed_default_parameter" => {
                    let name = ch
                        .child_by_field_name("name")
                        .map(|n| self.t(n).to_string())
                        .unwrap_or_default();
                    out.push(Param { name, optional: true, variadic: false, kw_only });
                }
                "typed_parameter" => {
                    let inner = ch.named_child(0);
                    match inner.map(|n| n.kind()) {
                        Some("list_splat_pattern") => {
                            out.push(Param {
                                name: self.t(inner.unwrap()).trim_start_matches('*').to_string(),
                                optional: true,
                                variadic: true,
                                kw_only: false,
                            });
                            kw_only = true;
                        }
                        Some("dictionary_splat_pattern") => out.push(Param {
                            name: format!("**{}", self.t(inner.unwrap()).trim_start_matches('*')),
                            optional: true,
                            variadic: false,
                            kw_only: true,
                        }),
                        Some(_) => out.push(Param {
                            name: self.t(inner.unwrap()).to_string(),
                            optional: false,
                            variadic: false,
                            kw_only,
                        }),
                        None => {}
                    }
                }
                "list_splat_pattern" => {
                    out.push(Param {
                        name: self.t(ch).trim_start_matches('*').to_string(),
                        optional: true,
                        variadic: true,
                        kw_only: false,
                    });
                    kw_only = true;
                }
                "dictionary_splat_pattern" => out.push(Param {
                    name: format!("**{}", self.t(ch).trim_start_matches('*')),
                    optional: true,
                    variadic: false,
                    kw_only: true,
                }),
                "keyword_separator" => kw_only = true,
                _ => {}
            }
        }
        out
    }

    fn py_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let (name, qual) = match f.kind() {
            "identifier" => (self.t(f).to_string(), None),
            "attribute" => {
                let Some(a) = f.child_by_field_name("attribute") else { return };
                (self.t(a).to_string(), Some(self.qual(f.child_by_field_name("object"))))
            }
            _ => return,
        };
        let args = self.count_args(
            node.child_by_field_name("arguments"),
            &["list_splat", "dictionary_splat"],
        );
        self.add_ref(&name, qual, "call", node, args, enclosing);
        let kw = self.py_kwargs(node.child_by_field_name("arguments"));
        if kw.is_some() {
            if let Some(r) = self.out.refs.last_mut() {
                if r.name == name {
                    r.kwargs = kw;
                }
            }
        }
    }

    fn py_import(&mut self, node: Node<'_>) {
        let line = Self::line(node);
        if node.kind() == "import_statement" {
            let mut c = node.walk();
            for n in node.children_by_field_name("name", &mut c) {
                match n.kind() {
                    "dotted_name" => {
                        let m = self.t(n).to_string();
                        self.add_import(&m, &m, None, false, line);
                    }
                    "aliased_import" => {
                        if let (Some(nm), Some(al)) =
                            (n.child_by_field_name("name"), n.child_by_field_name("alias"))
                        {
                            let (m, a) = (self.t(nm).to_string(), self.t(al).to_string());
                            self.add_import(&a, &m, None, false, line);
                        }
                    }
                    _ => {}
                }
            }
        } else {
            let Some(mn) = node.child_by_field_name("module_name") else { return };
            let module = self.t(mn).to_string();
            let mut c = node.walk();
            for n in node.children_by_field_name("name", &mut c) {
                match n.kind() {
                    "dotted_name" => {
                        let o = self.t(n).to_string();
                        self.add_import(&o, &module, Some(o.as_str()), false, line);
                    }
                    "aliased_import" => {
                        if let (Some(nm), Some(al)) =
                            (n.child_by_field_name("name"), n.child_by_field_name("alias"))
                        {
                            let (o, a) = (self.t(nm).to_string(), self.t(al).to_string());
                            self.add_import(&a, &module, Some(o.as_str()), false, line);
                        }
                    }
                    _ => {}
                }
            }
            let mut c2 = node.walk();
            for ch in node.children(&mut c2) {
                if ch.kind() == "wildcard_import" {
                    self.add_import("*", &module, None, true, line);
                }
            }
        }
    }

    // ---------------------------------------------------- TypeScript / JavaScript

    fn ts_exported(node: Node<'_>) -> bool {
        node.parent().map_or(false, |p| p.kind() == "export_statement")
    }

    fn ts(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_declaration" | "generator_function_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let params = self.ts_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let exported = Self::ts_exported(node);
                let mut sig = self.sig(node, body);
                if exported {
                    sig = format!("export {sig}");
                }
                let top = if exported { node.parent().unwrap_or(node) } else { node };
                let doc = self.leading_doc(top);
                Some(self.add(&name, "function", node, sig, doc, parent, exported, Some(params), false))
            }
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
            | "enum_declaration" | "type_alias_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = match node.kind() {
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    "type_alias_declaration" => "type",
                    _ => "class",
                };
                let body = node.child_by_field_name("body");
                let exported = Self::ts_exported(node);
                let mut sig = self.sig(node, body);
                if exported {
                    sig = format!("export {sig}");
                }
                let top = if exported { node.parent().unwrap_or(node) } else { node };
                let doc = self.leading_doc(top);
                Some(self.add(&name, kind, node, sig, doc, parent, exported, None, false))
            }
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                let p = parent?;
                if !matches!(self.out.symbols[p].kind.as_str(), "class" | "interface") {
                    return None;
                }
                let nn = node.child_by_field_name("name")?;
                if nn.kind() == "computed_property_name" {
                    return None;
                }
                let name = self.t(nn).to_string();
                let params = self.ts_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = !sig.starts_with("private") && !name.starts_with('#');
                let doc = self.leading_doc(node);
                Some(self.add(&name, "method", node, sig, doc, parent, exported, Some(params), false))
            }
            "variable_declarator" => {
                self.ts_require(node);
                if parent.is_some() {
                    return None;
                }
                let name_n = node.child_by_field_name("name")?;
                if name_n.kind() != "identifier" {
                    return None;
                }
                let value = node.child_by_field_name("value")?;
                if !matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function" | "generator_function"
                ) {
                    return None;
                }
                let name = self.t(name_n).to_string();
                let params = if let Some(p) = value.child_by_field_name("parameters") {
                    self.ts_params(Some(p))
                } else if let Some(p) = value.child_by_field_name("parameter") {
                    vec![Param {
                        name: self.t(p).to_string(),
                        optional: false,
                        variadic: false,
                        kw_only: false,
                    }]
                } else {
                    Vec::new()
                };
                let body = value.child_by_field_name("body");
                let decl = node.parent().unwrap_or(node);
                let exported = decl.parent().map_or(false, |p| p.kind() == "export_statement");
                let mut sig = self.sig(decl, body);
                if exported {
                    sig = format!("export {sig}");
                }
                let top = if exported { decl.parent().unwrap_or(decl) } else { decl };
                let doc = self.leading_doc(top);
                Some(self.add(&name, "function", decl, sig, doc, parent, exported, Some(params), false))
            }
            "call_expression" => {
                self.ts_call(node, parent);
                None
            }
            "new_expression" => {
                if let Some(c) = node.child_by_field_name("constructor") {
                    let (name, qual) = match c.kind() {
                        "identifier" => (self.t(c).to_string(), None),
                        "member_expression" => {
                            let Some(p) = c.child_by_field_name("property") else { return None };
                            (self.t(p).to_string(), Some(self.qual(c.child_by_field_name("object"))))
                        }
                        _ => return None,
                    };
                    let args = self.count_args(node.child_by_field_name("arguments"), &["spread_element"]);
                    self.add_ref(&name, qual, "call", node, args, parent);
                }
                None
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(n) = node.child_by_field_name("name") {
                    if n.kind() == "identifier" {
                        let name = self.t(n).to_string();
                        if name.chars().next().map_or(false, |c| c.is_uppercase()) {
                            self.add_ref(&name, None, "call", node, None, parent);
                        }
                    }
                }
                None
            }
            "export_statement" => {
                self.ts_reexport(node);
                None
            }
            "type_identifier" => {
                let skip = Self::is_decl_name(node)
                    || node.parent().map_or(false, |p| p.kind() == "nested_type_identifier");
                if !skip {
                    let n = self.t(node).to_string();
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            "nested_type_identifier" => {
                if let Some(nm) = node.child_by_field_name("name") {
                    let n = self.t(nm).to_string();
                    let q = self.qual(node.child_by_field_name("module"));
                    self.add_type_ref(&n, Some(q), node, parent);
                }
                None
            }
            "extends_clause" => {
                if let Some(v) = node.child_by_field_name("value") {
                    if v.kind() == "identifier" {
                        let n = self.t(v).to_string();
                        self.add_type_ref(&n, None, node, parent);
                    }
                }
                None
            }
            "import_statement" => {
                self.ts_import(node);
                None
            }
            _ => None,
        }
    }

    fn ts_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            let k = ch.kind();
            if k.contains("comment") {
                continue;
            }
            match k {
                "required_parameter" | "optional_parameter" => {
                    let pat = ch.child_by_field_name("pattern");
                    let pk = pat.map(|n| n.kind()).unwrap_or("");
                    if pat.map_or(false, |n| self.t(n) == "this") {
                        continue;
                    }
                    let name = pat
                        .map(|n| cap(self.t(n).trim_start_matches("..."), 30))
                        .unwrap_or_default();
                    let rest = pk == "rest_pattern";
                    let optional =
                        k == "optional_parameter" || ch.child_by_field_name("value").is_some();
                    out.push(Param { name, optional: optional && !rest, variadic: rest, kw_only: false });
                }
                "identifier" | "object_pattern" | "array_pattern" => out.push(Param {
                    name: cap(self.t(ch), 30),
                    optional: false,
                    variadic: false,
                    kw_only: false,
                }),
                "assignment_pattern" => {
                    let name = ch
                        .child_by_field_name("left")
                        .map(|n| cap(self.t(n), 30))
                        .unwrap_or_default();
                    out.push(Param { name, optional: true, variadic: false, kw_only: false });
                }
                "rest_pattern" => out.push(Param {
                    name: cap(self.t(ch).trim_start_matches("..."), 30),
                    optional: true,
                    variadic: true,
                    kw_only: false,
                }),
                _ => {}
            }
        }
        out
    }

    fn ts_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let (name, qual) = match f.kind() {
            "identifier" => (self.t(f).to_string(), None),
            "member_expression" => {
                let Some(p) = f.child_by_field_name("property") else { return };
                let q = self.qual(f.child_by_field_name("object"));
                if JS_GLOBALS.contains(&q.as_str()) {
                    return;
                }
                (self.t(p).to_string(), Some(q))
            }
            _ => return,
        };
        if name == "require" || name == "import" {
            return;
        }
        let args = self.count_args(node.child_by_field_name("arguments"), &["spread_element"]);
        self.add_ref(&name, qual, "call", node, args, enclosing);
    }

    fn ts_import(&mut self, node: Node<'_>) {
        let line = Self::line(node);
        let Some(src_n) = node.child_by_field_name("source") else { return };
        let module = self
            .t(src_n)
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string();
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            if ch.kind() != "import_clause" {
                continue;
            }
            let mut c2 = ch.walk();
            for part in ch.named_children(&mut c2) {
                match part.kind() {
                    "identifier" => {
                        let l = self.t(part).to_string();
                        self.add_import(&l, &module, Some("default"), false, line);
                    }
                    "namespace_import" => {
                        if let Some(id) = part.named_child(0) {
                            let l = self.t(id).to_string();
                            self.add_import(&l, &module, None, false, line);
                        }
                    }
                    "named_imports" => {
                        let mut c3 = part.walk();
                        for spec in part.named_children(&mut c3) {
                            if spec.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(nm) = spec.child_by_field_name("name") else { continue };
                            let orig = self.t(nm).to_string();
                            let local = spec
                                .child_by_field_name("alias")
                                .map(|a| self.t(a).to_string())
                                .unwrap_or_else(|| orig.clone());
                            self.add_import(&local, &module, Some(orig.as_str()), false, line);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// CommonJS: `const x = require('./x')` / `const { a, b: c } = require('./m')`.
    fn ts_require(&mut self, decl: Node<'_>) {
        let (Some(name_n), Some(value)) =
            (decl.child_by_field_name("name"), decl.child_by_field_name("value"))
        else {
            return;
        };
        if value.kind() != "call_expression" {
            return;
        }
        let Some(f) = value.child_by_field_name("function") else { return };
        if self.t(f) != "require" {
            return;
        }
        let Some(args) = value.child_by_field_name("arguments") else { return };
        let Some(arg) = args.named_child(0) else { return };
        if arg.kind() != "string" {
            return;
        }
        let module = self
            .t(arg)
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string();
        let line = Self::line(decl);
        match name_n.kind() {
            "identifier" => {
                let l = self.t(name_n).to_string();
                self.add_import(&l, &module, None, false, line);
            }
            "object_pattern" => {
                let mut c = name_n.walk();
                for ch in name_n.named_children(&mut c) {
                    match ch.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let l = self.t(ch).to_string();
                            self.add_import(&l, &module, Some(l.as_str()), false, line);
                        }
                        "pair_pattern" => {
                            if let (Some(k), Some(v)) =
                                (ch.child_by_field_name("key"), ch.child_by_field_name("value"))
                            {
                                let (o, l) = (self.t(k).to_string(), self.t(v).to_string());
                                self.add_import(&l, &module, Some(o.as_str()), false, line);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ Rust

    fn rs_pub(node: Node<'_>) -> bool {
        let mut c = node.walk();
        let found = node.children(&mut c).any(|ch| ch.kind() == "visibility_modifier");
        found
    }

    fn rs(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_item" | "function_signature_item" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let in_impl = parent.map_or(false, |p| {
                    matches!(self.out.symbols[p].kind.as_str(), "impl" | "trait")
                });
                let (params, has_self) = self.rs_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = Self::rs_pub(node);
                let doc = self.leading_doc(node);
                let kind = if in_impl { "method" } else { "function" };
                Some(self.add(&name, kind, node, sig, doc, parent, exported, Some(params), has_self))
            }
            "struct_item" => self.rs_type(node, parent, "struct"),
            "enum_item" => self.rs_type(node, parent, "enum"),
            "trait_item" => self.rs_type(node, parent, "trait"),
            "type_item" => self.rs_type(node, parent, "type"),
            "impl_item" => {
                let ty = self.t(node.child_by_field_name("type")?).to_string();
                let name = ty.split('<').next().unwrap_or("").trim().trim_start_matches('&').to_string();
                if name.is_empty() {
                    return None;
                }
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                Some(self.add(&name, "impl", node, sig, None, parent, false, None, false))
            }
            "type_identifier" => {
                let skip = Self::is_decl_name(node)
                    || node.parent().map_or(false, |p| p.kind() == "scoped_type_identifier");
                if !skip {
                    let n = self.t(node).to_string();
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            "scoped_type_identifier" => {
                if let Some(nm) = node.child_by_field_name("name") {
                    let n = self.t(nm).to_string();
                    let q = self.qual(node.child_by_field_name("path"));
                    self.add_type_ref(&n, Some(q), node, parent);
                }
                None
            }
            "call_expression" => {
                self.rs_call(node, parent);
                None
            }
            "use_declaration" => {
                if let Some(a) = node.child_by_field_name("argument") {
                    let line = Self::line(node);
                    self.rs_use(a, "", line);
                }
                None
            }
            _ => None,
        }
    }

    fn rs_type(&mut self, node: Node<'_>, parent: Option<usize>, kind: &str) -> Option<usize> {
        let name = self.t(node.child_by_field_name("name")?).to_string();
        let body = node.child_by_field_name("body");
        let sig = self.sig(node, body);
        let exported = Self::rs_pub(node);
        let doc = self.leading_doc(node);
        Some(self.add(&name, kind, node, sig, doc, parent, exported, None, false))
    }

    fn rs_params(&self, p: Option<Node<'_>>) -> (Vec<Param>, bool) {
        let mut out = Vec::new();
        let mut has_self = false;
        let Some(p) = p else { return (out, false) };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "self_parameter" => has_self = true,
                "parameter" => {
                    let name = ch
                        .child_by_field_name("pattern")
                        .map(|n| cap(self.t(n), 30))
                        .unwrap_or_default();
                    out.push(Param { name, optional: false, variadic: false, kw_only: false });
                }
                _ => {}
            }
        }
        (out, has_self)
    }

    fn rs_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(mut f) = node.child_by_field_name("function") else { return };
        if f.kind() == "generic_function" {
            if let Some(g) = f.child_by_field_name("function") {
                f = g;
            }
        }
        let (name, qual, kind) = match f.kind() {
            "identifier" => (self.t(f).to_string(), None, "call"),
            "scoped_identifier" => {
                let Some(n) = f.child_by_field_name("name") else { return };
                let q = self.qual(f.child_by_field_name("path"));
                (self.t(n).to_string(), Some(q), "path_call")
            }
            "field_expression" => {
                let Some(n) = f.child_by_field_name("field") else { return };
                let q = self.qual(f.child_by_field_name("value"));
                (self.t(n).to_string(), Some(q), "call")
            }
            _ => return,
        };
        if matches!(name.as_str(), "Some" | "Ok" | "Err") && qual.is_none() {
            return;
        }
        let args = self.count_args(node.child_by_field_name("arguments"), &[]);
        self.add_ref(&name, qual, kind, node, args, enclosing);
    }

    fn rs_use(&mut self, node: Node<'_>, prefix: &str, line: u32) {
        let join = |s: &str| -> String {
            if prefix.is_empty() {
                s.to_string()
            } else {
                format!("{prefix}::{s}")
            }
        };
        match node.kind() {
            "identifier" | "crate" | "self" | "super" => {
                let t = self.t(node).to_string();
                let full = join(&t);
                self.add_import(&t, &full, Some(t.as_str()), false, line);
            }
            "scoped_identifier" => {
                let full = join(self.t(node));
                let name = node
                    .child_by_field_name("name")
                    .map(|n| self.t(n).to_string())
                    .unwrap_or_default();
                if !name.is_empty() {
                    self.add_import(&name, &full, Some(name.as_str()), false, line);
                }
            }
            "use_as_clause" => {
                let (Some(path), Some(alias)) =
                    (node.child_by_field_name("path"), node.child_by_field_name("alias"))
                else {
                    return;
                };
                let p = self.t(path);
                let last = p.rsplit("::").next().unwrap_or(p).to_string();
                let full = join(p);
                let a = self.t(alias).to_string();
                self.add_import(&a, &full, Some(last.as_str()), false, line);
            }
            "use_list" => {
                let mut c = node.walk();
                for ch in node.named_children(&mut c) {
                    self.rs_use(ch, prefix, line);
                }
            }
            "scoped_use_list" => {
                let p2 = match node.child_by_field_name("path") {
                    Some(p) => join(self.t(p)),
                    None => prefix.to_string(),
                };
                if let Some(list) = node.child_by_field_name("list") {
                    self.rs_use(list, &p2, line);
                }
            }
            "use_wildcard" => {
                let t = self.t(node);
                let base = t.trim_end_matches('*').trim_end_matches("::");
                let full = if base.is_empty() { prefix.to_string() } else { join(base) };
                self.add_import("*", &full, None, true, line);
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------------- Go

    fn go(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let params = self.go_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let doc = self.leading_doc(node);
                let exported = name.chars().next().map_or(false, |c| c.is_uppercase());
                Some(self.add(&name, "function", node, sig, doc, parent, exported, Some(params), false))
            }
            "method_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let params = self.go_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let doc = self.leading_doc(node);
                let exported = name.chars().next().map_or(false, |c| c.is_uppercase());
                let recv = node
                    .child_by_field_name("receiver")
                    .map(|r| go_recv_type(self.t(r)))
                    .unwrap_or_default();
                let idx = self.add(&name, "method", node, sig, doc, None, exported, Some(params), false);
                if !recv.is_empty() {
                    self.out.symbols[idx].qualname = format!("{recv}.{name}");
                }
                Some(idx)
            }
            "type_spec" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let ty = node.child_by_field_name("type");
                let kind = match ty.map(|t| t.kind()) {
                    Some("struct_type") => "struct",
                    Some("interface_type") => "interface",
                    _ => "type",
                };
                let shown = ty.map(|t| cap(&collapse(self.t(t)), 80)).unwrap_or_default();
                let sig = format!("type {name} {shown}");
                let doc = self.leading_doc(node.parent().unwrap_or(node));
                let exported = name.chars().next().map_or(false, |c| c.is_uppercase());
                Some(self.add(&name, kind, node, sig, doc, parent, exported, None, false))
            }
            "type_identifier" => {
                let skip = Self::is_decl_name(node)
                    || node.parent().map_or(false, |p| p.kind() == "qualified_type");
                if !skip {
                    let n = self.t(node).to_string();
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            "qualified_type" => {
                if let Some(nm) = node.child_by_field_name("name") {
                    let n = self.t(nm).to_string();
                    let q = self.qual(node.child_by_field_name("package"));
                    self.add_type_ref(&n, Some(q), node, parent);
                }
                None
            }
            "call_expression" => {
                self.go_call(node, parent);
                None
            }
            "import_spec" => {
                let Some(p) = node.child_by_field_name("path") else { return None };
                let module = self.t(p).trim_matches('"').trim_matches('`').to_string();
                let alias = node.child_by_field_name("name").map(|n| self.t(n).to_string());
                let line = Self::line(node);
                match alias.as_deref() {
                    Some("_") => {}
                    Some(".") => self.add_import("*", &module, None, true, line),
                    Some(a) => self.add_import(a, &module, None, false, line),
                    None => {
                        let last = module.rsplit('/').next().unwrap_or(&module).to_string();
                        self.add_import(&last, &module, None, false, line);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn go_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "parameter_declaration" => {
                    let mut c2 = ch.walk();
                    let names: Vec<String> = ch
                        .children_by_field_name("name", &mut c2)
                        .map(|n| self.t(n).to_string())
                        .collect();
                    if names.is_empty() {
                        out.push(Param { name: "_".into(), optional: false, variadic: false, kw_only: false });
                    } else {
                        for n in names {
                            out.push(Param { name: n, optional: false, variadic: false, kw_only: false });
                        }
                    }
                }
                "variadic_parameter_declaration" => {
                    let name = ch
                        .child_by_field_name("name")
                        .map(|n| self.t(n).to_string())
                        .unwrap_or_else(|| "_".into());
                    out.push(Param { name, optional: true, variadic: true, kw_only: false });
                }
                _ => {}
            }
        }
        out
    }

    fn go_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(f) = node.child_by_field_name("function") else { return };
        let (name, qual) = match f.kind() {
            "identifier" => (self.t(f).to_string(), None),
            "selector_expression" => {
                let Some(fl) = f.child_by_field_name("field") else { return };
                (self.t(fl).to_string(), Some(self.qual(f.child_by_field_name("operand"))))
            }
            _ => return,
        };
        let args_node = node.child_by_field_name("arguments");
        let spread = args_node.map_or(false, |a| self.t(a).contains("..."));
        let args = if spread {
            None
        } else {
            self.count_args(args_node, &["variadic_argument"])
        };
        self.add_ref(&name, qual, "call", node, args, enclosing);
    }
}

fn go_recv_type(s: &str) -> String {
    let s = s.trim().trim_start_matches('(').trim_end_matches(')');
    let last = s.split_whitespace().last().unwrap_or("");
    last.trim_start_matches('*')
        .split('[')
        .next()
        .unwrap_or("")
        .to_string()
}

// =====================================================================
// Shared helpers and the additional language visitors
// =====================================================================

impl<'a> Ex<'a> {
    /// True when `node` is the `name` field of its parent (a declaration, not a use).
    fn is_decl_name(node: Node<'_>) -> bool {
        node.parent().map_or(false, |p| {
            p.child_by_field_name("name").map_or(false, |n| n.id() == node.id())
        })
    }

    fn add_type_ref(&mut self, name: &str, qual: Option<String>, node: Node<'_>, enclosing: Option<usize>) {
        if name.len() < 2 {
            return;
        }
        self.add_ref(name, qual, "type", node, None, enclosing);
    }

    fn last_segment<'s>(s: &'s str) -> &'s str {
        let s = s.split('<').next().unwrap_or(s);
        s.rsplit(|c| c == '.' || c == ':' || c == '\\').next().unwrap_or(s)
    }

    // ------------------------------------------------ Python additions

    fn py_bases(&mut self, class: Node<'_>, enclosing: Option<usize>) {
        let Some(sc) = class.child_by_field_name("superclasses") else { return };
        let mut c = sc.walk();
        for ch in sc.named_children(&mut c) {
            match ch.kind() {
                "identifier" => {
                    let n = self.t(ch).to_string();
                    self.add_type_ref(&n, None, ch, enclosing);
                }
                "attribute" => {
                    if let Some(a) = ch.child_by_field_name("attribute") {
                        let n = self.t(a).to_string();
                        let q = self.qual(ch.child_by_field_name("object"));
                        self.add_type_ref(&n, Some(q), ch, enclosing);
                    }
                }
                _ => {}
            }
        }
    }

    fn py_type_refs(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            match ch.kind() {
                "identifier" => {
                    let n = self.t(ch).to_string();
                    if n.chars().next().map_or(false, |c| c.is_uppercase()) {
                        self.add_type_ref(&n, None, ch, enclosing);
                    }
                }
                "attribute" => {
                    if let Some(a) = ch.child_by_field_name("attribute") {
                        let n = self.t(a).to_string();
                        let q = self.qual(ch.child_by_field_name("object"));
                        self.add_type_ref(&n, Some(q), ch, enclosing);
                    }
                }
                // nested annotations are visited by the main walker
                "type" => {}
                _ => self.py_type_refs(ch, enclosing),
            }
        }
    }

    fn py_kwargs(&self, args: Option<Node<'_>>) -> Option<String> {
        let a = args?;
        if a.kind() != "argument_list" && a.kind() != "arguments" {
            return None;
        }
        let mut names = Vec::new();
        let mut c = a.walk();
        for ch in a.named_children(&mut c) {
            if ch.kind() == "keyword_argument" {
                if let Some(n) = ch.child_by_field_name("name") {
                    names.push(self.t(n).to_string());
                }
            }
        }
        if names.is_empty() {
            None
        } else {
            Some(names.join(","))
        }
    }

    // ------------------------------------------------ TS re-exports

    /// `export { a as b } from './m'`, `export * from './m'`, `export * as ns from './m'`.
    fn ts_reexport(&mut self, node: Node<'_>) {
        let Some(src_n) = node.child_by_field_name("source") else { return };
        let module = self
            .t(src_n)
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string();
        let line = Self::line(node);
        let mut had_clause = false;
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            match ch.kind() {
                "export_clause" => {
                    had_clause = true;
                    let mut c2 = ch.walk();
                    for spec in ch.named_children(&mut c2) {
                        if spec.kind() != "export_specifier" {
                            continue;
                        }
                        let Some(nm) = spec.child_by_field_name("name") else { continue };
                        let orig = self.t(nm).to_string();
                        let local = spec
                            .child_by_field_name("alias")
                            .map(|a| self.t(a).to_string())
                            .unwrap_or_else(|| orig.clone());
                        self.add_import(&local, &module, Some(orig.as_str()), false, line);
                    }
                }
                "namespace_export" => {
                    had_clause = true;
                    if let Some(id) = ch.named_child(0) {
                        let l = self.t(id).to_string();
                        self.add_import(&l, &module, None, false, line);
                    }
                }
                _ => {}
            }
        }
        if !had_clause {
            self.add_import("*", &module, None, true, line);
        }
    }

    // ------------------------------------------------------------ Java

    fn mod_text(&self, node: Node<'_>) -> &'a str {
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            if ch.kind() == "modifiers" {
                return self.t(ch);
            }
        }
        ""
    }

    fn java_public(&self, node: Node<'_>) -> bool {
        let m = self.mod_text(node);
        m.contains("public") || m.contains("protected")
    }

    fn java(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration"
            | "record_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = match node.kind() {
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = self.java_public(node);
                let doc = self.leading_doc(node);
                Some(self.add(&name, kind, node, sig, doc, parent, exported, None, false))
            }
            "method_declaration" | "constructor_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let in_type = parent.map_or(false, |p| {
                    matches!(self.out.symbols[p].kind.as_str(), "class" | "interface" | "enum")
                });
                let in_iface = parent.map_or(false, |p| self.out.symbols[p].kind == "interface");
                let params = self.java_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = self.java_public(node) || in_iface;
                let doc = self.leading_doc(node);
                let kind = if in_type { "method" } else { "function" };
                Some(self.add(&name, kind, node, sig, doc, parent, exported, Some(params), false))
            }
            "method_invocation" => {
                let n = node.child_by_field_name("name")?;
                let name = self.t(n).to_string();
                let qual = node.child_by_field_name("object").map(|o| self.qual(Some(o)));
                let args = self.count_args(node.child_by_field_name("arguments"), &[]);
                self.add_ref(&name, qual, "call", node, args, parent);
                None
            }
            "object_creation_expression" => {
                if let Some(t) = node.child_by_field_name("type") {
                    let raw = self.t(t);
                    let base = raw.split('<').next().unwrap_or(raw);
                    let (name, qual) = match base.rsplit_once('.') {
                        Some((q, n)) => (n.to_string(), Some(q.to_string())),
                        None => (base.to_string(), None),
                    };
                    let args = self.count_args(node.child_by_field_name("arguments"), &[]);
                    self.add_ref(&name, qual, "call", node, args, parent);
                }
                None
            }
            "type_identifier" => {
                let n = self.t(node).to_string();
                self.add_type_ref(&n, None, node, parent);
                None
            }
            "import_declaration" => {
                self.java_import(node);
                None
            }
            _ => None,
        }
    }

    fn java_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "formal_parameter" => {
                    let name = ch
                        .child_by_field_name("name")
                        .map(|n| self.t(n).to_string())
                        .unwrap_or_default();
                    out.push(Param { name, optional: false, variadic: false, kw_only: false });
                }
                "spread_parameter" => {
                    let mut name = String::from("args");
                    let mut c2 = ch.walk();
                    for d in ch.named_children(&mut c2) {
                        if d.kind() == "variable_declarator" {
                            if let Some(n) = d.child_by_field_name("name") {
                                name = self.t(n).to_string();
                            }
                        }
                    }
                    out.push(Param { name, optional: true, variadic: true, kw_only: false });
                }
                _ => {}
            }
        }
        out
    }

    fn java_import(&mut self, node: Node<'_>) {
        let line = Self::line(node);
        let text = collapse(self.t(node));
        let t = text.strip_prefix("import ").unwrap_or(&text);
        let t = t.strip_prefix("static ").unwrap_or(t);
        let t = t.trim_end_matches(';').trim().to_string();
        if t.is_empty() {
            return;
        }
        if let Some(m) = t.strip_suffix(".*") {
            self.add_import("*", m, None, true, line);
        } else {
            let last = t.rsplit('.').next().unwrap_or(&t).to_string();
            self.add_import(&last, &t, Some(last.as_str()), false, line);
        }
    }

    // ------------------------------------------------------------- C#

    fn cs_public(&self, node: Node<'_>) -> bool {
        let mut c = node.walk();
        let found = node.children(&mut c).any(|ch| {
            ch.kind() == "modifier" && matches!(self.t(ch), "public" | "protected" | "internal")
        });
        found
    }

    fn csharp(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "class_declaration" | "struct_declaration" | "interface_declaration"
            | "enum_declaration" | "record_declaration" | "record_struct_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = match node.kind() {
                    "struct_declaration" | "record_struct_declaration" => "struct",
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = self.cs_public(node);
                let doc = self.leading_doc(node);
                Some(self.add(&name, kind, node, sig, doc, parent, exported, None, false))
            }
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let sig = format!("namespace {name}");
                Some(self.add(&name, "namespace", node, sig, None, parent, true, None, false))
            }
            "method_declaration" | "constructor_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let in_type = parent.map_or(false, |p| {
                    matches!(
                        self.out.symbols[p].kind.as_str(),
                        "class" | "struct" | "interface" | "enum"
                    )
                });
                let in_iface = parent.map_or(false, |p| self.out.symbols[p].kind == "interface");
                let params = self.cs_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = self.cs_public(node) || in_iface;
                let doc = self.leading_doc(node);
                let kind = if in_type { "method" } else { "function" };
                Some(self.add(&name, kind, node, sig, doc, parent, exported, Some(params), false))
            }
            "invocation_expression" => {
                let f = node.child_by_field_name("function")?;
                let (name, qual) = match f.kind() {
                    "identifier" => (self.t(f).to_string(), None),
                    "member_access_expression" => {
                        let n = f.child_by_field_name("name")?;
                        let base = self.t(n);
                        let base = base.split('<').next().unwrap_or(base).to_string();
                        (base, Some(self.qual(f.child_by_field_name("expression"))))
                    }
                    "generic_name" => {
                        let raw = self.t(f);
                        (raw.split('<').next().unwrap_or(raw).to_string(), None)
                    }
                    _ => return None,
                };
                let args = self.count_args(node.child_by_field_name("arguments"), &[]);
                self.add_ref(&name, qual, "call", node, args, parent);
                None
            }
            "object_creation_expression" => {
                if let Some(t) = node.child_by_field_name("type") {
                    let name = Self::last_segment(self.t(t)).to_string();
                    let args = self.count_args(node.child_by_field_name("arguments"), &[]);
                    self.add_ref(&name, None, "call", node, args, parent);
                }
                None
            }
            "base_list" => {
                let mut c = node.walk();
                for ch in node.named_children(&mut c) {
                    let n = Self::last_segment(self.t(ch)).to_string();
                    self.add_type_ref(&n, None, ch, parent);
                }
                None
            }
            "using_directive" => {
                self.cs_using(node);
                None
            }
            _ => None,
        }
    }

    fn cs_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            if ch.kind() != "parameter" {
                continue;
            }
            let name = ch
                .child_by_field_name("name")
                .map(|n| self.t(n).to_string())
                .unwrap_or_default();
            let mut c2 = ch.walk();
            let has_default = ch.named_children(&mut c2).any(|d| d.kind() == "equals_value_clause");
            let variadic = self.t(ch).starts_with("params ");
            out.push(Param { name, optional: has_default || variadic, variadic, kw_only: false });
        }
        out
    }

    fn cs_using(&mut self, node: Node<'_>) {
        let line = Self::line(node);
        let text = collapse(self.t(node));
        let t = text.trim_end_matches(';').trim();
        let t = t.strip_prefix("global ").unwrap_or(t);
        let t = t.strip_prefix("using ").unwrap_or(t).trim();
        let t = t.strip_prefix("static ").unwrap_or(t).trim();
        if let Some((alias, target)) = t.split_once('=') {
            let (alias, target) = (alias.trim().to_string(), target.trim().to_string());
            let last = Self::last_segment(&target).to_string();
            self.add_import(&alias, &target, Some(last.as_str()), false, line);
        } else if !t.is_empty() {
            self.add_import("*", t, None, true, line);
        }
    }

    // ------------------------------------------------------------- C / C++

    fn c_static(&self, node: Node<'_>) -> bool {
        let mut c = node.walk();
        let found = node
            .children(&mut c)
            .any(|ch| ch.kind() == "storage_class_specifier" && self.t(ch) == "static");
        found
    }

    /// Descend pointer/reference wrappers to the function declarator.
    /// Returns (function_declarator, name, optional C++ qualifier).
    fn c_declarator<'t>(&self, mut d: Node<'t>) -> Option<(Node<'t>, String, Option<String>)> {
        loop {
            match d.kind() {
                "pointer_declarator" | "reference_declarator" | "parenthesized_declarator"
                | "attributed_declarator" => {
                    let next = d.child_by_field_name("declarator").or_else(|| d.named_child(0))?;
                    d = next;
                }
                "function_declarator" => break,
                _ => return None,
            }
        }
        let inner = d.child_by_field_name("declarator")?;
        let raw = self.t(inner);
        let (qual, name) = match raw.rsplit_once("::") {
            Some((q, n)) => (Some(q.trim().to_string()), n.trim().to_string()),
            None => (None, raw.trim().to_string()),
        };
        let name = name.split('<').next().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some((d, name, qual))
    }

    fn c_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "parameter_declaration" | "optional_parameter_declaration" => {
                    let optional = ch.kind() == "optional_parameter_declaration";
                    let decl = ch.child_by_field_name("declarator");
                    let name = match decl {
                        Some(d) => self
                            .t(d)
                            .trim_start_matches(|c| c == '*' || c == '&')
                            .to_string(),
                        None => {
                            let ty = ch.child_by_field_name("type").map(|t| self.t(t)).unwrap_or("");
                            if ty == "void" {
                                continue;
                            }
                            "_".to_string()
                        }
                    };
                    out.push(Param { name, optional, variadic: false, kw_only: false });
                }
                "variadic_parameter" => out.push(Param {
                    name: "...".into(),
                    optional: true,
                    variadic: true,
                    kw_only: false,
                }),
                _ => {}
            }
        }
        out
    }

    fn c_like(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_definition" => {
                let decl = node.child_by_field_name("declarator")?;
                let (fd, name, qual) = self.c_declarator(decl)?;
                let params = self.c_params(fd.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let exported = !self.c_static(node);
                let in_type = parent.map_or(false, |p| {
                    matches!(self.out.symbols[p].kind.as_str(), "class" | "struct")
                });
                let kind = if in_type || qual.is_some() { "method" } else { "function" };
                let doc = self.leading_doc(node);
                let idx = self.add(&name, kind, node, sig, doc, parent, exported, Some(params), false);
                if let Some(q) = qual {
                    self.out.symbols[idx].qualname = format!("{q}.{name}");
                }
                Some(idx)
            }
            // Prototypes: kept for skeletons, excluded from call resolution.
            "declaration" | "field_declaration" => {
                let decl = node.child_by_field_name("declarator")?;
                let (fd, name, _) = self.c_declarator(decl)?;
                let params = self.c_params(fd.child_by_field_name("parameters"));
                let sig = self.sig(node, None);
                let doc = self.leading_doc(node);
                Some(self.add(&name, "declaration", node, sig, doc, parent, true, Some(params), false))
            }
            "class_specifier" | "struct_specifier" | "enum_specifier" => {
                let body = node.child_by_field_name("body")?;
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = match node.kind() {
                    "class_specifier" => "class",
                    "struct_specifier" => "struct",
                    _ => "enum",
                };
                let sig = self.sig(node, Some(body));
                let doc = self.leading_doc(node);
                Some(self.add(&name, kind, node, sig, doc, parent, true, None, false))
            }
            "namespace_definition" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let sig = format!("namespace {name}");
                Some(self.add(&name, "namespace", node, sig, None, parent, true, None, false))
            }
            "type_definition" => {
                let d = node.child_by_field_name("declarator")?;
                if d.kind() != "type_identifier" {
                    return None;
                }
                let name = self.t(d).to_string();
                let sig = cap(&collapse(self.t(node)), 200);
                Some(self.add(&name, "type", node, sig, None, parent, true, None, false))
            }
            "call_expression" => {
                self.c_call(node, parent);
                None
            }
            "new_expression" => {
                if let Some(t) = node.child_by_field_name("type") {
                    let name = Self::last_segment(self.t(t)).to_string();
                    let args = self.count_args(node.child_by_field_name("arguments"), &[]);
                    self.add_ref(&name, None, "call", node, args, parent);
                }
                None
            }
            "preproc_include" => {
                if let Some(p) = node.child_by_field_name("path") {
                    let path = self
                        .t(p)
                        .trim_matches(|c| c == '"' || c == '<' || c == '>')
                        .to_string();
                    self.add_import("*", &path, None, true, Self::line(node));
                }
                None
            }
            "type_identifier" => {
                let skip = Self::is_decl_name(node)
                    || node.parent().map_or(false, |p| p.kind() == "qualified_identifier");
                if !skip {
                    let n = self.t(node).to_string();
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            _ => None,
        }
    }

    fn c_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(mut f) = node.child_by_field_name("function") else { return };
        if f.kind() == "template_function" {
            if let Some(n) = f.child_by_field_name("name") {
                f = n;
            }
        }
        let (name, qual) = match f.kind() {
            "identifier" => (self.t(f).to_string(), None),
            "field_expression" => {
                let Some(fl) = f.child_by_field_name("field") else { return };
                (self.t(fl).to_string(), Some(self.qual(f.child_by_field_name("argument"))))
            }
            "qualified_identifier" => {
                let Some(n) = f.child_by_field_name("name") else { return };
                let q = f.child_by_field_name("scope").map(|s| self.qual(Some(s)));
                (self.t(n).to_string(), q)
            }
            _ => return,
        };
        let args = self.count_args(node.child_by_field_name("arguments"), &[]);
        self.add_ref(&name, qual, "call", node, args, enclosing);
    }

    // ------------------------------------------------------------- PHP

    fn php(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "function_definition" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let params = self.php_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let doc = self.leading_doc(node);
                Some(self.add(&name, "function", node, sig, doc, parent, true, Some(params), false))
            }
            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = match node.kind() {
                    "interface_declaration" => "interface",
                    "trait_declaration" => "trait",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let doc = self.leading_doc(node);
                Some(self.add(&name, kind, node, sig, doc, parent, true, None, false))
            }
            "method_declaration" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let params = self.php_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let mut c = node.walk();
                let private = node
                    .children(&mut c)
                    .any(|ch| ch.kind() == "visibility_modifier" && self.t(ch) == "private");
                let doc = self.leading_doc(node);
                Some(self.add(&name, "method", node, sig, doc, parent, !private, Some(params), false))
            }
            "function_call_expression" => {
                let f = node.child_by_field_name("function")?;
                let name = Self::last_segment(self.t(f)).to_string();
                let args = self.php_args(node.child_by_field_name("arguments"));
                self.add_ref(&name, None, "call", node, args, parent);
                None
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let n = node.child_by_field_name("name")?;
                let name = self.t(n).to_string();
                let q = self.qual(node.child_by_field_name("object"));
                let q = q.trim_start_matches('$').to_string();
                let args = self.php_args(node.child_by_field_name("arguments"));
                self.add_ref(&name, Some(q), "call", node, args, parent);
                None
            }
            "scoped_call_expression" => {
                let n = node.child_by_field_name("name")?;
                let name = self.t(n).to_string();
                let q = self.qual(node.child_by_field_name("scope"));
                let q = Self::last_segment(&q).to_string();
                let args = self.php_args(node.child_by_field_name("arguments"));
                let q = if q == "parent" || q == "static" { "self".to_string() } else { q };
                self.add_ref(&name, Some(q), "call", node, args, parent);
                None
            }
            "object_creation_expression" => {
                let mut c = node.walk();
                let mut target = None;
                let mut args = None;
                for ch in node.named_children(&mut c) {
                    match ch.kind() {
                        "name" | "qualified_name" => target = Some(ch),
                        "arguments" => args = Some(ch),
                        _ => {}
                    }
                }
                if let Some(t) = target {
                    let name = Self::last_segment(self.t(t)).to_string();
                    let n = self.php_args(args);
                    self.add_ref(&name, None, "call", node, n, parent);
                }
                None
            }
            "namespace_use_declaration" => {
                self.php_use(node);
                None
            }
            "include_expression" | "include_once_expression" | "require_expression"
            | "require_once_expression" => {
                if let Some(arg) = node.named_child(0) {
                    let m = self
                        .t(arg)
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();
                    self.add_import("*", &m, None, true, Self::line(node));
                }
                None
            }
            "base_clause" | "class_interface_clause" => {
                let mut c = node.walk();
                for ch in node.named_children(&mut c) {
                    if matches!(ch.kind(), "name" | "qualified_name") {
                        let n = Self::last_segment(self.t(ch)).to_string();
                        self.add_type_ref(&n, None, ch, parent);
                    }
                }
                None
            }
            "named_type" => {
                let n = Self::last_segment(self.t(node)).to_string();
                if n.chars().next().map_or(false, |c| c.is_uppercase()) {
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            _ => None,
        }
    }

    fn php_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            let variadic = ch.kind() == "variadic_parameter";
            if !matches!(
                ch.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            ) {
                continue;
            }
            let name = ch
                .child_by_field_name("name")
                .map(|n| self.t(n).trim_start_matches('$').to_string())
                .unwrap_or_default();
            let optional = variadic || ch.child_by_field_name("default_value").is_some();
            out.push(Param { name, optional, variadic, kw_only: false });
        }
        out
    }

    fn php_args(&self, a: Option<Node<'_>>) -> Option<u32> {
        let a = a?;
        if self.t(a).contains("...") {
            return None;
        }
        let mut n = 0u32;
        let mut c = a.walk();
        for ch in a.named_children(&mut c) {
            if ch.kind().contains("comment") {
                continue;
            }
            n += 1;
        }
        Some(n)
    }

    fn php_use(&mut self, node: Node<'_>) {
        let line = Self::line(node);
        let mut c = node.walk();
        for clause in node.named_children(&mut c) {
            if clause.kind() != "namespace_use_clause" {
                continue;
            }
            let mut c2 = clause.walk();
            let mut module = String::new();
            let mut alias: Option<String> = None;
            for ch in clause.named_children(&mut c2) {
                match ch.kind() {
                    "qualified_name" | "name" => {
                        if module.is_empty() {
                            module = self.t(ch).trim_start_matches('\\').to_string();
                        }
                    }
                    "namespace_aliasing_clause" => {
                        if let Some(n) = ch.named_child(0) {
                            alias = Some(self.t(n).to_string());
                        }
                    }
                    _ => {}
                }
            }
            if module.is_empty() {
                continue;
            }
            let last = module.rsplit('\\').next().unwrap_or(&module).to_string();
            let local = alias.unwrap_or_else(|| last.clone());
            self.add_import(&local, &module, Some(last.as_str()), false, line);
        }
    }

    // ------------------------------------------------------------ Ruby

    fn ruby(&mut self, node: Node<'_>, parent: Option<usize>) -> Option<usize> {
        match node.kind() {
            "method" | "singleton_method" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let in_scope = parent.map_or(false, |p| {
                    matches!(self.out.symbols[p].kind.as_str(), "class" | "module")
                });
                let params = self.ruby_params(node.child_by_field_name("parameters"));
                let body = node.child_by_field_name("body");
                let sig = self.sig(node, body);
                let sig = if body.is_none() { cap(sig.lines().next().unwrap_or(""), 160) } else { sig };
                let doc = self.leading_doc(node);
                let exported = !name.starts_with('_');
                let kind = if in_scope { "method" } else { "function" };
                Some(self.add(&name, kind, node, sig, doc, parent, exported, Some(params), false))
            }
            "class" | "module" => {
                let name = self.t(node.child_by_field_name("name")?).to_string();
                let kind = if node.kind() == "class" { "class" } else { "module" };
                let first = self.t(node).lines().next().unwrap_or("").to_string();
                let sig = cap(&collapse(&first), 160);
                let doc = self.leading_doc(node);
                Some(self.add(&name, kind, node, sig, doc, parent, true, None, false))
            }
            "call" => {
                self.ruby_call(node, parent);
                None
            }
            "constant" => {
                let skip = Self::is_decl_name(node)
                    || node.parent().map_or(false, |p| p.kind() == "scope_resolution");
                if !skip {
                    let n = self.t(node).to_string();
                    self.add_type_ref(&n, None, node, parent);
                }
                None
            }
            _ => None,
        }
    }

    fn ruby_params(&self, p: Option<Node<'_>>) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(p) = p else { return out };
        let mut c = p.walk();
        for ch in p.named_children(&mut c) {
            match ch.kind() {
                "identifier" => out.push(Param {
                    name: self.t(ch).to_string(),
                    optional: false,
                    variadic: false,
                    kw_only: false,
                }),
                "optional_parameter" => out.push(Param {
                    name: ch.child_by_field_name("name").map(|n| self.t(n).to_string()).unwrap_or_default(),
                    optional: true,
                    variadic: false,
                    kw_only: false,
                }),
                "splat_parameter" => out.push(Param {
                    name: self.t(ch).trim_start_matches('*').to_string(),
                    optional: true,
                    variadic: true,
                    kw_only: false,
                }),
                "hash_splat_parameter" => out.push(Param {
                    name: format!("**{}", self.t(ch).trim_start_matches('*')),
                    optional: true,
                    variadic: false,
                    kw_only: true,
                }),
                "keyword_parameter" => out.push(Param {
                    name: ch.child_by_field_name("name").map(|n| self.t(n).to_string()).unwrap_or_default(),
                    optional: ch.child_by_field_name("value").is_some(),
                    variadic: false,
                    kw_only: true,
                }),
                _ => {}
            }
        }
        out
    }

    fn ruby_args(&self, a: Option<Node<'_>>) -> Option<u32> {
        let a = a?;
        let mut n = 0u32;
        let mut c = a.walk();
        for ch in a.named_children(&mut c) {
            match ch.kind() {
                "pair" | "splat_argument" | "hash_splat_argument" | "block_argument" | "hash" => {
                    return None
                }
                k if k.contains("comment") => {}
                _ => n += 1,
            }
        }
        Some(n)
    }

    fn ruby_call(&mut self, node: Node<'_>, enclosing: Option<usize>) {
        let Some(m) = node.child_by_field_name("method") else { return };
        let mname = self.t(m).to_string();
        let recv = node.child_by_field_name("receiver");
        let args_n = node.child_by_field_name("arguments");
        if recv.is_none() && (mname == "require" || mname == "require_relative") {
            if let Some(a) = args_n.and_then(|a| a.named_child(0)) {
                let raw = self.t(a).trim_matches(|c| c == '"' || c == '\'').to_string();
                let module = if mname == "require_relative" && !raw.starts_with('.') {
                    format!("./{raw}")
                } else {
                    raw
                };
                self.add_import("*", &module, None, true, Self::line(node));
            }
            return;
        }
        if mname == "new" {
            if let Some(r) = recv {
                if matches!(r.kind(), "constant" | "scope_resolution") {
                    let n = Self::last_segment(self.t(r)).to_string();
                    let args = self.ruby_args(args_n);
                    self.add_ref(&n, None, "call", node, args, enclosing);
                    return;
                }
            }
        }
        let qual = recv.map(|r| self.qual(Some(r)));
        let args = self.ruby_args(args_n);
        self.add_ref(&mname, qual, "call", node, args, enclosing);
    }
}
