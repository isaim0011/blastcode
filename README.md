<div align="center">

<img src="https://raw.githubusercontent.com/isaim0011/blastcode/main/assets/banner.png" alt="BlastCode Banner" width="100%"/>
<br/><br/>

# ⚡ BlastCode (`blast`)

**Know what breaks before your agent edits.**

[![Crates.io](https://img.shields.io/crates/v/blastcode.svg?style=flat-square&logo=rust)](https://crates.io/crates/blastcode)
[![npm](https://img.shields.io/npm/v/blastcode.svg?style=flat-square&logo=npm)](https://www.npmjs.com/package/blastcode)
[![PyPI](https://img.shields.io/pypi/v/blastcode.svg?style=flat-square&logo=pypi)](https://pypi.org/project/blastcode/)
[![Open VSX](https://img.shields.io/badge/Open%20VSX-v0.3.0-purple.svg?style=flat-square&logo=visualstudiocode)](https://open-vsx.org/extension/isaim0011/blastcode-vscode)
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
<p align="center">
  <img src="https://raw.githubusercontent.com/isaim0011/blastcode/main/assets/demo_skeleton.png" alt="BlastCode Skeleton Demo" width="850"/>
</p>

---

### 🖥️ Live Preview 2: The Caretaker Change Digest (Zero-Token Sync)
<p align="center">
  <img src="https://raw.githubusercontent.com/isaim0011/blastcode/main/assets/demo_caretaker.png" alt="BlastCode Caretaker Demo" width="850"/>
</p>

---

### 🖥️ Live Preview 3: Pre-Edit Blast Radius (Know what breaks before editing)
<p align="center">
  <img src="https://raw.githubusercontent.com/isaim0011/blastcode/main/assets/demo_impact.png" alt="BlastCode Impact Radius Demo" width="850"/>
</p>

## 🚀 Installation & Zero-Install Running

### Via npm / npx (Universal Zero-Install)
```bash
# Run stdio MCP server anywhere immediately:
npx blastcode serve

# Or install globally:
npm install -g blastcode
```

### Via Python / PyPI / uvx
```bash
# Run stdio MCP server via uv:
uvx blastcode serve

# Or install via pip:
pip install blastcode
```

### Via Cargo (Rust)
```bash
cargo install blastcode
```

### Via Homebrew (macOS / Linux)
```bash
brew install isaim0011/tap/blastcode
```

### Via Smithery / PulseMCP Registry
```bash
npx -y @smithery/cli install blastcode --client claude
```

### Via VS Code / Cursor Extension
Install **BlastCode** from Open VSX or the VS Code Marketplace:
```bash
code --install-extension isaim0011.blastcode-vscode
# Or in Cursor:
cursor --install-extension isaim0011.blastcode-vscode
```

---

## 🤖 Agent Integration (MCP)

BlastCode works natively with any Model Context Protocol host (Claude Code, Cursor, Windsurf, Cline, Smithery).

### Claude Code
```bash
claude mcp add blastcode -- npx blastcode serve --root .
```

### Cursor / Windsurf / Claude Desktop (`mcp.json`)
```json
{
  "mcpServers": {
    "blastcode": {
      "command": "npx",
      "args": ["-y", "blastcode", "serve", "--root", "/absolute/path/to/project"]
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
> 6. `verify_patch`: Pre-flight validate proposed edit syntax and argument counts before writing to disk.
> 7. `get_affected_tests`: Find tests covering the changes and run the generated targeted test command.
> 8. `get_co_changed_files`: Discover implicit paired dependencies from git commit history.
> 9. `find_dead_code`: Detect unreferenced or orphaned symbols.
> 10. `grep_workspace`: Fast multi-threaded regex/token search across workspace files.
```

---

## 🛠️ MCP Tools Overview (14 Intelligent Capabilities)

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
| `get_workspace_changes` | View the change journal recorded by the caretaker background watcher. |
| `get_affected_tests` | 🧪 Discovers test files/symbols covering changed files & generates targeted CLI test commands (`cargo test`, `pytest`, `npm test`, `go test`). |
| `verify_patch` | 🛡️ Pre-flight validation of proposed code edits in memory: checks AST syntax and call-site arities before saving to disk. |
| `get_co_changed_files` | 🔗 Mines Git commit history to discover files that frequently change together (implicit dependencies). |
| `find_dead_code` | 🧹 Code graph analysis detecting unreferenced, dead, or orphaned functions and classes across the codebase. |
| `grep_workspace` | 🔍 High-speed multi-threaded regex and token search across workspace files. Respects .gitignore, skips binaries. |

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
blast tests src/tools.rs        # Find affected tests and targeted test command
blast verify src/auth.py        # Pre-flight syntax and arity validation
blast coupled Cargo.toml        # Find frequently co-committed files via Git
blast dead --limit 20           # Detect dead / unreferenced symbols
blast grep "pattern"            # Ultra-fast multi-threaded workspace regex search
blast watch                     # Live terminal stream of AST-level changes
```

---

## 🌐 Supported Languages (16 Languages & Web Formats)

| Tier | Languages & Formats | Details |
| :--- | :--- | :--- |
| **Core Systems** | **Python, TypeScript, JavaScript (TSX/JSX), Rust, Go** | Included by default, zero config |
| **Frontend & Web** | **Svelte (`.svelte`), Vue (`.vue`), Astro (`.astro`), HTML (`.html`), CSS/SCSS (`.css`, `.scss`, `.less`)** | Svelte 5 runes (`$props`, `$state`), Vue SFC (`defineProps`, `ref`), Astro frontmatter, template components & CSS class selectors |
| **Polyglot & Enterprise** | **Java, C#, C, C++, PHP, Ruby** | Cargo features (`lang-java`, `lang-csharp`, `lang-c`, `lang-cpp`, `lang-php`, `lang-ruby`) |

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
