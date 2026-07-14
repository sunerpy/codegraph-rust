# Supported Languages

CodeGraph extracts code structure deterministically using tree-sitter grammars and custom
embedded extractors. No AI, vectors, or embeddings are involved. The output is byte-stable
across runs.

**35 concrete languages** are supported, grouped into three extraction tiers based on what
the extractor produces.

> **Note on TypeScript/JavaScript variants:** `typescript` and `tsx`, and `javascript` and
> `jsx`, are distinct grammar variants internally (separate `Language` enum entries). They
> share grammars but handle different file-extension sets. The table lists each variant
> separately so the extension mapping is unambiguous.

---

## Tier 1 — Full symbol extraction (26 languages)

Tree-sitter parses the file and extracts all symbols (functions, classes, structs, methods,
variables, imports, etc.) plus call and dependency edges. This is the richest extraction
level.

| Language    | Extensions                                  | Extraction                | Notes                                                                                                                                                                                                                         |
| ----------- | ------------------------------------------- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript  | `.ts` `.mts` `.cts`                         | Full tree-sitter          |                                                                                                                                                                                                                               |
| TSX         | `.tsx`                                      | Full tree-sitter          | TypeScript grammar, JSX syntax                                                                                                                                                                                                |
| JavaScript  | `.js` `.mjs` `.cjs` `.xsjs` `.xsjslib`      | Full tree-sitter          |                                                                                                                                                                                                                               |
| JSX         | `.jsx`                                      | Full tree-sitter          | JavaScript grammar, JSX syntax                                                                                                                                                                                                |
| ArkTS       | `.ets`                                      | Full tree-sitter          | HarmonyOS / OpenHarmony; `tree-sitter-arkts` grammar. `@Component struct` → struct symbol. ArkUI dynamic-dispatch bridges deferred. Plain `.ts` stays TypeScript                                                              |
| Python      | `.py` `.pyw`                                | Full tree-sitter          |                                                                                                                                                                                                                               |
| Go          | `.go`                                       | Full tree-sitter          |                                                                                                                                                                                                                               |
| Rust        | `.rs`                                       | Full tree-sitter          |                                                                                                                                                                                                                               |
| Java        | `.java`                                     | Full tree-sitter          |                                                                                                                                                                                                                               |
| C           | `.c` `.h`                                   | Full tree-sitter          | `.h` may be promoted to C++ or Objective-C by heuristics                                                                                                                                                                      |
| C++         | `.cpp` `.cc` `.cxx` `.hpp` `.hxx`           | Full tree-sitter          |                                                                                                                                                                                                                               |
| C#          | `.cs`                                       | Full tree-sitter          |                                                                                                                                                                                                                               |
| PHP         | `.php` `.module` `.install` `.theme` `.inc` | Full tree-sitter          |                                                                                                                                                                                                                               |
| Ruby        | `.rb` `.rake`                               | Full tree-sitter          |                                                                                                                                                                                                                               |
| Swift       | `.swift`                                    | Full tree-sitter          |                                                                                                                                                                                                                               |
| Kotlin      | `.kt` `.kts`                                | Full tree-sitter          |                                                                                                                                                                                                                               |
| Dart        | `.dart`                                     | Full tree-sitter          |                                                                                                                                                                                                                               |
| Scala       | `.scala` `.sc`                              | Full tree-sitter          |                                                                                                                                                                                                                               |
| Lua         | `.lua`                                      | Full tree-sitter          |                                                                                                                                                                                                                               |
| Luau        | `.luau`                                     | Full tree-sitter          | Roblox Luau dialect                                                                                                                                                                                                           |
| Objective-C | `.m` `.mm`                                  | Full tree-sitter          |                                                                                                                                                                                                                               |
| R           | `.r`                                        | Full tree-sitter          |                                                                                                                                                                                                                               |
| Solidity    | `.sol`                                      | Full tree-sitter          | `tree-sitter-solidity` grammar; contracts/libraries/interfaces, structs, enums, modifiers, events, errors; `is`-inheritance → Extends (resolver promotes to Implements for interfaces); emit/revert/modifier-guard call edges |
| Nix         | `.nix`                                      | Full tree-sitter          | `tree-sitter-nix` grammar; `let`/attrset bindings, curried lambdas, `inherit`; `import`/`callPackage`/`imports`-list file imports; module-system option synthesizer deferred                                                  |
| GDScript    | `.gd`                                       | Full tree-sitter          | Godot scripting; extracts functions, classes, enums, variables, signals, extends, preload. Dynamic dispatch edges (connect/get_node/$/%/call/group) added by the Godot resolver — see [`docs/godot.md`](godot.md)             |
| Pascal      | `.pas` `.dpr` `.dpk` `.lpr` `.dfm` `.fmx`   | Full tree-sitter / custom | `.dfm`/`.fmx` form files use a custom path                                                                                                                                                                                    |

---

## Tier 2 — Embedded / template extraction (6 languages)

These languages wrap or embed code in another language (or use a custom extractor). The
host file gets its own node; inner code is delegated to the appropriate Tier-1 grammar
and merged back into the parent result.

| Language      | Extensions                                       | Extraction                    | Notes                                                                                    |
| ------------- | ------------------------------------------------ | ----------------------------- | ---------------------------------------------------------------------------------------- |
| Vue           | `.vue`                                           | Embedded (delegates to TS/JS) | `<script>` and `<script setup>` blocks delegated; `lang="ts"` selects TypeScript grammar |
| Svelte        | `.svelte`                                        | Embedded (delegates to TS/JS) | Script blocks extracted and delegated; component node created for the file               |
| Astro         | `.astro`                                         | Embedded                      | Detected via embedded pre-pass only (not in the built-in extension map)                  |
| Razor         | `.razor` `.cshtml`                               | Embedded (custom)             | Detected via embedded pre-pass only; C# snippets extracted from `.cshtml`/`.razor` files |
| Liquid        | `.liquid`, `templates/*.json`, `sections/*.json` | Custom regex extractor        | Shopify template support; path-based `.json` detection for templates and sections        |
| XML (MyBatis) | `.xml`                                           | Custom (MyBatis mapper)       | Extracts SQL-mapper nodes from MyBatis XML files; generic XML gets a file node only      |

---

## Tier 3 — File-level only (6 languages)

These files are indexed as file nodes so they appear in the graph and are searchable, but
no symbol extraction is performed at the language level. They contribute to traversal and
impact analysis at the file level.

| Language      | Extensions      | Extraction     | Notes                                                                                                                                    |
| ------------- | --------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| YAML          | `.yml` `.yaml`  | File node only | No symbol extraction                                                                                                                     |
| Twig          | `.twig`         | File node only | No symbol extraction                                                                                                                     |
| Properties    | `.properties`   | File node only | No symbol extraction                                                                                                                     |
| GodotScene    | `.tscn`         | File node only | Semantic graph (node tree, signals, scripts, groups, sub-scenes) built by the Godot framework resolver — see [`docs/godot.md`](godot.md) |
| GodotResource | `.tres`         | File node only | Resource→script/resource references built by the Godot framework resolver — see [`docs/godot.md`](godot.md)                              |
| GodotProject  | `project.godot` | File node only | Autoload singletons, input actions, plugins parsed by the Godot framework resolver — see [`docs/godot.md`](godot.md)                     |

> The three Godot file types carry file nodes only at the language-extraction level.
> All Godot-specific symbols, edges, and honesty signals are emitted by the `GodotResolver`
> (a `FrameworkResolver` that activates when `project.godot` is present). For the full
> extraction inventory and static-vs-runtime boundary, see [`docs/godot.md`](godot.md).

---

## Adding custom extension mappings

Non-standard extensions can be mapped to any supported language via `.codegraph/codegraph.json`:

```jsonc
{
  "extensions": {
    ".x": "lua",
    ".blade": "php",
  },
}
```

Keys are dot-stripped and lowercased before matching. Unknown language names are silently
skipped. The nearest config up the directory tree wins.

---

## See also

- [`docs/godot.md`](godot.md) — full Godot static-analysis reference: what gets extracted
  from `.tscn`/`.tres`/`project.godot`/`.gd`, the static-vs-runtime boundary, honesty
  signals, and the optional resource DSL hook.
- [`docs/grammar-manifest.md`](grammar-manifest.md) — engineering ABI manifest: per-language
  grammar crate, tier policy, and ABI smoke status (for contributors and grammar maintainers).
- [`docs/embedded-extraction.md`](embedded-extraction.md) — detailed description of the
  embedded extraction pipeline: region detection, line-number remapping, and node merging.
