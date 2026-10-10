# Changelog

All notable changes to **BlastCode** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.3.0] - 2026-10-11

### Added
- **Frontend Single-File Component (SFC) & Web Template Engine**:
  - **Svelte (`.svelte`)**: First-class support for Svelte 3, 4, and 5 runes (`$props()`, `$state()`, `$derived()`, `export let`). Extracts component definitions, props, reactive state, script functions, template component tags (`<Header />`), and event bindings (`on:click`, `onclick`).
  - **Vue (`.vue`)**: Full support for Vue 2 & 3 SFCs (`<script setup>`, `defineProps`, `defineEmits`, `ref`, `computed`, and Options API). Extracts template component instances and event directives (`@click`, `v-on:`).
  - **Astro (`.astro`)**: Native parsing of component frontmatter fences (`---` ... `---`), `interface Props`, `Astro.props`, client scripts, and layout/component tags.
  - **HTML (`.html`, `.htm`)**: Extracts embedded `<script>` blocks (functions, variables, imports), embedded `<style>` blocks, and DOM element anchor IDs (`id="..."`).
  - **CSS / SCSS (`.css`, `.scss`, `.sass`, `.less`)**: Multi-rule AST indexing via `tree-sitter-css`. Extracts class selectors (`.btn-primary`), ID selectors (`#app`), CSS custom variables (`--primary-color`), `@keyframes` animations, and SCSS `@mixin` blocks.
- **Cross-Component Call-Graph & Blast-Radius Tracing**:
  - Component usages in templates (`<ChildComponent />`) are automatically indexed as call-graph edges.
  - When a component is modified, `blast impact` and `blast trace` immediately identify all parent components that instantiate it.
- **Token-Lean Web Skeletons**:
  - Replaces massive 500-1500 line template files and thousands of lines of CSS with 20-50 token architectural skeletons with exact line numbers, saving massive agent context and token budgets.

## [0.2.1] - 2026-10-09

### Added
- **High-Speed Workspace Grep Engine (`grep_workspace`)**:
  - Multi-threaded regex and token search across workspace files powered by `ignore` and `regex`.
  - Automatically respects `.gitignore`, skips binary files, and filters hidden paths.
  - New CLI subcommand: `blast grep <pattern> [--path <glob>] [--case-sensitive] [--limit <n>] [--json]`.
- **Struct Field, Enum Variant, Const & Static Extraction**:
  - Extracted struct fields (e.g. `pub trailer_override: ...`), enum variants, consts, and statics into the symbol index across Rust, Go, TypeScript/JS, and Python.
  - Field members are now fully searchable via `blast search` and extractable via `blast source`.
- **Resilient Path Normalization & Fuzzy Fallback**:
  - Language path syntax normalization (`::` converted to `.`) for seamless lookup of scoped items (e.g. `Document::trailer` -> `Document.trailer`).
  - Substring fallback matching in `find_defs` so partial names (e.g. `trailer` for `trailer_override`) resolve to the exact symbol definition.

## [0.2.0] - 2026-10-09

### Added
- **Affected Test Discovery & Runner Generation (`get_affected_tests`)**:
  - Automatically identifies all unit and integration test files & test functions covering any modified file or symbol across 10 programming languages.
  - Generates the exact language-specific CLI test runner command (`cargo test`, `pytest`, `go test`, `npm test`, `mvn test`, `dotnet test`, `phpunit`, `rspec`).
- **Pre-Flight AST & Arity Patch Validator (`verify_patch`)**:
  - In-memory validation of proposed code edits before writing to disk.
  - Tree-sitter syntax parsing catches broken syntax tokens without disk I/O.
  - Call-site arity checks against indexed SQLite signatures flag incorrect parameter counts before code is saved.
- **Git Commit Co-Change Coupling Intelligence (`get_co_changed_files`)**:
  - Mines Git commit history to discover files that frequently change together.
  - Discovers implicit, non-syntactic dependencies (e.g. schema migrations, paired types, documentation).
- **Dead & Orphaned Symbol Graph Analysis (`find_dead_code`)**:
  - Detects unreferenced functions, classes, and structs with zero callers, type usages, and imports across the entire workspace.
  - Categorizes confidence levels (`probable` for internal symbols, `heuristic` for exported symbols).
- **Official Smithery Registry Manifest (`smithery.yaml`)**:
  - Standardized configuration manifest enabling zero-install startup via `@smithery/cli` and PulseMCP.
- **New CLI Subcommands**:
  - `blast tests <file> [--symbol <name>]`
  - `blast verify <file> [--stdin]`
  - `blast coupled <file> [--depth <n>]`
  - `blast dead [--prefix <dir>]`

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
