# Embedded-language extraction strategy

The Vue prototype validates the embedded extractor pattern. The important architecture is not a Vue grammar; it is a region pipeline:

1. Detect source regions with cheap outer-language scans.
2. Parse each embedded code region with the real language grammar.
3. Remap sub-parse line numbers back to the original file.
4. Merge nodes, edges, unresolved references, and parse errors into the parent file result.

## Vue prototype

The Vue extractor creates the `.vue` file's `file` node (`file:<path>`) holding one `component` node, then extracts `<script>` / `<script setup>` regions:

```text
/<script(\s[^>]*)?>(?<content>[\s\S]*?)<\/script>/g
```

The attribute string drives language selection:

- `lang="ts"` or `lang="typescript"` → TypeScript grammar.
- otherwise → JavaScript grammar.
- both normal and setup blocks are delegated to the TypeScript or JavaScript extractor; `setup` marks a block that runs once per component instance.

The delegated result is folded into the component (upstream `foldScriptResult`, #2268): nodes and refs take the parent language (`vue`) and their file lines; the component contains each symbol the block holds at its top level, while a nested symbol keeps its own parent; the block's own file node is dropped, since the `.vue` file node is the file. A per-instance block's top-level calls and references, a top-level constant's or variable's initializer's included, are the component's; its imports, and everything a non-setup block does at its top level, stay with the file node. Template component usages are scanned outside `<script>` / `<style>` ranges with:

```text
/<([A-Za-z][A-Za-z0-9_-]*)\b/g
```

PascalCase tags are references directly. Kebab-case tags are converted to PascalCase. Lowercase tags without hyphens are treated as native HTML. Vue built-ins (`Transition`, `TransitionGroup`, `KeepAlive`, `Suspense`, `Teleport`, `Component`, `Slot`) are skipped after kebab-to-Pascal conversion.

A non-setup block's Options API component (upstream #2140): when the default export is the options object, directly or through `defineComponent`, `defineNuxtComponent`, `Vue.extend` or `Vue.component('x', …)`, each function in it, the `methods`, `computed` and `watch` entries (a `get`/`set` or `{ handler }` object included) and the component's own `data`, `setup`, `render`, lifecycle hooks and Nuxt 2 `asyncData` / `fetch` / `head` / `validate` / `middleware`, is a `method` node of the component, `<Component>::<name>`, and the references and edges the script extractor gave the file inside it are its own. Resolution reaches such a method by name only as `this.m()` in its own file: `e.preventDefault()`, `this.editor.setValue()` and a template ref's `this.$refs.x.focus()` never do.

Calls written in the root `<template>` (upstream #2357): a quote-aware tag scanner reads `{{ }}` interpolations and the values of directives (`:x` / `v-bind`, `@x` / `v-on`, `v-if`, `v-show`, `v-model`, `v-for`'s source, custom `v-*`), and a small lexer names each callee the way a script call is named (`useBar`, `store.fetchUsers`). Names the template binds (`v-for` aliases and slot props in their element's subtree, arrow and function parameters), `$` helpers, Vue's template globals, keywords, `new X()`, object-method shorthand, `v-pre` subtrees and a non-HTML template (`lang="pug"`) are skipped. Each call is a `calls` reference from the component at its own line and column; a bare call to the component's own Options API method is linked to it at extraction. A handler written as a bare name (`@click="save"`) is no call here.

### Offset formula

The offset is line-based. It computes the embedded content offset as:

```text
scriptTagLine     = count('\n' before the <script> match)
openingTagLines   = count('\n' inside the opening <script ...> tag)
block.startLine   = scriptTagLine + openingTagLines + 1   // 0-indexed line after opening tag
originalLine      = delegatedLocalLine + block.startLine  // delegatedLocalLine is 1-indexed
```

The Rust prototype parses the captured content directly, including a possible leading newline immediately after `>`. For tree-sitter rows (`0`-indexed) the equivalent source-position mapping is:

```text
contentByte0Line  = scriptTagLine + openingTagLines        // 0-indexed line containing content byte 0
originalLine      = treeSitterRow + contentByte0Line + 1
```

This preserves original `.vue` coordinates. In `tests/fixtures/sample.vue`, `export function doSomething()` is reported at line 15 and `import MyComponent from './MyComponent.vue'` at line 13.

Tree-sitter 0.26 note: `QueryCursor::matches()` returns a `StreamingIterator`, not a Rust `Iterator`. Query loops must import `streaming_iterator::StreamingIterator` and use `while let Some(m) = matches.next() { ... }`.

## Per-format notes

### Svelte

- Region regex is the same script-block shape as Vue: `/<script(\s[^>]*)?>(?<content>[\s\S]*?)<\/script>/g`.
- `lang="ts"` / `lang="typescript"` selects TypeScript; otherwise JavaScript.
- `context="module"` (Svelte 4) or a bare `module` attribute (Svelte 5) marks a module script; any other script is an instance script.
- Script results fold into the component exactly as Vue's do, with language `svelte`: an instance script runs per component instance, so its top-level calls are the component's; a module script's stay with the `.svelte` file node.
- Template scan additionally finds calls inside `{...}` expressions with `/\{([^}#/:@][^}]*)\}/g` and call names with `/\b([a-zA-Z_$][\w$.]*)\s*\(/g`.
- Svelte 5 runes (`$props`, `$state`, `$derived`, `$effect`, etc.) are compiler built-ins and must be filtered.
- PascalCase tags become component references; lowercase native tags are skipped.

### Astro

- The `---`-fenced frontmatter and each `<script>` block are TypeScript, delegated to the TypeScript extractor at their file lines.
- Both fold into the component exactly as Vue's do, with language `astro`: the `.astro` file node holds the component, and since the frontmatter runs on every render and a `<script>` is the page's own client code, both are per-instance, so their top-level calls are the component's while their imports stay with the file node.
- The template contributes `{fn(...)}` calls and PascalCase component references outside the frontmatter and script ranges; `Fragment`, `Code` and `Debug` are built-ins.

### Razor / Blazor

- Always creates one component node for `.razor` / `.cshtml`.
- Markup references come from directives and Blazor tags, not a general HTML parser:
  - `@model Foo`, `@inherits Bar<Foo>`, `@inject IService svc`, `@typeof(MainLayout)`.
  - PascalCase Blazor component tags only for `.razor` files.
  - Generic component type arguments such as `TItem="CatalogItem"` become references.
- C# regions are extracted from `@code { ... }`, `@functions { ... }`, and `@{ ... }` by matching braces while skipping strings/comments.
- The C# block is wrapped as `class __RazorCode__ {\n<block>\n}` before delegation. The wrapper adds one synthetic line, so unresolved-reference lines map back as `ref.line + block.lineOffset - 1`.
- Built-in Blazor components are filtered to avoid unresolvable framework refs.

### Liquid

- Liquid is not delegated to tree-sitter; it is a template-specific regex/JSON extractor.
- A Shopify theme is a directory holding `layout/theme.liquid` or `config/settings_schema.json` beside its theme folders (`assets`, `blocks`, `config`, `layout`, `locales`, `sections`, `snippets`, `templates`, in any case), looked for under the project root. A marker counts wherever it is in the project, ignored or not: it only tells a theme apart.
- `.json` Shopify OS 2.0 templates and section groups, JSON under a theme's `templates/` or `sections/` (nested or not), are parsed as JSON and `sections.*.type` becomes `sections/<type>.liquid` references. JSON in folders of those names that no theme holds (a .NET project template's `templates/<name>/.template.config/template.json`, schematics, CMS content) is not Liquid, and without a project root no JSON is. A marker that comes or goes reclassifies the theme's unchanged JSON on the next sync, and `status`'s git fast path declines a marker among its candidates.
- A `sections/<name>.liquid` or `snippets/<name>.liquid` reference made in a file inside a theme (the directory holding the nearest theme folder the file sits in, when it holds a marker) resolves to that theme's own file or stays unresolved: Shopify never looks in another theme. A reference made in no theme resolves by path as before.
- `.liquid` templates scan:
  - snippets: `/\{%[-]?\s*(render|include)\s+['"]([^'"]+)['"]/g` → `snippets/<name>.liquid`.
  - sections: `/\{%[-]?\s*section\s+['"]([^'"]+)['"]/g` → `sections/<name>.liquid`.
  - schema blocks: `/\{%[-]?\s*schema\s*[-]?%\}([\s\S]*?)\{%[-]?\s*endschema\s*[-]?%\}/g`.
  - assignments: `/\{%[-]?\s*assign\s+(\w+)\s*=/g`.
- Line numbers are character-index based: count newlines before the regex match and add one.

### MyBatis XML

- Non-mapper XML returns only a file node. Mapper XML is detected by `<mapper namespace="...">`.
- Statement regions are top-level `<select|insert|update|delete|sql ...>...</...>` elements inside the mapper body.
- Each statement/fragment becomes a method-shaped node qualified as `<namespace>::<id>`.
- `<include refid="...">` becomes an unresolved reference to either `<namespace>::<refid>` or, for qualified refids, a dotted-name replacement (`a.b.Fragment` → `a::b::Fragment`).
- Offsets are byte-index based with a precomputed line-start table and binary search, which is preferable for XML because statement bodies and nested tags can be large.

## Guidance for task 18

- Keep Vue, Svelte, Razor, Liquid, and MyBatis as custom extractors. Do not wait for full outer-language grammars before shipping useful graph coverage.
- Reuse one embedded-region abstraction with: `content`, `language`, `content_byte_start`, `line_offset`, and optional `synthetic_wrapper_line_delta`.
- Prefer byte-offset-to-line remap when the captured content can include leading delimiters/newlines. It avoids the off-by-one ambiguity in line-only formulas.
- Preserve the merge semantics: embedded symbols keep the parent file path, parent embedded language (`vue`, `svelte`, `razor`, etc.), and receive parent-component/file containment edges; Vue, Svelte and Astro fold their scripts as described above.
- Template-only references should remain unresolved refs unless the extractor creates explicit nodes for them (Liquid snippets/sections are the notable exception).
- Rust `regex` has no backreferences; translate regexes such as `</\1>` into explicit alternation or a small scanner.
- Add fixtures that assert original-file line numbers, not region-local lines, for every delegated extractor.
