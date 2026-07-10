# Changelog

## [0.28.3](https://github.com/sunerpy/codegraph-rust/compare/v0.28.2...v0.28.3) (2026-07-10)


### Bug Fixes

* **cli:** serve MCP handshake immediately on cold start ([#129](https://github.com/sunerpy/codegraph-rust/issues/129)) ([d0d7298](https://github.com/sunerpy/codegraph-rust/commit/d0d72986fe6617b7cc0730cb26bedc6d5f37098f))

## [0.28.2](https://github.com/sunerpy/codegraph-rust/compare/v0.28.1...v0.28.2) (2026-07-09)


### Bug Fixes

* **cli:** authenticate self-update with GITHUB_TOKEN/GH_TOKEN and hint on rate limit ([#127](https://github.com/sunerpy/codegraph-rust/issues/127)) ([e62dc5c](https://github.com/sunerpy/codegraph-rust/commit/e62dc5cdd5a7506b345ae1d851373c67506a29d1))

## [0.28.1](https://github.com/sunerpy/codegraph-rust/compare/v0.28.0...v0.28.1) (2026-07-07)


### Bug Fixes

* **cli:** list affected files in codegraph affected output ([#125](https://github.com/sunerpy/codegraph-rust/issues/125)) ([e32620f](https://github.com/sunerpy/codegraph-rust/commit/e32620fde6724b46b1ea3ad1e63d05f6d3516a32))

## [0.28.0](https://github.com/sunerpy/codegraph-rust/compare/v0.27.0...v0.28.0) (2026-07-07)


### Features

* **mcp:** rescue a callable's buried signature types in explore ([#1064](https://github.com/sunerpy/codegraph-rust/issues/1064)) ([#123](https://github.com/sunerpy/codegraph-rust/issues/123)) ([7afa2f9](https://github.com/sunerpy/codegraph-rust/commit/7afa2f90fda2adc7e96170989339dcd4b8df67c2))

## [0.27.0](https://github.com/sunerpy/codegraph-rust/compare/v0.26.0...v0.27.0) (2026-07-07)


### Features

* **extract:** extract C++ class/struct inheritance incl. templated bases ([#1043](https://github.com/sunerpy/codegraph-rust/issues/1043)) ([#121](https://github.com/sunerpy/codegraph-rust/issues/121)) ([3657f15](https://github.com/sunerpy/codegraph-rust/commit/3657f1573c4a9195b55dc67caea74ef6e3d1a0f5))

## [0.26.0](https://github.com/sunerpy/codegraph-rust/compare/v0.25.6...v0.26.0) (2026-07-06)


### Features

* **resolve:** infer method-call receiver types across languages and prefer same-file targets ([#119](https://github.com/sunerpy/codegraph-rust/issues/119)) ([d807080](https://github.com/sunerpy/codegraph-rust/commit/d807080d98b2ae3eb50be1a85df918e3cebd03f2))

## [0.25.6](https://github.com/sunerpy/codegraph-rust/compare/v0.25.5...v0.25.6) (2026-07-06)


### Bug Fixes

* **extract:** extract Ruby receiver.method calls and record .new instantiation ([#1110](https://github.com/sunerpy/codegraph-rust/issues/1110)) ([#117](https://github.com/sunerpy/codegraph-rust/issues/117)) ([468446a](https://github.com/sunerpy/codegraph-rust/commit/468446a4ea1646074e14b7b1d09c8abaad04390d))

## [0.25.5](https://github.com/sunerpy/codegraph-rust/compare/v0.25.4...v0.25.5) (2026-07-06)


### Bug Fixes

* **extract:** recover garbled C++ names, skip forward decls, and record stack construction ([#115](https://github.com/sunerpy/codegraph-rust/issues/115)) ([530270c](https://github.com/sunerpy/codegraph-rust/commit/530270ca289a10f18cf954fc535d9493fadb24c3))

## [0.25.4](https://github.com/sunerpy/codegraph-rust/compare/v0.25.3...v0.25.4) (2026-07-06)


### Bug Fixes

* **graph:** complete edge sets and correct node limits in traversal ([#1086](https://github.com/sunerpy/codegraph-rust/issues/1086), [#1087](https://github.com/sunerpy/codegraph-rust/issues/1087), [#1089](https://github.com/sunerpy/codegraph-rust/issues/1089), [#1090](https://github.com/sunerpy/codegraph-rust/issues/1090)) ([#113](https://github.com/sunerpy/codegraph-rust/issues/113)) ([58b25d2](https://github.com/sunerpy/codegraph-rust/commit/58b25d2517149a18cd91e019d608779775bf0868))

## [0.25.3](https://github.com/sunerpy/codegraph-rust/compare/v0.25.2...v0.25.3) (2026-07-06)


### Bug Fixes

* **cli:** stop nonsensical query score %, exclude Android res/, back off failing auto-sync ([#111](https://github.com/sunerpy/codegraph-rust/issues/111)) ([14d1004](https://github.com/sunerpy/codegraph-rust/commit/14d10041cc5a45f540989201d0f10896b9e2cca8))

## [0.25.2](https://github.com/sunerpy/codegraph-rust/compare/v0.25.1...v0.25.2) (2026-07-06)


### Bug Fixes

* **store:** dedup edges with a UNIQUE identity index ([#1034](https://github.com/sunerpy/codegraph-rust/issues/1034)) ([#109](https://github.com/sunerpy/codegraph-rust/issues/109)) ([a7d4883](https://github.com/sunerpy/codegraph-rust/commit/a7d4883ab43832cf09e3ff230323b32fcbd87f76))

## [0.25.1](https://github.com/sunerpy/codegraph-rust/compare/v0.25.0...v0.25.1) (2026-07-06)


### Bug Fixes

* **godot:** resolve autoload calls and signal handlers, unify impact with audit ([#107](https://github.com/sunerpy/codegraph-rust/issues/107)) ([2d933a5](https://github.com/sunerpy/codegraph-rust/commit/2d933a5b38afe48358ae55687e65061314fb1a1b))

## [0.25.0](https://github.com/sunerpy/codegraph-rust/compare/v0.24.0...v0.25.0) (2026-07-05)


### Features

* **mcp:** migrate to the official rmcp SDK with async daemon, HTTP mode, and 95% coverage ([#105](https://github.com/sunerpy/codegraph-rust/issues/105)) ([56f757b](https://github.com/sunerpy/codegraph-rust/commit/56f757b4990c57b3ae5647559f4c01d62e205f99))

## [0.24.0](https://github.com/sunerpy/codegraph-rust/compare/v0.23.1...v0.24.0) (2026-07-01)


### Features

* **godot:** static graph fixes — res:// paths, ClassName.member resolution & qualified queries, impact input, reasons.target ([#103](https://github.com/sunerpy/codegraph-rust/issues/103)) ([364ebd8](https://github.com/sunerpy/codegraph-rust/commit/364ebd8cd0935db551c009d877026658307089b6))

## [0.23.1](https://github.com/sunerpy/codegraph-rust/compare/v0.23.0...v0.23.1) (2026-07-01)


### Bug Fixes

* **cli:** skip self-update download prompt when already on the latest version ([#101](https://github.com/sunerpy/codegraph-rust/issues/101)) ([2244c62](https://github.com/sunerpy/codegraph-rust/commit/2244c628f72f4c96ba77e0284f345b9fde802572))

## [0.23.0](https://github.com/sunerpy/codegraph-rust/compare/v0.22.0...v0.23.0) (2026-07-01)


### Features

* **godot:** richer edge subkinds and verifyPlan for the static graph ([#99](https://github.com/sunerpy/codegraph-rust/issues/99)) ([0c51ff3](https://github.com/sunerpy/codegraph-rust/commit/0c51ff3a8e3748a38eb6301f27d9b38fc22160dc))

## [0.22.0](https://github.com/sunerpy/codegraph-rust/compare/v0.21.0...v0.22.0) (2026-07-01)


### Features

* **installer:** add Trae and Qoder targets, read-only global Kiro entry, IDE docs ([#97](https://github.com/sunerpy/codegraph-rust/issues/97)) ([6b14f0e](https://github.com/sunerpy/codegraph-rust/commit/6b14f0e4057ae413936cf5ca655387f8a5d15bcc))

## [0.21.0](https://github.com/sunerpy/codegraph-rust/compare/v0.20.1...v0.21.0) (2026-06-30)


### Features

* **mcp:** always expose tools and require projectPath when no default project ([#94](https://github.com/sunerpy/codegraph-rust/issues/94)) ([#95](https://github.com/sunerpy/codegraph-rust/issues/95)) ([f4ec1bc](https://github.com/sunerpy/codegraph-rust/commit/f4ec1bc8e95bf728023fc8a7de835e511d9afd16))

## [0.20.1](https://github.com/sunerpy/codegraph-rust/compare/v0.20.0...v0.20.1) (2026-06-28)


### Bug Fixes

* **daemon:** enrich watcher sync log with timestamp, filenames, and counts ([#92](https://github.com/sunerpy/codegraph-rust/issues/92)) ([7a29cc4](https://github.com/sunerpy/codegraph-rust/commit/7a29cc4bb8fc97494022f47450be062efae6b1b3))

## [0.20.0](https://github.com/sunerpy/codegraph-rust/compare/v0.19.0...v0.20.0) (2026-06-28)


### Features

* port colby v1.1.2 (readOnlyHint, daemon FS fallback, name-ceiling, exclude, swift computed props) ([#90](https://github.com/sunerpy/codegraph-rust/issues/90)) ([c7e828a](https://github.com/sunerpy/codegraph-rust/commit/c7e828afbe52d873b1de46881607b5095044dddd))

## [0.19.0](https://github.com/sunerpy/codegraph-rust/compare/v0.18.0...v0.19.0) (2026-06-28)


### Features

* **graph:** add Godot edge subkind, impact target/verify-plan, orphan confidence ([#88](https://github.com/sunerpy/codegraph-rust/issues/88)) ([270a5e6](https://github.com/sunerpy/codegraph-rust/commit/270a5e693d397b3a9e4b86c4a3c7d725bfe7b0f6))

## [0.18.0](https://github.com/sunerpy/codegraph-rust/compare/v0.17.0...v0.18.0) (2026-06-27)


### Features

* **cli:** add files --language, audit include/exclude, impact edgeKind, accurate symbol count ([#87](https://github.com/sunerpy/codegraph-rust/issues/87)) ([8b07975](https://github.com/sunerpy/codegraph-rust/commit/8b0797578e1204d7d0aaa69cd17c1c16b0209c75))


### Bug Fixes

* **graph:** stop audit --dangling reporting bare signal methods as missing paths ([#85](https://github.com/sunerpy/codegraph-rust/issues/85)) ([091b3f3](https://github.com/sunerpy/codegraph-rust/commit/091b3f39983cf4c94d9a8bd3ce9baedc89775314))

## [0.17.0](https://github.com/sunerpy/codegraph-rust/compare/v0.16.1...v0.17.0) (2026-06-27)


### Features

* **graph:** add read-only Godot resource audit (orphan/dangling/impact) ([#82](https://github.com/sunerpy/codegraph-rust/issues/82)) ([7f80b0f](https://github.com/sunerpy/codegraph-rust/commit/7f80b0f10314951a5e9f808ca3ec0138c97834b0))
* **resolve:** add opt-in Godot idFields DSL indexing via godot:id sentinels ([#84](https://github.com/sunerpy/codegraph-rust/issues/84)) ([54bf901](https://github.com/sunerpy/codegraph-rust/commit/54bf901ec89dc97e1dd75fe36e1d02caa4e3ec06))

## [0.16.1](https://github.com/sunerpy/codegraph-rust/compare/v0.16.0...v0.16.1) (2026-06-26)


### Bug Fixes

* **daemon:** reap detached daemon child to avoid zombie ([#80](https://github.com/sunerpy/codegraph-rust/issues/80)) ([bc1d828](https://github.com/sunerpy/codegraph-rust/commit/bc1d828a7cd4f94e81f24c7e6f376ef3e73e96de))

## [0.16.0](https://github.com/sunerpy/codegraph-rust/compare/v0.15.8...v0.16.0) (2026-06-26)


### Features

* **cli:** add init --target to wire project-level editor MCP config ([#78](https://github.com/sunerpy/codegraph-rust/issues/78)) ([7301477](https://github.com/sunerpy/codegraph-rust/commit/7301477077ae712a52c295baa987ab623b291507))

## [0.15.8](https://github.com/sunerpy/codegraph-rust/compare/v0.15.7...v0.15.8) (2026-06-26)


### Bug Fixes

* **installer:** never write a broken global Kiro --path ([#76](https://github.com/sunerpy/codegraph-rust/issues/76)) ([2572eb2](https://github.com/sunerpy/codegraph-rust/commit/2572eb28f1ab1e976e84c44d03ace71d0077b026))

## [0.15.7](https://github.com/sunerpy/codegraph-rust/compare/v0.15.6...v0.15.7) (2026-06-26)


### Bug Fixes

* **mcp:** scope Kiro MCP to project path and guard home-root indexing ([#74](https://github.com/sunerpy/codegraph-rust/issues/74)) ([fd0c049](https://github.com/sunerpy/codegraph-rust/commit/fd0c04948b2679e0648ab6aa153529d54a2b9d95))

## [0.15.6](https://github.com/sunerpy/codegraph-rust/compare/v0.15.5...v0.15.6) (2026-06-25)


### Bug Fixes

* **mcp:** start daemon after client root adoption ([#72](https://github.com/sunerpy/codegraph-rust/issues/72)) ([7fd0ef8](https://github.com/sunerpy/codegraph-rust/commit/7fd0ef81dab8e9a2057a2f543a30e50e1b7102b0))

## [0.15.5](https://github.com/sunerpy/codegraph-rust/compare/v0.15.4...v0.15.5) (2026-06-25)


### Bug Fixes

* **mcp:** discover client roots for home-launched servers ([#70](https://github.com/sunerpy/codegraph-rust/issues/70)) ([f7e1813](https://github.com/sunerpy/codegraph-rust/commit/f7e1813cd5196f96ba11431f02424361e66842b1))

## [0.15.4](https://github.com/sunerpy/codegraph-rust/compare/v0.15.3...v0.15.4) (2026-06-25)


### Bug Fixes

* **cli:** never run daemon/catch-up when launched at $HOME or filesystem root ([#68](https://github.com/sunerpy/codegraph-rust/issues/68)) ([cf34500](https://github.com/sunerpy/codegraph-rust/commit/cf3450026725b6070bd8b16e1bc546fe46edbcc5))

## [0.15.3](https://github.com/sunerpy/codegraph-rust/compare/v0.15.2...v0.15.3) (2026-06-25)


### Bug Fixes

* **daemon:** setsid the detached daemon so init reaps it (no zombie) ([#66](https://github.com/sunerpy/codegraph-rust/issues/66)) ([8d8d26e](https://github.com/sunerpy/codegraph-rust/commit/8d8d26e341ffc8b58756046685be9a7ebbc89029))

## [0.15.2](https://github.com/sunerpy/codegraph-rust/compare/v0.15.1...v0.15.2) (2026-06-25)


### Bug Fixes

* **watch,mcp:** never watch $HOME, prune nested ignore dirs, adopt MCP workspace root ([#64](https://github.com/sunerpy/codegraph-rust/issues/64)) ([be63060](https://github.com/sunerpy/codegraph-rust/commit/be630608f4fb31315c033a875e6f54edfe099d11))

## [0.15.1](https://github.com/sunerpy/codegraph-rust/compare/v0.15.0...v0.15.1) (2026-06-25)


### Bug Fixes

* self-update jumps to latest + watcher prunes ignored dirs (inotify exhaustion / slow MCP startup) ([#62](https://github.com/sunerpy/codegraph-rust/issues/62)) ([da10bd7](https://github.com/sunerpy/codegraph-rust/commit/da10bd7c2a81d157aea6528e6d042c5ca4d6b062))

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
