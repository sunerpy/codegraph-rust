# Supported Languages

CodeGraph extracts code structure deterministically using tree-sitter grammars and custom
embedded extractors. No AI, vectors, or embeddings are involved. The output is byte-stable
across runs.

**38 concrete languages** are supported, grouped into three extraction tiers based on what
the extractor produces.

> **Note on TypeScript/JavaScript variants:** `typescript` and `tsx`, and `javascript` and
> `jsx`, are distinct grammar variants internally (separate `Language` enum entries). They
> share grammars but handle different file-extension sets. The table lists each variant
> separately so the extension mapping is unambiguous.

---

## Tier 1 — Full symbol extraction (29 languages)

Tree-sitter parses the file and extracts all symbols (functions, classes, structs, methods,
variables, imports, etc.) plus call and dependency edges. This is the richest extraction
level.

| Language    | Extensions                                  | Extraction                | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ----------- | ------------------------------------------- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript  | `.ts` `.mts` `.cts`                         | Full tree-sitter          | Direct callable members of local/imported object-literal constants resolve by source containment. Root `tsconfig.json` / `jsconfig.json` / `tsconfig.base.json` aliases follow relative, absolute, and `node_modules` package `extends` chains (depth 32, cycle-safe); nearest config wins, `paths` replaces wholesale, and each `baseUrl` is resolved relative to its declaring config.                                                                                      |
| TSX         | `.tsx`                                      | Full tree-sitter          | TypeScript grammar, JSX syntax                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| JavaScript  | `.js` `.mjs` `.cjs` `.xsjs` `.xsjslib`      | Full tree-sitter          | Direct callable object-literal members resolve like TypeScript. Extensionless imports consider `.xsjs` / `.xsjslib` after `.js` / `.jsx` / `.mjs` / `.cjs` and before index candidates, so ordinary JavaScript keeps collision precedence.                                                                                                                                                                                                                                    |
| JSX         | `.jsx`                                      | Full tree-sitter          | JavaScript grammar, JSX syntax                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| ArkTS       | `.ets`                                      | Full tree-sitter          | HarmonyOS / OpenHarmony; `tree-sitter-arkts` grammar. `@Component struct` → struct symbol. ArkUI dynamic-dispatch bridges deferred. Plain `.ts` stays TypeScript                                                                                                                                                                                                                                                                                                              |
| Python      | `.py` `.pyw`                                | Full tree-sitter          | Bare class names used as values resolve to `References` edges in return, assignment, pair, argument, and list positions. `import x as y` binds `y` to module `x`; `from p import x as y` prefers module `p.x` when that file exists, otherwise it remains a member import. Missing, duplicate, or ambiguous aliases stay unresolved and never fall through to a global bare-name guess. Tuple returns are not recursively traversed, and bare names never resolve to methods. |
| Go          | `.go`                                       | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Rust        | `.rs`                                       | Full tree-sitter          | Methods in generic, lifetime, reference, and qualified `impl` blocks belong to the implementing type; only trait impls emit `Implements`. Exact `self.field.method()` calls resolve through the owning struct field's declared project type, unwrapping only references and `Box` / `Rc` / `Arc`; containers, generics, external types, and ambiguity remain unresolved rather than guessed.                                                                                  |
| Java        | `.java`                                     | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| C           | `.c` `.h`                                   | Full tree-sitter          | `.h` may be promoted to C++ or Objective-C. C++ detection scans masked full source for ordinary `class/struct Derived : Base` forms while rejecting comments, strings, character literals, preprocessor text, bitfields, labels, and ternaries.                                                                                                                                                                                                                               |
| C++         | `.cpp` `.cc` `.cxx` `.hpp` `.hxx`           | Full tree-sitter          | Class/struct inheritance supports access modifiers, `virtual`, qualified bases, and templated-base stripping; plain derived declarations in `.h` are detected by the C/C++ content classifier.                                                                                                                                                                                                                                                                                |
| C#          | `.cs`                                       | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| PHP         | `.php` `.module` `.install` `.theme` `.inc` | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Ruby        | `.rb` `.rake`                               | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Swift       | `.swift`                                    | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Kotlin      | `.kt` `.kts`                                | Full tree-sitter          | Top-level functions, class methods, and extension functions include raw parameter-list signatures with optional declared return types; constructors, accessors, lambdas, and anonymous functions are not synthesized as callable nodes                                                                                                                                                                                                                                        |
| Dart        | `.dart`                                     | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Scala       | `.scala` `.sc`                              | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Lua         | `.lua`                                      | Full tree-sitter          | `local f = function` emits one Function (not a duplicate Variable); `M.f = function`, string-key table assignments, and nested table constructor functions emit qualified Methods. Body calls belong to the synthesized callable; `f()`, `M.f()`, and `M:f()` resolve, while computed keys stay dynamic.                                                                                                                                                                      |
| Luau        | `.luau`                                     | Full tree-sitter          | Roblox Luau dialect                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Objective-C | `.m` `.mm`                                  | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| R           | `.r`                                        | Full tree-sitter          |                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Solidity    | `.sol`                                      | Full tree-sitter          | `tree-sitter-solidity` grammar; contracts/libraries/interfaces, structs, enums, modifiers, events, errors; `is`-inheritance → Extends (resolver promotes to Implements for interfaces); emit/revert/modifier-guard call edges                                                                                                                                                                                                                                                 |
| Nix         | `.nix`                                      | Full tree-sitter          | `tree-sitter-nix` grammar; `let`/attrset bindings, curried lambdas, `inherit`; `import`/`callPackage`/`imports`-list file imports; module-system option synthesizer deferred                                                                                                                                                                                                                                                                                                  |
| Terraform   | `.tf` `.tfvars` `.tofu`                     | Full tree-sitter          | `tree-sitter-hcl` grammar (HCL/Terraform/OpenTofu); `resource`/`data`→class, `module`→module, `variable`/`output`→variable, `provider`→namespace, `locals`→constant; qualified names + `var`/`local`/`module`/`data`/resource traversal refs; module-boundary framework resolver deferred                                                                                                                                                                                     |
| Erlang      | `.erl` `.hrl`                               | Full tree-sitter          | Functions keep a bare display name and qualify as `module::function/arity`; clauses, exports, specs, local/remote calls, `fun`, static `gen_server` targets, and spawn/apply-style MFA lists are arity-aware. Binary-literal commas do not inflate arity. Missing or ambiguous arities remain unresolved; dynamic behaviour/resource wiring is still deferred.                                                                                                                |
| GDScript    | `.gd`                                       | Full tree-sitter          | Godot scripting; extracts functions, classes, enums, variables, signals, extends, preload. Dynamic dispatch edges (connect/get_node/$/%/call/group) added by the Godot resolver — see [`docs/godot.md`](godot.md)                                                                                                                                                                                                                                                             |
| Pascal      | `.pas` `.dpr` `.dpk` `.lpr` `.dfm` `.fmx`   | Full tree-sitter / custom | `.dfm`/`.fmx` form files use a custom path                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| CFML        | `.cfc` `.cfm` `.cfs`                        | Full tree-sitter          | ColdFusion; dual-grammar `tree-sitter-cfml` (cfscript + cfml tag), dialect chosen by first-token sniff. Bare-script `component`→class (name from file) + `extends`→extends; tag `<cfcomponent>`→class (name attr) + `<cffunction>`→method + `extends`/`implements`→refs. `<cfscript>`-in-tag body delegation and cfquery SQL-body extraction deferred                                                                                                                         |

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
skipped. Exactly one file is read — the resolved project's own `codegraph.json`
under the selected index root; there is no directory-tree walk or cross-project
inheritance.

---

## See also

- [`docs/godot.md`](godot.md) — full Godot static-analysis reference: what gets extracted
  from `.tscn`/`.tres`/`project.godot`/`.gd`, the static-vs-runtime boundary, honesty
  signals, and the optional resource DSL hook.
- [`docs/grammar-manifest.md`](grammar-manifest.md) — engineering ABI manifest: per-language
  grammar crate, tier policy, and ABI smoke status (for contributors and grammar maintainers).
- [`docs/embedded-extraction.md`](embedded-extraction.md) — detailed description of the
  embedded extraction pipeline: region detection, line-number remapping, and node merging.
