# Changelog

All notable changes to **BlastCode** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0] - 2026-10-08

### Added
- **10 Language Parsers**:
  - Core: Python, TypeScript / JavaScript (TSX/JSX), Rust, Go.
  - Extended: Java, C#, C, C++, PHP, Ruby.
- **Incremental AST-Derived SQLite Indexer**:
  - Blake3 content-hash caching; only changed files re-indexed.
  - Parallel AST extraction using Rayon thread pools.
- **Workspace Caretaker**:
  - Continuous background file watcher and symbol-level diff engine.
  - Bounded change journal reporting exact diffs once as separate response blocks.
  - Live terminal event streamer via `blast watch`.
- **Read-Less Structural Tools for AI Agents**:
  - `get_file_context`: File skeleton, classified imports (internal vs external), dependents, and recent modifications in a single call.
  - `get_symbol_source`: Surgical extraction of a single symbol/function definition by line range.
  - `get_file_skeleton`: Outline of declarations without function bodies.
  - `search_symbols`: Fast multi-word fuzzy symbol lookup.
  - `trace_symbol`: Definition, callers, callees, and type usages tagged with confidence (`exact`, `probable`, `heuristic`).
  - `get_impact_radius`: Static blast-radius calculation for proposed signature edits before and after code changes.
  - `get_workspace_map`: Structural workspace overview annotated with exported symbols.
  - `query_graph`: Graph querying with structural filters.
- **Model Context Protocol (MCP)**:
  - Full JSON-RPC 2.0 stdio server (`blast serve`).
  - Compatible with Claude Code, Cursor, Windsurf, Cline, and custom agent loops.
- **CLI Commands**:
  - `blast index`, `blast map`, `blast stats`, `blast search`, `blast trace`, `blast impact`, `blast watch`.
