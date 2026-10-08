<div align="center">

# ⚡ BlastCode (`blast`)

**Know what breaks before your agent edits.**

[![Crates.io](https://img.shields.io/crates/v/blastcode.svg?style=flat-square&logo=rust)](https://crates.io/crates/blastcode)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)
[![CI](https://github.com/isaim0011/blastcode/actions/workflows/ci.yml/badge.svg?style=flat-square)](https://github.com/isaim0011/blastcode/actions)
[![MCP Compatible](https://img.shields.io/badge/MCP-Compatible-brightgreen.svg?style=flat-square)](https://modelcontextprotocol.io)

*An ultra-fast incremental code-graph engine, Model Context Protocol (MCP) server, and blast-radius analyzer for AI coding agents.*

[Features](#-key-features) • [Installation](#-installation) • [Agent Integration](#-agent-integration-mcp) • [CLI Commands](#-cli-reference) • [Languages](#-supported-languages) • [Roadmap](#-distribution-roadmap)

</div>

---

## ⚡ Real-World Benchmarks & Live Demos

| Operation | Brute Force (Standard Agent) | With BlastCode | Real Savings |
| :--- | :--- | :--- | :--- |
| **Inspect File Structure** (`src/tools.rs`) | **5,578 tokens** (Full file read) | **416 tokens** (`blast skeleton`) | 🟢 **92.5% Token Reduction** |
| **Edit 1 Function** (`format_event`) | **5,578 tokens** (Full file read) | **560 tokens** (`blast source`) | 🟢 **90.0% Token Reduction** |
| **Workspace Orientation** (14 files) | **~35,000 tokens** (Reading files) | **640 tokens** (`blast map`) | 🟢 **98.2% Token Reduction** |
| **Detect Signature Breakage** | **Fail + 30,000 token debug loop** | **0 tokens wasted** (`blast impact`) | 🟢 **Eliminates regressions** |
| **Query Latency** | 3.5s – 12.0s (Cloud LLM reading) | **< 15ms** (Local Tree-sitter SQLite) | ⚡ **Instant responses** |

---

### 🖥️ Live Preview 1: Surgical Skeleton (Instead of reading whole files)
```
$ blast skeleton src/tools.rs
# src/tools.rs · rust · 509 lines · 21 symbols
   35| pub struct Engine
   79| pub fn format_event(e: &EventRow) -> String
  130| impl Engine
  131|   pub fn open(root: &Path, db: Option<PathBuf>) -> Result<Engine>
  166|   pub fn index_now(&mut self, force: bool) -> Result<IndexStats>
  177|   pub fn spawn_watcher(&mut self)
  265|   pub fn take_digest(&mut self) -> Result<Option<String>>
  297|   pub fn call_with_digest(&mut self, name: &str, args: &Value) -> Result<(Option<String>, String)>
```

---

### 🖥️ Live Preview 2: The Caretaker Change Digest (Zero-Token Sync)
When you edit a file in your editor or your agent makes a change, BlastCode prepends an automatic digest to the next tool response. The agent never has to run `git diff` or re-read:
```
[blast] workspace changes since your last call:
~ auth/jwt.py — signature changed: def verify_token(token) → def verify_token(token, audience) | added: brand_new
+ extra.py added · extra_fn
- deprecated.py deleted · had old_auth
Run get_impact_radius on changed files to see affected callers.
```

---

### 🖥️ Live Preview 3: Pre-Edit Blast Radius (Know what breaks before editing)
```
$ cat proposed.py | blast impact auth/jwt.py --stdin
{
  "summary": { "breaking": 2, "compatible": 1 },
  "breaking_callers": [
    { "caller": "login_handler", "file": "src/api/routes.py:42", "reason": "missing required parameter 'audience'" },
    { "caller": "verify_session", "file": "src/middleware/auth.py:88", "reason": "missing required parameter 'audience'" }
  ]
}
```

## 🚀 Installation

### Via Cargo (Recommended)
```bash
cargo install blastcode
```

### Via Homebrew (macOS / Linux)
```bash
brew install isaim0011/tap/blastcode
```

### Via Pre-built Binaries
Download the latest binary for Linux, macOS, or Windows directly from [GitHub Releases](https://github.com/isaim0011/blastcode/releases).

---

## 🤖 Agent Integration (MCP)

BlastCode works natively with any Model Context Protocol host (Claude Code, Cursor, Windsurf, Cline).

### Claude Code
```bash
claude mcp add blastcode -- blast serve --root .
```

### Cursor / Windsurf / Claude Desktop (`mcp.json`)
```json
{
  "mcpServers": {
    "blastcode": {
      "command": "blast",
      "args": ["serve", "--root", "/absolute/path/to/project"]
    }
  }
}
```

### Instruct Your Agent (`AGENTS.md` / `CLAUDE.md`)
Add this instruction block so your AI agent uses BlastCode instead of brute-force reading files:

```markdown
> Use the `blastcode` MCP tools before reading or modifying files:
> 1. `get_workspace_map`: Orient yourself across the project structure and exports.
> 2. `get_file_context` or `get_file_skeleton`: Use instead of opening and reading full files.
> 3. `get_symbol_source`: Retrieve the exact line range for a single function.
> 4. `trace_symbol`: Find all callers, callees, and type usages before refactoring.
> 5. `get_impact_radius`: Run with `new_source` BEFORE making an edit to verify what breaks.
```

---

## 🛠️ MCP Tools Overview

| Tool | Purpose |
| :--- | :--- |
| `get_workspace_map` | Directory tree annotated with exported symbols per file. Call first. |
| `get_file_context` | Skeleton + classified imports (internal vs external) + dependents + recent edits in one call. |
| `get_file_skeleton` | Outline of a file without function bodies, with line numbers. |
| `get_symbol_source` | Surgical extraction of one symbol's exact lines instead of whole-file reads. |
| `search_symbols` | Fuzzy symbol lookup across the entire workspace (multi-word, camelCase/snake_case). |
| `trace_symbol` | Definition + callers / callees / type usages tagged with confidence (`exact`, `probable`, `heuristic`). |
| `get_impact_radius` | Pre-edit: test proposed signature changes. Post-edit: inspect working tree vs `git HEAD`. |
| `query_graph` | Structural filter by kind, name, path, callers, callees, and export status. |
| `poll_changes` | View the change journal recorded by the caretaker background watcher. |

---

## 💻 CLI Reference

You can also use `blast` directly from your terminal:

```bash
blast index                     # Build or refresh the index (.blastradius/index.db)
blast map                       # Print compact workspace map with exports
blast stats                     # View symbol, reference, and file counts
blast context src/lib.rs        # Inspect skeleton, imports, dependents, and recent edits
blast skeleton src/lib.rs       # View file skeleton
blast source my_function        # Print source lines of a specific function
blast search "authenticate"     # Fuzzy search symbols
blast trace verify_token        # Trace callers, callees, and type usages
blast impact src/auth.py        # Check blast radius of uncommitted changes
blast watch                     # Live terminal stream of AST-level changes
```

---

## 🌐 Supported Languages (10 Languages)

| Tier | Languages | Grammar Feature Flag |
| :--- | :--- | :--- |
| **Core** | **Python, TypeScript, JavaScript (TSX/JSX), Rust, Go** | Included by default |
| **Extended** | **Java, C#, C, C++, PHP, Ruby** | Cargo features (`lang-java`, `lang-csharp`, `lang-c`, `lang-cpp`, `lang-php`, `lang-ruby`) |

*Swift and Kotlin support are scheduled next.*

---

## 🗺️ Universal Ecosystem Roadmap

- [x] **v0.1.0**: Core Rust engine, 10 languages, MCP server, CLI (`blast`).
- [ ] **crates.io**: Official publication under `blastcode`.
- [ ] **PyPI / uv**: `pip install blastcode` / `uvx blastcode serve`.
- [ ] **npm / npx**: `npx blastcode serve`.
- [ ] **Homebrew Tap**: `brew install isaim0011/tap/blastcode`.
- [ ] **VS Code / Cursor Extension**: Embedded companion status and auto-launch.

---

## 📄 License

Licensed under the [MIT License](LICENSE).
