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

## 💡 Why BlastCode?

AI coding agents waste tokens and context windows reading entire files, and they frequently make breaking changes to callers across directories because they don't know who calls what.

**BlastCode sits directly on your workspace and solves this locally:**
1. **Read Less**: Supplies file skeletons, single-symbol extracts, and 1-call full file context (outline + imports + dependents + recent changes).
2. **Blast Radius Analysis**: Tells the agent *"if I change this signature, what breaks?"* **before** the agent modifies code on disk.
3. **Workspace Caretaker**: A background watcher continuously diffs edits against previous AST symbols, reporting what changed in a clean digest at the start of the next tool response.
4. **Sub-second Speed**: Written in pure Rust with Tree-sitter and SQLite. Zero cloud roundtrips, 100% local, read-only on your codebase.

---

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
