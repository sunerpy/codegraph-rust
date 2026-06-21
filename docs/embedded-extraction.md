# Embedded-language extraction strategy

The Vue prototype validates the embedded extractor pattern. The important architecture is not a Vue grammar; it is a region pipeline:

1. Detect source regions with cheap outer-language scans.
2. Parse each embedded code region with the real language grammar.
3. Remap sub-parse line numbers back to the original file.
4. Merge nodes, edges, unresolved references, and parse errors into the parent file result.

## Vue prototype

The Vue extractor creates one `component` node for the `.vue` file, then extracts `<script>` / `<script setup>` regions:

```text
/<script(\s[^>]*)?>(?<content>[\s\S]*?)<\/script>/g
```

The attribute string drives language selection:

- `lang="ts"` or `lang="typescript"` → TypeScript grammar.
- otherwise → JavaScript grammar.
- `setup` is recorded as a script-block property, but both normal and setup blocks are delegated through the same JS/TS extraction path.

The delegated result is merged by changing embedded nodes and refs back to the parent language (`vue`) and adding a `contains` edge from the component node to each extracted symbol. Template component usages are scanned outside `<script>` / `<style>` ranges with:

```text
/<([A-Za-z][A-Za-z0-9_-]*)\b/g
```

PascalCase tags are references directly. Kebab-case tags are converted to PascalCase. Lowercase tags without hyphens are treated as native HTML. Vue built-ins (`Transition`, `TransitionGroup`, `KeepAlive`, `Suspense`, `Teleport`, `Component`, `Slot`) are skipped after kebab-to-Pascal conversion.

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
- `context="module"` is recorded for module scripts.
- Script result merge uses the same line-offset pattern as Vue and rewrites language to `svelte`.
- Template scan additionally finds calls inside `{...}` expressions with `/\{([^}#/:@][^}]*)\}/g` and call names with `/\b([a-zA-Z_$][\w$.]*)\s*\(/g`.
- Svelte 5 runes (`$props`, `$state`, `$derived`, `$effect`, etc.) are compiler built-ins and must be filtered.
- PascalCase tags become component references; lowercase native tags are skipped.

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
- `.json` Shopify OS 2.0 templates are parsed as JSON and `sections.*.type` becomes `sections/<type>.liquid` references.
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
- Preserve the merge semantics: embedded symbols keep the parent file path, parent embedded language (`vue`, `svelte`, `razor`, etc.), and receive parent-component/file containment edges.
- Template-only references should remain unresolved refs unless the extractor creates explicit nodes for them (Liquid snippets/sections are the notable exception).
- Rust `regex` has no backreferences; translate regexes such as `</\1>` into explicit alternation or a small scanner.
- Add fixtures that assert original-file line numbers, not region-local lines, for every delegated extractor.
