# Frontend SFC & Web Technologies Stack Digest

## 1. tree-sitter-html (v0.23.2)
- **Crate**: `tree-sitter-html = "0.23.2"`
- **API**: `tree_sitter_html::language()` -> `tree_sitter::Language`
- **Grammar Root**: `fragment` / `document`
- **Key Nodes**:
  - `element`: contains `start_tag`, child elements, `end_tag`
  - `script_element`: contains `start_tag`, `raw_text`, `end_tag`
  - `style_element`: contains `start_tag`, `raw_text`, `end_tag`
  - `tag_name`: identifier for element tag
  - `attribute`: contains `attribute_name` and optional `quoted_attribute_value`

## 2. tree-sitter-css (v0.23.1)
- **Crate**: `tree-sitter-css = "=0.23.1"` (pinned to ABI 13/14 for tree-sitter 0.24)
- **API**: `tree_sitter_css::language()` -> `tree_sitter::Language`
- **Grammar Root**: `stylesheet`
- **Key Nodes**:
  - `rule_set`: `selectors` and `block`
  - `class_selector`: `class_name`
  - `id_selector`: `id_name`
  - `declaration`: `property_name` (e.g. `--custom-var`) and `value`
  - `at_rule`: `@keyframes`, `@media`, `@mixin`, `@include`, `@import`
  - `keyframes_statement`: `@keyframes <name>`

## 3. Svelte (.svelte)
- **File Structure**:
  - `<script>` or `<script lang="ts">` or `<script context="module">`: TypeScript / JavaScript
  - Svelte 3/4 Props: `export let foo: string;`
  - Svelte 5 Runes: `let { x, y } = $props();`, `let count = $state(0);`, `let double = $derived(...)`
  - Template: Subcomponents `<Header />`, event bindings `on:click={handleClick}`, snippets `{#snippet}`
  - `<style>`: Scoped CSS
- **Indexing Strategy**:
  - Extract TypeScript/JS from script blocks with virtual line-matched buffer into Tree-sitter TSX.
  - Component-level symbol from file basename (e.g. `App.svelte` -> `App` component).
  - Extract template component references (`<Header>`, `<Modal>`) for cross-file call/dependency graph.
  - Extract CSS classes and variables from `<style>`.

## 4. Vue (.vue)
- **File Structure**:
  - `<script setup lang="ts">` or `<script lang="ts">` or `<script>`: TypeScript / JavaScript
  - Options API: `export default { props, data, methods }`
  - Composition API / script setup: `defineProps<Props>()`, `defineEmits()`, `ref()`, `computed()`, functions
  - `<template>`: Subcomponents `<UserCard>`, `<el-button>`, event handlers `@click="save"`
  - `<style scoped>`: Scoped CSS
- **Indexing Strategy**:
  - Extract TypeScript/JS from script blocks with virtual line-matched buffer into Tree-sitter TSX.
  - Component-level symbol from file basename.
  - Extract template component references and event bindings into call-graph.
  - Extract CSS classes and variables from `<style>`.

## 5. Astro (.astro)
- **File Structure**:
  - Frontmatter fences `---` ... `---`: pure TypeScript/JavaScript!
  - `interface Props { ... }`, `const { title } = Astro.props;`
  - Client scripts: `<script>`
  - Template markup: `<Layout>`, `<Header>`
  - `<style>`: Scoped CSS
- **Indexing Strategy**:
  - Extract frontmatter into Tree-sitter TSX.
  - Component-level symbol from file basename.
  - Template component references.
  - CSS from `<style>`.

## 6. HTML (.html, .htm)
- **File Structure**:
  - `<script>` blocks: JavaScript/TypeScript functions, imports, variables.
  - `<style>` blocks: CSS.
  - Elements with `id="..."`: IDs as anchor symbols.
