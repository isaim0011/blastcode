use std::fs;
use std::path::Path;

use blastcode::tools::Engine;
use serde_json::{json, Value};

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn engine(root: &Path) -> Engine {
    let mut e = Engine::open(root, None).unwrap();
    e.index_now(true).unwrap();
    e
}

fn call_json(e: &mut Engine, tool: &str, args: Value) -> Value {
    let out = e.call(tool, &args).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|err| panic!("not JSON ({err}): {out}"))
}

fn py_repo(root: &Path) {
    write(root, "auth/__init__.py", "");
    write(
        root,
        "auth/jwt.py",
        "def verify_token(token):\n    \"\"\"Check a JWT.\"\"\"\n    return token\n\nclass Signer:\n    def sign(self, payload):\n        return payload\n",
    );
    write(
        root,
        "api/routes.py",
        "from auth.jwt import verify_token\n\ndef handler(req):\n    return verify_token(req.token)\n\ndef other(req):\n    return verify_token(req.token)\n",
    );
    write(
        root,
        "unrelated.py",
        "def verify_token(x, y):\n    return x\n\ndef use():\n    return verify_token(1, 2)\n",
    );
}

#[test]
fn python_skeleton_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let sk = e.call("get_file_skeleton", &json!({"file_path": "auth/jwt.py"})).unwrap();
    assert!(sk.contains("def verify_token(token)"), "{sk}");
    assert!(sk.contains("class Signer"), "{sk}");
    assert!(sk.contains("def sign(self, payload)"), "{sk}");
    assert!(sk.contains("Check a JWT."), "{sk}");
    assert!(!sk.contains("return payload"), "bodies must be stripped: {sk}");

    let v = call_json(&mut e, "search_symbols", json!({"query": "verify token"}));
    assert!(v["total_matches"].as_u64().unwrap() >= 2);
}

#[test]
fn python_callers_use_import_scope_not_name_only() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let v = call_json(
        &mut e,
        "trace_symbol",
        json!({"symbol_name": "verify_token", "file_path": "auth/jwt.py", "direction": "callers"}),
    );
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    let call_sites: Vec<&Value> = callers.iter().filter(|c| c["usage"] == "call").collect();
    assert_eq!(call_sites.len(), 2, "{v}");
    for c in &call_sites {
        assert_eq!(c["file"], "api/routes.py");
        assert_eq!(c["confidence"], "exact");
    }
    // The same-named function in unrelated.py must NOT be attributed to auth/jwt.py.
    assert!(callers.iter().all(|c| c["file"] != "unrelated.py"), "{v}");
    // The import statement itself is reported as a usage.
    assert!(callers.iter().any(|c| c["usage"] == "import"), "{v}");
}

#[test]
fn python_impact_proposed_signature_change_is_breaking() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let v = call_json(
        &mut e,
        "get_impact_radius",
        json!({
            "file_path": "auth/jwt.py",
            "new_source": "def verify_token(token, audience):\n    return token\n\nclass Signer:\n    def sign(self, payload):\n        return payload\n"
        }),
    );
    assert_eq!(v["summary"]["breaking"], 2, "{v}");
    let alert = v["agent_alert"].as_str().unwrap();
    assert!(alert.contains("api/routes.py:4"), "{alert}");
    assert!(alert.contains("api/routes.py:7"), "{alert}");
}

#[test]
fn python_impact_removed_symbol_and_compatible_change() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    // Adding an optional parameter is not provably breaking.
    let v = call_json(
        &mut e,
        "get_impact_radius",
        json!({
            "file_path": "auth/jwt.py",
            "new_source": "def verify_token(token, audience=None):\n    return token\n\nclass Signer:\n    def sign(self, payload):\n        return payload\n"
        }),
    );
    assert_eq!(v["summary"]["breaking"], 0, "{v}");
    // Removing it is.
    let v = call_json(
        &mut e,
        "get_impact_radius",
        json!({
            "file_path": "auth/jwt.py",
            "new_source": "class Signer:\n    def sign(self, payload):\n        return payload\n"
        }),
    );
    assert!(v["summary"]["breaking"].as_u64().unwrap() >= 2, "{v}");
}

#[test]
fn incremental_reindex_picks_up_edits_and_deletes() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    write(tmp.path(), "auth/jwt.py", "def renamed_fn(a):\n    return a\n");
    e.index_now(false).unwrap();
    let v = call_json(&mut e, "search_symbols", json!({"query": "renamed_fn"}));
    assert_eq!(v["total_matches"], 1);
    fs::remove_file(tmp.path().join("unrelated.py")).unwrap();
    let st = e.index_now(false).unwrap();
    assert_eq!(st.removed, 1);
}

#[test]
fn typescript_imports_and_arity() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/services/user.ts",
        "export class UserService {\n  getUserById(id: string): User {\n    return db.find(id);\n  }\n}\nexport function helper(a: number, b?: number) { return a; }\n",
    );
    write(
        tmp.path(),
        "src/controllers/auth.ts",
        "import { UserService, helper } from '../services/user';\nconst svc = new UserService();\nexport function login(id: string) {\n  helper(1);\n  return svc.getUserById(id);\n}\n",
    );
    let mut e = engine(tmp.path());
    let sk = e.call("get_file_skeleton", &json!({"file_path": "src/services/user.ts"})).unwrap();
    assert!(sk.contains("export class UserService"), "{sk}");
    assert!(sk.contains("getUserById(id: string): User"), "{sk}");
    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "helper", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    assert!(callers.iter().any(|c| c["file"] == "src/controllers/auth.ts" && c["usage"] == "call" && c["confidence"] == "exact"), "{v}");
    let v = call_json(
        &mut e,
        "get_impact_radius",
        json!({
            "file_path": "src/services/user.ts",
            "new_source": "export class UserService {\n  getUserById(id: string): User {\n    return db.find(id);\n  }\n}\nexport function helper(a: number, b: number, c: number) { return a; }\n"
        }),
    );
    assert_eq!(v["summary"]["breaking"], 1, "{v}");
}

#[test]
fn rust_and_go_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/models.rs",
        "pub struct User { pub id: u32 }\n\nimpl User {\n    /// Build a user.\n    pub fn new(id: u32) -> Self { User { id } }\n}\n\npub fn make(id: u32) -> User { User::new(id) }\n",
    );
    write(
        tmp.path(),
        "src/main.rs",
        "use crate::models::make;\n\nfn main() {\n    let _u = make(1);\n}\n",
    );
    write(
        tmp.path(),
        "pkg/db/db.go",
        "package db\n\n// Open opens a connection.\nfunc Open(dsn string) error { return nil }\n\ntype Conn struct{}\n\nfunc (c *Conn) Close() error { return nil }\n",
    );
    write(
        tmp.path(),
        "cmd/app/main.go",
        "package main\n\nimport \"example.com/app/pkg/db\"\n\nfunc main() {\n\t_ = db.Open(\"x\")\n}\n",
    );
    let mut e = engine(tmp.path());
    let sk = e.call("get_file_skeleton", &json!({"file_path": "src/models.rs"})).unwrap();
    assert!(sk.contains("pub struct User"), "{sk}");
    assert!(sk.contains("pub fn new(id: u32) -> Self"), "{sk}");
    assert!(sk.contains("Build a user."), "{sk}");

    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "make", "direction": "callers"}));
    assert!(v["matches"][0]["callers"].as_array().unwrap().iter().any(|c| c["file"] == "src/main.rs"), "{v}");

    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "Open", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    assert!(callers.iter().any(|c| c["file"] == "cmd/app/main.go" && c["confidence"] == "exact"), "{v}");

    let sk = e.call("get_file_skeleton", &json!({"file_path": "pkg/db/db.go"})).unwrap();
    assert!(sk.contains("func Open(dsn string) error"), "{sk}");
    assert!(sk.contains("func (c *Conn) Close() error"), "{sk}");
}

#[test]
fn workspace_map_and_query_graph() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let map = e.call("get_workspace_map", &json!({})).unwrap();
    assert!(map.contains("auth/"), "{map}");
    assert!(map.contains("jwt.py"), "{map}");
    assert!(map.contains("verify_token"), "{map}");
    let v = call_json(&mut e, "query_graph", json!({"calls": "verify_token"}));
    assert!(v["count"].as_u64().unwrap() >= 2, "{v}");
}

#[test]
fn path_traversal_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    assert!(e.call("get_file_skeleton", &json!({"file_path": "../etc/passwd"})).is_err());
}

// ---------------------------------------------------------------------------
// Workspace caretaker: journal + digest
// ---------------------------------------------------------------------------

#[test]
fn journal_records_changes_and_digest_reports_them_once() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    assert!(e.take_digest().unwrap().is_none(), "first index must not flood the journal");

    write(
        tmp.path(),
        "auth/jwt.py",
        "def verify_token(token, audience):\n    return token\n\nclass Signer:\n    def sign(self, payload):\n        return payload\n\ndef brand_new():\n    pass\n",
    );
    write(tmp.path(), "extra.py", "def extra_fn():\n    pass\n");
    fs::remove_file(tmp.path().join("unrelated.py")).unwrap();
    e.index_now(false).unwrap();

    let digest = e.take_digest().unwrap().expect("changes must be reported");
    assert!(digest.contains("auth/jwt.py"), "{digest}");
    assert!(digest.contains("signature changed"), "{digest}");
    assert!(digest.contains("verify_token(token, audience)"), "{digest}");
    assert!(digest.contains("brand_new"), "{digest}");
    assert!(digest.contains("+ extra.py"), "{digest}");
    assert!(digest.contains("- unrelated.py"), "{digest}");
    // Reported exactly once.
    assert!(e.take_digest().unwrap().is_none());

    let v = call_json(&mut e, "get_workspace_changes", json!({"file_path": "auth/jwt.py"}));
    assert!(v["events"].as_array().unwrap().len() >= 1, "{v}");
}

#[test]
fn symbol_source_returns_only_that_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "verify_token", "file_path": "auth/jwt.py"}))
        .unwrap();
    assert!(src.contains("def verify_token(token):"), "{src}");
    assert!(src.contains("return token"), "{src}");
    assert!(!src.contains("class Signer"), "{src}");
}

#[test]
fn file_context_lists_dependents_and_imports() {
    let tmp = tempfile::tempdir().unwrap();
    py_repo(tmp.path());
    let mut e = engine(tmp.path());
    let v = call_json(&mut e, "get_file_context", json!({"file_path": "auth/jwt.py"}));
    let deps = v["dependents"]["files"].as_array().unwrap();
    assert!(deps.iter().any(|d| d["file"] == "api/routes.py"), "{v}");
    let v = call_json(&mut e, "get_file_context", json!({"file_path": "api/routes.py"}));
    let imports = v["imports"].as_array().unwrap();
    assert!(
        imports.iter().any(|i| i["internal"].as_array().map_or(false, |a| a.iter().any(|x| x == "auth/jwt.py"))),
        "{v}"
    );
    assert!(v["skeleton"].as_str().unwrap().contains("def handler(req)"), "{v}");
}

// ---------------------------------------------------------------------------
// Resolution limits that were fixed
// ---------------------------------------------------------------------------

#[test]
fn ts_reexport_barrel_resolves_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib/util.ts", "export function helper(a: number) { return a; }\n");
    write(tmp.path(), "src/lib/index.ts", "export * from './util';\n");
    write(
        tmp.path(),
        "src/app.ts",
        "import { helper } from './lib';\nexport function main() { return helper(1); }\n",
    );
    let mut e = engine(tmp.path());
    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "helper", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    assert!(
        callers.iter().any(|c| c["file"] == "src/app.ts" && c["usage"] == "call" && c["confidence"] == "exact"),
        "{v}"
    );
}

#[test]
fn tsconfig_path_aliases_are_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "tsconfig.json",
        "{\n  // comment\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@/*\": [\"src/*\"], },\n  },\n}\n",
    );
    write(tmp.path(), "src/util.ts", "export function helper(a: number) { return a; }\n");
    write(
        tmp.path(),
        "src/app.ts",
        "import { helper } from '@/util';\nexport function main() { return helper(1); }\n",
    );
    let mut e = engine(tmp.path());
    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "helper", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    assert!(
        callers.iter().any(|c| c["file"] == "src/app.ts" && c["usage"] == "call" && c["confidence"] == "exact"),
        "{v}"
    );
}

#[test]
fn python_keyword_argument_rename_is_breaking() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "lib.py", "def fetch(url, timeout=5):\n    return url\n");
    write(
        tmp.path(),
        "app.py",
        "from lib import fetch\n\ndef go():\n    return fetch('x', timeout=3)\n",
    );
    let mut e = engine(tmp.path());
    let v = call_json(
        &mut e,
        "get_impact_radius",
        json!({"file_path": "lib.py", "new_source": "def fetch(url, deadline=5):\n    return url\n"}),
    );
    assert_eq!(v["summary"]["breaking"], 1, "{v}");
    assert!(v["agent_alert"].as_str().unwrap().contains("app.py:4"), "{v}");
}

#[test]
fn inheritance_and_annotations_count_as_type_usages() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "base.py", "class Base:\n    pass\n");
    write(
        tmp.path(),
        "child.py",
        "from base import Base\n\nclass Child(Base):\n    pass\n\ndef make(b: Base) -> Base:\n    return b\n",
    );
    let mut e = engine(tmp.path());
    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "Base", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    let type_uses: Vec<&Value> = callers.iter().filter(|c| c["usage"] == "type").collect();
    assert!(type_uses.len() >= 2, "{v}");
    assert!(type_uses.iter().all(|c| c["file"] == "child.py"), "{v}");
    // Removing the class is provably breaking for every usage.
    let v = call_json(&mut e, "get_impact_radius", json!({"file_path": "base.py", "new_source": "x = 1\n"}));
    assert!(v["summary"]["breaking"].as_u64().unwrap() >= 3, "{v}");
}

// ---------------------------------------------------------------------------
// Additional languages (grammar node names are exercised here)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn skeleton_of(e: &mut Engine, file: &str) -> String {
    e.call("get_file_skeleton", &json!({"file_path": file})).unwrap()
}

#[cfg(feature = "lang-java")]
#[test]
fn java_extraction_and_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/com/acme/util/Helper.java",
        "package com.acme.util;\n\npublic class Helper {\n    public static int twice(int a) { return a * 2; }\n}\n",
    );
    write(
        tmp.path(),
        "src/com/acme/Service.java",
        "package com.acme;\n\nimport com.acme.util.Helper;\n\npublic class Service {\n    public int run(int a) { return Helper.twice(a); }\n}\n",
    );
    let mut e = engine(tmp.path());
    let sk = skeleton_of(&mut e, "src/com/acme/Service.java");
    assert!(sk.contains("public class Service"), "{sk}");
    assert!(sk.contains("run(int a)"), "{sk}");
    let v = call_json(&mut e, "trace_symbol", json!({"symbol_name": "twice", "direction": "callers"}));
    let callers = v["matches"][0]["callers"].as_array().unwrap();
    assert!(
        callers.iter().any(|c| c["file"] == "src/com/acme/Service.java" && c["usage"] == "call"),
        "{v}"
    );
}

#[cfg(feature = "lang-csharp")]
#[test]
fn csharp_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "Svc.cs",
        "namespace Acme\n{\n    public class Svc\n    {\n        public int Run(int a, int b) { return a + b; }\n    }\n}\n",
    );
    let mut e = engine(tmp.path());
    let sk = skeleton_of(&mut e, "Svc.cs");
    assert!(sk.contains("class Svc"), "{sk}");
    assert!(sk.contains("Run(int a, int b)"), "{sk}");
}

#[cfg(all(feature = "lang-c", feature = "lang-cpp"))]
#[test]
fn c_and_cpp_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "math.c", "int add(int a, int b) { return a + b; }\n\nstatic int hidden(void) { return 1; }\n");
    write(
        tmp.path(),
        "shape.cpp",
        "class Foo {\npublic:\n    int bar(int x) { return x; }\n};\n\nint Foo::baz(int y) { return y; }\n",
    );
    let mut e = engine(tmp.path());
    let sk = skeleton_of(&mut e, "math.c");
    assert!(sk.contains("int add(int a, int b)"), "{sk}");
    let sk = skeleton_of(&mut e, "shape.cpp");
    assert!(sk.contains("class Foo"), "{sk}");
    assert!(sk.contains("bar(int x)"), "{sk}");
    assert!(sk.contains("baz(int y)"), "{sk}");
}

#[cfg(feature = "lang-php")]
#[test]
fn php_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "svc.php",
        "<?php\nclass Svc {\n    public function run($a, $b = 1) { return $a; }\n}\nfunction helper($x) { return $x; }\n",
    );
    let mut e = engine(tmp.path());
    let sk = skeleton_of(&mut e, "svc.php");
    assert!(sk.contains("class Svc"), "{sk}");
    assert!(sk.contains("function helper($x)"), "{sk}");
    assert!(sk.contains("run($a, $b = 1)"), "{sk}");
}

#[cfg(feature = "lang-ruby")]
#[test]
fn ruby_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "svc.rb",
        "class Svc\n  def run(a, b = 1)\n    a\n  end\nend\n",
    );
    let mut e = engine(tmp.path());
    let sk = skeleton_of(&mut e, "svc.rb");
    assert!(sk.contains("class Svc"), "{sk}");
    assert!(sk.contains("def run(a, b = 1)"), "{sk}");
}

// ---------------------------------------------------------------------------
// Next-Gen Intelligence Engine: Tests, Verify, Git Coupling, Dead Code
// ---------------------------------------------------------------------------

#[test]
fn test_get_affected_tests() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/calc.py",
        "def add(a, b):\n    \"\"\"Add numbers.\"\"\"\n    return a + b\n\ndef sub(a, b):\n    return a - b\n",
    );
    write(
        tmp.path(),
        "tests/test_calc.py",
        "from src.calc import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    );
    let mut e = engine(tmp.path());

    // 1. Query affected tests by file
    let v = call_json(&mut e, "get_affected_tests", json!({"file_path": "src/calc.py"}));
    let files = v["affected_test_files"].as_array().unwrap();
    assert!(files.iter().any(|f| f.as_str().unwrap() == "tests/test_calc.py"), "{v}");
    let cmds = v["suggested_commands"].as_array().unwrap();
    assert!(cmds.iter().any(|c| c.as_str().unwrap().contains("pytest tests/test_calc.py")), "{v}");

    // 2. Query affected tests by symbol
    let v2 = call_json(&mut e, "get_affected_tests", json!({"symbol_name": "add"}));
    let callers = v2["affected_test_symbols"].as_array().unwrap();
    assert!(callers.iter().any(|c| c["test_symbol"] == "test_add"), "{v2}");
}

#[test]
fn test_verify_patch() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "core/math_util.py",
        "def compute(x, y):\n    return x + y\n",
    );
    let mut e = engine(tmp.path());

    // 1. Valid clean patch
    let v = call_json(
        &mut e,
        "verify_patch",
        json!({
            "file_path": "core/math_util.py",
            "patch": "def compute(x, y):\n    return x + y\n\ndef call_it():\n    return compute(10, 20)\n"
        }),
    );
    assert_eq!(v["valid"], true, "{v}");
    assert_eq!(v["syntax_errors"].as_array().unwrap().len(), 0);
    assert_eq!(v["arity_mismatches"].as_array().unwrap().len(), 0);

    // 2. Syntax error patch
    let v_err = call_json(
        &mut e,
        "verify_patch",
        json!({
            "file_path": "core/math_util.py",
            "patch": "def broken_syntax(:\n    return\n"
        }),
    );
    assert_eq!(v_err["valid"], false, "{v_err}");
    assert!(!v_err["syntax_errors"].as_array().unwrap().is_empty());

    // 3. Arity mismatch patch (calling compute with 1 arg instead of 2)
    let v_arity = call_json(
        &mut e,
        "verify_patch",
        json!({
            "file_path": "core/math_util.py",
            "patch": "def test_caller():\n    return compute(42)\n"
        }),
    );
    assert_eq!(v_arity["valid"], false, "{v_arity}");
    let mismatches = v_arity["arity_mismatches"].as_array().unwrap();
    assert!(!mismatches.is_empty(), "{v_arity}");
    assert_eq!(mismatches[0]["symbol"], "compute");
    assert_eq!(mismatches[0]["passed_args"], 1);
    assert_eq!(mismatches[0]["expected_min"], 2);
}

#[test]
fn test_get_co_changed_files() {
    let tmp = tempfile::tempdir().unwrap();
    // Initialize a git repo and make 2 commits touching paired files
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .arg("init")
        .output();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["config", "user.email", "test@example.com"])
        .output();

    write(tmp.path(), "schema.json", "{}");
    write(tmp.path(), "models.ts", "export interface User {}");
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["add", "."])
        .output();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["commit", "-m", "init schema and models"])
        .output();

    write(tmp.path(), "schema.json", "{\"version\": 2}");
    write(tmp.path(), "models.ts", "export interface User { id: string }");
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["commit", "-am", "update schema and models"])
        .output();

    let mut e = engine(tmp.path());
    let v = call_json(
        &mut e,
        "get_co_changed_files",
        json!({"file_path": "schema.json"}),
    );
    assert_eq!(v["file"], "schema.json");
    let list = v["co_changed_files"].as_array().unwrap();
    assert!(!list.is_empty(), "{v}");
    assert_eq!(list[0]["file"], "models.ts");
    assert_eq!(list[0]["co_change_count"], 2);
}

#[test]
fn test_find_dead_code() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/util.py",
        "def dead_orphan_helper():\n    return 42\n\ndef active_helper():\n    return 100\n",
    );
    write(
        tmp.path(),
        "src/app.py",
        "from src.util import active_helper\n\ndef main():\n    return active_helper()\n",
    );
    let mut e = engine(tmp.path());

    let v = call_json(&mut e, "find_dead_code", json!({}));
    let candidates = v["candidates"].as_array().unwrap();
    // dead_orphan_helper should be flagged as dead
    assert!(
        candidates.iter().any(|c| c["name"].as_str().unwrap().contains("dead_orphan_helper")),
        "{v}"
    );
    // active_helper should NOT be flagged as dead
    assert!(
        !candidates.iter().any(|c| c["name"].as_str().unwrap().contains("active_helper")),
        "{v}"
    );
}

#[test]
fn test_rust_struct_field_extraction_and_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/document.rs",
        "pub struct Document {\n    pub trailer_override: usize,\n}\n",
    );
    let mut e = engine(tmp.path());

    // 1. Search "trailer" matches "trailer_override" field
    let v = call_json(&mut e, "search_symbols", json!({"query": "trailer"}));
    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{v}");
    assert_eq!(results[0]["name"], "Document.trailer_override");

    // 2. Search "Document::trailer" normalizes :: and finds it
    let v2 = call_json(&mut e, "search_symbols", json!({"query": "Document::trailer"}));
    let results2 = v2["results"].as_array().unwrap();
    assert!(!results2.is_empty(), "{v2}");
    assert_eq!(results2[0]["name"], "Document.trailer_override");

    // 3. Source lookup for "trailer" falls back to trailer_override
    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "trailer"}))
        .unwrap();
    assert!(src.contains("trailer_override"), "{src}");

    // 4. Source lookup with Rust :: syntax
    let src2 = e
        .call("get_symbol_source", &json!({"symbol_name": "Document::trailer"}))
        .unwrap();
    assert!(src2.contains("trailer_override"), "{src2}");
}

#[test]
fn test_grep_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/document.rs",
        "pub struct Document {\n    pub trailer_override: Option<String>,\n}\n",
    );
    write(
        tmp.path(),
        "src/main.rs",
        "fn main() {\n    let _x = doc.trailer_override.unwrap();\n}\n",
    );
    let mut e = engine(tmp.path());

    let v = call_json(
        &mut e,
        "grep_workspace",
        json!({"pattern": "trailer_override"}),
    );
    assert_eq!(v["total_matches"], 2, "{v}");
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches[0]["file"], "src/document.rs");
    assert_eq!(matches[0]["line"], 2);
    assert!(matches[0]["content"].as_str().unwrap().contains("pub trailer_override"));
    assert_eq!(matches[1]["file"], "src/main.rs");
    assert_eq!(matches[1]["line"], 2);
}

#[test]
fn test_svelte_extraction_skeleton_and_trace() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Header.svelte",
        "<script lang=\"ts\">\n  export let title: string = 'Welcome';\n</script>\n<header>{title}</header>\n",
    );
    write(
        tmp.path(),
        "src/App.svelte",
        "<script lang=\"ts\">\n  import Header from './Header.svelte';\n  export let name: string = 'World';\n  let count = $state(0);\n  function handleClick() {\n    count += 1;\n  }\n</script>\n\n<Header {name} />\n<button on:click={handleClick}>Clicks: {count}</button>\n\n<style>\n  .highlight {\n    color: red;\n  }\n</style>\n",
    );
    let mut e = engine(tmp.path());

    let sk = e
        .call("get_file_skeleton", &json!({"file_path": "src/App.svelte"}))
        .unwrap();
    assert!(sk.contains("name"), "skeleton should contain prop name: {sk}");
    assert!(sk.contains("count"), "skeleton should contain count: {sk}");
    assert!(sk.contains("handleClick"), "skeleton should contain handleClick: {sk}");
    assert!(sk.contains(".highlight"), "skeleton should contain .highlight: {sk}");

    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "handleClick", "file_path": "src/App.svelte"}))
        .unwrap();
    assert!(src.contains("function handleClick()"), "{src}");
    assert!(src.contains("count += 1"), "{src}");

    // Trace handleClick -> called at on:click line
    let tr = call_json(&mut e, "trace_symbol", json!({"symbol_name": "handleClick", "file_path": "src/App.svelte"}));
    let callers = tr["matches"][0]["callers"].as_array().unwrap();
    assert!(!callers.is_empty(), "template event handler should register call: {tr}");

    // Trace Header -> called/referenced in App.svelte template
    let tr_hdr = call_json(&mut e, "trace_symbol", json!({"symbol_name": "Header"}));
    let hdr_callers = tr_hdr["matches"][0]["callers"].as_array().unwrap();
    assert!(
        hdr_callers.iter().any(|c| c["file"].as_str() == Some("src/App.svelte")),
        "Header should be referenced in App.svelte: {tr_hdr}"
    );
}

#[test]
fn test_vue_extraction_and_components() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Child.vue",
        "<script setup lang=\"ts\">\n  defineProps<{ msg: string }>();\n</script>\n<template><span>{{ msg }}</span></template>\n",
    );
    write(
        tmp.path(),
        "src/Parent.vue",
        "<script setup lang=\"ts\">\n  import Child from './Child.vue';\n  import { ref } from 'vue';\n  const count = ref(0);\n  function increment() {\n    count.value++;\n  }\n</script>\n\n<template>\n  <Child :msg=\"'Hello'\" />\n  <button @click=\"increment\">+1</button>\n</template>\n\n<style scoped>\n  .container {\n    display: flex;\n  }\n</style>\n",
    );
    let mut e = engine(tmp.path());

    let sk = e
        .call("get_file_skeleton", &json!({"file_path": "src/Parent.vue"}))
        .unwrap();
    assert!(sk.contains("count"), "{sk}");
    assert!(sk.contains("increment"), "{sk}");
    assert!(sk.contains(".container"), "{sk}");

    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "increment", "file_path": "src/Parent.vue"}))
        .unwrap();
    assert!(src.contains("count.value++"), "{src}");

    // Trace Child component
    let tr = call_json(&mut e, "trace_symbol", json!({"symbol_name": "Child"}));
    let callers = tr["matches"][0]["callers"].as_array().unwrap();
    assert!(
        callers.iter().any(|c| c["file"].as_str() == Some("src/Parent.vue")),
        "Child component should be called in Parent.vue: {tr}"
    );
}

#[test]
fn test_astro_frontmatter_and_template() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Card.astro",
        "---\ninterface Props {\n  title: string;\n}\nconst { title } = Astro.props;\nfunction formatTitle(t: string) {\n  return t.toUpperCase();\n}\n---\n<div class=\"card\"><h1>{formatTitle(title)}</h1></div>\n",
    );
    let mut e = engine(tmp.path());

    let sk = e
        .call("get_file_skeleton", &json!({"file_path": "src/Card.astro"}))
        .unwrap();
    assert!(sk.contains("Props"), "{sk}");
    assert!(sk.contains("formatTitle"), "{sk}");

    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "formatTitle", "file_path": "src/Card.astro"}))
        .unwrap();
    assert!(src.contains("t.toUpperCase()"), "{src}");
}

#[test]
fn test_css_extraction_and_variables() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/theme.css",
        ":root {\n  --primary-color: #3b82f6;\n  --spacing-md: 16px;\n}\n\n.btn {\n  padding: var(--spacing-md);\n}\n\n.btn-primary {\n  background: var(--primary-color);\n}\n\n#app-root {\n  margin: 0 auto;\n}\n\n@keyframes slideIn {\n  from { opacity: 0; }\n  to { opacity: 1; }\n}\n",
    );
    let mut e = engine(tmp.path());

    let sk = e
        .call("get_file_skeleton", &json!({"file_path": "src/theme.css"}))
        .unwrap();
    assert!(sk.contains("--primary-color"), "{sk}");
    assert!(sk.contains(".btn"), "{sk}");
    assert!(sk.contains(".btn-primary"), "{sk}");
    assert!(sk.contains("#app-root"), "{sk}");
    assert!(sk.contains("@keyframes slideIn"), "{sk}");

    let search = call_json(&mut e, "search_symbols", json!({"query": "btn-primary"}));
    assert!(search["total_matches"].as_u64().unwrap() >= 1, "{search}");
}

#[test]
fn test_sfc_cross_file_impact() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Header.svelte",
        "<script lang=\"ts\">\n  export let title: string = 'Welcome';\n</script>\n<h1>{title}</h1>\n",
    );
    write(
        tmp.path(),
        "src/App.svelte",
        "<script lang=\"ts\">\n  import Header from './Header.svelte';\n</script>\n<Header title=\"Hello\" />\n",
    );
    let mut e = engine(tmp.path());

    // Context of Header.svelte should list App.svelte as dependent
    let ctx = call_json(&mut e, "get_file_context", json!({"file_path": "src/Header.svelte"}));
    let deps = ctx["dependents"]["files"].as_array().unwrap();
    assert!(
        deps.iter().any(|d| d["file"].as_str() == Some("src/App.svelte")),
        "Header.svelte should have App.svelte as dependent: {ctx}"
    );
}

#[test]
fn test_html_scripts_styles_and_ids() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "index.html",
        "<!DOCTYPE html>\n<html>\n<head>\n  <style>\n    .hero-title {\n      font-size: 2rem;\n    }\n  </style>\n</head>\n<body>\n  <div id=\"main-content\">\n    <h1 class=\"hero-title\">Hello</h1>\n  </div>\n  <script>\n    function startApp() {\n      console.log('App started');\n    }\n    startApp();\n  </script>\n</body>\n</html>\n",
    );
    let mut e = engine(tmp.path());

    let sk = e
        .call("get_file_skeleton", &json!({"file_path": "index.html"}))
        .unwrap();
    assert!(sk.contains("startApp"), "{sk}");
    assert!(sk.contains(".hero-title"), "{sk}");
    assert!(sk.contains("#main-content"), "{sk}");

    let src = e
        .call("get_symbol_source", &json!({"symbol_name": "startApp", "file_path": "index.html"}))
        .unwrap();
    assert!(src.contains("console.log('App started')"), "{src}");
}



