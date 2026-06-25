# Changelog

## [0.15.0](https://github.com/sunerpy/codegraph-rust/compare/v0.14.0...v0.15.0) (2026-06-25)


### Features

* **extract:** ignore Godot .godot/ and addons/ dirs by default ([#60](https://github.com/sunerpy/codegraph-rust/issues/60)) ([8a2dffa](https://github.com/sunerpy/codegraph-rust/commit/8a2dffa326645f82fc6c2a4b53f81267b4c67bee))

## [0.14.0](https://github.com/sunerpy/codegraph-rust/compare/v0.13.0...v0.14.0) (2026-06-25)


### Features

* **graph:** report Godot dynamic reachability instead of false dead-code ([#56](https://github.com/sunerpy/codegraph-rust/issues/56)) ([18d0e2b](https://github.com/sunerpy/codegraph-rust/commit/18d0e2bbd4a73d8f92bcabdb5c8f90acfff54018))
* **resolve:** optional Godot resource DSL hook + Godot docs (L5) ([#58](https://github.com/sunerpy/codegraph-rust/issues/58)) ([de7e5f6](https://github.com/sunerpy/codegraph-rust/commit/de7e5f621f0139dbedd8744553edcbcb4619357d))


### Bug Fixes

* **resolve:** anchor Godot from="." scene connections to the root node ([#59](https://github.com/sunerpy/codegraph-rust/issues/59)) ([2a33e62](https://github.com/sunerpy/codegraph-rust/commit/2a33e62440cb121356b9fbb33bc35dc9d42075b6))

## [0.13.0](https://github.com/sunerpy/codegraph-rust/compare/v0.12.1...v0.13.0) (2026-06-25)


### Features

* **godot:** dynamic GDScript edges + fix determinism flake (L3) ([#53](https://github.com/sunerpy/codegraph-rust/issues/53)) ([2d1e53c](https://github.com/sunerpy/codegraph-rust/commit/2d1e53c1b55d37631e4bfda7464519f16fb0d088))
* **godot:** file ingestion + GodotResolver + project.godot autoload graph (L1) ([#49](https://github.com/sunerpy/codegraph-rust/issues/49)) ([2ecfe5a](https://github.com/sunerpy/codegraph-rust/commit/2ecfe5ab05e4ed9285de45f82b3fc1bd526e1564))
* **godot:** parse .tscn scenes and .tres resources (L2/L4) ([#52](https://github.com/sunerpy/codegraph-rust/issues/52)) ([1269003](https://github.com/sunerpy/codegraph-rust/commit/1269003d1a28da52d9ea026296b6f74a991528b8))
* **resolve:** resolve Godot autoload access cross-file via roster-gated resolve() ([#54](https://github.com/sunerpy/codegraph-rust/issues/54)) ([564fc58](https://github.com/sunerpy/codegraph-rust/commit/564fc58c7b39117042b9939e32bf57a34fddd760))


### Bug Fixes

* **daemon:** bound MCP proxy hello read so a wedged daemon socket never hangs the handshake ([#55](https://github.com/sunerpy/codegraph-rust/issues/55)) ([50580e3](https://github.com/sunerpy/codegraph-rust/commit/50580e3fa7e53c6dbadf82cba5fd8d3075a68a12))

## [0.12.1](https://github.com/sunerpy/codegraph-rust/compare/v0.12.0...v0.12.1) (2026-06-25)


### Bug Fixes

* **mcp:** assert serverInfo.version dynamically so release bumps never break golden ([#45](https://github.com/sunerpy/codegraph-rust/issues/45)) ([1138fe7](https://github.com/sunerpy/codegraph-rust/commit/1138fe7f0c90de1d5421ed1ad2f357f6719dc03e))

## [0.12.0](https://github.com/sunerpy/codegraph-rust/compare/v0.11.0...v0.12.0) (2026-06-25)


### Features

* **installer:** codegraph skill install/update/uninstall/status across 8 agents ([#43](https://github.com/sunerpy/codegraph-rust/issues/43)) ([0c6164f](https://github.com/sunerpy/codegraph-rust/commit/0c6164fe050ce864372ced6e3ff2f5a1eb480b05))

## [0.11.0](https://github.com/sunerpy/codegraph-rust/compare/v0.10.0...v0.11.0) (2026-06-25)


### Features

* **daemon:** shared detached daemon with live file-watch incremental re-index ([#41](https://github.com/sunerpy/codegraph-rust/issues/41)) ([ee9de19](https://github.com/sunerpy/codegraph-rust/commit/ee9de19145ba7827691cc284d1ca7eae2966da75))

## [0.10.0](https://github.com/sunerpy/codegraph-rust/compare/v0.9.0...v0.10.0) (2026-06-24)


### Features

* **extract:** add GDScript (.gd) language support ([#38](https://github.com/sunerpy/codegraph-rust/issues/38)) ([74d799a](https://github.com/sunerpy/codegraph-rust/commit/74d799a1e01248e6e5254202682d322ad5820b85))

## [0.9.0](https://github.com/sunerpy/codegraph-rust/compare/v0.8.0...v0.9.0) (2026-06-24)


### Features

* **index:** real parse progress and parse/persist streaming overlap ([cec7f9a](https://github.com/sunerpy/codegraph-rust/commit/cec7f9ab6ec83921cf70c6900cfef0116948d5b8))

## [0.8.0](https://github.com/sunerpy/codegraph-rust/compare/v0.7.0...v0.8.0) (2026-06-23)


### Features

* **index:** parallelize parsing and reference resolution ([5692768](https://github.com/sunerpy/codegraph-rust/commit/56927686f402a325bc9e4cf984a3b84cf199643f))

## [0.7.0](https://github.com/sunerpy/codegraph-rust/compare/v0.6.0...v0.7.0) (2026-06-23)


### Features

* **index:** show per-phase progress with elapsed time ([1fa33e9](https://github.com/sunerpy/codegraph-rust/commit/1fa33e900bf3a03f2f732f5882833ebc0ee16ff2))

## [0.6.0](https://github.com/sunerpy/codegraph-rust/compare/v0.5.3...v0.6.0) (2026-06-23)


### Features

* **index:** styled progress bar for index and sync ([6abc762](https://github.com/sunerpy/codegraph-rust/commit/6abc7628f6fdf24f2ac4ff41dd00759237d3eb9a))

## [0.5.3](https://github.com/sunerpy/codegraph-rust/compare/v0.5.2...v0.5.3) (2026-06-22)


### Bug Fixes

* **deps:** bump quinn-proto to 0.11.15 for RUSTSEC-2026-0185 ([c6a1c4c](https://github.com/sunerpy/codegraph-rust/commit/c6a1c4c53f5a2ba899e3dd9813b392e4dea6681b))

## [0.5.2](https://github.com/sunerpy/codegraph-rust/compare/v0.5.1...v0.5.2) (2026-06-22)


### Bug Fixes

* **cli:** default serve --mcp project to the current directory ([26be9df](https://github.com/sunerpy/codegraph-rust/commit/26be9df27a2061fe58b5f939230dc693ef42796b))

## [0.5.1](https://github.com/sunerpy/codegraph-rust/compare/v0.5.0...v0.5.1) (2026-06-22)


### Bug Fixes

* **cli:** enable self_update zip extraction for Windows releases ([d4f7766](https://github.com/sunerpy/codegraph-rust/commit/d4f776691bcd2832b2b1727003cada8c2017a712))

## [0.5.0](https://github.com/sunerpy/codegraph-rust/compare/v0.4.0...v0.5.0) (2026-06-22)


### Features

* **cli:** one-command shell completion install (completions --install) ([c7961b7](https://github.com/sunerpy/codegraph-rust/commit/c7961b7cfe137060d3783ff178f47f6dea9041c4))

## [0.4.0](https://github.com/sunerpy/codegraph-rust/compare/v0.3.0...v0.4.0) (2026-06-22)


### Features

* **install:** one-liner install scripts, Windows ARM64 target, completions, release gating, slim READMEs ([#17](https://github.com/sunerpy/codegraph-rust/issues/17)) ([9504080](https://github.com/sunerpy/codegraph-rust/commit/95040800bbe5be6d34a5ee9e80c0322a496cc716))

## [0.3.0](https://github.com/sunerpy/codegraph-rust/compare/v0.2.1...v0.3.0) (2026-06-22)


### Features

* **daemon:** cross-platform Windows support via named-pipe IPC ([f742a43](https://github.com/sunerpy/codegraph-rust/commit/f742a43fa3a21fd504ad3f86de3e8e2793ca9971))

## [0.2.1](https://github.com/sunerpy/codegraph-rust/compare/v0.2.0...v0.2.1) (2026-06-22)


### Bug Fixes

* **build:** drop pinned inter-crate version requirements so workspace bumps build ([#13](https://github.com/sunerpy/codegraph-rust/issues/13)) ([e836c0f](https://github.com/sunerpy/codegraph-rust/commit/e836c0f5225d21671eda66d2a35384e27c54b0fe))

## [0.2.0](https://github.com/sunerpy/codegraph-rust/compare/v0.1.2...v0.2.0) (2026-06-22)


### Features

* **cli:** add version + self-update commands and parameterized installer tests ([#11](https://github.com/sunerpy/codegraph-rust/issues/11)) ([62bb65d](https://github.com/sunerpy/codegraph-rust/commit/62bb65dd5c5f92265693b173d9e188ed216eb713))

## [0.1.2](https://github.com/sunerpy/codegraph-rust/compare/v0.1.1...v0.1.2) (2026-06-22)


### Bug Fixes

* **installer:** preserve comments and key order when editing agent configs ([#9](https://github.com/sunerpy/codegraph-rust/issues/9)) ([844388a](https://github.com/sunerpy/codegraph-rust/commit/844388af82a7d57b074678f7c5326441aec8d041))

## [0.1.1](https://github.com/sunerpy/codegraph-rust/compare/v0.1.0...v0.1.1) (2026-06-21)


### Bug Fixes

* **installer:** never overwrite JSONC agent configs on parse failure ([#6](https://github.com/sunerpy/codegraph-rust/issues/6)) ([0843ff0](https://github.com/sunerpy/codegraph-rust/commit/0843ff0f9a5b1f55ec29dbd8206795e73df7b416))

## [0.1.0](https://github.com/sunerpy/codegraph-rust/compare/v0.1.0...v0.1.0) (2026-06-21)


### Features

* codegraph-rs 0.1.0 — deterministic tree-sitter + SQLite/FTS5 code knowledge graph ([d1ebef0](https://github.com/sunerpy/codegraph-rust/commit/d1ebef0be8e8b461a12a1ba7326069e96a1ae33b))
