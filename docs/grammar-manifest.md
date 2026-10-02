# Tree-sitter and custom extractor manifest

This is the contributor-facing ABI/ownership map for language extraction. Exact
crate versions are pinned in `crates/codegraph-extract/Cargo.toml` and
`Cargo.lock`; this document deliberately names crates without copying version
numbers that can drift independently.

The public taxonomy is:

- 38 code/template language IDs: 29 grammar-backed, 6 embedded/custom, and 3
  ordinary file-level formats;
- 3 additional Godot project/resource format IDs handled at file/framework level;
- `unknown`, which is an internal fallback and not supported-language coverage.

That yields 42 entries in `LANGUAGE_STRINGS`: 41 concrete IDs plus `unknown`.
`scripts/docs-check.py` derives and checks those counts from
`codegraph-core/src/types.rs`.

## Grammar-backed `LanguageSpec` implementations

`spec_for_language` in `crates/codegraph-extract/src/lang/mod.rs` is the runtime
authority. TSX and JSX are separate IDs that share their TypeScript/JavaScript
grammar crates.

| Language ID         | Grammar crate / entry    | Notes                                                                  |
| ------------------- | ------------------------ | ---------------------------------------------------------------------- |
| `typescript`, `tsx` | `tree-sitter-typescript` | TypeScript and TSX entry points                                        |
| `javascript`, `jsx` | `tree-sitter-javascript` | JS grammar includes JSX; separate specs preserve ID/extension behavior |
| `arkts`             | `tree-sitter-arkts`      | `.ets`; ArkUI syntax; calls in `struct` methods such as `build()`      |
| `python`            | `tree-sitter-python`     | `.py`, `.pyw`                                                          |
| `go`                | `tree-sitter-go`         | `.go`                                                                  |
| `rust`              | `tree-sitter-rust`       | `.rs`                                                                  |
| `java`              | `tree-sitter-java`       | `.java`                                                                |
| `c`                 | `tree-sitter-c`          | `.c`, ambiguous `.h` before source classification                      |
| `cpp`               | `tree-sitter-cpp`        | C++, Metal, and CUDA source families                                   |
| `csharp`            | `tree-sitter-c-sharp`    | `.cs`                                                                  |
| `php`               | `tree-sitter-php`        | PHP plus Drupal-style PHP extensions                                   |
| `ruby`              | `tree-sitter-ruby`       | `.rb`, `.rake`                                                         |
| `swift`             | `tree-sitter-swift`      | `.swift`                                                               |
| `kotlin`            | `tree-sitter-kotlin-ng`  | `.kt`, `.kts`; do not substitute the legacy grammar                    |
| `dart`              | `tree-sitter-dart`       | `.dart`                                                                |
| `pascal`            | `tree-sitter-pascal`     | Pascal source; DFM/FMX takes the custom path first                     |
| `scala`             | `tree-sitter-scala`      | `.scala`, `.sc`                                                        |
| `lua`               | `tree-sitter-lua`        | `.lua`                                                                 |
| `luau`              | `tree-sitter-luau`       | `.luau`                                                                |
| `objc`              | `tree-sitter-objc`       | `.m`, `.mm`, and classified headers                                    |
| `r`                 | `tree-sitter-r`          | `.r`                                                                   |
| `solidity`          | `tree-sitter-solidity`   | `.sol`                                                                 |
| `nix`               | `tree-sitter-nix`        | `.nix`                                                                 |
| `terraform`         | `tree-sitter-hcl`        | `.tf`, `.tfvars`, `.tofu`                                              |
| `erlang`            | `tree-sitter-erlang`     | `.erl`, `.hrl`                                                         |
| `cfml`              | `tree-sitter-cfml`       | dual script/tag entry selected from source                             |
| `gdscript`          | `tree-sitter-gdscript`   | `.gd`; framework resolution adds Godot relationships                   |

`LanguageSpec` maps grammar node types to symbols and unresolved references, and
owns language-specific name/body/signature/visibility/import behavior. Changing a
grammar or spec is a graph change unless proven otherwise by affected goldens.

## Embedded and custom paths

These run before ordinary grammar dispatch:

| Language ID    | Implementation        | Delegation / output                                                   |
| -------------- | --------------------- | --------------------------------------------------------------------- |
| `vue`          | `embedded/vue.rs`     | script regions to TypeScript/JavaScript plus component/file ownership |
| `svelte`       | `embedded/svelte.rs`  | script regions to TypeScript/JavaScript plus component/file ownership |
| `astro`        | `embedded/astro.rs`   | frontmatter/scripts/templates with source-line remapping              |
| `razor`        | `embedded/razor.rs`   | Razor/C#-like regions through custom extraction                       |
| `liquid`       | `embedded/liquid.rs`  | Liquid and Shopify template JSON custom scan                          |
| `xml`          | `embedded/mybatis.rs` | MyBatis mapper statements; generic XML remains file-level             |
| Pascal DFM/FMX | `embedded/dfm.rs`     | `.dfm`/`.fmx` detect as Pascal but bypass Pascal grammar              |

See [`embedded-extraction.md`](embedded-extraction.md) for region remapping and
merge invariants.

## File/framework-level IDs

| Language ID      | Runtime behavior                                                     |
| ---------------- | -------------------------------------------------------------------- |
| `yaml`           | file-level only; no language symbols                                 |
| `twig`           | file-level only; no language symbols                                 |
| `properties`     | file-level only; no language symbols                                 |
| `godot_scene`    | `.tscn`; file-level language ID plus Godot scene resolver extraction |
| `godot_resource` | `.tres`; file-level language ID plus Godot resource relationships    |
| `godot_project`  | `project.godot`; autoload/input/plugin/framework extraction          |

The first three are part of the 38 public code/template language count. The three
Godot resource-format IDs are reported separately so “38 languages” does not
silently become “41” in another page.

## ABI-smoke-only grammar dependencies

`tree-sitter-yaml`, `tree-sitter-properties`, and `tree-sitter-xml` are linked and
smoke-tested even though their current production paths are file/custom level.
`tree-sitter-html`, `tree-sitter-css`, and `tree-sitter-json` are also ABI-smoke
helpers; HTML/CSS/general JSON are not standalone `Language` IDs. Shopify JSON is
claimed only by the Liquid embedded detector.

SQL is not a standalone language ID or extension mapping. MyBatis extraction can
emit SQL-related mapper symbols from XML without adding `.sql` source support.

## Extension and dialect classification

`builtin_language_for_ext` and embedded detection are authoritative. Notable
classifications:

- `.h` begins as C, then masked source heuristics may promote it to C++ or
  Objective-C;
- `.metal`, `.cu`, and `.cuh` use the C++ grammar with dialect-specific preparse;
- `.xsjs` and `.xsjslib` use JavaScript;
- `.dfm` and `.fmx` use the Pascal language ID but custom extraction;
- `project.godot` is detected by filename because it has no extension;
- project extension overrides can claim only extensions left unclaimed by built-in
  and embedded detection.

## Contributor checks

Run the ABI smoke and relevant extraction/golden tests after any grammar change:

```bash
cargo run --locked -p codegraph-extract --example abi_smoke
cargo test --locked -p codegraph-extract
cargo test --locked -p codegraph-bench --test equivalence
```

Review `cargo tree -p codegraph-extract` for duplicate/incompatible tree-sitter
cores. Do not vendor a grammar or change a pin merely to match upstream's WASM
packaging; the Rust crate ABI and actual fixtures decide compatibility.
