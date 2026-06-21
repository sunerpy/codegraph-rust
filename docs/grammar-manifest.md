# Tree-sitter grammar ABI manifest

Task 5 smoke target: `tree-sitter = "0.26"` from the workspace. The authoritative language set is the `LANGUAGES` table plus the grammar manifest (WASM_GRAMMAR_FILES, extension delegation, custom/file-level branches). `unknown` is intentionally excluded from dependency planning.

The current `LANGUAGES` set has 29 entries including `unknown`; this manifest accounts for the 28 concrete entries, `dfm` as the Pascal custom extension path, and the user-requested future/static-resource grammars `sql`, `html`, `css`, and `json`. In this language set, `sql`, `html`, `css`, and general `json` are not `Language` enum entries; Shopify `templates/*.json` and `sections/*.json` delegate to Liquid.

## Tier policy

| Tier   | Meaning                                                                                                         |
| ------ | --------------------------------------------------------------------------------------------------------------- |
| a      | crates.io grammar crate links directly with workspace `tree-sitter = "0.26"` and passes `abi_smoke`             |
| b      | git dependency pinned by rev and passes `abi_smoke`                                                             |
| c      | vendored grammar plan required; task 17 should add generated C/CPP sources via `cc`                             |
| custom | no native grammar dependency selected because the language uses custom/file-level extraction or host delegation |

## Manifest

| Language   | Upstream source status                                                              | Chosen strategy                                            | Tier   | ABI status | Notes                                                                                                                         |
| ---------- | ----------------------------------------------------------------------------------- | ---------------------------------------------------------- | ------ | ---------- | ----------------------------------------------------------------------------------------------------------------------------- |
| typescript | WASM `tree-sitter-typescript.wasm`                                                  | `tree-sitter-typescript = "0.23.2"`, `LANGUAGE_TYPESCRIPT` | a      | PASS       | Crate exposes both TypeScript and TSX grammars.                                                                               |
| javascript | WASM `tree-sitter-javascript.wasm`                                                  | `tree-sitter-javascript = "0.25.0"`, `LANGUAGE`            | a      | PASS       | Used for `.js`, `.mjs`, `.cjs`, `.xsjs`, `.xsjslib`.                                                                          |
| tsx        | WASM `tree-sitter-tsx.wasm`                                                         | `tree-sitter-typescript = "0.23.2"`, `LANGUAGE_TSX`        | a      | PASS       | Same crate as TypeScript.                                                                                                     |
| jsx        | WASM reuses `tree-sitter-javascript.wasm`                                           | `tree-sitter-javascript = "0.25.0"`, `LANGUAGE`            | a      | PASS       | JSX is parsed by the JavaScript grammar.                                                                                      |
| python     | WASM `tree-sitter-python.wasm`                                                      | `tree-sitter-python = "0.25.0"`, `LANGUAGE`                | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| go         | WASM `tree-sitter-go.wasm`                                                          | `tree-sitter-go = "0.25.0"`, `LANGUAGE`                    | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| rust       | WASM `tree-sitter-rust.wasm`                                                        | `tree-sitter-rust = "0.24.2"`, `LANGUAGE`                  | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| java       | WASM `tree-sitter-java.wasm`                                                        | `tree-sitter-java = "0.23.5"`, `LANGUAGE`                  | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| c          | WASM `tree-sitter-c.wasm`                                                           | `tree-sitter-c = "0.24.2"`, `LANGUAGE`                     | a      | PASS       | `.h` may be promoted to C++/ObjC by source heuristics.                                                                        |
| cpp        | WASM `tree-sitter-cpp.wasm`                                                         | `tree-sitter-cpp = "0.23.4"`, `LANGUAGE`                   | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| csharp     | WASM `tree-sitter-c_sharp.wasm` (WASM grammar)                                      | `tree-sitter-c-sharp = "0.23.5"`, `LANGUAGE`               | a      | PASS       | Crates.io release parses primary-constructor smoke under TS 0.26 ABI.                                                         |
| razor      | custom Razor extractor                                                              | CUSTOM                                                     | custom | CUSTOM     | `.cshtml` / `.razor`; markup is not parsed via tree-sitter. C# snippets can reuse `tree-sitter-c-sharp` later.                |
| php        | WASM `tree-sitter-php.wasm`                                                         | `tree-sitter-php = "0.24.2"`, `LANGUAGE_PHP`               | a      | PASS       | Use PHP grammar, not `LANGUAGE_PHP_ONLY`, for regular files.                                                                  |
| ruby       | WASM `tree-sitter-ruby.wasm`                                                        | `tree-sitter-ruby = "0.23.1"`, `LANGUAGE`                  | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| swift      | WASM `tree-sitter-swift.wasm`                                                       | `tree-sitter-swift = "0.7.3"`, `LANGUAGE`                  | a      | PASS       | Verified against 0.26 despite historical old-core risk; crate uses `tree-sitter-language` and only dev-depends on older core. |
| kotlin     | WASM `tree-sitter-kotlin.wasm`                                                      | `tree-sitter-kotlin-ng = "1.1.0"`, `LANGUAGE`              | a      | PASS       | Use `kotlin-ng`; avoid legacy `tree-sitter-kotlin = 0.3.8` for this workspace.                                                |
| dart       | WASM `tree-sitter-dart.wasm`                                                        | `tree-sitter-dart = "0.2.0"`, `LANGUAGE`                   | a      | PASS       | Crate is old but exposes `LanguageFn`, so it links with workspace TS 0.26.                                                    |
| svelte     | custom extractor                                                                    | CUSTOM                                                     | custom | CUSTOM     | Delegates script blocks to TypeScript/JavaScript grammars.                                                                    |
| vue        | custom extractor                                                                    | CUSTOM                                                     | custom | CUSTOM     | Delegates `<script>` / `<script setup>` to TypeScript/JavaScript grammars.                                                    |
| liquid     | custom regex extractor                                                              | CUSTOM                                                     | custom | CUSTOM     | Shopify `templates/*.json` and `sections/*.json` delegate to Liquid.                                                          |
| pascal     | WASM `tree-sitter-pascal.wasm` (WASM grammar)                                       | `tree-sitter-pascal = "0.10.2"`, `LANGUAGE`                | a      | PASS       | Handles `.pas`, `.dpr`, `.dpk`, `.lpr`; DFM/FMX uses custom row below.                                                        |
| scala      | WASM `tree-sitter-scala.wasm` (WASM grammar)                                        | `tree-sitter-scala = "0.26.0"`, `LANGUAGE`                 | a      | PASS       | Direct ABI match version.                                                                                                     |
| lua        | WASM `tree-sitter-lua.wasm` (WASM grammar)                                          | `tree-sitter-lua = "0.5.0"`, `LANGUAGE`                    | a      | PASS       | Crates.io release avoids the old WASM heap issue.                                                                             |
| luau       | WASM `tree-sitter-luau.wasm` (WASM grammar)                                         | `tree-sitter-luau = "1.2.0"`, `LANGUAGE`                   | a      | PASS       | Direct crates.io grammar.                                                                                                     |
| objc       | WASM `tree-sitter-objc.wasm`                                                        | `tree-sitter-objc = "3.0.2"`, `LANGUAGE`                   | a      | PASS       | Used for `.m`, `.mm`, and `.h` heuristic promotion.                                                                           |
| yaml       | file-level only                                                                     | `tree-sitter-yaml = "0.7.2"`, `LANGUAGE`                   | a      | PASS       | Native grammar is available for future extraction; no tree-sitter symbols are emitted for YAML today.                         |
| twig       | file-level only                                                                     | CUSTOM                                                     | custom | CUSTOM     | No selected Rust crate; keep file-level/custom behavior.                                                                      |
| xml        | custom MyBatis extractor                                                            | `tree-sitter-xml = "0.7.0"`, `LANGUAGE_XML`                | a      | PASS       | Native grammar is available; uses MyBatis custom extraction and non-mapper XML file nodes.                                    |
| properties | file-level/custom Spring config keys                                                | `tree-sitter-properties = "0.3.0"`, `LANGUAGE`             | a      | PASS       | Native grammar is available; properties are treated as file-level for core extraction.                                        |
| dfm        | `.dfm` / `.fmx` extension maps to `pascal`, then custom DFM extractor               | CUSTOM                                                     | custom | CUSTOM     | Do not parse DFM as Pascal source; port the DFM/FMX extractor later.                                                          |
| sql        | not in the current `LANGUAGES` set; MyBatis emits SQL statement nodes from XML text | `tree-sitter-sequel = "0.3.11"`, `LANGUAGE`                | a      | PASS       | Use `tree-sitter-sequel`, not stale `tree-sitter-sql = 0.0.2`.                                                                |
| html       | not in the current `LANGUAGES` set                                                  | `tree-sitter-html = "0.23.2"`, `LANGUAGE`                  | a      | PASS       | Added for future embedded/markup reuse.                                                                                       |
| css        | not in the current `LANGUAGES` set                                                  | `tree-sitter-css = "0.25.0"`, `LANGUAGE`                   | a      | PASS       | Added for future embedded/style reuse.                                                                                        |
| json       | not a general `Language`; Shopify JSON templates delegate to Liquid                 | `tree-sitter-json = "0.24.8"`, `LANGUAGE`                  | a      | PASS       | Added for future JSON-resource extraction; current Shopify route remains Liquid custom.                                       |

## Ready-to-copy dependency block

```toml
tree-sitter = { workspace = true }
tree-sitter-c = "0.24.2"
tree-sitter-c-sharp = "0.23.5"
tree-sitter-cpp = "0.23.4"
tree-sitter-css = "0.25.0"
tree-sitter-dart = "0.2.0"
tree-sitter-go = "0.25.0"
tree-sitter-html = "0.23.2"
tree-sitter-java = "0.23.5"
tree-sitter-javascript = "0.25.0"
tree-sitter-json = "0.24.8"
tree-sitter-kotlin-ng = "1.1.0"
tree-sitter-lua = "0.5.0"
tree-sitter-luau = "1.2.0"
tree-sitter-objc = "3.0.2"
tree-sitter-pascal = "0.10.2"
tree-sitter-php = "0.24.2"
tree-sitter-properties = "0.3.0"
tree-sitter-python = "0.25.0"
tree-sitter-ruby = "0.23.1"
tree-sitter-rust = "0.24.2"
tree-sitter-scala = "0.26.0"
tree-sitter-sequel = "0.3.11"
tree-sitter-swift = "0.7.3"
tree-sitter-typescript = "0.23.2"
tree-sitter-xml = "0.7.0"
tree-sitter-yaml = "0.7.2"
```

## Smoke command

```sh
cargo run -p codegraph-extract --example abi_smoke
```

All tier-a rows must print `PASS`. Custom rows print `CUSTOM (...)` and do not exercise tree-sitter.

## Task 17 outcome (risk-grammar wiring)

The plan's tier-c warnings for swift/kotlin/sql resolved as follows once wired:

- **swift** — `tree-sitter-swift = "0.7.3"` from crates.io (alex-pinkus grammar,
  repo `https://github.com/alex-pinkus/tree-sitter-swift`, crate release tag
  `v0.7.3`) links and parses against workspace tree-sitter 0.26 directly. The
  crate uses `tree-sitter-language` `LanguageFn` so no vendored `cc` build.rs
  was needed; the "pins an older core" risk applied only to dev-dependencies.
  Spec: `crates/codegraph-extract/src/lang/swift.rs` (ports the upstream
  `swift.ts:43-138` rules).
- **kotlin** — `tree-sitter-kotlin-ng = "1.1.0"` (NOT legacy
  `tree-sitter-kotlin = 0.3.8`). Node names differ from the fwcd WASM
  build (`import`/`qualified_identifier` vs `import_header`, `identifier` vs
  `simple_identifier`); `crates/codegraph-extract/src/lang/kotlin.rs` maps
  the kotlin.ts rules onto the -ng shapes.
- **sql** — intentionally NOT wired as a language. The `LANGUAGES` set
  has no `sql` entry and EXTENSION_MAP
  has no `.sql`; adding it would violate golden parity. SQL statement nodes
  only come from the MyBatis XML extractor. `tree-sitter-sequel = "0.3.11"`
  remains the documented choice if a future upstream pin adds the language.
- **dfm** — custom extractor `crates/codegraph-extract/src/embedded/dfm.rs`
  (port of `dfm-extractor.ts`); `.dfm`/`.fmx` detect as pascal and route
  through the embedded dispatch before grammar parsing.
- **yaml / twig / properties** — file-level-only (extract stage returns an
  empty result, mirroring `isFileLevelOnlyLanguage`); their grammar rows above
  stay unused by the extractor.
- **html / css / json** — not in the language set; no specs were wired (rows above
  are reserved for future use only).
