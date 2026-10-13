//! Real-pipeline probes: small trees indexed by the built `codegraph init`,
//! with assertions over the published graph.
//!
//! A probe pins one behavior through the same scan, extraction, framework
//! detection and resolution a user gets, which an in-process unit test can
//! bypass. Name a probe `pr<number>_<behavior>` after the upstream or project
//! PR whose behavior it locks, or `smoke_<behavior>` for a harness check, so
//! `cargo test --test probes -- pr2392` finds every probe of one family. The
//! harness and its selector notation are documented in [`support`].

mod support;

use support::{Project, assert_edge, assert_no_edge, assert_unresolved};

#[test]
fn smoke_typescript_call_edge_through_a_named_import() {
    let project = Project::new()
        .file(
            "src/math.ts",
            "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
        )
        .file(
            "src/app.ts",
            "import { add } from './math';\n\nexport function run(): number {\n  return add(1, 2);\n}\n",
        )
        .index();

    assert_edge!(project, "function run" => calls => "function add", resolved_by = "import");
    assert_edge!(project, "file:src/app.ts" => imports => "function add @src/math.ts:1");
    assert_no_edge!(project, "function add" => calls => "function run");
}

#[test]
fn smoke_external_import_stays_unresolved() {
    let project = Project::new()
        .file(
            "src/app.ts",
            "import { debounce } from 'lodash';\n\nexport function run(): void {\n  debounce(run);\n}\n",
        )
        .index();

    assert_unresolved!(project, "file:src/app.ts" => imports => "debounce");
    assert_unresolved!(project, "function run" => calls => "debounce");
    assert!(
        project
            .graph()
            .nodes_named("function", "debounce")
            .is_empty(),
        "an external import must not mint a project node:\n{}",
        project.graph().report()
    );
}

#[test]
fn smoke_python_method_call_through_a_local_instance() {
    let project = Project::new()
        .file(
            "greeter.py",
            "class Greeter:\n    def greet(self):\n        return \"hi\"\n\n\ndef main():\n    g = Greeter()\n    return g.greet()\n",
        )
        .index();

    assert_edge!(
        project,
        "function main" => calls => "method Greeter::greet",
        resolved_by = "instance-method",
    );
    assert_edge!(project, "function main" => instantiates => "class Greeter");
}

#[test]
fn smoke_sync_rereads_the_graph_after_an_edit() {
    let mut project = Project::new()
        .file("lib.py", "def helper():\n    return 1\n")
        .file(
            "main.py",
            "from lib import helper\n\n\ndef main():\n    return helper()\n",
        )
        .index();
    assert_edge!(project, "function main" => calls => "function helper @lib.py");

    project.write("lib.py", "def assist():\n    return 1\n");
    project.sync();

    assert_unresolved!(project, "function main" => calls => "helper");
    assert_edge!(project, "file:lib.py" => contains => "function assist");
}

#[test]
fn smoke_file_selector_matches_an_embedded_file_node() {
    // The Liquid extractor keys its file node by a hash, not `file:{path}`;
    // a `file:` selector still names it by path.
    let project = Project::new()
        .file(
            "templates/product.liquid",
            "<div class=\"product\">\n  {% render 'price' %}\n</div>\n",
        )
        .file(
            "snippets/price.liquid",
            "<span class=\"price\">{{ product.price }}</span>\n",
        )
        .index();

    assert_edge!(
        project,
        "file:templates/product.liquid" => references => "file:snippets/price.liquid",
        resolved_by = "file-path",
    );
}

/// Upstream #965 (`0a91d0f5`): an import statement is not a definition, so no
/// reference binds another file's import node by name. App.vue's import of
/// `vue` used to land on Widget.svelte's import of `vue`.
/// Upstream #1789's prerequisite phase (W4-CPP-05): the wiring references
/// (imports, extends, implements) are resolved and persisted before the rest,
/// so a call on a receiver typed as a subclass declared in the same file
/// reaches the method its base declares, on a full index and after a sync
/// that adds the call.
#[test]
fn pr1789_a_subclass_call_reaches_the_inherited_method_on_index_and_sync() {
    const CSHARP_BASE: &str = "namespace Json\n{\n    public abstract class JsonWriter\n    {\n        public virtual void WriteEndObject() { }\n    }\n}\n";
    const CSHARP_USE: &str = "namespace Json\n{\n    public class JsonTextWriter : JsonWriter\n    {\n        public void Flush() { }\n    }\n\n    public class Program\n    {\n        public void Run()\n        {\n            JsonTextWriter writer = new JsonTextWriter();\n            writer.WriteEndObject();\n        }\n    }\n}\n";
    const JAVA_BASE: &str = "package app;\n\npublic class Base {\n    public void run() { }\n}\n";
    const JAVA_USE: &str = "package app;\n\nclass Child extends Base {\n}\n\npublic class Use {\n    public void go() {\n        Child c = new Child();\n        c.run();\n    }\n}\n";
    let indexed = Project::new()
        .file("src/JsonWriter.cs", CSHARP_BASE)
        .file("src/Program.cs", CSHARP_USE)
        .file("src/app/Base.java", JAVA_BASE)
        .file("src/app/Use.java", JAVA_USE)
        .index();
    assert_edge!(indexed, "method Json::Program::Run" => calls => "method Json::JsonWriter::WriteEndObject");
    assert_edge!(indexed, "method app::Use::go" => calls => "method app::Base::run");

    let mut synced = Project::new()
        .file("src/JsonWriter.cs", CSHARP_BASE)
        .file(
            "src/Program.cs",
            "namespace Json\n{\n    public class Program\n    {\n    }\n}\n",
        )
        .file("src/app/Base.java", JAVA_BASE)
        .file(
            "src/app/Use.java",
            "package app;\n\npublic class Use {\n}\n",
        )
        .index();
    synced
        .write("src/Program.cs", CSHARP_USE)
        .write("src/app/Use.java", JAVA_USE);
    let graph = synced.sync();
    assert_edge!(graph, "method Json::Program::Run" => calls => "method Json::JsonWriter::WriteEndObject");
    assert_edge!(graph, "method app::Use::go" => calls => "method app::Base::run");
}

#[test]
fn pr0965_vue_import_never_binds_a_svelte_import_node() {
    let project = Project::new()
        .file(
            "src/App.vue",
            "<template>\n  <div>{{ count }}</div>\n</template>\n\n<script>\nimport { ref } from \"vue\";\n\nexport default {\n  setup() {\n    const count = ref(0);\n    return { count };\n  },\n};\n</script>\n",
        )
        .file(
            "src/Widget.svelte",
            "<script>\n  import { ref } from \"vue\";\n  let count = ref(0);\n</script>\n\n<p>{count}</p>\n",
        )
        .index();

    assert_no_edge!(
        project,
        "component src/App.vue::App" => imports => "import vue @src/Widget.svelte"
    );
    assert_no_edge!(project, "component src/App.vue::App" => imports => "import vue");
    assert_no_edge!(project, "file:src/App.vue" => imports => "import vue");
    // The package is not in the project, so the import stays unresolved, on
    // the file that holds the component (an SFC's imports are its file's).
    assert_unresolved!(project, "file:src/App.vue" => imports => "vue");
}

/// Upstream #2437: a catch-all `"*"` path alias has an empty prefix, which
/// every package specifier starts with. Matching it no longer makes
/// `@mui/material` or `react` look local, so neither the import nor the
/// supertype binds the project's same-named symbol. (A `calls` or
/// `references` ref through an out-of-repo import is the wider #2134 gate.)
#[test]
fn pr2437_catch_all_alias_keeps_package_imports_external() {
    let project = Project::new()
        .file(
            "tsconfig.json",
            "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"*\": [\"./typings/*\"] }\n  }\n}\n",
        )
        .file(
            "src/Typography.tsx",
            "export function Typography(props: { text: string }) {\n  return <span>{props.text}</span>;\n}\n",
        )
        .file("src/Component.ts", "export class Component {}\n")
        .file(
            "src/App.tsx",
            "import { Typography } from '@mui/material';\n\nexport function App() {\n  return <Typography variant=\"h1\" />;\n}\n",
        )
        .file(
            "src/Widget.ts",
            "import { Component } from 'react';\n\nexport class Widget extends Component {}\n",
        )
        .file(
            "src/Page.tsx",
            "import { Typography } from './Typography';\n\nexport function Page() {\n  return <Typography text=\"hi\" />;\n}\n",
        )
        .index();

    assert_no_edge!(project, "file:src/App.tsx" => imports => "function Typography");
    assert_no_edge!(project, "class Widget" => extends => "class Component");
    assert_unresolved!(project, "class Widget" => extends => "Component");
    // The project's own import still resolves.
    assert_edge!(
        project,
        "file:src/Page.tsx" => imports => "function Typography @src/Typography.tsx",
        resolved_by = "import",
    );
}

/// Upstream #2437: an alias that lands on disk outside the index (here in
/// `node_modules`) does not make the import the project's either.
#[test]
fn pr2437_alias_into_node_modules_keeps_the_import_external() {
    let project = Project::new()
        .file(
            "tsconfig.json",
            "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"lit/format\": [\"./node_modules/lit/format.js\"] }\n  }\n}\n",
        )
        .file(
            "node_modules/lit/format.js",
            "export function format(value) { return String(value); }\n",
        )
        .file(
            "src/util.ts",
            "export function format(value: number): string {\n  return value.toFixed(2);\n}\n",
        )
        .file(
            "src/card.ts",
            "import { format } from 'lit/format';\n\nexport function render(): string {\n  return format(1);\n}\n",
        )
        .index();

    assert_no_edge!(project, "file:src/card.ts" => imports => "function format");
    assert_unresolved!(project, "file:src/card.ts" => imports => "format");
}

/// W3: the cross-language resolution seams (plan section 3 "W3"). Every probe
/// of this wave sits here, so `cargo test --test probes -- seams` runs them.
mod seams {
    use super::support::{Project, assert_edge, assert_no_edge, assert_unresolved};

    /// Upstream #2392 (`3f5a9e2e`) and #2263 (`c4babfce`): an `imports`
    /// reference never binds the import statement it was written in. A module
    /// path names a file: `import './side'` links `src/side.ts`, while a path
    /// no file answers (`./missing`, `<stdio.h>`, a Lua module) parks as
    /// unresolved instead of an edge to its own statement, which a sync never
    /// retries. A Lua `require` reaches a module file or nothing.
    #[test]
    fn pr2392_an_import_never_binds_its_own_statement() {
        let project = Project::new()
            .file(
                "src/a.ts",
                "import { X } from './x';\nimport './side';\nimport './missing';\nexport function run() { return X; }\n",
            )
            .file("src/side.ts", "console.log(1);\n")
            .file(
                "src/main.c",
                "#include \"a/b.h\"\n#include <stdio.h>\nint main(void) { return 0; }\n",
            )
            .file(
                "main.lua",
                "local m = require(\"missing.mod\")\nlocal p = require(\"pkg\")\nreturn { m, p }\n",
            )
            .file("pkg/init.lua", "local P = {}\nreturn P\n")
            .index();

        assert_edge!(project, "file:src/a.ts" => imports => "file:src/side.ts", resolved_by = "import");
        assert_no_edge!(project, "file:src/a.ts" => imports => "import ./side");
        assert_no_edge!(project, "file:src/a.ts" => imports => "import ./missing");
        assert_no_edge!(project, "file:src/a.ts" => imports => "import ./x");
        assert_unresolved!(project, "file:src/a.ts" => imports => "./missing");
        assert_unresolved!(project, "file:src/a.ts" => imports => "./x");

        assert_no_edge!(project, "file:src/main.c" => imports => "import a/b.h");
        assert_no_edge!(project, "file:src/main.c" => imports => "import stdio.h");
        assert_unresolved!(project, "file:src/main.c" => imports => "a/b.h");
        assert_unresolved!(project, "file:src/main.c" => imports => "stdio.h");

        assert_edge!(project, "file:main.lua" => imports => "file:pkg/init.lua");
        assert_no_edge!(project, "file:main.lua" => imports => "import missing.mod");
        assert_unresolved!(project, "file:main.lua" => imports => "missing.mod");
    }

    /// A C# `using` of a namespace the project does not declare stays
    /// unresolved, in each file that writes it (a C# `using` mints no import
    /// node, so there is no statement to bind).
    #[test]
    fn pr2392_an_outside_csharp_namespace_stays_unresolved() {
        let project = Project::new()
            .file(
                "src/Svc.cs",
                "using System;\nusing System.Text;\nnamespace App { class Svc {} }\n",
            )
            .file(
                "src/Other.cs",
                "using System.Text;\nnamespace App { class Other {} }\n",
            )
            .file(
                "src/Scoped.cs",
                "using System.Text;\nnamespace App;\nclass Scoped {}\n",
            )
            .index();

        // A `using` outside a block namespace is its file's reference, as a
        // block namespace holds only its body (upstream #2220); a file-scoped
        // namespace holds its file's usings.
        assert_unresolved!(project, "file:src/Svc.cs" => imports => "System");
        assert_unresolved!(project, "file:src/Svc.cs" => imports => "System.Text");
        assert_unresolved!(project, "file:src/Other.cs" => imports => "System.Text");
        assert_unresolved!(project, "namespace App @src/Scoped.cs" => imports => "System.Text");
    }

    /// Upstream #2120 (`7a51969f`), its general half: a receiver-less JS/TS
    /// call never reaches a property or a field (mocha's `it`, `describe`); a
    /// member call on a namespace import, or on a binding from a package
    /// outside the repository, never takes a method picked by name; and a bare
    /// call to a name imported from outside the repository has no project
    /// candidate. A package importing itself is not outside.
    #[test]
    fn pr2120_bare_and_imported_receivers_never_guess_a_member() {
        let project = Project::new()
            .file(
                "package.json",
                "{\"name\":\"app\",\"dependencies\":{\"zod\":\"*\",\"supertest\":\"*\",\"vitest\":\"*\"}}\n",
            )
            .file(
                "src/test/xfail.ts",
                "export interface XFailFunction {\n  it: (title: string) => void;\n}\n",
            )
            .file(
                "src/commands/CacheClear.ts",
                "export class CacheClearCommand {\n  describe = \"Clears the cache.\";\n  run() { return 1; }\n}\n",
            )
            .file(
                "src/test/Mocker.ts",
                "export class Mocker {\n  string(): string { return 'x'; }\n  record(): number { return 1; }\n}\n",
            )
            .file(
                "src/helpers.ts",
                "export function test(name: string) { return name; }\nexport function request(url: string) { return url; }\n",
            )
            .file(
                "src/schemas.ts",
                "import * as mocker from 'zod';\nimport { Mocker as mockerLib } from 'zod';\n\nexport function schemas() {\n  const a = mocker.string();\n  const b = mockerLib.record();\n  return [a, b];\n}\n",
            )
            .file(
                "src/schemas.test.ts",
                "import { test } from 'vitest';\nconst request = require('supertest');\n\ndescribe('schemas', () => {\n  it('works', () => { test('x'); request('app'); });\n});\n",
            )
            .file("src/lib/adapters.ts", "export function getAdapter() { return 1; }\n")
            .file(
                "src/own.ts",
                "import { getAdapter } from 'app';\n\nexport function own() {\n  return getAdapter();\n}\n",
            )
            .index();

        let test_file = "file:src/schemas.test.ts";
        assert_no_edge!(project, test_file => calls => "property XFailFunction::it");
        assert_no_edge!(project, test_file => calls => "property CacheClearCommand::describe");
        assert_no_edge!(project, test_file => calls => "function test");
        assert_no_edge!(project, test_file => calls => "function request");
        // The CommonJS binding is the package's value, not a project symbol.
        assert_no_edge!(project, test_file => calls => "constant request");
        assert_no_edge!(project, "function schemas" => calls => "method Mocker::string");
        assert_no_edge!(project, "function schemas" => calls => "method Mocker::record");
        assert_edge!(project, "function own" => calls => "function getAdapter");
    }

    /// Upstream #2134 (`67744468`), its resolution half: a JS/TS member call
    /// on an import binding never picks a method declared in the calling file
    /// (expo-camera's `CameraManager.isAvailableAsync()` inside `CameraView`'s
    /// own `isAvailableAsync`), and a name the file binds from a package
    /// outside the repository names nothing in it, for every reference kind
    /// (`type Command` from `@tiptap/core` is not a local `command`).
    #[test]
    fn pr2134_imported_names_never_bind_the_caller_or_a_namesake() {
        let project = Project::new()
            .file(
                "package.json",
                "{\"name\":\"app\",\"dependencies\":{\"@tiptap/core\":\"*\",\"@rsbuild/core\":\"*\"}}\n",
            )
            .file("src/ExpoCameraManager.ts", "export default {};\n")
            .file(
                "src/CameraView.ts",
                "import CameraManager from './ExpoCameraManager';\n\nexport class CameraView {\n  static isAvailableAsync() {\n    return CameraManager.isAvailableAsync();\n  }\n}\n",
            )
            .file("src/menu.ts", "export function command() { return 1; }\n")
            .file(
                "src/gap.ts",
                "import type { Command } from '@tiptap/core';\n\nexport function gapCursor(): Command {\n  return () => true;\n}\n",
            )
            .file(
                "src/rsbuild.ts",
                "import { defineConfig, type RsbuildConfig } from '@rsbuild/core';\n\nexport function rsbuildConfig(): RsbuildConfig {\n  return defineConfig({});\n}\n",
            )
            .index();

        assert_no_edge!(
            project,
            "method CameraView::isAvailableAsync" => calls => "method CameraView::isAvailableAsync"
        );
        assert_no_edge!(project, "function gapCursor" => references => "function command");
        assert_no_edge!(project, "function rsbuildConfig" => references => "function rsbuildConfig");
    }

    /// Upstream #2136 (`a4f98202`): a call written through `super` / `base`
    /// (C#) / `super()` (Python) inside an override calls the parent's
    /// implementation, never the method making it. Real recursion keeps its
    /// edge.
    #[test]
    fn pr2136_a_super_call_is_not_a_self_call() {
        let project = Project::new()
            .file(
                "ios/CameraView.swift",
                "import UIKit\nclass CameraView: UIView {\n  override func didMoveToWindow() {\n    super.didMoveToWindow()\n    setup()\n  }\n  func setup() {}\n  func countdown(_ n: Int) {\n    if n > 0 { countdown(n - 1) }\n  }\n}\n",
            )
            .file(
                "android/CameraView.kt",
                "package app\nclass CameraView(context: Context) : FrameLayout(context) {\n  override fun onAttachedToWindow() {\n    super.onAttachedToWindow()\n  }\n}\n",
            )
            .file(
                "java/Service.java",
                "package app;\npublic class Service extends Base {\n  @Override\n  public void start() {\n    super.start();\n  }\n}\n",
            )
            .file(
                "py/views.py",
                "class ProfileView(BaseView):\n    def dispatch(self, request):\n        return super().dispatch(request)\n",
            )
            .file(
                "ts/list.ts",
                "export class List extends Base {\n  render(): string {\n    return super.render() + '!';\n  }\n}\n",
            )
            .file(
                "cs/Handler.cs",
                "public class Handler : BaseHandler {\n  public override void Handle() {\n    base.Handle();\n  }\n}\n",
            )
            .index();

        for method in [
            "method CameraView::didMoveToWindow",
            "method app::CameraView::onAttachedToWindow",
            "method app::Service::start",
            "method ProfileView::dispatch",
            "method List::render",
            "method Handler::Handle",
        ] {
            assert_no_edge!(project, method => calls => method);
        }
        assert_edge!(project, "method CameraView::countdown" => calls => "method CameraView::countdown");
    }

    /// Upstream #2221 (`ff190b30`, its `window` half) and #2223
    /// (`ca1eba98`): a call the extractor recorded by its bare name but the
    /// source writes on another object (`window.$events.listen()`, Scala
    /// `requestToArmeria(request).execute()`, Rust `self.0.into_route(state)`)
    /// is not the calling method. A call on the method's own object, and one
    /// through a field of the class's own type, are.
    #[test]
    fn pr2223_a_call_handed_on_to_another_object_is_not_the_caller() {
        let project = Project::new()
            .file(
                "resources/js/editor-toolbox.ts",
                "export class EditorToolbox {\n  container: HTMLElement;\n\n  toggle(): void {\n    this.container.classList.toggle('open');\n    this.toggle();\n  }\n}\n",
            )
            .file(
                "resources/js/tree.ts",
                "export class TreeNode {\n  left: TreeNode | null = null;\n\n  insert(v: number): void {\n    this.left.insert(v);\n  }\n}\n",
            )
            .file(
                "resources/js/common-events.js",
                "export function listen(editor) {\n  window.$events.listen('editor::replace', () => editor.reset());\n}\n",
            )
            .file(
                "core/src/main/scala/sttp/ArmeriaBackend.scala",
                "package sttp\n\nclass ArmeriaBackend {\n  def requestToArmeria(request: String): Client = new Client\n\n  def execute(request: String): Unit = {\n    val armeriaRes = requestToArmeria(request).execute()\n  }\n}\n",
            )
            .file(
                "src/routing/route.rs",
                "pub struct BoxedIntoRoute(Box<dyn ErasedIntoRoute>);\n\nimpl BoxedIntoRoute {\n    pub fn into_route(self, state: u32) -> u32 {\n        self.0.into_route(state)\n    }\n}\n",
            )
            .index();

        assert_no_edge!(project, "function listen" => calls => "function listen");
        assert_no_edge!(project, "method ArmeriaBackend::execute" => calls => "method ArmeriaBackend::execute");
        assert_no_edge!(project, "method BoxedIntoRoute::into_route" => calls => "method BoxedIntoRoute::into_route");
        assert_edge!(project, "method EditorToolbox::toggle" => calls => "method EditorToolbox::toggle");
        assert_edge!(project, "method TreeNode::insert" => calls => "method TreeNode::insert");
    }

    /// Upstream #2229 (`b2818198`): a type never inherits from itself. cats'
    /// `trait BigDecimalInstances extends cats.kernel.instances.BigDecimalInstances`
    /// names the other type of its name the written qualifier leads to.
    #[test]
    fn pr2229_a_type_never_inherits_itself() {
        let project = Project::new()
            .file(
                "kernel/src/main/scala/cats/kernel/instances/BigDecimalInstances.scala",
                "package cats.kernel\npackage instances\n\ntrait BigDecimalInstances {\n  def order: Int = 0\n}\n",
            )
            .file(
                "core/src/main/scala/cats/instances/bigDecimal.scala",
                "package cats\npackage instances\n\ntrait BigDecimalInstances extends cats.kernel.instances.BigDecimalInstances {\n  def show: String = \"\"\n}\n",
            )
            .file(
                "core/src/main/scala/cats/Functor.scala",
                "package cats\n\nobject Functor {\n  trait AllOps[F[_], A]\n}\n\nobject Apply {\n  trait AllOps[F[_], A] extends Functor.AllOps[F, A]\n}\n\nobject Foldable {\n  trait AllOps[F[_], A]\n}\n",
            )
            .index();

        let core = "trait BigDecimalInstances @core/src/main/scala/cats/instances/bigDecimal.scala";
        let kernel = "trait BigDecimalInstances @kernel/src/main/scala/cats/kernel/instances/BigDecimalInstances.scala";
        assert_no_edge!(project, core => extends => core);
        assert_edge!(project, core => extends => kernel);
        assert_no_edge!(project, "trait Apply::AllOps" => extends => "trait Apply::AllOps");
        assert_edge!(project, "trait Apply::AllOps" => extends => "trait Functor::AllOps");
    }

    /// Upstream #2139 (`66d5dc36`) and #2141 (c) (`ca3bb444`): the last-resort
    /// fuzzy match ignores case only in a language whose identifiers do (PHP,
    /// Pascal, CFML), and a fuzzy `instantiates` never names a method. (A PHP
    /// call written in another case first needs the case-insensitive
    /// prefilter of #2141 (a); `match_fuzzy`'s unit tests cover its fuzzy
    /// half.)
    #[test]
    fn pr2139_fuzzy_matching_ignores_case_only_where_the_language_does() {
        let project = Project::new()
            .file(
                "src/main/java/app/CharPredicate.java",
                "package app;\npublic interface CharPredicate {\n  boolean test(char c);\n}\n",
            )
            .file(
                "src/main/java/app/Request.java",
                "package app;\npublic class Request {\n  public Object cookieManager() { return null; }\n}\n",
            )
            .file(
                "src/test/java/app/ParserTest.java",
                "package app;\nimport org.junit.jupiter.api.Test;\nimport java.net.CookieManager;\npublic class ParserTest {\n  @Test void parses() {\n    Object manager = new CookieManager();\n  }\n}\n",
            )
            .file("src/mocks.ts", "export function mock() { return 1; }\n")
            .file(
                "src/fixtures.ts",
                "export class Store {\n  mockData() { return 1; }\n}\n",
            )
            .file(
                "src/use.test.ts",
                "export function check() {\n  const m = Mock();\n  return new MockData(m);\n}\n",
            )
            .file("tools/paths.py", "def path():\n    return 1\n")
            .file("tools/main.py", "def main():\n    return Path(\".\")\n")
            .file("pkg/builder.go", "package pkg\n\nfunc builder() int { return 1 }\n")
            .file("pkg/main.go", "package pkg\n\nfunc Run() int { return Builder() }\n")
            .index();

        assert_no_edge!(project, "method app::ParserTest::parses" => decorates => "method app::CharPredicate::test");
        assert_no_edge!(project, "class app::ParserTest" => decorates => "method app::CharPredicate::test");
        assert_no_edge!(project, "method app::ParserTest::parses" => instantiates => "method app::Request::cookieManager");
        assert_no_edge!(project, "function check" => calls => "function mock");
        assert_no_edge!(project, "function check" => instantiates => "method Store::mockData");
        assert_no_edge!(project, "function main" => calls => "function path");
        assert_no_edge!(project, "function Run" => calls => "function builder");
    }

    /// Upstream #2166 (`05740b55`) and #2169 (`791ae397`), its general half: a
    /// call on a receiver written as a type the project does not declare
    /// (`Object.assign`, Java `String.valueOf`), or on one whose inferred
    /// type the project does not declare (`List<String> list`, Python
    /// `path = Path("x")`), is that outside type's, never a project method's
    /// guessed by name. Go's exported package variables are not type names,
    /// and the project's own type still resolves.
    #[test]
    fn pr2166_a_call_on_an_outside_type_is_not_a_project_method() {
        let project = Project::new()
            .file(
                "src/main/java/app/StringUtils.java",
                "package app;\npublic class StringUtils {\n  public static String valueOf(Object o) { return String.valueOf(o); }\n}\n",
            )
            .file(
                "src/main/java/app/Use.java",
                "package app;\npublic class Use {\n  public int run() {\n    Integer.valueOf(1);\n    return StringUtils.valueOf(2).length();\n  }\n}\n",
            )
            .file(
                "src/main/java/app/NonNullElementWrapperList.java",
                "package app;\npublic class NonNullElementWrapperList {\n  public boolean add(Object o) { return true; }\n}\n",
            )
            .file(
                "src/main/java/app/UseList.java",
                "package app;\nimport java.util.ArrayList;\nimport java.util.List;\npublic class UseList {\n  public void run() {\n    List<String> list = new ArrayList<>();\n    list.add(\"x\");\n  }\n}\n",
            )
            .file(
                "src/ts/ObjectUtils.ts",
                "export class ObjectUtils {\n  static assign(a: unknown) { return a; }\n}\n",
            )
            .file(
                "src/ts/merge.ts",
                "export function merge(a: unknown) {\n  return Object.assign(a);\n}\n",
            )
            .file(
                "tools/helper.py",
                "class PathHelper:\n    def exists(self):\n        return True\n",
            )
            .file(
                "tools/check.py",
                "from pathlib import Path\n\ndef check():\n    path = Path(\"x\")\n    return path.exists()\n",
            )
            .file(
                "pkg/form.go",
                "package pkg\n\ntype form struct{}\n\nfunc (f form) Bind() int { return 1 }\n\nvar FormPost = form{}\n\nfunc Run() int { return FormPost.Bind() }\n",
            )
            .index();

        assert_no_edge!(project, "method app::StringUtils::valueOf" => calls => "method app::StringUtils::valueOf");
        assert_edge!(project, "method app::Use::run" => calls => "method app::StringUtils::valueOf");
        assert_no_edge!(project, "method app::UseList::run" => calls => "method app::NonNullElementWrapperList::add");
        assert_no_edge!(project, "function merge" => calls => "method ObjectUtils::assign");
        assert_no_edge!(project, "function check" => calls => "method PathHelper::exists");
        assert_edge!(project, "function Run" => calls => "method form::Bind");
    }

    /// Upstream #2169 reads a C++ receiver's declared type through
    /// `normalizeCppTypeName`: `SkipList<Key, Comparator> list_` is the
    /// project's `SkipList`, so its call keeps the method (leveldb's
    /// `list_.Insert(key)`), while a receiver of a type the project does not
    /// declare stops the guess (`QString s; s.size()`).
    #[test]
    fn pr2169_a_cpp_receiver_of_a_project_class_template_keeps_its_method() {
        let project = Project::new()
            .file(
                "db/skiplist.h",
                "namespace leveldb {\ntemplate <typename Key, class Comparator>\nclass SkipList {\n public:\n  void Insert(const Key& key) {}\n  bool Contains(const Key& key) const { return false; }\n};\nclass Sizer {\n public:\n  int size() const { return 0; }\n};\n}  // namespace leveldb\n",
            )
            .file(
                "db/skiplist_test.cc",
                "#include \"db/skiplist.h\"\n#include <QString>\nnamespace leveldb {\nclass ConcurrentTest {\n  SkipList<int, int> list_;\n\n public:\n  void WriteStep() { list_.Insert(1); }\n};\nbool Probe() {\n  SkipList<int, int> list;\n  return list.Contains(2);\n}\nint Length() {\n  QString s;\n  return s.size();\n}\n}  // namespace leveldb\n",
            )
            .index();

        assert_edge!(
            project,
            "method leveldb::ConcurrentTest::WriteStep" => calls => "method leveldb::SkipList::Insert"
        );
        assert_edge!(project, "function leveldb::Probe" => calls => "method leveldb::SkipList::Contains");
        assert_no_edge!(project, "function leveldb::Length" => calls => "method leveldb::Sizer::size");
    }

    /// W3-05c (KEEP-RUST, owner 2026-10-11): a C# call on a type the project
    /// does not declare reaches the one project extension method in scope
    /// that takes it as `this` and fits the call's arguments (Newtonsoft's
    /// `objectType.IsEnum()` on a `System.Type`), and the overload pass keeps
    /// it (`propertyInfo.GetGetMethod(true)` stays the `bool` overload). Two
    /// fitting overloads, an extension whose namespace is not in scope (also a
    /// sibling block's local `using`), a qualified `this` type, and an outside
    /// type with no extension all stay unresolved.
    #[test]
    fn pr2169_a_csharp_call_on_an_outside_type_reaches_the_projects_extension_method() {
        let project = Project::new()
            .file(
                "src/Utilities/TypeExtensions.cs",
                "using System;\nusing System.Collections.Generic;\nusing System.Reflection;\n\nnamespace App.Utilities\n{\n    internal static class TypeExtensions\n    {\n        public static bool IsEnum(this Type type)\n        {\n            return false;\n        }\n\n        public static MethodInfo GetGetMethod(this PropertyInfo propertyInfo)\n        {\n            return propertyInfo.GetGetMethod(false);\n        }\n\n        public static MethodInfo GetGetMethod(this PropertyInfo propertyInfo, bool nonPublic)\n        {\n            return null;\n        }\n\n        public static MethodInfo GetMethod(this Type type, string name, BindingFlags bindingFlags)\n        {\n            return null;\n        }\n\n        public static MethodInfo GetMethod(this Type type, string name, IList<Type> parameterTypes)\n        {\n            return null;\n        }\n\n        public static int Scale(this System.Timers.Timer timer)\n        {\n            return 0;\n        }\n    }\n}\n",
            )
            .file(
                "src/Utilities/ILGeneratorExtensions.cs",
                "using System.Reflection;\nusing System.Reflection.Emit;\n\nnamespace App.Utilities\n{\n    internal static class ILGeneratorExtensions\n    {\n        public static void CallMethod(this ILGenerator generator, MethodInfo methodInfo)\n        {\n        }\n    }\n}\n",
            )
            .file(
                "src/Hidden/HiddenExtensions.cs",
                "using System;\n\nnamespace App.Hidden\n{\n    internal static class HiddenExtensions\n    {\n        public static bool IsHidden(this Type type)\n        {\n            return false;\n        }\n    }\n}\n",
            )
            .file(
                "src/Utilities/StringUtils.cs",
                "using System;\n\nnamespace App.Utilities\n{\n    internal static class StringUtils\n    {\n        public static string FormatWith(this string format, IFormatProvider provider, object? arg0)\n        {\n            return format;\n        }\n\n        public static string FormatWith(this string format, IFormatProvider provider, object? arg0, object? arg1)\n        {\n            return format.FormatWith(provider, new object?[] { arg0, arg1 });\n        }\n\n        private static string FormatWith(this string format, IFormatProvider provider, params object?[] args)\n        {\n            return format;\n        }\n    }\n}\n",
            )
            .file(
                "src/Utilities/Boxes.cs",
                "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Open<T>(this Box<T> box, params T[] values)\n        {\n            return box;\n        }\n\n        public static object Open<U>(this Box<U> box, U value)\n        {\n            return box;\n        }\n\n        public static object Seal<T>(this Box<T> box, params T[] values) where T : struct\n        {\n            return box;\n        }\n\n        public static object Seal<U>(this Box<U> box, U value) where U : class\n        {\n            return box;\n        }\n\n        public static object Wrap<T>(this Box<T> box, params T[] values) where T: struct\n        {\n            return box;\n        }\n\n        public static object Wrap<T>(this Box< T > /* boxed */ box, T value) where T : struct // value types\n        {\n            return box;\n        }\n\n        public static object Note<T>(this Box<T> box, params T[] values) where T : struct { return box; }\n\n        public static object Note<U>(this Box<U> box, U value, string note = \"\"\"x\" // literal text\"\"\") where U : struct { return box; }\n    }\n}\n",
            )
            .file(
                "src/Utilities/Picks.cs",
                "using System;\n\nnamespace App.Utilities\n{\n    internal static class Picks\n    {\n        public static object Pick(this Type type, params object?[] args)\n        {\n            return type;\n        }\n\n        public static object Pick(object? arg)\n        {\n            return arg;\n        }\n\n        public static object Take(this Type type, params object?[] args)\n        {\n            return type;\n        }\n\n        public static object Take(this Exception error, object? arg)\n        {\n            return error;\n        }\n    }\n}\n",
            )
            .file(
                "src/Model/Money.cs",
                "namespace App.Model\n{\n    public class Money\n    {\n        public override string ToString()\n        {\n            return \"0\";\n        }\n    }\n}\n",
            )
            .file(
                "src/Serialization/Reader.cs",
                "using System;\nusing System.Collections.Generic;\nusing System.IO;\nusing System.Reflection;\nusing System.Reflection.Emit;\nusing App.Utilities;\n\nnamespace App.Serialization\n{\n    public class Reader\n    {\n        public bool Check(Type objectType)\n        {\n            return objectType.IsEnum();\n        }\n\n        public void Emit(ILGenerator generator, MethodInfo m)\n        {\n            generator.CallMethod(m);\n        }\n\n        public MethodInfo Getter(PropertyInfo propertyInfo)\n        {\n            return propertyInfo.GetGetMethod(true);\n        }\n\n        public MethodInfo Find(Type type, IList<Type> parameterTypes)\n        {\n            return type.GetMethod(null, parameterTypes);\n        }\n\n        public bool Hidden(Type type)\n        {\n            return type.IsHidden();\n        }\n\n        public int Tick(System.Timers.Timer timer)\n        {\n            return timer.Scale();\n        }\n\n        public string Text(StringWriter writer)\n        {\n            return writer.ToString();\n        }\n\n        public object Choose(Type type, object? arg)\n        {\n            return type.Pick(arg);\n        }\n\n        public object Keep(Type type, object? arg)\n        {\n            return type.Take(arg);\n        }\n\n        public object Unbox(Box<int> box)\n        {\n            return box.Open(1);\n        }\n\n        public object Close(Box<int> box)\n        {\n            return box.Seal(1);\n        }\n\n        public object Fold(Box<int> box)\n        {\n            return box.Wrap(1);\n        }\n\n        public object Jot(Box<int> box)\n        {\n            return box.Note(1);\n        }\n    }\n}\n",
            )
            .file(
                "src/Nest.cs",
                "using System;\n\nnamespace Outer\n{\n    internal static class OuterExtensions\n    {\n        public static bool IsOuter(this Type type)\n        {\n            return false;\n        }\n    }\n\n    namespace Inner\n    {\n        internal static class InnerExtensions\n        {\n            public static bool IsInner(this Type type)\n            {\n                return false;\n            }\n        }\n\n        public class Deep\n        {\n            public bool Down(Type type)\n            {\n                return type.IsOuter();\n            }\n        }\n    }\n\n    public class Top\n    {\n        public bool Up(Type type)\n        {\n            return type.IsInner();\n        }\n    }\n}\n",
            )
            .file(
                "src/Two.cs",
                "using System;\n\nnamespace App.First\n{\n    using App.Hidden;\n\n    public class A\n    {\n        public bool Seen(Type type)\n        {\n            return type.IsHidden();\n        }\n    }\n}\n\nnamespace App.Second\n{\n    public class B\n    {\n        public bool Unseen(Type type)\n        {\n            return type.IsHidden();\n        }\n    }\n}\n",
            )
            .index();

        let extensions = "src/Utilities/TypeExtensions.cs";
        assert_edge!(
            project,
            "method App.Serialization::Reader::Check" => calls => "method App.Utilities::TypeExtensions::IsEnum"
        );
        assert_edge!(
            project,
            "method App.Serialization::Reader::Emit" => calls => "method App.Utilities::ILGeneratorExtensions::CallMethod"
        );
        assert_edge!(
            project,
            "method App.Serialization::Reader::Getter" => calls => &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:19")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Getter" => calls => &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:14")
        );
        assert_edge!(
            project,
            &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:14") => calls => &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:19")
        );
        let strings = "src/Utilities/StringUtils.cs";
        assert_edge!(
            project,
            &format!("method App.Utilities::StringUtils::FormatWith @{strings}:12") => calls => &format!("method App.Utilities::StringUtils::FormatWith @{strings}:17")
        );
        assert_no_edge!(
            project,
            &format!("method App.Utilities::StringUtils::FormatWith @{strings}:12") => calls => &format!("method App.Utilities::StringUtils::FormatWith @{strings}:7")
        );
        // Only overloads that take the receiver as `this` compete: neither
        // a plain `Pick(object?)` nor `Take(this Exception, …)` replaces the
        // `params` overload the receiver's type chose, though each fits the
        // one argument in its normal form.
        let picks = "src/Utilities/Picks.cs";
        assert_edge!(
            project,
            "method App.Serialization::Reader::Choose" => calls => &format!("method App.Utilities::Picks::Pick @{picks}:7")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Choose" => calls => &format!("method App.Utilities::Picks::Pick @{picks}:12")
        );
        assert_edge!(
            project,
            "method App.Serialization::Reader::Keep" => calls => &format!("method App.Utilities::Picks::Take @{picks}:17")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Keep" => calls => &format!("method App.Utilities::Picks::Take @{picks}:22")
        );
        // Overloads whose `this` types differ only in the names of their own
        // type parameters (`Box<T>`, `Box<U>`) extend the same type: the one
        // argument fits `Open<U>(this Box<U>, U)` in its normal form.
        let boxes = "src/Utilities/Boxes.cs";
        assert_edge!(
            project,
            "method App.Serialization::Reader::Unbox" => calls => &format!("method App.Utilities::BoxExtensions::Open @{boxes}:15")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Unbox" => calls => &format!("method App.Utilities::BoxExtensions::Open @{boxes}:10")
        );
        // Their `where` clauses are part of the receiver: `U : class` takes
        // no `Box<int>`, so `box.Seal(1)` keeps the `T : struct` overload.
        assert_edge!(
            project,
            "method App.Serialization::Reader::Close" => calls => &format!("method App.Utilities::BoxExtensions::Seal @{boxes}:20")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Close" => calls => &format!("method App.Utilities::BoxExtensions::Seal @{boxes}:25")
        );
        // Spacing and comments are no part of a receiver or a constraint:
        // `Box< T > /* boxed */` with `where T : struct // …` is `Box<T>` with
        // `where T: struct`, so `box.Wrap(1)` takes the normal-form overload.
        assert_edge!(
            project,
            "method App.Serialization::Reader::Fold" => calls => &format!("method App.Utilities::BoxExtensions::Wrap @{boxes}:35")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Fold" => calls => &format!("method App.Utilities::BoxExtensions::Wrap @{boxes}:30")
        );
        // A comment marker inside a raw string literal is text: the default
        // value `"""x" // literal text"""` hides no `where` clause.
        assert_edge!(
            project,
            "method App.Serialization::Reader::Jot" => calls => &format!("method App.Utilities::BoxExtensions::Note @{boxes}:42")
        );
        assert_no_edge!(
            project,
            "method App.Serialization::Reader::Jot" => calls => &format!("method App.Utilities::BoxExtensions::Note @{boxes}:40")
        );
        assert_no_edge!(project, "method App.Serialization::Reader::Find" => calls => "method App.Utilities::TypeExtensions::GetMethod");
        assert_no_edge!(project, "method App.Serialization::Reader::Hidden" => calls => "method App.Hidden::HiddenExtensions::IsHidden");
        assert_no_edge!(project, "method App.Serialization::Reader::Tick" => calls => "method App.Utilities::TypeExtensions::Scale");
        assert_no_edge!(project, "method App.Serialization::Reader::Text" => calls => "method App.Model::Money::ToString");
        assert_edge!(project, "method App.First::A::Seen" => calls => "method App.Hidden::HiddenExtensions::IsHidden");
        // `B` is in `App.Second`, whose block imports no `App.Hidden`.
        assert_no_edge!(project, "method App.Second::B::Unseen" => calls => "method App.Hidden::HiddenExtensions::IsHidden");
        assert_unresolved!(project, "method App.Second::B::Unseen" => calls => "type.IsHidden");
        // An enclosing namespace's extension is in scope in a nested one; a
        // nested one's is not in scope outside it.
        assert_edge!(project, "method Outer.Inner::Deep::Down" => calls => "method Outer::OuterExtensions::IsOuter");
        assert_no_edge!(project, "method Outer::Top::Up" => calls => "method Outer.Inner::InnerExtensions::IsInner");
        assert_unresolved!(project, "method Outer::Top::Up" => calls => "type.IsInner");

        // The same overload pair with an attributed `this` parameter.
        let attributed = Project::new()
            .file(
                "src/Utilities/TypeExtensions.cs",
                "using System.Diagnostics.CodeAnalysis;\nusing System.Reflection;\n\nnamespace App.Utilities\n{\n    internal static class TypeExtensions\n    {\n        public static MethodInfo GetGetMethod([NotNull] this PropertyInfo propertyInfo)\n        {\n            return propertyInfo.GetGetMethod(false);\n        }\n\n        public static MethodInfo GetGetMethod([NotNull] this PropertyInfo propertyInfo, bool nonPublic)\n        {\n            return null;\n        }\n    }\n}\n",
            )
            .file(
                "src/Reader.cs",
                "using System.Reflection;\nusing App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public MethodInfo Getter(PropertyInfo propertyInfo)\n        {\n            return propertyInfo.GetGetMethod(true);\n        }\n    }\n}\n",
            )
            .index();
        let extensions = "src/Utilities/TypeExtensions.cs";
        assert_edge!(
            attributed,
            "method App::Reader::Getter" => calls => &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:13")
        );
        assert_no_edge!(
            attributed,
            "method App::Reader::Getter" => calls => &format!("method App.Utilities::TypeExtensions::GetGetMethod @{extensions}:8")
        );
    }

    /// Upstream #2180 (`aa0c73c4`): a call through a class name reaches the
    /// class method it inherits, by the supertypes its declaration heads name
    /// (Pascal `class(…)`, Python bases), before any guess by name.
    #[test]
    fn pr2180_a_call_through_a_class_name_reaches_the_method_it_inherits() {
        let project = Project::new()
            .file(
                "src/Horse.Core.pas",
                "unit Horse.Core;\n\ninterface\n\ntype\n  THorseCore = class\n  public\n    class function Get(const APath: string): THorseCore;\n  end;\n\nimplementation\n\nclass function THorseCore.Get(const APath: string): THorseCore;\nbegin\n  Result := nil;\nend;\n\nend.\n",
            )
            .file(
                "src/Horse.Core.Group.pas",
                "unit Horse.Core.Group;\n\ninterface\n\ntype\n  IHorseCoreGroup = interface\n    function Get(const APath: string): IHorseCoreGroup;\n  end;\n\nimplementation\n\nend.\n",
            )
            .file(
                "src/Horse.pas",
                "unit Horse;\n\ninterface\n\nuses Horse.Core;\n\ntype\n  THorseProvider = class(THorseCore)\n  end;\n\n  THorse = class(THorseProvider)\n  end;\n\nimplementation\n\nend.\n",
            )
            .file(
                "samples/Console.dpr",
                "program Console;\n\nuses Horse;\n\nbegin\n  THorse.Get('/ping');\nend.\n",
            )
            .file(
                "app/views.py",
                "class APIView:\n    @classmethod\n    def as_view(cls):\n        return cls\n\n\nclass GenericAPIView(APIView):\n    pass\n\n\nclass UserList(GenericAPIView):\n    pass\n\n\nclass Router:\n    def as_view(self):\n        return None\n",
            )
            .file(
                "app/urls.py",
                "from app.views import UserList\n\n\ndef routes():\n    return [UserList.as_view()]\n",
            )
            .index();

        assert_edge!(project, "file:samples/Console.dpr" => calls => "method THorseCore::Get @src/Horse.Core.pas:8");
        assert_no_edge!(project, "file:samples/Console.dpr" => calls => "method IHorseCoreGroup::Get");
        // (In a function: a module-level assignment's initializer is not
        // walked for calls, as before.)
        assert_edge!(project, "function routes" => calls => "method APIView::as_view");
        assert_no_edge!(project, "function routes" => calls => "method Router::as_view");
    }

    /// Upstream #2175 (`c0a38a87`), #2197 (`85aad42f`) and #2221 (`ff190b30`,
    /// its Strategy 3 half): a receiver-word guess is never a handler the web
    /// framework dispatches to, never a test double the receiver and the
    /// calling file do not name, and never the calling method itself. A test
    /// that names its double still calls it.
    #[test]
    fn pr2175_a_receiver_guess_is_never_a_handler_a_double_or_the_caller() {
        let project = Project::new()
            .file(
                "app/handlers.py",
                "from django.views import View\n\n\nclass ClientRegistrationView(View):\n    def post(self, request):\n        return None\n\n\nclass SessionView(View):\n    def post(self, request):\n        return None\n",
            )
            .file(
                "tests/fakes.py",
                "class _FakeHTTPResponse:\n    def json(self):\n        return {}\n\n\nclass Payload:\n    def json(self):\n        return {}\n",
            )
            .file(
                "tests/test_views.py",
                "def test_login(client):\n    resp = client.post(\"/login\")\n    assert resp.status_code == 200\n\n\ndef test_api(api_client):\n    response = api_client.get(\"/recipes\")\n    data = response.json()\n    assert data\n",
            )
            .file(
                "tests/mocking.py",
                "class MockedResponse:\n    def __init__(self, data):\n        self.data = data\n\n    def json(self):\n        return self.data\n",
            )
            .file(
                "tests/test_mocking.py",
                "from tests.mocking import MockedResponse\n\n\ndef test_mocked():\n    resp = MockedResponse({\"ok\": True})\n    assert resp.json()[\"ok\"]\n",
            )
            .file(
                "app/Uploads/FileStorage.php",
                "<?php\n\nclass FileStorage\n{\n    public function delete(string $path): void\n    {\n        $storage = $this->getStorageDisk();\n        $storage->delete($path);\n    }\n\n    protected function getStorageDisk()\n    {\n        return null;\n    }\n}\n",
            )
            .file(
                "app/Uploads/ImageStorage.php",
                "<?php\n\nclass ImageStorage\n{\n    public function delete(string $path): void\n    {\n    }\n}\n",
            )
            .index();

        assert_no_edge!(project, "function test_login" => calls => "method ClientRegistrationView::post");
        assert_no_edge!(project, "function test_login" => calls => "method SessionView::post");
        assert_no_edge!(project, "function test_api" => calls => "method _FakeHTTPResponse::json");
        assert_no_edge!(project, "function test_api" => calls => "method MockedResponse::json");
        assert_edge!(project, "function test_mocked" => calls => "method MockedResponse::json");
        assert_no_edge!(project, "method FileStorage::delete" => calls => "method FileStorage::delete");
    }

    /// Upstream #2209 (`de9f346c`): a test suite is not linked into the
    /// program, so production code never names one of its symbols, while
    /// test-support code a project ships (`testing/`) stays in reach and a
    /// test reaches another test's helpers.
    #[test]
    fn pr2209_production_code_never_names_a_test_suite_symbol() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"orm\",\"private\":true}\n")
            .file("test/functional/entity/Record.ts", "export class Record {\n  id = 0;\n}\n")
            .file(
                "test/functional/helpers.ts",
                "export function makeFixture(): number {\n  return 1;\n}\n",
            )
            .file(
                "test/functional/record.test.ts",
                "export function uses(): number {\n  return makeFixture();\n}\n",
            )
            .file(
                "src/testing/stub.ts",
                "export function stubConnection(): number {\n  return 2;\n}\n",
            )
            .file(
                "src/columns.ts",
                "export function toJson(values: Record<string, number>): string {\n  return JSON.stringify(values);\n}\n\nexport function connect(): number {\n  return stubConnection();\n}\n",
            )
            .file(
                "src/main/java/app/Service.java",
                "package app;\n\npublic class Service implements Runnable {\n  @Override\n  public void run() { }\n}\n",
            )
            .file(
                "src/test/java/app/InterceptorTest.java",
                "package app;\n\npublic class InterceptorTest {\n  @interface Override { }\n}\n",
            )
            .index();

        assert_no_edge!(project, "function toJson" => references => "class Record");
        assert_no_edge!(project, "method app::Service::run" => decorates => "interface app::InterceptorTest::Override");
        assert_edge!(project, "function connect" => calls => "function stubConnection");
        assert_edge!(project, "function uses" => calls => "function makeFixture");
    }

    /// Upstream #2421 (`22a7ba85`) and #2434 (`1fb8691d`): a C or C++ file
    /// named like a test is visible to the code that includes it, a source
    /// file's definition through the same-named header it includes
    /// (`conformance_test.cc` through `conformance_test.h`); and Google's
    /// `foo_unittest.cc` is a test suite production code never reaches.
    #[test]
    fn pr2421_an_included_test_named_file_is_part_of_the_translation_unit() {
        let project = Project::new()
            .file(
                "conformance/conformance_test.h",
                "class ConformanceTestSuite {\n public:\n  void ReportFailure(int code);\n};\n",
            )
            .file(
                "conformance/conformance_test.cc",
                "#include \"conformance_test.h\"\n\nvoid ConformanceTestSuite::ReportFailure(int code) {}\n",
            )
            .file(
                "conformance/binary_suite.cc",
                "#include \"conformance_test.h\"\n\nvoid RunSuite(ConformanceTestSuite& suite_) {\n  suite_.ReportFailure(1);\n}\n",
            )
            .file(
                "src/cpp/stack_unittest.cc",
                "struct StackHelper {\n  int size() { return 0; }\n};\n",
            )
            .file(
                "src/cpp/walk.cc",
                "int Walk() {\n  return StackHelper().size();\n}\n",
            )
            .index();

        assert_edge!(project, "function RunSuite" => calls => "method ConformanceTestSuite::ReportFailure");
        assert_no_edge!(project, "function Walk" => calls => "method StackHelper::size");
        assert_no_edge!(project, "function Walk" => instantiates => "struct StackHelper");
    }

    /// Upstream #2212 (`c20283e2`), #2213 (`5f1a6357`), #2238 (`31c5153d`) and
    /// #2241 (`a5da8daf`): a call reaches the overload its arguments fit,
    /// counted at the call (defaults and varargs counted at the declaration);
    /// Swift by its argument labels. Real recursion keeps its self-edge.
    #[test]
    fn pr2212_a_call_reaches_the_overload_its_arguments_fit() {
        let project = Project::new()
            .file(
                "src/main/java/app/Instants.java",
                "package app;\n\npublic class Instants {\n  public static Object toInstant(final Object instant) {\n    return toInstant(instant, null);\n  }\n\n  public static Object toInstant(final Object instant, final Object defaultInstant) {\n    return instant != null ? instant : defaultInstant;\n  }\n\n  public static int depth(final int n) {\n    return n <= 0 ? 0 : depth(n - 1);\n  }\n}\n",
            )
            .file(
                "src/main/java/app/HashCodeBuilder.java",
                "package app;\n\npublic class HashCodeBuilder {\n  public static int reflectionHashCode(final int initial, final int multiplier, final Object object) {\n    return initial * multiplier;\n  }\n\n  public static int reflectionHashCode(final Object object, final String... excludeFields) {\n    return 1;\n  }\n}\n",
            )
            .file(
                "src/main/java/app/Point.java",
                "package app;\n\npublic class Point {\n  public int hash() {\n    return HashCodeBuilder.reflectionHashCode(this);\n  }\n}\n",
            )
            .file(
                "src/Json/Convert.cs",
                "namespace App\n{\n    public static class Convert\n    {\n        public static string DeserializeXNode(string value)\n        {\n            return DeserializeXNode(value, null);\n        }\n\n        public static string DeserializeXNode(string value, string root)\n        {\n            return value + root;\n        }\n    }\n}\n",
            )
            .file(
                "Sources/TableController.swift",
                "class TableController {\n    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {\n        return 0\n    }\n\n    func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {\n        if self.tableView(tableView, numberOfRowsInSection: section) == 0 { return nil }\n        return \"x\"\n    }\n\n    func cancel(producingResumeData shouldProduce: Bool) -> Self {\n        return cancel(optionallyProducingResumeData: nil)\n    }\n\n    private func cancel(optionallyProducingResumeData handler: ((Data?) -> Void)?, retry: Bool = false) -> Self {\n        return self\n    }\n}\n",
            )
            .file(
                "contracts/access/AccessControl.sol",
                "pragma solidity ^0.8.20;\n\nabstract contract AccessControl {\n    function _checkRole(bytes32 role) internal view virtual {\n        _checkRole(role, msg.sender);\n    }\n\n    function _checkRole(bytes32 role, address account) internal view virtual {\n    }\n}\n",
            )
            .index();

        let java = "src/main/java/app/Instants.java";
        assert_edge!(project, &format!("method app::Instants::toInstant @{java}:4") => calls => &format!("method app::Instants::toInstant @{java}:8"));
        assert_no_edge!(project, &format!("method app::Instants::toInstant @{java}:4") => calls => &format!("method app::Instants::toInstant @{java}:4"));
        assert_edge!(project, "method app::Instants::depth" => calls => "method app::Instants::depth");
        let builder = "src/main/java/app/HashCodeBuilder.java";
        assert_edge!(project, "method app::Point::hash" => calls => &format!("method app::HashCodeBuilder::reflectionHashCode @{builder}:8"));
        assert_no_edge!(project, "method app::Point::hash" => calls => &format!("method app::HashCodeBuilder::reflectionHashCode @{builder}:4"));
        let cs = "src/Json/Convert.cs";
        assert_edge!(project, &format!("method App::Convert::DeserializeXNode @{cs}:5") => calls => &format!("method App::Convert::DeserializeXNode @{cs}:10"));
        let swift = "Sources/TableController.swift";
        assert_edge!(project, &format!("method TableController::tableView @{swift}:6") => calls => &format!("method TableController::tableView @{swift}:2"));
        assert_edge!(project, &format!("method TableController::cancel @{swift}:11") => calls => &format!("method TableController::cancel @{swift}:15"));
        let sol = "contracts/access/AccessControl.sol";
        assert_edge!(project, &format!("method AccessControl::_checkRole @{sol}:4") => calls => &format!("method AccessControl::_checkRole @{sol}:8"));
    }

    /// Upstream #2415 (`19f91e27`), #2211 (`5c01f017`) and #2210 (`d7cdb0c0`):
    /// a framework resolver answers only references written in its own
    /// languages. A React app's hook and context rules never take a Python
    /// or Java `useSession()` / `SecurityContext`, and the Vue resolver never
    /// takes a Python call for a `.vue` component; the app's own TSX hook call
    /// still resolves.
    #[test]
    fn pr2415_a_framework_resolves_only_its_own_languages() {
        let project = Project::new()
            .file(
                "package.json",
                "{\"name\":\"web\",\"dependencies\":{\"react\":\"^19.0.0\",\"vue\":\"^3.0.0\"}}\n",
            )
            .file(
                "src/hooks/useSession.tsx",
                "export function useSession() {\n  return null;\n}\n\nexport const SecurityContext = { user: null };\n",
            )
            .file(
                "src/App.tsx",
                "import { useSession } from './hooks/useSession';\n\nexport function App() {\n  const session = useSession();\n  return <div>{String(session)}</div>;\n}\n",
            )
            .file(
                "src/components/QueryFilterBuilder.vue",
                "<template><div /></template>\n<script setup lang=\"ts\">\nconst x = 1;\n</script>\n",
            )
            .file(
                "backend/session.py",
                "def handle():\n    useSession()\n    return SecurityContext()\n\n\ndef build():\n    return QueryFilterBuilder()\n",
            )
            .file(
                "server/src/main/java/app/Auth.java",
                "package app;\n\npublic class Auth {\n  public void check() {\n    useSession();\n  }\n}\n",
            )
            .index();

        assert_edge!(project, "function App" => calls => "function useSession");
        assert_no_edge!(project, "function handle" => calls => "function useSession");
        assert_no_edge!(project, "function handle" => calls => "constant SecurityContext");
        assert_no_edge!(project, "method app::Auth::check" => calls => "function useSession");
        assert_no_edge!(
            project,
            "function build" => calls => "component src/components/QueryFilterBuilder.vue::QueryFilterBuilder"
        );
    }
    /// Upstream #2123 (`c5ba6807`): a name an enclosing declaration declares
    /// as a type parameter is that parameter (Scala `def zipWith[A, B, C]`,
    /// Java `<T extends …> T max(`, TS `wrap<T>`, Dart `first<T>`), a Scala
    /// def's value parameter is that parameter (`f(fa.head)`), and a type
    /// declared inside a function is only in scope in there. A bare Scala
    /// type name reaches another type's member only from inside it, a
    /// subtype, or a file importing its members; otherwise the type of that
    /// name in scope is taken (`trait FlatMap`, not `Eval.FlatMap`).
    #[test]
    fn pr2123_type_parameters_and_function_local_types_stay_in_scope() {
        let project = Project::new()
            .file("src/Inst.scala", "package app\nobject Inst {\n  implicit def A: Int = 1\n}\n")
            .file(
                "src/Seqs.scala",
                "package app\nobject Seqs {\n  def zipWith[A, B, C](fa: List[A], fb: List[B])(f: (A, B) => C): List[C] = ???\n}\n",
            )
            .file(
                "src/Ops.scala",
                "package app\nobject Ops {\n  def |+|[A](x: A, y: A): A = x\n  def *[B](b: B): B = b\n}\ntrait Heyting[@sp(Int, Long) A] {\n  def and(a: A, b: A): A\n}\nfinal case class FlatMapped(f: Int => Int) {\n  type A = Int\n}\ntrait Laws {\n  type L = Int\n}\nclass MyLaws extends Laws {\n  def check(x: L): Boolean = true\n}\nobject UsesImport {\n  import FlatMapped._\n  def viaImport(x: A): Int = 1\n}\nobject Folds {\n  def foldMap[A](fa: List[A])(f: A => Int)(implicit G: Ordering[Int]): Int = { f(fa.head); G.max(1, 2) }\n}\nobject TraverseLaws {\n  def updatedRef[A, B >: A](fa: List[A], b: B): Option[B] = None\n}\ntrait UnorderedTraverseTests {\n  def unorderedTraverse[\n    A,\n    B,\n    C,\n    X,\n    Y\n  ](implicit\n    ArbXB: Option[X],\n    ArbYB: List[B]\n  ): Unit = ()\n}\n",
            )
            .file(
                "src/Elsewhere.scala",
                "package app\nobject Elsewhere {\n  def combineAll(xs: List[A]): Int = 1\n  def lift(implicit F: FlatMap): Int = 1\n}\n",
            )
            .file("src/FlatMap.scala", "package app\ntrait FlatMap\n")
            .file("src/Eval.scala", "package app\nobject Eval {\n  final case class FlatMap(x: Int)\n}\n")
            .file("src/Fns.scala", "package app\nobject Fns {\n  def f(x: Int): Int = x\n}\n")
            .file(
                "src/SyntaxSuite.scala",
                "package app\nclass SyntaxSuite {\n  def testOptionEmpty(): Unit = {\n    case class B()\n    val b: B = B()\n  }\n}\n",
            )
            .file("src/T.java", "package app;\npublic class T {}\n")
            .file("src/Foo.java", "package app;\npublic class Foo {}\n")
            .file(
                "src/Util.java",
                "package app;\nimport java.util.List;\nimport java.util.Map;\npublic class Util {\n  public static <T extends Comparable<T>> T max(List<T> xs) { return null; }\n  public static Map<String, Foo> index(List<Foo> xs) { return null; }\n}\n",
            )
            .file("src/wrap.ts", "export class T {}\nexport function wrap<T>(x: T): T { return x; }\n")
            .file(
                "src/local.ts",
                "export function build() {\n  class Local {}\n  return new Local();\n}\n\nexport function other(): Local {\n  return null as any;\n}\n",
            )
            .file("lib/first.dart", "const T = 1;\nT first<T>(List<T> xs) => xs.first;\n")
            .index();

        let local_b = "class SyntaxSuite::testOptionEmpty::B";
        for owner in [
            "method Seqs::zipWith",
            "method Ops::*",
            "method TraverseLaws::updatedRef",
            "method UnorderedTraverseTests::unorderedTraverse",
        ] {
            assert_no_edge!(project, owner => references => local_b);
        }
        assert_no_edge!(project, "method Seqs::zipWith" => references => "method Inst::A");
        assert_no_edge!(project, "method Elsewhere::combineAll" => references => "method Inst::A");
        for owner in [
            "method Ops::|+|",
            "method Heyting::and",
            "method Folds::foldMap",
            "method TraverseLaws::updatedRef",
        ] {
            assert_no_edge!(project, owner => references => "type_alias FlatMapped::A");
        }
        assert_no_edge!(project, "method Folds::foldMap" => calls => "method Fns::f");
        assert_edge!(project, "method Elsewhere::lift" => references => "trait FlatMap");
        assert_no_edge!(project, "method Elsewhere::lift" => references => "class Eval::FlatMap");
        assert_edge!(project, "method MyLaws::check" => references => "type_alias Laws::L");
        assert_edge!(project, "method UsesImport::viaImport" => references => "type_alias FlatMapped::A");
        assert_edge!(project, "method SyntaxSuite::testOptionEmpty" => instantiates => local_b);

        assert_no_edge!(project, "method app::Util::max" => references => "class app::T");
        assert_edge!(project, "method app::Util::index" => references => "class app::Foo");
        assert_no_edge!(project, "function wrap" => references => "class T");
        assert_no_edge!(project, "function first" => references => "constant T");
        assert_edge!(project, "function build" => instantiates => "class build::Local");
        assert_no_edge!(project, "function other" => references => "class build::Local");
    }

    /// Upstream #2219 (`03123335`): a name passed as a value is what is in
    /// scope where it is written. A function nested in another function is
    /// reached only from inside it (httpx's `self._build_auth(auth)` passes
    /// its parameter, not a test's nested `auth`); a Python name the function
    /// around it binds is that local's (`auth_flow(self, request)` handing
    /// `request` on is not the package's `request()`); a pytest fixture is a
    /// test's parameter of its name in its own module or under its
    /// `conftest.py`. The `_run_both(func)` call half of the upstream test is
    /// a call, not a value: it lands with Python's bare-call rule (W4-PY-01).
    #[test]
    fn pr2219_a_name_passed_as_a_value_is_what_is_in_scope_where_it_is_written() {
        let project = Project::new()
            .file(
                "httpx/_client.py",
                "class Client:\n    @property\n    def auth(self):\n        return self._auth\n\n    def _build_auth(self, auth):\n        return auth\n\n    def set_auth(self, auth):\n        self._auth = self._build_auth(auth)\n",
            )
            .file("httpx/_api.py", "def request(method, url):\n    return method, url\n")
            .file(
                "httpx/_auth.py",
                "class Auth:\n    def auth_flow(self, request):\n        yield request\n\n    def sync_auth_flow(self, request):\n        flow = self.auth_flow(request)\n        return flow\n\n\nclass Recorder:\n    def request(self):\n        return None\n",
            )
            .file(
                "tests/test_auth.py",
                "def test_custom_auth():\n    def auth(request):\n        return request\n\n    return register(auth)\n\n\ndef test_basic_auth():\n    auth = (\"user\", \"pass\")\n    return register(auth)\n",
            )
            .file(
                "docs/example/test_order.py",
                "import pytest\n\n\n@pytest.fixture\ndef func():\n    return 1\n",
            )
            .file(
                "tests/test_coverage.py",
                "import pytest\n\n\ndef _run_both(func):\n    return func()\n\n\n@pytest.fixture(\n    scope=\"module\",\n)\ndef recipe():\n    return {}\n\n\ndef test_recipe(recipe):\n    return use(recipe)\n",
            )
            .index();

        let nested = "function test_custom_auth::auth";
        assert_no_edge!(project, "method Client::_build_auth" => references => nested);
        assert_no_edge!(project, "method Client::set_auth" => references => nested);
        assert_no_edge!(project, "function test_basic_auth" => references => nested);
        assert_edge!(project, "function test_custom_auth" => references => nested);
        assert_no_edge!(project, "method Auth::sync_auth_flow" => references => "function request");
        assert_edge!(project, "function test_recipe" => references => "function recipe @tests/test_coverage.py");
    }
    /// Upstream #2122 (`068c422e`) and #2204 (`e7be363f`): a script a bundler
    /// or minifier wrote (named `*.min.js` / `*-min.js`, or a few enormous
    /// lines of code) is generated, and no name written in another file
    /// resolves into it: jQuery's global `$(…)` never reaches a vendored
    /// bundle's one-letter helper. Calls inside a bundle still resolve, and
    /// an import still binds what it names.
    #[test]
    fn pr2204_no_name_from_another_file_resolves_into_a_minified_script() {
        let functions: Vec<String> = (0..120)
            .map(|i| format!("function f{i}(e,t){{return e+t+{i}}}"))
            .collect();
        let bundle = format!(
            "!function(){{{};function u(e){{return f1(e,2)}}}}();\n",
            functions.join(";")
        )
        .repeat(4);
        let project = Project::new()
            .file(
                "static/js/vendor.min.js",
                "function $(e){return document.querySelector(e)}function q(e){return $(e)}\n",
            )
            .file("static/js/bundle.js", &bundle)
            .file("static/js/lib-min.js", "export function debounce(e){return e}\n")
            .file(
                "static/js/app.js",
                "function init() {\n  $(\"#edit-name\").click(function () {});\n  $('#app');\n  u(\"x\");\n}\n",
            )
            .file(
                "src/main.js",
                "import { debounce } from '../static/js/lib-min.js';\n\nexport function run() {\n  return debounce(1);\n}\n",
            )
            .index();

        assert_no_edge!(project, "function init" => calls => "function $ @static/js/vendor.min.js");
        assert_no_edge!(project, "function init" => calls => "function u @static/js/bundle.js");
        assert_unresolved!(project, "function init" => calls => "$");
        assert_unresolved!(project, "function init" => calls => "u");
        assert_edge!(project, "function q" => calls => "function $ @static/js/vendor.min.js");
        assert_edge!(
            project,
            "function u @static/js/bundle.js:1" => calls => "function f1 @static/js/bundle.js:1"
        );
        assert_edge!(
            project,
            "function run" => calls => "function debounce @static/js/lib-min.js",
            resolved_by = "import"
        );
        let generated: Vec<(&str, bool)> = project
            .graph()
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.generated))
            .collect();
        assert_eq!(
            generated,
            [
                ("src/main.js", false),
                ("static/js/app.js", false),
                ("static/js/bundle.js", true),
                ("static/js/lib-min.js", true),
                ("static/js/vendor.min.js", true),
            ]
        );
    }
}

/// W4a: the JavaScript/TypeScript resolution family (plan section 3 "W4a").
/// `cargo test --test probes -- js::` runs them.
mod js {
    use super::support::{Graph, NodeRow, Project, assert_edge, assert_no_edge};

    /// The qualified names `calls` edges from any node of `file` reach,
    /// sorted (upstream's `callsFrom`).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "calls"
                    && graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.clone())
            .collect();
        targets.sort();
        targets
    }

    /// Upstream #2128 (`1ef1eb88`): calls in Vue, Svelte and Astro files
    /// follow the JS/TS rules, and "outside the repository" is a package the
    /// importing file's package.json chain (or a Deno import map) declares, a
    /// Node built-in, `@std/…` or a framework's virtual module. An alias the
    /// resolver cannot follow (`~/…`, SvelteKit's `$lib/…`) stays the
    /// project's, a `workspace:` dependency is in the repository, SFC files
    /// get the JS built-ins, and a file's own import of a built-in's name is
    /// the import.
    #[test]
    fn pr2128_sfc_calls_follow_the_js_rules_and_an_unknown_alias_stays_the_projects() {
        let project = Project::new()
            .file(
                "frontend/package.json",
                r#"{"name":"frontend","dependencies":{"vue-i18n":"*","dayjs":"*","@app/shared":"workspace:*"}}"#,
            )
            .file(
                "shared/index.ts",
                "export function formatUrl(u: string) { return u; }\n",
            )
            .file(
                "frontend/types/context.ts",
                "export interface ProvidersContext {\n  t: (key: string) => string;\n}\n",
            )
            .file(
                "frontend/test/mocks.ts",
                "export function dayjs() { return 0; }\n",
            )
            .file(
                "frontend/composables/store.ts",
                "export function useLabelStore() { return {}; }\n",
            )
            .file(
                "frontend/composables/use-labels.ts",
                "import { useLabelStore } from '~/composables/store';\nexport function useLabels() { return useLabelStore(); }\n",
            )
            .file(
                "frontend/pages/labels.vue",
                "<script setup lang=\"ts\">\nimport { useI18n } from 'vue-i18n';\nimport dayjs from 'dayjs';\nimport { useLabelStore } from '~/composables/store';\nimport { formatUrl } from '@app/shared';\nconst { t } = useI18n();\nconst url = formatUrl('/labels');\nconst title = t('labels.title');\nconst today = dayjs();\nconst store = useLabelStore();\n</script>\n<template><h1>{{ title }}</h1></template>\n",
            )
            .file(
                "docs/package.json",
                r#"{"name":"docs","devDependencies":{"@sveltejs/kit":"*"}}"#,
            )
            .file(
                "docs/src/lib/design.ts",
                "export function useDesignSystem() { return 1; }\n",
            )
            .file(
                "docs/src/lib/router.ts",
                "export function goto(path: string) { return path; }\n",
            )
            .file(
                "docs/src/lib/use.ts",
                "import { useDesignSystem } from '$lib/design';\nexport const current = () => useDesignSystem();\n",
            )
            .file(
                "docs/src/lib/instance.js",
                "export const DEV = true;\nif (DEV) {\n  const fetch = globalThis.fetch;\n  globalThis.fetch = async (info) => fetch(info);\n}\n",
            )
            .file("docs/src/lib/Map.svelte", "<div>map</div>\n")
            .file(
                "docs/src/routes/+page.svelte",
                "<script lang=\"ts\">\n  import Map from '../lib/Map.svelte';\n  import { useDesignSystem } from '$lib/design';\n  import { goto } from '$app/navigation';\n  const ds = useDesignSystem();\n  function go() { goto('/'); fetch('/api'); }\n</script>\n<button on:click={go}>{ds}</button>\n",
            )
            .file(
                "deno/deno.json",
                r#"{"imports":{"@std/assert":"jsr:@std/assert@^1","app/":"../src/"}}"#,
            )
            .file(
                "deno/test/assert.ts",
                "export function assertEquals(a: unknown, b: unknown) { return a === b; }\n",
            )
            .file(
                "deno/app.test.ts",
                "import { assertEquals } from '@std/assert';\nassertEquals(1, 1);\n",
            )
            .index();
        let graph = project.graph();

        let from_vue = calls_from(graph, "frontend/pages/labels.vue");
        assert!(
            !from_vue.iter().any(|q| q == "ProvidersContext::t"),
            "{from_vue:?}"
        );
        assert!(!from_vue.iter().any(|q| q == "dayjs"), "{from_vue:?}");
        assert!(
            from_vue.iter().any(|q| q == "useLabelStore"),
            "{from_vue:?}"
        );
        // A `workspace:*` dependency is in the repository.
        assert!(from_vue.iter().any(|q| q == "formatUrl"), "{from_vue:?}");
        let from_labels = calls_from(graph, "frontend/composables/use-labels.ts");
        assert!(
            from_labels.iter().any(|q| q == "useLabelStore"),
            "{from_labels:?}"
        );

        let from_svelte = calls_from(graph, "docs/src/routes/+page.svelte");
        assert!(
            from_svelte.iter().any(|q| q == "useDesignSystem"),
            "{from_svelte:?}"
        );
        // SvelteKit's `$app/…` is the framework's; `fetch` is a JS built-in.
        assert!(!from_svelte.iter().any(|q| q == "goto"), "{from_svelte:?}");
        assert!(!from_svelte.iter().any(|q| q == "fetch"), "{from_svelte:?}");
        let from_use = calls_from(graph, "docs/src/lib/use.ts");
        assert!(
            from_use.iter().any(|q| q == "useDesignSystem"),
            "{from_use:?}"
        );

        // Deno's import map says `@std/assert` comes from JSR.
        let from_deno = calls_from(graph, "deno/app.test.ts");
        assert!(
            !from_deno.iter().any(|q| q == "assertEquals"),
            "{from_deno:?}"
        );

        // A built-in's name imported from the project is the import.
        assert_edge!(project, "file:docs/src/routes/+page.svelte" => imports => "file:docs/src/lib/Map.svelte");
        assert_no_edge!(project, "file:docs/src/routes/+page.svelte" => imports => "file:docs/src/lib/router.ts");
    }

    /// The `file:name` of every non-`contains` edge target from the nodes
    /// `select` picks (upstream's `targetsFrom`).
    fn targets_from(
        graph: &Graph,
        select: impl Fn(&super::support::NodeRow) -> bool,
    ) -> Vec<String> {
        let mut targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains" && graph.node(&edge.source).is_some_and(&select))
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{}:{}", node.file_path, node.name))
            .collect();
        targets.sort();
        targets
    }

    /// Upstream #2162 (`d03e4dbf`): a top-level binding an ES module does not
    /// export is not reached from another file; every export form still is
    /// (`export { a as b }`, `export default x`, `export default { a, b }`),
    /// and so are a function assigned onto a prototype and classic scripts.
    #[test]
    fn pr2162_an_unexported_module_binding_is_not_reached_from_another_file() {
        let project = Project::new()
            .file("package.json", r#"{"name":"app","type":"module"}"#)
            .file(
                "src/manifest.js",
                "import { join } from 'node:path';\nfunction resolve(event) { return join(event); }\nexport function manifest() { return resolve('x'); }\n",
            )
            .file(
                "src/adapters.js",
                "import { a } from './a.js';\nfunction getAdapter(name) { return name; }\nexport default { getAdapter };\n",
            )
            .file(
                "src/listed.js",
                "import { a } from './a.js';\nfunction helper() { return 1; }\nexport { helper };\n",
            )
            .file(
                "src/proto.js",
                "import { a } from './a.js';\nfunction Params() {}\nconst prototype = Params.prototype;\nprototype.render = function render() { return ''; };\nexport default Params;\n",
            )
            .file("src/a.js", "export const a = 1;\n")
            .file(
                "src/use.js",
                "import { helper } from './listed.js';\nimport Params from './proto.js';\nexport async function handle(event) {\n  await resolve(event);\n  getAdapter('xhr');\n  helper();\n  new Params().render();\n}\n",
            )
            .file(
                "src/classic.js",
                "function legacy() { return 1; }\n",
            )
            .file(
                "src/old.js",
                "export function old() { return legacy(); }\n",
            )
            .index();
        let graph = project.graph();
        let from_handle = targets_from(graph, |node| {
            node.name == "handle" && node.file_path == "src/use.js"
        });
        assert!(
            !from_handle.iter().any(|t| t == "src/manifest.js:resolve"),
            "{from_handle:?}"
        );
        assert!(
            from_handle
                .iter()
                .any(|t| t == "src/adapters.js:getAdapter"),
            "{from_handle:?}"
        );
        assert!(
            from_handle.iter().any(|t| t == "src/listed.js:helper"),
            "{from_handle:?}"
        );
        assert!(
            from_handle.iter().any(|t| t == "src/proto.js:render"),
            "{from_handle:?}"
        );
        // A classic script (no import, no export) keeps its globals.
        let from_old = targets_from(graph, |node| node.name == "old");
        assert!(
            from_old.iter().any(|t| t == "src/classic.js:legacy"),
            "{from_old:?}"
        );
        // The module's own call still reaches it.
        assert_edge!(project, "function manifest" => calls => "function resolve @src/manifest.js");
    }

    /// Upstream #2227 (`3e6252f3`): what a Svelte instance `<script>` or a Vue
    /// `<script setup>` declares is the component's own; a type a
    /// `<script setup>` exports stays importable.
    #[test]
    fn pr2227_a_components_instance_script_declarations_are_its_own() {
        let project = Project::new()
            .file("package.json", r#"{"name":"docs","private":true}"#)
            .file(
                "src/lib/examples/attachment-group.svelte",
                "<script lang=\"ts\">\n\ttype Item = {\n\t\tname: string;\n\t};\n\tconst items: Item[] = [];\n</script>\n\n<div>{items.length}</div>\n",
            )
            .file(
                "src/lib/examples/item-demo.svelte",
                "<script lang=\"ts\">\n\timport * as Item from \"$lib/ui/item/index.js\";\n\tlet label: Item = null;\n</script>\n\n<Item.Root>{label}</Item.Root>\n",
            )
            .file(
                "src/components/TabItem.vue",
                "<script lang=\"ts\" setup>\nimport { computed } from \"vue\";\nconst isActive = computed(() => true);\n</script>\n\n<template><div v-if=\"isActive\" /></template>\n",
            )
            .file(
                "src/extensions/code-block.ts",
                "import { isActive } from \"@/tiptap/core\";\n\nexport function active(state: unknown) {\n  return isActive(state);\n}\n",
            )
            .file(
                "src/components/CrudTable.vue",
                "<script setup lang=\"ts\">\nexport interface TableConfig {\n  hideColumns: boolean;\n}\ndefineProps<{ config: TableConfig }>();\n</script>\n\n<template><table /></template>\n",
            )
            .file(
                "src/pages/GroupDataPage.vue",
                "<script setup lang=\"ts\">\nimport type { TableConfig } from \"~/components/CrudTable.vue\";\nconst config: TableConfig = { hideColumns: false };\n</script>\n\n<template><div>{{ config }}</div></template>\n",
            )
            .index();
        let graph = project.graph();
        let in_file =
            |file: &'static str| move |node: &super::support::NodeRow| node.file_path == file;
        let from_demo = targets_from(graph, in_file("src/lib/examples/item-demo.svelte"));
        assert!(
            !from_demo
                .iter()
                .any(|t| t == "src/lib/examples/attachment-group.svelte:Item"),
            "{from_demo:?}"
        );
        let from_block = targets_from(graph, in_file("src/extensions/code-block.ts"));
        assert!(
            !from_block
                .iter()
                .any(|t| t == "src/components/TabItem.vue:isActive"),
            "{from_block:?}"
        );
        let from_page = targets_from(graph, in_file("src/pages/GroupDataPage.vue"));
        assert!(
            from_page
                .iter()
                .any(|t| t == "src/components/CrudTable.vue:TableConfig"),
            "{from_page:?}"
        );
    }

    /// Upstream #2226 (`83dac15c`): a JS/TS name the calling function binds
    /// itself (a parameter, or a `var` / `let` / `const` above the reference)
    /// is that local, never a same-named function declared elsewhere in the
    /// file; an unbound name and a `const`'s own recursion are left alone, and
    /// `if (handler) handler()` is no parameter list.
    #[test]
    fn pr2226_a_name_the_calling_function_binds_is_its_local() {
        let project = Project::new()
            .file(
                "lodash.js",
                "function runInContext(context) {\n  var baseCreate = (function() {\n    function object() {}\n    return function(proto) {\n      object.prototype = proto;\n      return new object;\n    };\n  }());\n\n  function baseHas(object, key) {\n    return object != null && hasOwnProperty.call(object, key);\n  }\n\n  function mixin(object, source) {\n    var result = object(this.__wrapped__);\n    return result;\n  }\n\n  function handler() {}\n\n  function run() {\n    if (handler) {\n      handler();\n    }\n    const walk = (node) => (node ? walk(node.next) : null);\n    return walk(context);\n  }\n\n  return { baseCreate, baseHas, mixin, run };\n}\n",
            )
            .index();
        let graph = project.graph();
        let named = |qualified: &'static str| {
            move |node: &super::support::NodeRow| node.qualified_name == qualified
        };
        let qualified_targets = |qualified: &'static str| -> Vec<String> {
            let mut targets: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| {
                    edge.kind != "contains"
                        && graph.node(&edge.source).is_some_and(named(qualified))
                })
                .filter_map(|edge| graph.node(&edge.target))
                .map(|node| node.qualified_name.clone())
                .collect();
            targets.sort();
            targets
        };
        let from_base_has = qualified_targets("runInContext::baseHas");
        assert!(
            !from_base_has.iter().any(|t| t == "runInContext::object"),
            "{from_base_has:?}"
        );
        let from_mixin = qualified_targets("runInContext::mixin");
        assert!(
            !from_mixin.iter().any(|t| t == "runInContext::object"),
            "{from_mixin:?}"
        );
        let from_run = qualified_targets("runInContext::run");
        assert!(
            from_run.iter().any(|t| t == "runInContext::handler"),
            "{from_run:?}"
        );
        let from_walk = qualified_targets("runInContext::run::walk");
        assert!(
            from_walk.iter().any(|t| t == "runInContext::run::walk"),
            "{from_walk:?}"
        );
    }

    /// Upstream #2163 (`cf449fda`): a bare call through a name destructured
    /// from a call's result reaches the function the callee returns under
    /// that key: one declared in the callee's body, else a module-level one
    /// it returns. A later declaration of the name at the call's scope
    /// shadows the binding.
    #[test]
    fn pr2163_a_name_destructured_from_a_call_reaches_what_the_callee_returns() {
        let project = Project::new()
            .file("package.json", r#"{"name":"app","dependencies":{"vue":"^3"}}"#)
            .file(
                "src/composables/use-default-activity.ts",
                "function getDefaultActivityRoute(key?: string): string {\n  return key ?? '/';\n}\nexport default function useDefaultActivity() {\n  return { getDefaultActivityRoute };\n}\n",
            )
            .file(
                "src/hooks/useAuth.ts",
                "export function useAuth() {\n  function login(user: string) {\n    return user;\n  }\n  const logout = () => null;\n  return { login, signOut: logout };\n}\n",
            )
            .file(
                "src/pages/index.ts",
                "import useDefaultActivity from '../composables/use-default-activity';\nimport { useAuth } from '../hooks/useAuth';\nexport function go() {\n  const { getDefaultActivityRoute } = useDefaultActivity();\n  const { login, signOut: leave } = useAuth();\n  login('ada');\n  leave();\n  return getDefaultActivityRoute('x');\n}\nexport function shadowed() {\n  const { login } = useAuth();\n  {\n    const login = (name: string) => name;\n    return login('bob');\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let from_go = targets_from(graph, |node| node.name == "go");
        assert!(
            from_go
                .iter()
                .any(|t| t == "src/composables/use-default-activity.ts:getDefaultActivityRoute"),
            "{from_go:?}"
        );
        assert!(
            from_go.iter().any(|t| t == "src/hooks/useAuth.ts:login"),
            "{from_go:?}"
        );
        let from_shadowed = targets_from(graph, |node| node.name == "shadowed");
        assert!(
            !from_shadowed
                .iter()
                .any(|t| t == "src/hooks/useAuth.ts:login"),
            "{from_shadowed:?}"
        );
    }

    /// Upstream #2357 (`8998697f`) (a): a destructuring declaration at module
    /// or `<script setup>` scope mints no symbol, yet its initializer runs:
    /// `const { a } = useFoo(1)` calls `useFoo` from the file, or from the
    /// component a `<script setup>` folds into.
    #[test]
    fn pr2357_module_scope_destructuring_calls_its_initializer() {
        let project = Project::new()
            .file("package.json", r#"{"name":"app","dependencies":{"vue":"^3"}}"#)
            .file(
                "src/composables/useFoo.ts",
                "export function useFoo(n: number) {\n  return { a: n };\n}\n",
            )
            .file(
                "src/app.ts",
                "import { useFoo } from './composables/useFoo';\nconst { a } = useFoo(1);\nexport const value = a;\n",
            )
            .file(
                "src/components/Panel.vue",
                "<script setup lang=\"ts\">\nimport { useFoo } from '../composables/useFoo';\nconst { a } = useFoo(2);\n</script>\n<template><p>{{ a }}</p></template>\n",
            )
            .index();
        assert_edge!(project, "file:src/app.ts" => calls => "function useFoo");
        assert_edge!(project, "component src/components/Panel.vue::Panel" => calls => "function useFoo");
    }

    /// Upstream #2357 (d): a file's own function shadows a Nuxt auto-import
    /// of its name: elk's lists page calls its own `clearError(true)`, not
    /// Nuxt's, and the call is no edge from the caller to itself.
    #[test]
    fn pr2357_a_files_own_function_shadows_a_nuxt_auto_import() {
        let project = Project::new()
            .file("package.json", r#"{"name":"app","dependencies":{"nuxt":"^3","vue":"^3"}}"#)
            .file("nuxt.config.ts", "export default defineNuxtConfig({});\n")
            .file(
                "pages/lists.vue",
                "<script setup lang=\"ts\">\nfunction clearError(reset: boolean) {\n  return reset;\n}\nfunction retry() {\n  return clearError(true);\n}\n</script>\n<template><button @click=\"retry\">retry</button></template>\n",
            )
            .index();
        assert_edge!(project, "function retry" => calls => "function clearError @pages/lists.vue");
        assert_no_edge!(project, "function retry" => calls => "function retry");
    }

    /// The distinct files an `imports` edge from any node of `file` reaches,
    /// other than `file` itself.
    fn imported_files(graph: &Graph, file: &str) -> Vec<String> {
        let mut files: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "imports"
                    && graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.file_path.clone())
            .filter(|path| path != file)
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// `kind file:name` of the calls and instantiations out of `file`'s nodes.
    fn out_of(graph: &Graph, file: &str) -> Vec<String> {
        let mut edges: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                (edge.kind == "calls" || edge.kind == "instantiates")
                    && graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| {
                graph
                    .node(&edge.target)
                    .map(|target| format!("{} {}:{}", edge.kind, target.file_path, target.name))
            })
            .collect();
        edges.sort();
        edges
    }

    /// `kind source-file resolvedBy` of the edges of `kinds` into the node of
    /// `file` named `name` (an import node aside).
    fn into(graph: &Graph, file: &str, name: &str, kinds: &[&str]) -> Vec<String> {
        let target = graph
            .nodes
            .iter()
            .find(|node| node.file_path == file && node.name == name && node.kind != "import")
            .unwrap_or_else(|| panic!("{file}: {name}"));
        let mut edges: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.target == target.id && kinds.contains(&edge.kind.as_str()))
            .map(|edge| {
                format!(
                    "{} {} {}",
                    edge.kind,
                    graph
                        .node(&edge.source)
                        .map_or("?", |node| node.file_path.as_str()),
                    edge.resolved_by().unwrap_or("?")
                )
            })
            .collect();
        edges.sort();
        edges
    }

    /// Upstream #2263 (`c4babfce`), its extraction half: a CommonJS
    /// `require('./x')` imports the file it names, as ESM's `import` does,
    /// used as a value or not; a package stays outside the index, and a
    /// directory require reaches its package entry (with #2189's root
    /// `index.js`).
    #[test]
    fn pr2263_a_commonjs_require_imports_the_file_it_names() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"main\":\"index.js\"}")
            .file("index.js", "module.exports = require('./lib/express');\n")
            .file("lib/express.js", "var EventEmitter = require('events').EventEmitter;\nvar mixin = require('merge-descriptors');\nvar proto = require('./application');\nvar req = require('./request');\n\nfunction createApplication() {\n  var app = function () {};\n  mixin(app, EventEmitter.prototype, false);\n  mixin(app, proto, false);\n  app.request = Object.create(req);\n  return app;\n}\n\nmodule.exports = createApplication;\n")
            .file("lib/application.js", "var app = exports = module.exports = {};\napp.init = function init() {};\n")
            .file("lib/request.js", "var req = Object.create({});\nmodule.exports = req;\n")
            .file("test/app.js", "var express = require('..');\nvar assert = require(\"assert\");\n")
            .file("src/main.ts", "import './polyfills';\nimport helper = require('./helper');\nexport const value = helper.run();\n")
            .file("src/polyfills.ts", "export {};\n")
            .file("src/helper.ts", "export function run(): number {\n  return 1;\n}\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            imported_files(graph, "lib/express.js"),
            ["lib/application.js", "lib/request.js"]
        );
        assert_eq!(imported_files(graph, "index.js"), ["lib/express.js"]);
        assert_eq!(imported_files(graph, "test/app.js"), ["index.js"]);
        assert!(
            imported_files(graph, "src/main.ts")
                .iter()
                .any(|f| f == "src/polyfills.ts")
        );
        let to_helper = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "imports"
                    && graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == "src/main.ts")
                    && graph.node(&edge.target).is_some_and(|node| {
                        node.file_path == "src/helper.ts" && node.kind == "file"
                    })
            })
            .count();
        assert!(to_helper <= 1, "{to_helper}");
    }

    /// Upstream #2189 (`e77ca836`): calls through CommonJS `require` bindings
    /// follow `module.exports` through the root `index.js` to the exported
    /// function, read every declarator of a list, take `require(…).member` as
    /// that member, and leave a package's call without an edge.
    #[test]
    fn pr2189_calls_through_commonjs_require_bindings_reach_the_right_function() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"main\":\"index.js\",\"devDependencies\":{\"supertest\":\"^6\"}}")
            .file("index.js", "'use strict';\n\nmodule.exports = require('./lib/express');\n")
            .file("lib/express.js", "'use strict';\n\nexports = module.exports = createApplication;\n\nfunction createApplication() {\n  return {};\n}\n")
            .file("lib/utils.js", "'use strict';\n\nexports.normalizeType = function (type) {\n  return type;\n};\n\nexports.setCharset = function setCharset(type, charset) {\n  return type + charset;\n};\n")
            .file("lib/response.js", "'use strict';\n\nvar normalizeType = require('./utils').normalizeType;\nvar setCharset = require('./utils').setCharset;\n\nfunction send(body) {\n  return setCharset(normalizeType(body), 'utf-8');\n}\n\nmodule.exports = { send };\n")
            .file("test/app.js", "'use strict';\n\nvar express = require('../')\n  , request = require('supertest');\n\ndescribe('app', function () {\n  it('responds', function () {\n    var app = express();\n    request(app);\n  });\n});\n")
            .index();
        let graph = project.graph();
        let calls_out = |file: &str| -> Vec<String> {
            let mut calls: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| {
                    edge.kind == "calls"
                        && graph
                            .node(&edge.source)
                            .is_some_and(|node| node.file_path == file)
                })
                .filter_map(|edge| graph.node(&edge.target))
                .map(|node| format!("{}: {}", node.file_path, node.qualified_name))
                .collect();
            calls.sort();
            calls
        };
        assert_eq!(
            calls_out("test/app.js"),
            ["lib/express.js: createApplication"]
        );
        assert_eq!(
            calls_out("lib/response.js"),
            ["lib/utils.js: normalizeType", "lib/utils.js: setCharset"]
        );
    }

    /// Upstream #2433 (`9a1d4bd9`): `require('./x').default` and
    /// `const { default: X } = require('./x')` read the module's `default`
    /// property: what `exports.default` names in a CommonJS module, else an
    /// ES module's default export (in the module asked for only), and nothing
    /// for a CommonJS module that sets no `default`.
    #[test]
    fn pr2433_require_default_reaches_the_modules_default_export() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"desktop\",\"devDependencies\":{\"svelte\":\"^4\"}}")
            .file("src/biometrics/os-biometrics.service.ts", "export interface OsBiometricService {\n  enrollPersistent(userId: string): Promise<string>;\n}\n")
            .file("src/biometrics/os-biometrics-mac.service.ts", "import { OsBiometricService } from './os-biometrics.service';\n\nfunction getLookupKeyForUser(userId: string): string {\n  return `${userId}_user_biometric`;\n}\n\nexport default class OsBiometricsServiceMac implements OsBiometricService {\n  async enrollPersistent(userId: string): Promise<string> {\n    return getLookupKeyForUser(userId);\n  }\n}\n")
            .file("src/biometrics/main-biometrics.service.ts", "import { OsBiometricService } from './os-biometrics.service';\n\nexport class MainBiometricsService {\n  private osBiometricsService: OsBiometricService | undefined;\n\n  constructor(platform: string) {\n    if (platform === 'darwin') {\n      // eslint-disable-next-line\n      const OsBiometricsServiceMac = require('./os-biometrics-mac.service').default;\n      this.osBiometricsService = new OsBiometricsServiceMac();\n    }\n  }\n}\n")
            .file("src/biometrics/os-biometrics-mac.service.spec.ts", "describe('OsBiometricsServiceMac', () => {\n  it('enrolls in an isolated module registry', async () => {\n    const { default: ServiceCtor } = require('./os-biometrics-mac.service');\n    const isolated = new ServiceCtor();\n    await isolated.enrollPersistent('user');\n  });\n});\n")
            .file("src/App.svelte", "<script>\n  export let name = 'world';\n</script>\n\n<h1>Hello {name}!</h1>\n")
            .file("src/main.js", "const App = require('./App.svelte').default;\n\nconst app = new App({ target: document.body });\n\nmodule.exports = app;\n")
            .file("lib/format.js", "'use strict';\n\nexports.pad = function (value) {\n  return String(value).padStart(2, '0');\n};\n\nfunction formatDate(date) {\n  return exports.pad(date.getDate());\n}\n\nexports.default = formatDate;\n")
            .file("lib/server.js", "'use strict';\n\nfunction createServer(options) {\n  return { options };\n}\n\nmodule.exports = createServer;\nmodule.exports.default = createServer;\nmodule.exports.createServer = createServer;\n")
            .file("lib/plain.js", "'use strict';\n\nexports.first = function () {\n  return 1;\n};\n\nexports.second = () => 2;\n")
            .file("src/widgets/Dial.ts", "export const DIAL_SIZE = 3;\n\nexport default class Dial {\n  size = DIAL_SIZE;\n}\n")
            .file("src/widgets/index.ts", "export * from './Dial';\n")
            .file("src/dashboard.ts", "export function mountDashboard(): unknown {\n  const Widget = require('./widgets').default;\n  return new Widget();\n}\n")
            .file("lib/app.js", "'use strict';\n\nconst format = require('./format').default;\nconst build = require('./server').default;\nconst { default: fastify } = require('./server');\nconst plain = require('./plain').default;\n\nfunction start() {\n  const server = build({ port: 3000 });\n  fastify({ logger: true });\n  plain();\n  return format(new Date()) + server.options.port;\n}\n\nmodule.exports = start;\n")
            .file("py/__init__.py", "")
            .file("py/mod.py", "def first():\n    return 1\n")
            .file("py/use.py", "from .mod import default\n\n\ndef run():\n    return default()\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            into(
                graph,
                "src/biometrics/os-biometrics-mac.service.ts",
                "OsBiometricsServiceMac",
                &["instantiates"]
            ),
            [
                "instantiates src/biometrics/main-biometrics.service.ts import",
                "instantiates src/biometrics/os-biometrics-mac.service.spec.ts import",
            ]
        );
        assert_eq!(
            into(graph, "src/App.svelte", "App", &["instantiates"]),
            ["instantiates src/main.js import"]
        );
        assert!(
            out_of(graph, "lib/app.js")
                .iter()
                .any(|e| e == "calls lib/format.js:formatDate")
        );
        assert!(into(graph, "lib/format.js", "pad", &["calls"]).is_empty());
        assert_eq!(
            into(graph, "lib/server.js", "createServer", &["calls"]),
            ["calls lib/app.js import", "calls lib/app.js import"]
        );
        assert!(into(graph, "lib/plain.js", "first", &["calls"]).is_empty());
        assert!(into(graph, "lib/plain.js", "second", &["calls"]).is_empty());
        assert!(
            into(
                graph,
                "src/widgets/Dial.ts",
                "Dial",
                &["instantiates", "calls"]
            )
            .is_empty()
        );
        assert!(
            out_of(graph, "src/dashboard.ts").is_empty(),
            "{:?}",
            out_of(graph, "src/dashboard.ts")
        );
        let foreign: Vec<String> = out_of(graph, "lib/app.js")
            .into_iter()
            .filter(|edge| !edge.contains(" lib/app.js:"))
            .collect();
        assert_eq!(
            foreign,
            [
                "calls lib/format.js:formatDate",
                "calls lib/server.js:createServer",
                "calls lib/server.js:createServer",
            ]
        );
        assert!(into(graph, "py/mod.py", "first", &["calls"]).is_empty());
    }

    /// Upstream #2192 (`7c1c9a36`): `export * as ns from './x'` exports the
    /// namespace, not every member: a later `export *` supplies `z.number()`,
    /// `z.core.safeParse()` continues in the namespaced module, and a member a
    /// barrel forwards by name is found for a namespace import.
    #[test]
    fn pr2192_a_namespace_reexport_exports_the_namespace_only() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"schema\",\"private\":true}")
            .file("src/core/regexes.ts", "export const number = /^-?\\d+$/;\n")
            .file("src/core/parse.ts", "export function safeParse(value: unknown) {\n  return { success: true, data: value };\n}\n")
            .file("src/core/api.ts", "export function _lt(limit: number) {\n  return limit;\n}\n")
            .file("src/core/index.ts", "export * as regexes from \"./regexes.js\";\nexport * from \"./parse.js\";\nexport * from \"./api.js\";\n")
            .file("src/classic/schemas.ts", "export function number() {\n  return { kind: \"number\" };\n}\n")
            .file("src/classic/external.ts", "export * as core from \"../core/index.js\";\nexport * from \"./schemas.js\";\nexport { _lt as lt } from \"../core/index.js\";\n")
            .file("src/mini/parse.ts", "export { safeParse } from \"../core/index.js\";\n")
            .file("src/mini/external.ts", "export * as core from \"../core/index.js\";\nexport * from \"./parse.js\";\n")
            .file("src/tests/classic.test.ts", "import * as z from \"../classic/external.js\";\n\nexport function classic() {\n  z.number();\n  z.lt(10);\n  z.core.safeParse(1);\n}\n")
            .file("src/tests/mini.test.ts", "import * as m from \"../mini/external.js\";\n\nexport function mini() {\n  m.safeParse(1);\n}\n")
            .index();
        let graph = project.graph();
        let calls_out = |file: &str| -> Vec<String> {
            let mut calls: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| {
                    edge.kind == "calls"
                        && graph
                            .node(&edge.source)
                            .is_some_and(|node| node.file_path == file)
                })
                .filter_map(|edge| graph.node(&edge.target))
                .map(|node| format!("{}: {}", node.file_path, node.qualified_name))
                .collect();
            calls.sort();
            calls
        };
        assert_eq!(
            calls_out("src/tests/classic.test.ts"),
            [
                "src/classic/schemas.ts: number",
                "src/core/api.ts: _lt",
                "src/core/parse.ts: safeParse",
            ]
        );
        assert_eq!(
            calls_out("src/tests/mini.test.ts"),
            ["src/core/parse.ts: safeParse"]
        );
    }

    /// Upstream #2412 (`f261fda5`): `import { default as X }` is the module's
    /// default import, so `AppRoot()` reaches the module's default export,
    /// not another file's `AppRoot`, and the binding's import links the module
    /// file when its default has no symbol.
    #[test]
    fn pr2412_import_default_as_is_the_default_import() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"dependencies\":{\"react\":\"^18\"}}")
            .file("src/app/routes/app/root.tsx", "export const ErrorBoundary = () => {\n  return <div>Something went wrong!</div>;\n};\n\nconst AppRoot = () => {\n  return <main />;\n};\n\nexport default AppRoot;\n")
            .file("src/config.ts", "export default { theme: 'dark' };\n")
            .file(
                "src/app.ts",
                "import { default as config } from './config';\n\nexport const theme = config.theme;\n",
            )
            .file("src/legacy/shell.tsx", "export function AppRoot() {\n  return <section />;\n}\n")
            .file("src/app/router.tsx", "import { default as AppRoot, ErrorBoundary as AppRootErrorBoundary } from './routes/app/root';\n\nexport function render() {\n  AppRootErrorBoundary();\n  return AppRoot();\n}\n")
            .index();
        assert_edge!(project, "function render" => calls => "function AppRoot @src/app/routes/app/root.tsx", resolved_by = "import");
        assert_no_edge!(project, "function render" => calls => "function AppRoot @src/legacy/shell.tsx");
        assert_edge!(project, "function render" => calls => "function ErrorBoundary @src/app/routes/app/root.tsx");
        // A default export with no symbol of its own still links the module
        // file through the binding's import, as `import config from` does.
        let graph = project.graph();
        let parked: Vec<&str> = graph
            .unresolved
            .iter()
            .filter(|reference| {
                reference.file_path == "src/app.ts" && reference.reference_kind == "imports"
            })
            .map(|reference| reference.reference_name.as_str())
            .collect();
        assert!(!parked.contains(&"config"), "{parked:?}");
    }

    /// Upstream #2378 (`05712869`): an import binding whose name contains `$`
    /// maps like any other (a default `$fmt`, a renamed `$alias`).
    #[test]
    fn pr2378_a_dollar_import_binding_is_an_import() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"private\":true}")
            .file("src/fmt.ts", "export default function $fmt(value: string) {\n  return value;\n}\n")
            .file("src/other.ts", "export function $fmt(value: string) {\n  return value + '!';\n}\n")
            .file("src/use.ts", "import $fmt from './fmt';\nimport { $fmt as $alias } from './other';\n\nexport function show(value: string) {\n  return $fmt(value) + $alias(value);\n}\n\nexport function shadow($fmt: (v: string) => string) {\n  return $fmt('x');\n}\n")
            .index();
        assert_edge!(project, "function show" => calls => "function $fmt @src/fmt.ts", resolved_by = "import");
        assert_edge!(project, "function show" => calls => "function $fmt @src/other.ts", resolved_by = "import");
    }

    /// Upstream #2384 (`5f5b8db6`): a store exported by an `export { … }` list
    /// owns its actions when its name ends with `$`; `export { useStore$ }`
    /// does not export a store named `useStore`.
    #[test]
    fn pr2384_an_export_list_exports_a_dollar_named_store() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"dependencies\":{\"zustand\":\"^4\"}}")
            .file("src/items.ts", "import { create } from 'zustand';\n\nconst items$ = create((set) => ({\n  inc: () => set({}),\n}));\n\nconst useStore = create((set) => ({\n  reset: () => set({}),\n}));\n\nexport { items$ };\nexport { useStore$ };\n")
            .index();
        let graph = project.graph();
        let names: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == "src/items.ts")
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"items$::inc"), "{names:?}");
        assert!(!names.contains(&"useStore::reset"), "{names:?}");
    }

    /// Upstream #2378 / #2384's identifier grammar in the local alias forms:
    /// `export default $fmt;`, `export default items$;` and `export const
    /// $make = make$;` bind the declaration they name, so a default import
    /// reaches that function or constant rather than the module's first
    /// exported function. `items$` is not its namesake `items`, the default
    /// is never another module's `$fmt`, and a function binding `$fmt`
    /// itself calls its local.
    #[test]
    fn pr2378_a_dollar_default_or_const_alias_binds_its_declaration() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"web\",\"private\":true}")
            .file("src/fmt.ts", "export function helper(value: string) {\n  return value;\n}\n\nconst $fmt = (value: string) => value + '!';\n\nexport default $fmt;\n")
            .file("src/items.ts", "export function reset() {\n  return 0;\n}\n\nexport const items = { grow() { return 0; } };\n\nconst items$ = { grow() { return 1; } };\n\nexport default items$;\n")
            .file("src/alias.ts", "export function other() {\n  return 2;\n}\n\nfunction make$() {\n  return 1;\n}\n\nexport const $make = make$;\n")
            .file("src/other.ts", "export function $fmt(value: string) {\n  return value + '?';\n}\n")
            .file("src/use.ts", "import fmt from './fmt';\nimport store from './items';\nimport { $make } from './alias';\n\nexport function show(value: string) {\n  return fmt(value) + $make() + store.grow();\n}\n\nexport function local(value: string) {\n  const $fmt = (v: string) => v;\n  return $fmt(value);\n}\n")
            .index();
        assert_edge!(project, "function show" => calls => "function $fmt @src/fmt.ts", resolved_by = "import");
        assert_no_edge!(project, "function show" => calls => "function helper");
        assert_no_edge!(project, "function show" => calls => "function $fmt @src/other.ts");
        assert_edge!(project, "function show" => calls => "function make$ @src/alias.ts");
        assert_no_edge!(project, "function show" => calls => "function other");
        assert_edge!(project, "function show" => calls => "function items$::grow @src/items.ts", resolved_by = "import");
        assert_no_edge!(project, "function show" => calls => "function items::grow");
        assert_no_edge!(project, "function show" => calls => "function reset");
        assert_no_edge!(project, "function local" => calls => "function $fmt @src/fmt.ts");
        assert_no_edge!(project, "function local" => calls => "function $fmt @src/other.ts");
    }

    /// Upstream #2161 (`2d1b6441`): a `.js` whose leading comments carry
    /// Flow's `@flow` pragma is read as TSX once Flow's own syntax is blanked,
    /// so `render(): React.Node` no longer cuts its class short, and its file
    /// row records `tsx`; a `.js` without the pragma, or with `@noflow`,
    /// stays JavaScript.
    #[test]
    fn pr2161_a_flow_typed_javascript_file_is_read_as_tsx() {
        let project = Project::new()
            .file("js/SegmentedControl.js", "/**\n * @flow strict-local\n * @format\n */\nimport typeof * as React from 'react';\nimport * as React from 'react';\n\ntype Props = $ReadOnly<{|\n  values: ?Array<string>,\n  onChange?: ?(event: {nativeEvent: {value: string}, ...}) => mixed,\n|}>;\n\nopaque type Token = string;\n\nclass SegmentedControl extends React.Component<Props> {\n  render(): React.Node {\n    return this._renderItems();\n  }\n\n  _renderItems(): React.Node {\n    return null;\n  }\n}\n\nmodule.exports = SegmentedControl;\n")
            .file("js/plain.js", "function plain() { return 1; }\nmodule.exports = plain;\n")
            .file("js/noflow.js", "// @noflow\nfunction off() { return 1; }\nmodule.exports = off;\n")
            .index();
        let graph = project.graph();
        let language = |path: &str| {
            graph
                .files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.language.as_str())
        };
        assert_eq!(language("js/SegmentedControl.js"), Some("tsx"));
        assert_eq!(language("js/plain.js"), Some("javascript"));
        assert_eq!(language("js/noflow.js"), Some("javascript"));
        let mut methods: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == "js/SegmentedControl.js" && node.kind == "method")
            .map(|node| node.name.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(methods, ["_renderItems", "render"], "{}", graph.report());
        assert_edge!(project, "method SegmentedControl::render" => calls => "method SegmentedControl::_renderItems");
    }

    /// The one node with this qualified name (in `file`, when given).
    #[track_caller]
    fn the_node<'g>(graph: &'g Graph, qualified: &str, file: Option<&str>) -> &'g NodeRow {
        let found: Vec<&NodeRow> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.qualified_name == qualified && file.is_none_or(|file| node.file_path == file)
            })
            .collect();
        assert_eq!(found.len(), 1, "{qualified}:\n{}", graph.report());
        found[0]
    }

    /// The qualified names at the other end of a node's `calls` edges, sorted.
    fn calls_of(graph: &Graph, id: &str, incoming: bool) -> Vec<String> {
        let mut names: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "calls"
                    && if incoming {
                        edge.target == id
                    } else {
                        edge.source == id
                    }
            })
            .filter_map(|edge| graph.node(if incoming { &edge.source } else { &edge.target }))
            .map(|node| node.qualified_name.clone())
            .collect();
        names.sort();
        names
    }

    #[track_caller]
    fn callers_of(graph: &Graph, qualified: &str) -> Vec<String> {
        calls_of(graph, &the_node(graph, qualified, None).id, true)
    }

    #[track_caller]
    fn callees_of(graph: &Graph, qualified: &str) -> Vec<String> {
        calls_of(graph, &the_node(graph, qualified, None).id, false)
    }

    /// Upstream #2363 (`f40db4b9`, issue #2300): a named object literal owns
    /// its function members whether it is exported or not, declared in an
    /// IIFE, or hung on the global object, and the calls written in a member
    /// are the member's; `window.WS.wsM()` and another classic script's
    /// `WS.wsM()` reach `window.WS::wsM`.
    #[test]
    fn pr2363_every_named_literal_owns_its_function_members() {
        let project = Project::new()
            .file("src/a_export.ts", "export const objTsExp = { tsExpM() { return 1; }, tsExpProp: () => 1 };\n")
            .file("src/b_plain.ts", "const objTsPlain = { tsPlainM() { return 1; }, tsPlainProp: () => 1 };\n")
            .file("src/c_export.js", "export const objJsExp = { jsExpM() { return 1; }, jsExpProp: () => 1 };\n")
            .file("src/d_plain.js", "const objJsPlain = { jsPlainM() { return 1; }, jsPlainProp: () => 1 };\n")
            .file("src/e_iife.js", "(function () {\n  const objIife = { iifeM() { return 1; } };\n  window.WS = { wsM() { return objIife.iifeM(); } };\n})();\n")
            .file("src/f_calls.js", "function helper() { return 2; }\nvar store = { shorthand() { return helper(); } };\nfunction useStore() { return store.shorthand(); }\nwindow.api = { load() { return store.shorthand(); } };\nfunction viaWindow() { return window.WS.wsM(); }\nfunction viaGlobal() { return WS.wsM(); }\n")
            .index();
        let graph = project.graph();
        for qualified in [
            "objTsExp::tsExpM",
            "objTsExp::tsExpProp",
            "objTsPlain::tsPlainM",
            "objTsPlain::tsPlainProp",
            "objJsExp::jsExpM",
            "objJsExp::jsExpProp",
            "objJsPlain::jsPlainM",
            "objJsPlain::jsPlainProp",
            "objIife::iifeM",
            "window.WS::wsM",
            "store::shorthand",
            "window.api::load",
        ] {
            assert_eq!(
                the_node(graph, qualified, None).kind,
                "function",
                "{qualified}"
            );
        }
        let plain = the_node(graph, "objTsPlain", None);
        let mut members: Vec<&str> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "contains" && edge.source == plain.id)
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.name.as_str())
            .collect();
        members.sort_unstable();
        assert_eq!(members, ["tsPlainM", "tsPlainProp"]);
        let ws: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.name == "WS")
            .map(|node| format!("{} {}", node.kind, node.qualified_name))
            .collect();
        assert_eq!(ws, ["variable window.WS"]);
        // An exported literal's members stay exported, a plain one's do not.
        assert!(the_node(graph, "objTsExp::tsExpM", None).is_exported);
        assert!(!the_node(graph, "objTsPlain::tsPlainM", None).is_exported);
        // The call inside a member is the member's, not the constant's.
        assert_eq!(callers_of(graph, "helper"), ["store::shorthand"]);
        assert_eq!(callers_of(graph, "objIife::iifeM"), ["window.WS::wsM"]);
        assert_eq!(
            callers_of(graph, "store::shorthand"),
            ["useStore", "window.api::load"]
        );
        assert_eq!(
            callers_of(graph, "window.WS::wsM"),
            ["viaGlobal", "viaWindow"]
        );
    }

    /// Upstream #2363: a member is reached through the object a call names
    /// (`App.init()`, `window.App.render()`, `App.utils.pad()`, a sibling's
    /// `this.render()`), from another classic script too, and never by its
    /// bare name; a parameter named like the global shadows it.
    #[test]
    fn pr2363_a_member_is_reached_through_its_object_never_a_bare_name() {
        let project = Project::new()
            .file("app.js", "function init() { return \"global\"; }\nvar App = {\n  init: function () { this.render(); helpers.fmt(); },\n  render: function () { return 1; },\n};\nvar helpers = { fmt() { return 2; } };\nApp.utils = { pad(s) { return s; } };\nfunction boot() { App.init(); App.utils.pad(\"x\"); init(); }\ndw_page = { start() { App.render(); } };\n")
            .file("page.js", "function main() { App.init(); window.App.render(); dw_page.start(); App.utils.pad(\"y\"); }\nfunction shadow(App) { return App.init(); }\nfunction bare() { return render(); }\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            callees_of(graph, "boot"),
            ["App.utils::pad", "App::init", "init"]
        );
        assert_eq!(
            callees_of(graph, "App::init"),
            ["App::render", "helpers::fmt"]
        );
        assert_eq!(callees_of(graph, "dw_page::start"), ["App::render"]);
        assert_eq!(
            callees_of(graph, "main"),
            [
                "App.utils::pad",
                "App::init",
                "App::render",
                "dw_page::start"
            ]
        );
        assert_eq!(callees_of(graph, "shadow"), Vec::<String>::new());
        assert_eq!(callees_of(graph, "bare"), Vec::<String>::new());
        assert_eq!(callers_of(graph, "init"), ["boot"]);
    }

    /// Upstream #2363: an object hung on a path (`$.event.special.swipe =
    /// {…}`) or local to an IIFE is no name elsewhere: `this.swipe()` and
    /// `new this.model()` are not those objects, and a plain script's `$n` is
    /// its own function.
    #[test]
    fn pr2363_a_path_or_iife_object_is_no_name_elsewhere() {
        let project = Project::new()
            .file("lib.js", "$.event.special.swipe = { setup: function () { return 1; } };\ntodos.model = { add: function () { return 2; } };\n(function () { var n = { touch: function () { return 3; } }; n.touch(); })();\n")
            .file("widget.js", "var Widget = {\n  drag: function () { this.swipe(1); },\n  make: function () { return new this.model(); },\n};\n")
            .file("gwt1.js", "function $n(a) { this.a = a; }\nfunction make1() { return new $n(1); }\n")
            .file("gwt2.js", "function $n(a) { this.a = a; }\nfunction make2() { return new $n(2); }\n")
            .index();
        let graph = project.graph();
        let edges = |qualified: &str| -> Vec<String> {
            let id = &the_node(graph, qualified, None).id;
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| &edge.source == id && edge.kind != "contains")
                .filter_map(|edge| {
                    graph.node(&edge.target).map(|target| {
                        format!(
                            "{} {} {}",
                            edge.kind, target.qualified_name, target.file_path
                        )
                    })
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            the_node(graph, "$.event.special.swipe::setup", None).kind,
            "function"
        );
        assert_eq!(the_node(graph, "n::touch", None).kind, "function");
        assert_eq!(callers_of(graph, "n::touch"), ["lib.js"]);
        assert_eq!(edges("Widget::drag"), Vec::<String>::new());
        assert_eq!(edges("Widget::make"), Vec::<String>::new());
        assert_eq!(edges("make1"), ["instantiates $n gwt1.js"]);
        assert_eq!(edges("make2"), ["instantiates $n gwt2.js"]);
    }

    /// Upstream #2363: each IIFE and function keeps its own literal, and a
    /// module its private one; a call outside them reaches none.
    #[test]
    fn pr2363_each_iife_and_function_keeps_its_own_literal() {
        let project = Project::new()
            .file("iife.js", "(function () { const Api = { run() { left(); } }; Api.run(); })();\n(function () { const Api = { run() { right(); } }; Api.run(); })();\nfunction left() {}\nfunction right() {}\nfunction outside() { return Api.run(); }\nfunction a() { const local = { go() { left(); } }; local.go(); }\nfunction b() { const local = { go() { right(); } }; local.go(); }\n")
            .file("mod.js", "import { x } from \"./x.js\";\nconst Local = { run() { return x; } };\nexport function useLocal() { return Local.run(); }\n")
            .file("other.js", "import { y } from \"./y.js\";\nexport function nope() { return Local.run(y); }\n")
            .index();
        let graph = project.graph();
        let mut runs: Vec<&NodeRow> = graph
            .nodes
            .iter()
            .filter(|node| node.qualified_name == "Api::run")
            .collect();
        runs.sort_by_key(|node| node.start_line);
        assert_eq!(runs.len(), 2, "{}", graph.report());
        assert_eq!(calls_of(graph, &runs[0].id, false), ["left"]);
        assert_eq!(calls_of(graph, &runs[1].id, false), ["right"]);
        assert_eq!(calls_of(graph, &runs[0].id, true), ["iife.js"]);
        assert_eq!(calls_of(graph, &runs[1].id, true), ["iife.js"]);
        // Outside both IIFEs, `Api` is neither of them, not even as a value read.
        let outside = &the_node(graph, "outside", None).id;
        assert!(
            graph
                .edges
                .iter()
                .all(|edge| &edge.source != outside || edge.kind == "contains"),
            "{}",
            graph.report()
        );
        assert_eq!(callees_of(graph, "a"), ["a::local::go"]);
        assert_eq!(callees_of(graph, "b"), ["b::local::go"]);
        assert_eq!(callees_of(graph, "useLocal"), ["Local::run"]);
        assert_eq!(callees_of(graph, "nope"), Vec::<String>::new());
    }

    /// Upstream #2193 (`8410db42`), the part #2363's local owners rely on: a
    /// `const` declared in a function body is that body's alone, so a bare
    /// `handlers()` or `new handlers()` in another function of the file never
    /// reaches the multi-line function `a`'s local owner `a::handlers`, while
    /// `a`'s own `handlers.load()` still does (carried from W4-JS-07).
    #[test]
    fn pr2193_a_function_local_owner_is_no_name_in_another_function() {
        let project = Project::new()
            .file(
                "src/app.js",
                "function a() {\n  const handlers = {\n    load() { return 1; },\n  };\n  return handlers.load();\n}\n\nfunction b() {\n  return handlers();\n}\n\nfunction c() {\n  return new handlers();\n}\n",
            )
            .index();
        let graph = project.graph();
        for function in ["b", "c"] {
            let id = &the_node(graph, function, None).id;
            assert!(
                graph.edges.iter().all(|edge| &edge.source != id
                    || edge.kind == "contains"
                    || graph
                        .node(&edge.target)
                        .is_none_or(|target| !target.qualified_name.starts_with("a::"))),
                "{function}: {}",
                graph.report()
            );
        }
        assert_eq!(
            callees_of(graph, "a"),
            ["a::handlers::load"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2363: an arrow member and a value written in a method's
    /// literal take the method's `this`; an object's own method's `this` is
    /// the object.
    #[test]
    fn pr2363_arrow_members_take_the_this_of_the_method_around_the_literal() {
        let project = Project::new()
            .file("app.ts", "class Emitter { on(cb: () => void) { return cb; } }\nexport class App {\n  private emitter = new Emitter();\n  updateScene() { return 1; }\n  createApi() {\n    const api = {\n      updateScene: this.updateScene,\n      onChange: (cb: () => void) => this.emitter.on(cb),\n      refresh: () => this.updateScene(),\n      own() { return this.refresh(); },\n    };\n    return api;\n  }\n}\n")
            .index();
        let graph = project.graph();
        let create_api = &the_node(graph, "App::createApi", None).id;
        assert!(
            graph.edges.iter().any(|edge| {
                &edge.source == create_api
                    && edge.kind == "references"
                    && graph
                        .node(&edge.target)
                        .is_some_and(|target| target.qualified_name == "App::updateScene")
            }),
            "{}",
            graph.report()
        );
        assert_eq!(
            callees_of(graph, "App::createApi::api::onChange"),
            ["Emitter::on"]
        );
        assert_eq!(
            callees_of(graph, "App::createApi::api::refresh"),
            ["App::updateScene"]
        );
        assert_eq!(
            callees_of(graph, "App::createApi::api::own"),
            ["App::createApi::api::refresh"]
        );
    }

    /// Upstream #2363: a cross-file member edge follows edits of the script
    /// that defines the namespace.
    #[test]
    fn pr2363_a_cross_file_member_edge_follows_edits_of_its_script() {
        let mut project = Project::new()
            .file(
                "ns.js",
                "window.WS = { a() { return 1; }, wsM() { return 2; } };\n",
            )
            .file("use.js", "function viaGlobal() { return WS.wsM(); }\n")
            .index();
        assert_eq!(callers_of(project.graph(), "window.WS::wsM"), ["viaGlobal"]);
        project.write(
            "ns.js",
            "// moved\nwindow.WS = {\n  wsM() { return 3; },\n};\n",
        );
        assert_eq!(callers_of(project.sync(), "window.WS::wsM"), ["viaGlobal"]);
        project.write("ns.js", "window.WS = { renamed() { return 4; } };\n");
        let graph = project.sync();
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.qualified_name != "window.WS::wsM")
        );
        assert_eq!(callees_of(graph, "viaGlobal"), Vec::<String>::new());
    }

    /// Upstream #2363: an imported literal's member, and a binding
    /// destructured off a literal, reach the member; a name never
    /// destructured does not.
    #[test]
    fn pr2363_an_imported_literal_and_a_destructured_binding_reach_the_member() {
        let project = Project::new()
            .file("api.js", "export const api = { get() { return 1; }, post() { return 2; } };\nconst { post } = api;\nexport function b() { return post(); }\nexport function c() { return get(); }\n")
            .file("use.js", "import { api } from \"./api.js\";\nexport function a() { return api.get(); }\n")
            .index();
        let graph = project.graph();
        assert_eq!(callees_of(graph, "a"), ["api::get"]);
        assert_eq!(callees_of(graph, "b"), ["api::post"]);
        assert_eq!(callees_of(graph, "c"), Vec::<String>::new());
    }

    /// Upstream #2363: CommonJS exports, prototypes, call arguments, data,
    /// `this.x = {…}`, a plain name reassigned in a function and minified
    /// scripts keep their shape: no member nodes.
    #[test]
    fn pr2363_commonjs_prototypes_arguments_data_and_minified_scripts_keep_their_shape() {
        let bundle = "var q={b:function(){return r(1,2)},c:function(s){return s}};window.M={d:function(){return q.b()}};"
            .repeat(60);
        let project = Project::new()
            .file("shapes.js", "module.exports = { cjs() { return 1; } };\nFoo.prototype = { proto() { return 1; } };\nconsume({ ephemeral() { return 1; } });\nconst data = { a: 1, b: [1, 2] };\nthis.state = { onThis() { return 1; } };\n")
            .file("assets/lib.min.js", "var a={b:function(){return 1},c:function(){return 2}};window.L={d:function(){return a.b()}};\n")
            .file("assets/bundle.js", &format!("{bundle}\n{bundle}\n"))
            .file("iife.js", "(function () { var e; if (!e) { e = { getItem: function () { return 1; } }; } })();\n")
            .index();
        let graph = project.graph();
        for name in [
            "cjs",
            "proto",
            "ephemeral",
            "onThis",
            "b",
            "c",
            "d",
            "getItem",
        ] {
            assert!(
                graph.nodes.iter().all(|node| node.name != name),
                "{name}:\n{}",
                graph.report()
            );
        }
        assert_eq!(the_node(graph, "data", None).kind, "constant");
    }

    /// Upstream #2363: a `<script setup>` literal's members stay under their
    /// owner, and a call through the owner reaches them.
    #[test]
    fn pr2363_a_script_setup_literal_keeps_its_members_under_their_owner() {
        let project = Project::new()
            .file("Panel.vue", "<template><button @click=\"save\">Save</button></template>\n<script setup>\nconst handlers = { onSave() { persist(); } };\nfunction persist() {}\nfunction save() { handlers.onSave(); }\n</script>\n")
            .index();
        let graph = project.graph();
        let owner = &the_node(graph, "handlers", None).id;
        let member = &the_node(graph, "handlers::onSave", None).id;
        let parents: Vec<&str> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "contains" && &edge.target == member)
            .map(|edge| edge.source.as_str())
            .collect();
        assert_eq!(parents, [owner.as_str()]);
        assert_eq!(callees_of(graph, "handlers::onSave"), ["persist"]);
        assert_eq!(callees_of(graph, "save"), ["handlers::onSave"]);
    }

    /// Upstream #2363 (`extraction.test.ts`, #693): a module-scope literal's
    /// values run where it is written, so their calls are the constant's, and
    /// each function member's calls are its own; a data literal's calls are
    /// its constant's too.
    #[test]
    fn pr2363_a_module_literal_walks_its_values_under_its_constant() {
        let project = Project::new()
            .file("app.ts", "\nconst eager = load();\nconst obj = { handler: () => target(), plain: target() };\nconst list = [() => target()];\nexport const exported = { handler: () => target() };\nconst cfg = { a: compute(), b: [1, 2] };\n")
            .index();
        let graph = project.graph();
        let callers = |name: &str| -> Vec<String> {
            let mut out: Vec<String> = graph
                .unresolved
                .iter()
                .filter(|reference| {
                    reference.reference_kind == "calls" && reference.reference_name == name
                })
                .map(|reference| {
                    graph
                        .node(&reference.from_node_id)
                        .map_or("?".to_string(), |node| {
                            format!("{}:{}", node.kind, node.qualified_name)
                        })
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(callers("load"), ["constant:eager"]);
        assert_eq!(
            callers("target"),
            [
                "constant:list",
                "constant:obj",
                "function:exported::handler",
                "function:obj::handler"
            ]
        );
        assert_eq!(callers("compute"), ["constant:cfg"]);
    }

    /// Upstream #2363's negatives: a namespace two pages define with the same
    /// member is the caller's own directory's or nobody's; a binding
    /// destructured under another key, a global whose holder lacks the
    /// member, and a file that imports the name never reach a global member.
    #[test]
    fn pr2363_ambiguous_or_shadowed_holders_stay_unresolved() {
        let project = Project::new()
            .file("a/ns.js", "window.App = { go() { return 1; } };\n")
            .file("b/ns.js", "window.App = { go() { return 2; } };\n")
            .file("a/use.js", "function near() { return App.go(); }\n")
            .file("c/use.js", "function far() { return App.go(); }\n")
            .file("d/keys.js", "var Tool = { load() { return 1; }, x() { return 2; } };\nvar { x: load } = Tool;\nfunction renamed() { return load(); }\n")
            .file("d/other.js", "window.Other = { ping() { return 1; } };\nfunction missing() { return window.Other.go(); }\n")
            .file("e/imported.js", "import { App } from './app.js';\nexport function viaImport() { return App.go(); }\n")
            .index();
        let graph = project.graph();
        let go_files = |caller: &str| -> Vec<String> {
            let id = &the_node(graph, caller, None).id;
            let mut files: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| &edge.source == id && edge.kind == "calls")
                .filter_map(|edge| graph.node(&edge.target))
                .map(|node| format!("{} {}", node.qualified_name, node.file_path))
                .collect();
            files.sort();
            files
        };
        assert_eq!(go_files("near"), ["window.App::go a/ns.js"]);
        assert_eq!(go_files("far"), Vec::<String>::new());
        assert_eq!(go_files("renamed"), Vec::<String>::new());
        assert_eq!(go_files("missing"), Vec::<String>::new());
        assert_eq!(go_files("viaImport"), Vec::<String>::new());
    }

    /// `name file:line` of each node a JSX tag in the node named `parent`
    /// reaches (a `references` edge written right after `<`), sorted: the
    /// components the parent renders.
    fn renders(project: &super::support::Indexed, parent: &str) -> Vec<String> {
        let graph = project.graph();
        let parents: Vec<&super::support::NodeRow> = graph
            .nodes
            .iter()
            .filter(|node| node.name == parent && node.kind != "import")
            .collect();
        let at_tag = |file: &str, line: Option<i64>, col: Option<i64>| -> bool {
            let (Some(line), Some(col)) = (line, col) else {
                return false;
            };
            let text = std::fs::read_to_string(project.root().join(file)).unwrap_or_default();
            let Some(row) = text
                .lines()
                .nth(usize::try_from(line - 1).unwrap_or(usize::MAX))
            else {
                return false;
            };
            usize::try_from(col)
                .ok()
                .and_then(|col| col.checked_sub(1))
                .is_some_and(|before| row.as_bytes().get(before) == Some(&b'<'))
        };
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "references"
                    && parents.iter().any(|parent| {
                        parent.id == edge.source && at_tag(&parent.file_path, edge.line, edge.col)
                    })
            })
            .filter_map(|edge| graph.node(&edge.target))
            .filter(|node| matches!(node.kind.as_str(), "component" | "function" | "class"))
            .map(|node| format!("{} {}:{}", node.name, node.file_path, node.start_line))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Upstream #2442 (`47311494`): a JSX tag whose name its component binds
    /// itself, a `const` or a parameter by the tag's line, renders that local:
    /// a component the parent declares inside itself, else nothing; a tag
    /// above the local still renders the outer one. A name written only in a
    /// type argument or parameter list is no tag of the syntax tree, so it
    /// renders nothing either.
    #[test]
    fn pr2442_a_jsx_tag_bound_to_a_local_renders_no_other_component() {
        let project = Project::new()
            .file("package.json", "{\"dependencies\":{\"react\":\"^18.0.0\"}}")
            .file("app/components/CommandBarItem.tsx", "export function Content() {\n  return <div />;\n}\n\nexport function Icon() {\n  return <svg />;\n}\n")
            .file("shared/editor/Widget.tsx", "export function Widget() {\n  return <div />;\n}\n")
            .file("app/components/Portal.tsx", "export function Portal({ children }) {\n  return <div>{children}</div>;\n}\n")
            .file("app/components/Menu.tsx", "import * as DropdownMenu from '@radix-ui/react-dropdown-menu';\nimport * as ContextMenu from '@radix-ui/react-context-menu';\n\nconst Portal = DropdownMenu.Portal;\n\nexport function SubMenuContent({ variant, children }) {\n  const Portal =\n    variant === 'dropdown' ? DropdownMenu.Portal : ContextMenu.Portal;\n  const Content =\n    variant === 'dropdown'\n      ? DropdownMenu.SubContent\n      : ContextMenu.SubContent;\n  return (\n    <Portal>\n      <Content collisionPadding={6}>{children}</Content>\n    </Portal>\n  );\n}\n\nexport function SettingsLinks({ config }) {\n  return (\n    <ul>\n      {config.map((item) => {\n        const Icon = item.icon;\n        return <li key={item.path}><Icon /></li>;\n      })}\n    </ul>\n  );\n}\n\nexport function Widgets({ widgets }) {\n  return <div>{Object.values(widgets).map((Widget, index) => <Widget key={index} />)}</div>;\n}\n")
            .file("a-decoy/row.tsx", "export function Row() {\n  return <tr />;\n}\n")
            .file("app/icon.tsx", "export function Icon() {\n  return <svg />;\n}\n")
            .file("app/table.tsx", "import { Icon } from './icon';\n\nfunction Row() {\n  return <tr className=\"outer\" />;\n}\n\nexport function Table({ rows }) {\n  const Row = ({ row }) => <tr>{row.name}</tr>;\n  return <table>{rows.map((row) => <Row key={row.id} row={row} />)}</table>;\n}\n\nexport function Toolbar({ actions }) {\n  return (\n    <div>\n      <Icon />\n      {actions.map((action) => {\n        const Icon = action.icon;\n        return <Icon key={action.id} />;\n      })}\n    </div>\n  );\n}\n")
            .file("app/profile/entry.tsx", "export function Entry() {\n  return <div />;\n}\n")
            .file("app/components/GenericTable.tsx", "type BaseEntity = { id: string };\n\nexport const GenericTable = <Entry extends BaseEntity>({ data }: { data: Entry[] }) => {\n  return <table>{data.length}</table>;\n};\n")
            .index();
        assert_eq!(
            renders(&project, "SubMenuContent"),
            Vec::<String>::new(),
            "{}",
            project.graph().report()
        );
        assert_eq!(renders(&project, "SettingsLinks"), Vec::<String>::new());
        assert_eq!(renders(&project, "Widgets"), Vec::<String>::new());
        assert_eq!(renders(&project, "Table"), ["Row app/table.tsx:8"]);
        assert_eq!(renders(&project, "Toolbar"), ["Icon app/icon.tsx:1"]);
        assert_eq!(renders(&project, "GenericTable"), Vec::<String>::new());
    }

    /// Upstream #2140 (`a3d675ef`): a Vue Options API component's functions
    /// (`methods`, `computed`, `watch` entries, `data`, lifecycle hooks) are
    /// method nodes of the component, each owning the calls written in it;
    /// the member-call and exact-name strategies reach such a method only as
    /// `this.m()` in its own file, never through `e.preventDefault()` or
    /// `this.editor.setValue()`. (`@click="handleLogin"` reaching the method
    /// is upstream's vue-handler synthesizer, W7-18.)
    #[test]
    fn pr2140_options_api_functions_are_methods_of_their_component() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"admin\",\"dependencies\":{\"vue\":\"^2.6.0\"}}")
            .file("src/views/login/index.vue", "<template>\n  <form>\n    <button @click=\"handleLogin\">Sign in</button>\n  </form>\n</template>\n\n<script>\nexport default {\n  name: 'Login',\n  data() {\n    return { loading: false }\n  },\n  methods: {\n    handleLogin() {\n      if (this.validate()) {\n        this.loading = true\n      }\n    },\n    validate() {\n      return true\n    }\n  }\n}\n</script>\n")
            .file("src/components/ImageCropper.vue", "<template><div /></template>\n<script>\nexport default {\n  methods: {\n    preventDefault(e) { return e },\n    setValue(v) { return v },\n    click() { return 1 }\n  }\n}\n</script>\n")
            .file("src/components/Editor.vue", "<template><input ref=\"input\" /></template>\n<script>\nexport default {\n  methods: {\n    onKey(e) {\n      e.preventDefault()\n      this.editor.setValue(1)\n      this.cropper.preventDefault()\n    }\n  }\n}\n</script>\n")
            .file("src/components/Tab.vue", "<template><b /></template>\n<script>\nexport default {\n  methods: {\n    focus() { return 1 }\n  }\n}\n</script>\n")
            .file("src/components/Pane.vue", "<template><i /></template>\n<script>\nexport default {\n  methods: {\n    focus() { return 2 }\n  }\n}\n</script>\n")
            .file("src/components/Panel.vue", "<template><input ref=\"input\" /></template>\n<script>\nexport default {\n  methods: {\n    open() {\n      this.$refs['input'].focus()\n    }\n  }\n}\n</script>\n")
            .index();
        let graph = project.graph();
        let mut methods: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == "src/views/login/index.vue" && node.kind == "method")
            .map(|node| node.name.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(
            methods,
            ["data", "handleLogin", "validate"],
            "{}",
            graph.report()
        );
        assert_eq!(the_node(graph, "index::handleLogin", None).kind, "method");
        assert_eq!(callees_of(graph, "index::handleLogin"), ["index::validate"]);
        assert_eq!(callees_of(graph, "Editor::onKey"), Vec::<String>::new());
        // A template ref names a child this cannot tell apart: neither
        // component's own `focus`.
        assert_eq!(callees_of(graph, "Panel::open"), Vec::<String>::new());
    }

    /// Upstream #2357 (b) (`8998697f`, issue #2340): a call written in a Vue
    /// template is the component's, at its own line: an interpolation, a
    /// binding, a handler call; a bare call to the component's own Options
    /// API method links to it, and the import decides which helper.
    #[test]
    fn pr2357_a_template_call_is_the_components() {
        let project = Project::new()
            .file("package.json", "{\"name\":\"nuxt-app\",\"dependencies\":{\"nuxt\":\"^4.0.0\"}}")
            .file("composables/useBar.ts", "export function useBar(x: unknown) { return String(x); }\n")
            .file("components/H.vue", "<template><div>{{ useBar(link) }}</div></template>\n")
            .file("components/I.vue", "<template><NuxtLink :to=\"useBar(link.location)\">x</NuxtLink></template>\n")
            .file("utils/format.ts", "export function formatPrice(n: number) { return `$${n}`; }\n")
            .file("other/format.ts", "export function formatPrice(n: number) { return n; }\n")
            .file("components/Setup.vue", "<template>\n  <p>{{ formatPrice(item.price) }}</p>\n  <button @click=\"save(item)\">save</button>\n</template>\n\n<script setup lang=\"ts\">\nimport { formatPrice } from '../utils/format';\nconst props = defineProps<{ item: { price: number } }>();\nfunction save(item: unknown) { return item; }\n</script>\n")
            .file("components/Options.vue", "<template>\n  <p>{{ total(items) }}</p>\n  <button @click=\"submit(form)\">send</button>\n</template>\n\n<script>\nexport default {\n  methods: {\n    total(items) { return items.length },\n    submit(form) { return form }\n  }\n}\n</script>\n")
            .file("pages/lists.vue", "<template>\n  <button @click=\"clearError(true)\">x</button>\n</template>\n\n<script setup lang=\"ts\">\nfunction clearError(focus: boolean) { return focus; }\nonDeactivated(() => clearError(false));\n</script>\n")
            .file("lib/bus.ts", "export class Bus {\n  emit(event: string) { return event; }\n}\n")
            .file("components/Dialog.vue", "<template>\n  <button @click=\"close\">x</button>\n  <button @click=\"emit('cancel')\">cancel</button>\n  <button @click=\"open = !open\">toggle</button>\n</template>\n\n<script setup lang=\"ts\">\nconst emit = defineEmits(['cancel']);\nlet open = false;\nfunction close() { open = false; }\n</script>\n")
            .index();
        let graph = project.graph();
        let component_calls = |file: &str| -> Vec<String> {
            let component = graph
                .nodes
                .iter()
                .find(|node| node.file_path == file && node.kind == "component")
                .unwrap_or_else(|| panic!("component of {file}"));
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| edge.source == component.id && edge.kind == "calls")
                .filter_map(|edge| {
                    graph.node(&edge.target).map(|target| {
                        format!(
                            "{}@{}:{}",
                            target.name,
                            target.file_path,
                            edge.line.unwrap_or(0)
                        )
                    })
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            component_calls("components/H.vue"),
            ["useBar@composables/useBar.ts:1"]
        );
        assert_eq!(
            component_calls("components/I.vue"),
            ["useBar@composables/useBar.ts:1"]
        );
        assert_eq!(
            component_calls("components/Setup.vue"),
            [
                "formatPrice@utils/format.ts:2",
                "save@components/Setup.vue:3"
            ]
        );
        // A template calls the component's own methods without `this.`.
        assert_eq!(
            component_calls("components/Options.vue"),
            [
                "submit@components/Options.vue:3",
                "total@components/Options.vue:2"
            ]
        );
        // Both calls are the local function's, not Nuxt's `clearError`.
        assert_eq!(
            component_calls("pages/lists.vue"),
            [
                "clearError@pages/lists.vue:2",
                "clearError@pages/lists.vue:7"
            ]
        );
        // `emit('cancel')` is the component's `defineEmits` binding, and `open
        // = !open` calls nothing: neither reaches Bus.emit.
        assert!(
            component_calls("components/Dialog.vue")
                .iter()
                .all(|call| !call.starts_with("emit@lib/bus.ts")),
            "{:?}",
            component_calls("components/Dialog.vue")
        );
    }

    /// Upstream #2177 (`48498021`), its JavaScript part: fetch's Response,
    /// Request and Blob body methods (`text`, `json`, `arrayBuffer`, `blob`,
    /// `formData`) are builtins, so a call on an untyped receiver is fetch's,
    /// not a project class's method of that name, whatever the receiver is
    /// called; a receiver the code constructs still reaches the class.
    #[test]
    fn pr2177_fetch_body_methods_need_receiver_evidence() {
        let project = Project::new()
            .file("src/file.js", "export class LazyFile {\n  text() { return ''; }\n  json() { return {}; }\n  blob() { return null; }\n  formData() { return null; }\n  arrayBuffer() { return null; }\n}\n\nexport async function load(response, file, lazyFile) {\n  const body = await response.text();\n  await file.text();\n  await lazyFile.json();\n  await lazyFile.blob();\n  await file.arrayBuffer();\n  await file.formData();\n  return body;\n}\n\nexport function local() {\n  const made = new LazyFile();\n  return made.text();\n}\n")
            .index();
        let graph = project.graph();
        let id = |name: &str| the_node(graph, name, None).id.clone();
        assert_eq!(
            calls_of(graph, &id("load"), false),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(calls_of(graph, &id("local"), false), ["LazyFile::text"]);
    }
}

mod python {
    use super::support::{EdgeRow, Graph, Project};

    /// `file: qualified` of every `kind` edge out of `file`'s nodes that
    /// `keep` accepts, sorted.
    fn edges_where(
        graph: &Graph,
        file: &str,
        kind: &str,
        keep: impl Fn(&EdgeRow) -> bool,
    ) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == kind
                    && keep(edge)
                    && graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{}: {}", node.file_path, node.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// `file: qualified` of every `kind` edge out of `file`'s nodes, sorted.
    fn edges_from(graph: &Graph, file: &str, kind: &str) -> Vec<String> {
        edges_where(graph, file, kind, |_| true)
    }

    /// `file: qualified` of every `calls` edge out of `file`'s nodes, sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        edges_from(graph, file, "calls")
    }

    /// `file: qualified` of every `calls` edge out of the node named `name`.
    fn calls_of(graph: &Graph, name: &str) -> Vec<String> {
        let ids: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.name == name && node.kind != "import")
            .map(|node| node.id.as_str())
            .collect();
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" && ids.contains(&edge.source.as_str()))
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{}: {}", node.file_path, node.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2125 (`99209b6f`): a Python call whose receiver the
    /// extractor could not keep (`User.objects.get`, `self.client.login`)
    /// reaches only a member of what its chain names last; a bare call
    /// (`get(1)`) is no method, and a name taken `from` a module outside the
    /// project (`from django.shortcuts import render`) is the package's; a
    /// module path names the module's function; `self.helper()` is the
    /// class's own.
    #[test]
    fn pr2125_a_python_method_is_reached_through_its_receiver() {
        let project = Project::new()
            .file("app/tests.py", "class AuthTestCase:\n    def get(self, url):\n        return url\n\n    def login(self, name):\n        return name\n")
            .file("app/curl.py", "def get(url):\n    return url\n")
            .file("app/helpers.py", "def slugify(text):\n    return text\n")
            .file("app/tags.py", "def render(context):\n    return context\n")
            .file("app/pages.py", "import app.helpers\nfrom django.shortcuts import render\n\ndef page(request):\n    kind = request.POST.get(\"kind\")\n    slug = app.helpers.slugify(kind)\n    return render(request, \"page.html\", {\"slug\": slug})\n")
            .file("app/views.py", "from django.contrib.auth.models import User\n\nclass Backend:\n    def authenticate(self, username):\n        user = User.objects.get(email=username)\n        self.client.login(username=\"a\")\n        self.helper()\n        self.assertEqual(self.filterset(params, self.queryset).qs.get(), 2)\n        get(1)\n        return user\n\n    def helper(self):\n        return 1\n")
            .file("app/config.py", "class UserConfig:\n    def all(self):\n        return []\n\n\ndef run(prefs):\n    everything = prefs.objects.all()\n    return all([everything])\n")
            .file("app/bare.py", "def bare_login():\n    return login(\"x\")\n")
            .index();
        let graph = project.graph();
        let authenticate = calls_of(graph, "authenticate");
        assert!(
            !authenticate.contains(&"app/tests.py: AuthTestCase::get".to_string()),
            "{authenticate:?}"
        );
        assert!(
            !authenticate.contains(&"app/tests.py: AuthTestCase::login".to_string()),
            "{authenticate:?}"
        );
        assert!(
            authenticate.contains(&"app/views.py: Backend::helper".to_string()),
            "{authenticate:?}"
        );
        let page = calls_of(graph, "page");
        assert!(!page.contains(&"app/curl.py: get".to_string()), "{page:?}");
        assert!(
            !page.contains(&"app/tags.py: render".to_string()),
            "{page:?}"
        );
        assert!(
            page.contains(&"app/helpers.py: slugify".to_string()),
            "{page:?}"
        );
        // `all(…)` is the builtin and `prefs.objects.all()` a member of
        // `objects`: neither is `UserConfig.all`.
        assert_eq!(calls_of(graph, "run"), Vec::<String>::new());
        // A bare call is no method: Python has no implicit self.
        assert_eq!(calls_of(graph, "bare_login"), Vec::<String>::new());
    }

    /// Upstream #2125: a method of a class declared inside a function is in
    /// scope only in that function, in every language: a `this.run()` in
    /// another class is not `f`'s local `Local.run`, while the function's own
    /// code still reaches its local class's method.
    #[test]
    fn pr2125_a_method_of_a_local_class_is_reachable_only_in_its_function() {
        let project = Project::new()
            .file("tests/test_perms.py", "def test_perm():\n    class Request:\n        def has_perm(self, perm):\n            return perm\n\n        def check(self):\n            return self.has_perm(\"x\")\n\n    return Request().check()\n")
            .file("web/a.js", "function f() {\n  class Local { run() { return 1; } }\n  return new Local();\n}\nclass Other {\n  go() { return this.run(); }\n}\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "go"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert!(
            calls_of(graph, "check")
                .contains(&"tests/test_perms.py: test_perm::Request::has_perm".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2198 (`71202c38`): a bare Python call to a name its function
    /// (or module) binds itself is that value, not another file's function
    /// of the name; a pytest fixture the test takes as a parameter is still
    /// the fixture, and an imported name is the import's whatever a
    /// docstring says.
    #[test]
    fn pr2198_a_bare_call_to_a_locally_bound_name_is_that_value() {
        let project = Project::new()
            .file("tests/test_testing.py", "def view(request):\n    return request\n")
            .file("tests/test_versioning.py", "class TestVersion:\n    def test_version(self, request):\n        view = MyView.as_view()\n        response = view(request)\n        return response\n")
            .file("tests/conftest.py", "import pytest\n\n\n@pytest.fixture\ndef user_factory():\n    def make():\n        return 1\n    return make\n")
            .file("tests/test_users.py", "def test_user(user_factory):\n    user = user_factory()\n    return user\n")
            .file("app/utils.py", "def user_display(user):\n    return str(user)\n")
            .file("app/tags.py", "from app.utils import user_display\n\n\ndef user_display_tag(user):\n    \"\"\"\n    {% user_display user as user_display %}\n    \"\"\"\n    return user_display(user)\n")
            .file("docs/example.py", "import pytest\n\n\n@pytest.fixture\ndef func():\n    return 1\n")
            .file("src/runner.py", "def _run_both(func):\n    return func()\n")
            .index();
        let graph = project.graph();
        assert!(
            !calls_from(graph, "tests/test_versioning.py")
                .contains(&"tests/test_testing.py: view".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            calls_from(graph, "tests/test_users.py")
                .contains(&"tests/conftest.py: user_factory".to_string())
        );
        assert!(
            calls_from(graph, "app/tags.py").contains(&"app/utils.py: user_display".to_string())
        );
        // A parameter is its own value: a fixture another module defines,
        // with no conftest above the caller, is out of reach.
        assert_eq!(calls_from(graph, "src/runner.py"), Vec::<String>::new());
    }

    /// Upstream #2247 (`6d8802ce`): a name imported from a package follows
    /// the package's re-exports to its definition, by name (`from .tokens
    /// import Token`) and through a star import (`from .users import *`),
    /// never to a test's or a migration's namesake.
    #[test]
    fn pr2247_an_imported_name_follows_the_package_reexports() {
        let project = Project::new()
            .file("netbox/users/__init__.py", "")
            .file(
                "netbox/users/models/__init__.py",
                "from .tokens import Token\nfrom .users import *\n",
            )
            .file("netbox/users/models/users.py", "class User:\n    pass\n")
            .file("netbox/users/models/tokens.py", "class Token:\n    pass\n")
            .file("netbox/extras/tests/test_templatetags.py", "class User:\n    pass\n")
            .file("netbox/alembic/versions/0001_tokens.py", "class Token:\n    pass\n")
            .file(
                "netbox/core/views.py",
                "from users.models import Token, User\n\n\ndef make():\n    return User(), Token()\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "netbox/core/views.py", "instantiates"),
            [
                "netbox/users/models/tokens.py: Token",
                "netbox/users/models/users.py: User"
            ],
            "{}",
            graph.report()
        );
    }

    /// The re-export walk fails closed: two star-imported modules that both
    /// define the name, two imports rebinding it, a cycle of star imports
    /// and a chain four modules deep all leave the name unresolved.
    #[test]
    fn pr2247_an_ambiguous_or_endless_reexport_stays_unresolved() {
        let project = Project::new()
            .file("app/__init__.py", "")
            .file("app/both/__init__.py", "from .one import *\nfrom .two import *\n")
            .file("app/both/one.py", "class Client:\n    pass\n")
            .file("app/both/two.py", "class Client:\n    pass\n")
            .file(
                "app/rebound/__init__.py",
                "try:\n    from .fast import Parser\nexcept ImportError:\n    from .slow import Parser\n",
            )
            .file("app/rebound/fast.py", "class Parser:\n    pass\n")
            .file("app/rebound/slow.py", "class Parser:\n    pass\n")
            .file("app/loop/__init__.py", "from .a import *\n")
            .file("app/loop/a.py", "from .b import *\n")
            .file("app/loop/b.py", "from .a import *\n")
            .file("app/deep/__init__.py", "from .l1 import *\n")
            .file("app/deep/l1.py", "from .l2 import *\n")
            .file("app/deep/l2.py", "from .l3 import *\n")
            .file("app/deep/l3.py", "from .l4 import *\n")
            .file("app/deep/l4.py", "class Engine:\n    pass\n")
            .file("app/near/__init__.py", "from .l1 import *\n")
            .file("app/near/l1.py", "from .l2 import *\n")
            .file("app/near/l2.py", "from .l3 import *\n")
            .file("app/near/l3.py", "class Motor:\n    pass\n")
            .file(
                "app/use.py",
                "from app.both import Client\nfrom app.rebound import Parser\nfrom app.loop import Missing\nfrom app.deep import Engine\nfrom app.near import Motor\n\n\ndef build():\n    return Client(), Parser(), Missing(), Engine(), Motor()\n",
            )
            .file("other/client.py", "class Client:\n    pass\n")
            .index();
        let graph = project.graph();
        // Three modules deep is followed; four is not. (What the import
        // declines, name matching may still guess, as before.)
        assert_eq!(
            edges_where(graph, "app/use.py", "instantiates", |edge| {
                edge.resolved_by() == Some("import")
            }),
            ["app/near/l3.py: Motor"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2248 (`34ede4de`): a module member used through the
    /// imported module (`generic.ObjectEditView` after `from netbox.views
    /// import generic`) follows the package's re-exports too.
    #[test]
    fn pr2248_a_module_member_follows_the_package_reexports() {
        let project = Project::new()
            .file("netbox/netbox/__init__.py", "")
            .file("netbox/netbox/views/__init__.py", "")
            .file(
                "netbox/netbox/views/generic/__init__.py",
                "from .object_views import *\n",
            )
            .file(
                "netbox/netbox/views/generic/object_views.py",
                "class ObjectEditView:\n    pass\n",
            )
            .file(
                "netbox/dcim/views.py",
                "from netbox.views import generic\n\n\nclass DeviceEditView(generic.ObjectEditView):\n    pass\n",
            )
            .file("netbox/extras/views.py", "class ObjectEditView:\n    pass\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "netbox/dcim/views.py", "extends"),
            ["netbox/netbox/views/generic/object_views.py: ObjectEditView"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2291 (`f838c5a0`, parts b and c): a parenthesized
    /// from-import, one name per line and with a comment holding a call,
    /// binds every name it lists; and `pkg.mod.Cls` is the class `import
    /// pkg.mod` names, unless another import shares the module's last
    /// segment.
    #[test]
    fn pr2291_parenthesized_imports_and_dotted_module_classes_resolve() {
        let project = Project::new()
            .file("store/__init__.py", "")
            .file(
                "store/base.py",
                "class Base:\n    def fetch(self, ids):\n        return ids\n\n\ndef helper():\n    return 1\n",
            )
            .file("decoy/base.py", "class Base:\n    pass\n\n\ndef helper():\n    return 2\n")
            .file(
                "store/rows.py",
                "from store.base import (\n    Base,\n    helper as h,  # a comment, with a comma and a call()\n)\n\n\nclass Row(Base):\n    def load(self):\n        return h()\n",
            )
            .file("alpha/__init__.py", "")
            .file("beta/__init__.py", "")
            .file("alpha/foo.py", "class Client:\n    def fetch(self):\n        return 1\n")
            .file("beta/foo.py", "class Client:\n    def fetch(self):\n        return 2\n")
            .file(
                "one.py",
                "import alpha.foo\n\n\nclass Sub(alpha.foo.Client):\n    pass\n\n\ndef go(pool, s: Sub):\n    pool.submit(s.fetch)\n",
            )
            .file(
                "two.py",
                "import beta.foo\nimport alpha.foo\n\n\nclass Sub(alpha.foo.Client):\n    pass\n\n\ndef go(pool, s: Sub):\n    pool.submit(s.fetch)\n",
            )
            .index();
        let graph = project.graph();
        // Name matching guessed `Base` before; the import binds it now.
        assert_eq!(
            edges_where(graph, "store/rows.py", "extends", |edge| {
                edge.resolved_by() == Some("import")
            }),
            ["store/base.py: Base"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_from(graph, "store/rows.py"),
            ["store/base.py: helper"],
            "{}",
            graph.report()
        );
        let fn_ref = |edge: &EdgeRow| edge.metadata["fnRef"] == true;
        assert_eq!(
            edges_where(graph, "one.py", "references", fn_ref),
            ["alpha/foo.py: Client::fetch"],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_where(graph, "two.py", "references", fn_ref),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// `source name -> target qualified name` of every function-value
    /// `references` edge into a node named `member`, sorted.
    fn fn_refs_into(graph: &Graph, member: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "references" && edge.metadata["fnRef"] == true)
            .filter_map(|edge| {
                let target = graph.node(&edge.target)?;
                let source = graph.node(&edge.source)?;
                (target.name == member)
                    .then(|| format!("{} -> {}", source.name, target.qualified_name))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2290 (`a4bd890e`): `self.get_ip(request)` is a method call
    /// on the instance even when the file imports a function named `get_ip`;
    /// a plain `get_ip(request)` is still the import's.
    #[test]
    fn pr2290_a_self_call_never_resolves_through_a_same_named_import() {
        let httpkit = "def get_ip(request):\n    return \"1.2.3.4\"\n";
        let own = Project::new()
            .file("pkg/__init__.py", "")
            .file("pkg/httpkit.py", httpkit)
            .file(
                "pkg/adapter.py",
                "from pkg.httpkit import get_ip\n\n\nclass Adapter:\n    def get_ip(self, request):\n        return get_ip(request)\n\n    def notify(self, request):\n        return self.get_ip(request)\n",
            )
            .index();
        let graph = own.graph();
        assert_eq!(
            calls_of(graph, "get_ip"),
            ["pkg/httpkit.py: get_ip"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "notify"),
            ["pkg/adapter.py: Adapter::get_ip"],
            "{}",
            graph.report()
        );
        let inherited = Project::new()
            .file("pkg/__init__.py", "")
            .file("pkg/httpkit.py", httpkit)
            .file(
                "pkg/base.py",
                "class Base:\n    def get_ip(self, request):\n        return \"base\"\n",
            )
            .file(
                "pkg/adapter.py",
                "from pkg.base import Base\nfrom pkg.httpkit import get_ip\n\n\nclass Adapter(Base):\n    def notify(self, request):\n        return self.get_ip(request)\n\n    def raw(self, request):\n        return get_ip(request)\n",
            )
            .index();
        let graph = inherited.graph();
        assert_eq!(
            calls_of(graph, "notify"),
            ["pkg/base.py: Base::get_ip"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "raw"),
            ["pkg/httpkit.py: get_ip"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2291 (a): a method value on a module global with no static
    /// type (`conn = None`, rebound by `global conn; conn = Store()`) is a
    /// method of the class its module assigns; a parameter, a loop variable
    /// or an import shadowing the module is not the global.
    #[test]
    fn pr2291_a_module_global_bound_by_its_assignments_resolves_as_a_receiver() {
        let project = Project::new()
            .file("store.py", "class Store:\n    def fetch(self, ids):\n        return ids\n")
            .file("decoy.py", "class Decoy:\n    def fetch(self, ids):\n        return []\n")
            .file(
                "settings.py",
                "from decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    from store import Store\n    conn = Store()\n\ndef shadow():\n    conn = Decoy()\n    return conn\n\ndef same_file(pool):\n    pool.submit(conn.fetch, [])\n\ndef param_shadow(pool, conn):\n    pool.submit(conn.fetch, [])\n\ndef loop_shadow(pool, items):\n    for conn in items:\n        pool.submit(conn.fetch, [])\n",
            )
            .file(
                "consumer.py",
                "import settings\nfrom settings import conn\n\ndef via_module(pool):\n    pool.submit(settings.conn.fetch, [])\n\ndef via_name(pool):\n    pool.submit(conn.fetch, [])\n\ndef import_shadow(pool, settings):\n    pool.submit(settings.conn.fetch, [])\n\ndef local_import(pool):\n    from settings import conn\n    pool.submit(conn.fetch, [])\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            fn_refs_into(graph, "fetch"),
            [
                "local_import -> Store::fetch",
                "same_file -> Store::fetch",
                "via_module -> Store::fetch",
                "via_name -> Store::fetch"
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2291 (a): a module global several backends can hold binds
    /// to the nearest declaration they all inherit; an opaque initializer
    /// (`make_conn()`, a conditional) leaves its type unknown.
    #[test]
    fn pr2291_a_module_global_with_several_backends_binds_their_shared_declaration() {
        let project = Project::new()
            .file("backends/__init__.py", "")
            .file(
                "backends/base.py",
                "class Base:\n    def fetch(self, ids):\n        raise NotImplementedError(\"backend\")\n    def search(self):\n        raise NotImplementedError(\"backend\")\n",
            )
            .file(
                "backends/es.py",
                "from backends.base import Base\nclass ES(Base):\n    def search(self):\n        return []\n",
            )
            .file(
                "backends/vec.py",
                "from backends.base import (\n    Base,\n)\nclass Vec(Base):\n    def fetch(self, ids):\n        return ids\n    def search(self):\n        return []\n",
            )
            .file("other.py", "class Other:\n    def fetch(self, ids):\n        return ids\n")
            .file(
                "settings.py",
                "import backends.es\nconn = None\n\ndef init(engine):\n    global conn\n    if engine == \"es\":\n        conn = backends.es.ES()\n    else:\n        from backends import vec as vec_module\n        conn = vec_module.Vec()\n",
            )
            .file(
                "unknown.py",
                "from other import Other\nconn = None\nmixed = None\n\ndef init(flag):\n    global conn, mixed\n    conn = make_conn()\n    mixed = Other() if flag else None\n",
            )
            .file(
                "consumer.py",
                "import settings\nimport unknown\n\ndef gc(pool):\n    pool.submit(settings.conn.fetch, [])\n\ndef find(pool):\n    pool.submit(settings.conn.search)\n\ndef opaque(pool):\n    pool.submit(unknown.conn.fetch, [])\n    pool.submit(unknown.mixed.fetch, [])\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            fn_refs_into(graph, "fetch"),
            ["gc -> Base::fetch"],
            "{}",
            graph.report()
        );
        assert_eq!(
            fn_refs_into(graph, "search"),
            ["find -> Base::search"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2291 (a): the module-global rule never binds through a
    /// shadow, an unknown value, a deeper chain, a write it cannot place, or
    /// a test double: upstream's adversarial table, one project per case.
    #[test]
    fn pr2291_a_module_global_never_binds_through_a_shadow_or_an_unknown_value() {
        #[allow(clippy::type_complexity)]
        let cases: &[(&str, &[(&str, &str)], &[&str])] = &[
            (
                "control",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "tuple_first",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool):\n    conn, _ = (Decoy(), None)\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "loop_and_nested_global",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, items):\n    for conn in items:\n        pass\n    def nested():\n        global conn\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "multiline_conditional",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store(\n        x=1,\n    ) if flag else Decoy()\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "multiline_constructor",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store(\n        x=1,\n    )\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "closure_param",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef outer(conn):\n    def cb(pool):\n        pool.submit(conn.fetch)\n    return cb\n",
                )],
                &[],
            ),
            (
                "closure_local",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef outer(pool):\n    conn = Decoy()\n    def cb():\n        pool.submit(conn.fetch)\n    return cb\n",
                )],
                &[],
            ),
            (
                "lambda_param",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool):\n    pool.submit(lambda conn: pool.map(conn.fetch))\n",
                )],
                &[],
            ),
            (
                "comprehension",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, xs):\n    [pool.submit(conn.fetch) for conn in xs]\n",
                )],
                &[],
            ),
            (
                "except_as",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool):\n    try:\n        pass\n    except Exception as conn:\n        pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "match_as",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, x):\n    match x:\n        case Decoy() as conn:\n            pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "global_in_nested_def",
                &[(
                    "m.py",
                    "from decoy import Decoy\nconn = None\n\ndef init():\n    def helper():\n        global conn\n    conn = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "keyword_in_multiline_call",
                &[(
                    "m.py",
                    "from decoy import Decoy\nconn = None\napp = dict(\n    conn=Decoy()\n)\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "tuple_rebind",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    global conn\n    conn, other = Decoy(), 1\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "with_rebind",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    global conn\n    with open_decoy() as conn:\n        pass\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "docstring",
                &[(
                    "m.py",
                    "from store import Store\nconn = None\n\"\"\"\nconn = Store()\n\"\"\"\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "deeper_chain",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.pool.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "importer_rebinds",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "c.py",
                        "from settings import conn\nfrom decoy import Decoy\n\ndef reset():\n    global conn\n    conn = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "local_import_of_other_module",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "other_settings.py",
                        "from decoy import Decoy\nconn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    import other_settings as settings\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "local_from_import_of_other_module",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "other_settings.py",
                        "from decoy import Decoy\nconn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "from settings import conn\n\ndef cb(pool):\n    from other_settings import conn\n    pool.submit(conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "comment_in_parenthesized_import",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nother = 1\n",
                    ),
                    (
                        "c.py",
                        "from settings import (\n    other,  # see fetch()\n    conn,\n)\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "star_import_override",
                &[
                    ("local.py", "from decoy import Decoy\nconn = Decoy()\n"),
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nfrom local import *\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "match_sequence_capture",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, x):\n    match x:\n        case [conn]:\n            pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "match_keyword_capture",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, x):\n    match x:\n        case Decoy(inner=conn):\n            pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "match_bare_capture",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, x):\n    match x:\n        case conn:\n            pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "multiline_loop_target",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool, xs):\n    for (a,\n         conn) in xs:\n        pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "multiline_tuple_local",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef cb(pool):\n    (a,\n     conn) = 1, Decoy()\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "multiline_tuple_rebind",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    global conn\n    (a,\n     conn) = 1, Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "annotation_contradicts_value",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn: Store = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "annotation_over_factory",
                &[(
                    "m.py",
                    "from store import Store\nconn: Store = make_store()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "external_production_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "external_aliased_write",
                &[
                    ("pkg/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import pkg.settings as cfg\n\ndef reset():\n    cfg.conn = make_conn()\n",
                    ),
                    (
                        "c.py",
                        "from pkg import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "external_relative_write",
                &[
                    ("pkg/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "pkg/reset.py",
                        "from . import settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "from pkg import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "external_setattr",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom decoy import Decoy\n\ndef reset():\n    setattr(settings, \"conn\", Decoy())\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "external_subclass_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "sub.py",
                        "from store import Store\nclass SubStore(Store):\n    def fetch(self, ids):\n        return ids\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom sub import SubStore\n\ndef reset():\n    settings.conn = SubStore()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "test_double_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "tests/test_c.py",
                        "import settings\nfrom unittest.mock import MagicMock\n\ndef test_cb(pool):\n    settings.conn = MagicMock()\n    pool.submit(settings.conn.fetch)\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "test_monkeypatch",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "tests/test_c.py",
                        "import settings\n\ndef test_cb(monkeypatch, pool):\n    monkeypatch.setattr(settings, \"conn\", object())\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "globals_literal_write",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    globals()[\"conn\"] = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "globals_dynamic_write",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nfor name, obj in [(\"conn\", Decoy())]:\n    globals()[name] = obj\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "unrelated_attribute_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "other.py",
                        "from decoy import Decoy\n\nclass Holder:\n    def __init__(self):\n        self.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "write_path_is_exact",
                &[
                    ("x/__init__.py", ""),
                    ("y/__init__.py", ""),
                    (
                        "x/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "y/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "from x import settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "cx.py",
                        "from x import settings\n\ndef cb_x(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                    (
                        "cy.py",
                        "from y import settings\n\ndef cb_y(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb_y -> Store::fetch"],
            ),
            (
                "ambiguous_write_target",
                &[
                    ("x/__init__.py", ""),
                    ("y/__init__.py", ""),
                    (
                        "x/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "y/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "cx.py",
                        "from x import settings\n\ndef cb_x(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                    (
                        "cy.py",
                        "from y import settings\n\ndef cb_y(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "relative_write_is_exact",
                &[
                    ("pkg/__init__.py", ""),
                    ("pkg/sub/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    ("other/__init__.py", ""),
                    ("other/pkg/__init__.py", ""),
                    (
                        "other/pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "pkg/sub/reset.py",
                        "from .. import settings\n\ndef reset():\n    settings.conn = make_conn()\n",
                    ),
                    (
                        "c2.py",
                        "from other.pkg import settings\n\ndef cb2(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb2 -> Store::fetch"],
            ),
            (
                "namespace_import_binds_root",
                &[
                    ("pkg/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "c.py",
                        "import pkg.settings\nfrom decoy import Decoy\n\nsettings = Bag()\nsettings.conn = Decoy()\n",
                    ),
                    (
                        "d.py",
                        "from pkg import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "alias_of_longer_module_is_not_explicit",
                &[
                    ("pkg/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    ("other/__init__.py", ""),
                    ("other/pkg/__init__.py", ""),
                    ("other/pkg/settings.py", "conn = None\n"),
                    (
                        "reset.py",
                        "import pkg.settings\nimport other.pkg.settings as settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "pkg/d.py",
                        "from . import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "alias_in_string_is_not_explicit",
                &[
                    ("pkg/__init__.py", ""),
                    ("pkg/settings.py", "conn = None\n"),
                    (
                        "reset.py",
                        "import pkg.settings\nfrom decoy import Decoy\nnote = \"import pkg.settings as settings\"\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "d.py",
                        "from pkg import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "explicit_alias_equal_to_leaf",
                &[
                    ("pkg/__init__.py", ""),
                    (
                        "pkg/settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import pkg.settings as settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "d.py",
                        "from pkg import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "one_line_main_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "run.py",
                        "import settings\nfrom decoy import Decoy\n\nif __name__ == \"__main__\": settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "globals_in_main_block",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nif __name__ == \"__main__\":\n    globals()[\"conn\"] = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "vars_in_function_is_locals",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef dump():\n    return vars()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "example_write_is_production",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "examples/wire.py",
                        "import settings\nfrom decoy import Decoy\n\ndef wire():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "conftest_write_is_test",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "pkg/conftest.py",
                        "import settings\nfrom decoy import Decoy\n\ndef fixture():\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "main_block_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "run.py",
                        "import settings\nfrom decoy import Decoy\n\nif __name__ == \"__main__\":\n    settings.conn = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "own_main_block_write",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nif __name__ == \"__main__\":\n    conn = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "globals_continued_key",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    globals()[\n        \"conn\"] = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "globals_alias",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\ndef reset():\n    g = globals()\n    g[\"conn\"] = Decoy()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &[],
            ),
            (
                "globals_in_string",
                &[(
                    "m.py",
                    "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\nNOTE = \"globals().update(mapping)\"\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                )],
                &["cb -> Store::fetch"],
            ),
            (
                "external_dict_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom decoy import Decoy\n\ndef reset():\n    settings.__dict__[\"conn\"] = Decoy()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "external_annotated_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom store import Store\n\ndef reset():\n    settings.conn: Store = Store()\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "external_parenthesized_write",
                &[
                    (
                        "settings.py",
                        "from store import Store\nfrom decoy import Decoy\nconn = None\n\ndef init():\n    global conn\n    conn = Store()\n",
                    ),
                    (
                        "reset.py",
                        "import settings\nfrom store import Store\n\ndef reset():\n    settings.conn = (Store())\n",
                    ),
                    (
                        "c.py",
                        "import settings\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
                    ),
                ],
                &["cb -> Store::fetch"],
            ),
            (
                "dotted_import_collision",
                &[
                    ("alpha/__init__.py", ""),
                    ("beta/__init__.py", ""),
                    (
                        "alpha/foo.py",
                        "class Client:\n    def fetch(self):\n        return 1\n",
                    ),
                    (
                        "beta/foo.py",
                        "class Client:\n    def fetch(self):\n        return 2\n",
                    ),
                    (
                        "m.py",
                        "import beta.foo\nimport alpha.foo\nconn = None\n\ndef init():\n    global conn\n    conn = alpha.foo.Client()\n\ndef cb(pool):\n    pool.submit(conn.fetch)\n",
                    ),
                ],
                &[],
            ),
            (
                "dotted_base_collision",
                &[
                    ("alpha/__init__.py", ""),
                    ("beta/__init__.py", ""),
                    (
                        "alpha/foo.py",
                        "class Client:\n    def fetch(self):\n        return 1\n",
                    ),
                    (
                        "beta/foo.py",
                        "class Client:\n    def fetch(self):\n        return 2\n",
                    ),
                    (
                        "m.py",
                        "import beta.foo\nimport alpha.foo\n\nclass Sub(alpha.foo.Client):\n    pass\n\ndef go(pool, s: Sub):\n    pool.submit(s.fetch)\n",
                    ),
                ],
                &[],
            ),
        ];
        let mut mismatches = Vec::new();
        for (name, files, want) in cases {
            let mut project = Project::named(name)
                .file(
                    "store.py",
                    "class Store:\n    def fetch(self, ids):\n        return ids\n",
                )
                .file(
                    "decoy.py",
                    "class Decoy:\n    def fetch(self, ids):\n        return []\n",
                );
            for (path, contents) in *files {
                project = project.file(path, contents);
            }
            let indexed = project.index();
            let got = fn_refs_into(indexed.graph(), "fetch");
            if got != *want {
                mismatches.push(format!("{name}: got {got:?}, want {want:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// Upstream #2228 (`92f141f0`): a pytest test's parameter is the class
    /// its in-reach fixture returns (`return Cls(…)`, or a local assigned
    /// `Cls(…)`), so a call on it is that class's method, and none of the
    /// project's when the class comes from outside. A fixture is in reach
    /// from its own module, a `conftest.py` above the test, or a module such
    /// a conftest star-imports or lists in `pytest_plugins`.
    #[test]
    fn pr2228_a_pytest_parameter_is_the_class_its_fixture_returns() {
        let project = Project::new()
            .file(
                "app/cli.py",
                "class AlembicGroup:\n    def invoke(self, ctx):\n        return ctx\n",
            )
            .file(
                "app/models.py",
                "class Forum:\n    def save(self):\n        return self\n\n\nclass Topic:\n    def save(self):\n        return self\n\n\nclass Board:\n    def pin(self):\n        return self\n\n\nclass Stray:\n    def pin(self):\n        return self\n",
            )
            .file(
                "tests/conftest.py",
                "from tests.fixtures.cli import *  # noqa\nfrom tests.fixtures.forum import *  # noqa\n\npytest_plugins = [\"tests.fixtures.plugged\"]\n",
            )
            .file(
                "tests/fixtures/cli.py",
                "import pytest\nfrom click.testing import CliRunner\n\n\n@pytest.fixture\ndef runner():\n    return CliRunner()\n",
            )
            .file(
                "tests/fixtures/forum.py",
                "import pytest\nfrom app.models import Forum\n\n\n@pytest.fixture\ndef account():\n    forum = Forum()\n    return forum\n",
            )
            .file(
                "tests/fixtures/plugged.py",
                "import pytest\nfrom app.models import Board\n\n\n@pytest.fixture\ndef board():\n    return Board()\n",
            )
            .file(
                "tests/fixtures/stray.py",
                "import pytest\nfrom app.models import Stray\n\n\n@pytest.fixture\ndef loose():\n    return Stray()\n",
            )
            .file(
                "tests/test_cli.py",
                "def test_routes(runner):\n    result = runner.invoke(cli, [\"routes\"])\n    return result\n\n\ndef test_forum(account):\n    account.save()\n\n\ndef test_board(board):\n    board.pin()\n\n\ndef test_loose(loose):\n    loose.pin()\n",
            )
            .file(
                "tests/test_local.py",
                "import pytest\nfrom app.models import Topic\n\n\n@pytest.fixture\ndef thread():\n    return Topic()\n\n\ndef test_thread(thread):\n    thread.save()\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "test_routes"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "test_forum"),
            ["app/models.py: Forum::save"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "test_board"),
            ["app/models.py: Board::pin"],
            "{}",
            graph.report()
        );
        // A module no conftest pulls in is out of reach.
        assert_eq!(
            calls_of(graph, "test_loose"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "test_thread"),
            ["app/models.py: Topic::save"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2175 (`c0a38a87`), its Python part: `x =
    /// Model.objects.create|get|first|last|latest|earliest(…)` makes `x` a
    /// `Model`, a Django manager call returning one instance; `filter` and
    /// `all` return a QuerySet and type nothing.
    #[test]
    fn pr2175_a_django_manager_call_types_its_result() {
        let project = Project::new()
            .file(
                "app/models.py",
                "from django.db import models\n\n\nclass Site(models.Model):\n    def save(self, *args, **kwargs):\n        super().save(*args, **kwargs)\n\n\nclass Rack(models.Model):\n    def save(self, *args, **kwargs):\n        super().save(*args, **kwargs)\n",
            )
            .file(
                "tests/test_sites.py",
                "from app.models import Site\n\n\ndef test_site():\n    site = Site.objects.create(name=\"a\")\n    site.save()\n\n\ndef test_record():\n    record = Site.objects.get(pk=1)\n    record.save()\n\n\ndef test_many():\n    rows = Site.objects.filter(name=\"a\")\n    rows.save()\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "test_site"),
            ["app/models.py: Site::save"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "test_record"),
            ["app/models.py: Site::save"],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "test_many"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2169 (`791ae397`), its Python part: a keyword argument
    /// inside a call (`prefix=IPNetwork(…),`) is not a binding of `prefix`;
    /// the statement above it is.
    #[test]
    fn pr2169_a_python_keyword_argument_is_no_binding() {
        let project = Project::new()
            .file(
                "app/models.py",
                "class Prefix:\n    def get_available_ip_count(self):\n        return 0\n",
            )
            .file(
                "app/tests.py",
                "from app.models import Prefix\nfrom netaddr import IPNetwork\n\n\ndef test_count():\n    prefix = Prefix(\n        prefix=IPNetwork('192.0.2.0/24'),\n    )\n    return prefix.get_available_ip_count()\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "test_count"),
            ["app/models.py: Prefix::get_available_ip_count"],
            "{}",
            graph.report()
        );
    }
}

mod cpp {
    use super::support::{Graph, Project, assert_edge, codegraph};

    /// The qualified names of `file`'s nodes of `kind`, sorted.
    fn qualified(graph: &Graph, file: &str, kind: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == file && node.kind == kind)
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// The qualified names of `file`'s nodes that contain one of `parts`.
    fn named_after(graph: &Graph, file: &str, parts: &[&str]) -> Vec<String> {
        graph
            .nodes
            .iter()
            .filter(|node| node.file_path == file)
            .filter(|node| parts.iter().any(|part| node.qualified_name.contains(part)))
            .map(|node| node.qualified_name.clone())
            .collect()
    }

    const MESSAGE_H: &str = r#"namespace google {
namespace protobuf {

class Message {
 public:
  virtual ~Message();
};

}  // namespace protobuf
}  // namespace google
"#;
    const PB_H: &str = r#"// Generated by the protocol buffer compiler.  DO NOT EDIT!
#ifndef GOOGLE_PROTOBUF_INCLUDED_any_2eproto
#define GOOGLE_PROTOBUF_INCLUDED_any_2eproto

#include "google/protobuf/message.h"

namespace google {
namespace protobuf {

class PROTOBUF_EXPORT  PROTOBUF_FUTURE_ADD_EARLY_WARN_UNUSED Any final : public ::google::protobuf::Message
/* @@protoc_insertion_point(class_definition:google.protobuf.Any) */ {
 public:
  Any() : Any(nullptr) {}
  ~Any() PROTOBUF_FINAL;

  [[nodiscard]] const ::google::protobuf::UnknownFieldSet& unknown_fields() const
      ABSL_ATTRIBUTE_LIFETIME_BOUND {
    return _internal_metadata_.unknown_fields();
  }
  [[nodiscard]] ::google::protobuf::UnknownFieldSet* PROTOBUF_NONNULL mutable_unknown_fields()
      ABSL_ATTRIBUTE_LIFETIME_BOUND {
    return _internal_metadata_.mutable_unknown_fields();
  }

  [[nodiscard]] static const ::google::protobuf::Descriptor* PROTOBUF_NONNULL descriptor() {
    return GetDescriptor();
  }
  [[nodiscard]] static const ::google::protobuf::Descriptor* PROTOBUF_NONNULL
  GetDescriptor() {
    return default_instance().GetMetadata().descriptor;
  }
  [[nodiscard]] static const ::google::protobuf::Reflection* PROTOBUF_NONNULL GetReflection() {
    return default_instance().GetMetadata().reflection;
  }
  void Swap(Any* PROTOBUF_NONNULL other) {
    InternalSwap(other);
  }
  [[nodiscard]] ::std::string* PROTOBUF_NONNULL mutable_type_url();

 private:
  void InternalSwap(Any* PROTOBUF_NONNULL other);
};

inline ::std::string* PROTOBUF_NONNULL Any::mutable_type_url() {
  return _impl_.type_url_.Mutable();
}

}  // namespace protobuf
}  // namespace google

#endif  // GOOGLE_PROTOBUF_INCLUDED_any_2eproto
"#;
    const PB_CC: &str = r#"#include "google/protobuf/any.pb.h"

namespace google {
namespace protobuf {

PROTOBUF_ATTRIBUTE_NO_DESTROY PROTOBUF_CONSTINIT PROTOBUF_EXPORT
    PROTOBUF_ATTRIBUTE_INIT_PRIORITY1 const AnyGlobalsTypeInternal Any_globals_
        PROTOBUF_MESSAGE_GLOBALS_SECTION(.data.rel.ro);

void Any::InternalSwap(Any* PROTOBUF_RESTRICT PROTOBUF_NONNULL other) {
  _impl_.type_url_.InternalSwap(&other->_impl_.type_url_);
}

}  // namespace protobuf
}  // namespace google
"#;
    const REPEATED_H: &str = r#"#include <cstddef>

namespace google {
namespace protobuf {

template <typename Element>
class ABSL_ATTRIBUTE_WARN_UNUSED PROTOBUF_DECLSPEC_EMPTY_BASES
    RepeatedField final
    : private internal::RepeatedFieldBase {
 public:
  int size() const { return size_; }

 private:
  int size_;
};

}  // namespace protobuf
}  // namespace google
"#;
    const DESCRIPTOR_H: &str = r#"#include <string>

namespace google {
namespace protobuf {

class PROTOBUF_EXPORT Descriptor : private internal::SymbolBase {
 public:
  PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view name() const {
    return all_names_[0];
  }
  PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view full_name() const {
    return all_names_[1];
  }
  PROTOBUF_FUTURE_ADD_EARLY_NODISCARD const FileDescriptor* file() const;
};

}  // namespace protobuf
}  // namespace google
"#;
    const FMT_H: &str = r#"#include <string>

#define FMT_BEGIN_EXPORT
FMT_BEGIN_NAMESPACE

FMT_BEGIN_EXPORT

// A generic formatting context with custom output iterator and character
// (code unit) support.
template <typename OutputIt, typename Char> class generic_context {
 private:
  OutputIt out_;

 public:
  constexpr auto out() -> OutputIt { return out_; }
};

FMT_END_EXPORT
FMT_END_NAMESPACE
"#;
    const LEVELDB_CC: &str = r#"#include "port/port.h"

namespace leveldb {

class Counter {
 public:
  void IncrementBy(int count) LOCKS_EXCLUDED(mu_) {
    MutexLock l(&mu_);
    count_ += count;
  }
  void Lock() EXCLUSIVE_LOCK_FUNCTION() { mu_.Lock(); }

 private:
  port::Mutex mu_;
  int count_ GUARDED_BY(mu_);
};

}  // namespace leveldb
"#;

    /// Upstream #2449 (`cdb994b4`), with the passes it builds on (#1070,
    /// #1100, #1346): classes written around attribute macros keep their
    /// members. Several macros, or one with arguments, between `class` and the
    /// name; a macro between a pointer and the declared name, after a
    /// parameter list, after a declared name, or opening a declaration; a lone
    /// macro line with a comment under it. Before the C++ pre-parse blanked
    /// them, protobuf's members were namespace-level functions named after
    /// the macros, and leveldb's annotated methods phantoms named after the
    /// annotation.
    #[test]
    fn pr2449_classes_written_around_attribute_macros_keep_their_members() {
        let project = Project::new()
            .file("src/google/protobuf/message.h", MESSAGE_H)
            .file("src/google/protobuf/any.pb.h", PB_H)
            .file("src/google/protobuf/any.pb.cc", PB_CC)
            .file("src/google/protobuf/repeated_field.h", REPEATED_H)
            .file("src/google/protobuf/descriptor.h", DESCRIPTOR_H)
            .file("include/fmt/format.h", FMT_H)
            .file("db/counter.cc", LEVELDB_CC)
            .index();
        let graph = project.graph();
        let pb_h = "src/google/protobuf/any.pb.h";
        assert_eq!(
            qualified(graph, pb_h, "class"),
            ["google::protobuf::Any"],
            "{}",
            graph.report()
        );
        let methods = qualified(graph, pb_h, "method");
        for method in [
            "google::protobuf::Any::Any",
            "google::protobuf::Any::GetDescriptor",
            "google::protobuf::Any::GetReflection",
            "google::protobuf::Any::Swap",
            "google::protobuf::Any::descriptor",
            "google::protobuf::Any::mutable_type_url",
            "google::protobuf::Any::mutable_unknown_fields",
            "google::protobuf::Any::unknown_fields",
        ] {
            assert!(
                methods.iter().any(|name| name == method),
                "{method}: {methods:?}"
            );
        }
        // No member is left at namespace level, and no node is named after a macro.
        assert_eq!(qualified(graph, pb_h, "function"), Vec::<String>::new());
        assert_eq!(
            named_after(graph, pb_h, &["PROTOBUF_", "ABSL_"]),
            Vec::<String>::new()
        );
        // The base clause is read, and `::google::protobuf::Message` is found
        // from the global scope (upstream #2407, W4-CPP-05).
        assert_edge!(project, "class google::protobuf::Any" => extends => "class google::protobuf::Message");
        assert_eq!(
            qualified(graph, "src/google/protobuf/any.pb.cc", "method"),
            ["google::protobuf::Any::InternalSwap"]
        );
        let repeated = "src/google/protobuf/repeated_field.h";
        assert_eq!(
            qualified(graph, repeated, "class"),
            ["google::protobuf::RepeatedField"]
        );
        assert_eq!(
            qualified(graph, repeated, "method"),
            ["google::protobuf::RepeatedField::size"]
        );
        assert_eq!(
            qualified(graph, "src/google/protobuf/descriptor.h", "method"),
            [
                "google::protobuf::Descriptor::full_name",
                "google::protobuf::Descriptor::name"
            ]
        );
        assert_eq!(
            qualified(graph, "include/fmt/format.h", "method"),
            ["generic_context::out"]
        );
        assert_eq!(
            qualified(graph, "include/fmt/format.h", "function"),
            Vec::<String>::new()
        );
        assert_eq!(
            qualified(graph, "db/counter.cc", "method"),
            ["leveldb::Counter::IncrementBy", "leveldb::Counter::Lock"]
        );
        assert_eq!(
            named_after(graph, "db/counter.cc", &["LOCKS", "GUARDED"]),
            Vec::<String>::new()
        );
    }

    /// Upstream #2449 / OD-5 seed: rocksdb's `struct ALIGN_AS(64U) HandleImpl
    /// : public ClockHandle` is the struct `HandleImpl` with its base and
    /// members; no node named after the macro's arguments (`(64U)
    /// HandleImpl`) is minted.
    #[test]
    fn pr2449_a_class_head_macro_with_arguments_mints_no_pseudo_node() {
        let project = Project::new()
            .file(
                "cache/clock_cache.h",
                "namespace rocksdb {\n\nstruct ClockHandle {\n  int refs;\n};\n\nstruct ALIGN_AS(64U) HandleImpl : public ClockHandle {\n  void Reset() { refs = 0; }\n};\n\n}  // namespace rocksdb\n",
            )
            .index();
        let graph = project.graph();
        let file = "cache/clock_cache.h";
        let mut types = qualified(graph, file, "struct");
        types.extend(qualified(graph, file, "class"));
        types.sort();
        assert_eq!(
            types,
            ["rocksdb::ClockHandle", "rocksdb::HandleImpl"],
            "{}",
            graph.report()
        );
        assert_eq!(
            qualified(graph, file, "method"),
            ["rocksdb::HandleImpl::Reset"]
        );
        assert_eq!(
            named_after(graph, file, &["(64U)", "ALIGN_AS"]),
            Vec::<String>::new()
        );
    }

    const JSON_HPP: &str = r#"#pragma once
#define JSON_PRIVATE_UNLESS_TESTED private
#define JSON_HEDLEY_RETURNS_NON_NULL
#define JSON_HEDLEY_WARN_UNUSED_RESULT

template<typename T>
class basic_json
{
  JSON_PRIVATE_UNLESS_TESTED:
    using lexer = int;

    template<typename U, typename... Args>
    JSON_HEDLEY_RETURNS_NON_NULL
    static U* create(Args&& ... args)
    {
        return nullptr;
    }

    class nesting_depth_guard
    {
      public:
        nesting_depth_guard() noexcept
#ifdef JSON_NO_THREAD_LOCAL
            : m_okay(false)
#else
            : m_okay(depth() < 10)
#endif
        {
        }
      private:
        bool m_okay;
    };

  public:
    /// @brief returns version information on the library
    /// @sa https://json.nlohmann.me/api/basic_json/meta/
    JSON_HEDLEY_WARN_UNUSED_RESULT
    static basic_json meta()
    {
        return basic_json();
    }

    template < typename ValueType >
#if defined(JSON_HAS_CPP_14)
    constexpr
#endif
    auto get() const -> ValueType
    {
        return ValueType();
    }

    void dump() const {}
};
"#;

    /// Upstream #2186 (`5c3aa92f`): nlohmann/json's `basic_json` keeps its
    /// members although it spells an access specifier as a macro
    /// (`JSON_PRIVATE_UNLESS_TESTED:`), puts lone attribute macros after a
    /// template head and after a `/// @sa https://…/` comment, and opens a
    /// preprocessor conditional inside a member-initializer list and between
    /// a template head and its function.
    #[test]
    fn pr2186_a_class_around_macros_and_conditionals_keeps_its_members() {
        let project = Project::new().file("include/json.hpp", JSON_HPP).index();
        let graph = project.graph();
        let members: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.file_path == "include/json.hpp"
                    && node.qualified_name.starts_with("basic_json::")
                    && (node.kind == "method" || node.kind == "class")
            })
            .map(|node| node.name.as_str())
            .collect();
        for member in ["create", "dump", "get", "meta", "nesting_depth_guard"] {
            assert!(
                members.contains(&member),
                "{member}: {members:?}\n{}",
                graph.report()
            );
        }
    }

    /// Upstream #2446 (`235387ec`): a struct or class defined in the type of
    /// a variable declaration is a node, so calls on its methods link and a
    /// base class defined this way has a node to resolve to; an unnamed
    /// struct is found by its variable's name, documented by the comment
    /// above the declaration. Before, each declaration was handed whole to
    /// the variable extractor and the type never became a node.
    #[test]
    fn pr2446_types_defined_in_declarations_link_and_are_found() {
        let project = Project::new()
            .file(
                "src/registry.cc",
                "namespace app {\nstruct Registry { int size() const { return 2; } } registry;\nclass Base { public: virtual int run() { return 1; } } base_instance;\nclass Worker : public Base { public: int run() override { return registry.size(); } };\nint tick() { Worker w; return w.run() + registry.size(); }\n}\n",
            )
            .file(
                "src/setproctitle.c",
                "/* Saved argv. */\nstatic struct { int argc; char **argv; } SPT;\nint spt_init(int argc) { SPT.argc = argc; return SPT.argc; }\n",
            )
            .index();
        assert_edge!(project, "method app::Worker::run" => calls => "method app::Registry::size");
        assert_edge!(project, "function app::tick" => calls => "method app::Registry::size");
        assert_edge!(project, "class app::Worker" => extends => "class app::Base");

        let output = codegraph(
            project.root(),
            ["search", "SPT", "-p", ".", "--limit", "5", "--json"],
        );
        let results: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("search --json output");
        let found: Vec<(String, String, Option<String>)> = results
            .as_array()
            .expect("search results")
            .iter()
            .map(|result| {
                let node = &result["node"];
                (
                    node["kind"].as_str().unwrap_or_default().to_string(),
                    node["name"].as_str().unwrap_or_default().to_string(),
                    node["docstring"].as_str().map(str::to_string),
                )
            })
            .collect();
        assert!(
            found.contains(&(
                "struct".to_string(),
                "SPT".to_string(),
                Some("Saved argv.".to_string())
            )),
            "{found:?}"
        );
    }

    /// `calls` edges of every method or function qualified `caller` (a
    /// prototype and its definition alike), as `qualified (file)`, sorted.
    fn callees(graph: &Graph, caller: &str) -> Vec<String> {
        calls_touching(graph, caller, false)
    }

    /// `calls` edges into every method or function qualified `callee`, as
    /// `qualified (file)` of the caller, sorted.
    fn callers(graph: &Graph, callee: &str) -> Vec<String> {
        calls_touching(graph, callee, true)
    }

    fn calls_touching(graph: &Graph, qualified: &str, incoming: bool) -> Vec<String> {
        let ids: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.qualified_name == qualified
                    && (node.kind == "method" || node.kind == "function")
            })
            .map(|node| node.id.as_str())
            .collect();
        assert!(!ids.is_empty(), "no function or method {qualified}");
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == "calls"
                    && ids.contains(&if incoming {
                        edge.target.as_str()
                    } else {
                        edge.source.as_str()
                    })
            })
            .filter_map(|edge| graph.node(if incoming { &edge.source } else { &edge.target }))
            .map(|node| format!("{} ({})", node.qualified_name, node.file_path))
            .collect();
        out.sort();
        out
    }

    /// google/leveldb's shapes, trimmed (upstream #2399's fixture): the skip
    /// list, the unrelated `Table`, and the decoy `Insert`.
    const LEVELDB_ALIAS: &[(&str, &str)] = &[
        (
            "include/leveldb/table.h",
            "namespace leveldb {\nclass Table {\n public:\n  explicit Table(int* rep);\n  int ApproximateOffsetOf(int key) const;\n};\n}  // namespace leveldb\n",
        ),
        (
            "util/cache.cc",
            "namespace leveldb {\nclass HandleTable {\n public:\n  void Insert(int h) { last_ = h; }\n private:\n  int last_;\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/skiplist.h",
            "namespace leveldb {\ntemplate <typename Key, class Comparator>\nclass SkipList {\n public:\n  explicit SkipList(Comparator cmp, int* arena);\n  void Insert(const Key& key);\n  class Iterator {\n   public:\n    explicit Iterator(const SkipList* list);\n    void Seek(const Key& target);\n  };\n};\ntemplate <typename Key, class Comparator>\nSkipList<Key, Comparator>::SkipList(Comparator cmp, int* arena) {}\ntemplate <typename Key, class Comparator>\nvoid SkipList<Key, Comparator>::Insert(const Key& key) {}\ntemplate <typename Key, class Comparator>\nSkipList<Key, Comparator>::Iterator::Iterator(const SkipList* list) {}\ntemplate <typename Key, class Comparator>\nvoid SkipList<Key, Comparator>::Iterator::Seek(const Key& target) {}\n}  // namespace leveldb\n",
        ),
        (
            "db/memtable.h",
            "#include \"db/skiplist.h\"\nnamespace leveldb {\nstruct KeyComparator {\n  int operator()(const char* a, const char* b) const;\n};\nclass MemTable {\n public:\n  void Add(const char* buf);\n  bool Get(const char* key);\n  void Rebuild(int* arena);\n  void Index(const char* key);\n private:\n  typedef SkipList<const char*, KeyComparator> Table;\n  using Lookup = Table;\n  Table table_;\n  Lookup lookup_;\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/memtable.cc",
            "#include \"db/memtable.h\"\nnamespace leveldb {\nvoid MemTable::Add(const char* buf) {\n  table_.Insert(buf);\n}\nbool MemTable::Get(const char* key) {\n  Table::Iterator iter(&table_);\n  iter.Seek(key);\n  return true;\n}\nvoid MemTable::Rebuild(int* arena) {\n  KeyComparator cmp;\n  Table fresh(cmp, &arena[0]);\n  Table stale(&arena[1]);\n}\nvoid MemTable::Index(const char* key) {\n  lookup_.Insert(key);\n}\n}  // namespace leveldb\n",
        ),
    ];

    fn leveldb_alias_project(extra: &[(&str, &str)]) -> Project {
        LEVELDB_ALIAS
            .iter()
            .chain(extra)
            .fold(Project::new(), |project, (path, text)| {
                project.file(path, text)
            })
    }

    /// Upstream #2399 (`02d22aeb`): a C++ receiver declared through a
    /// `typedef` / `using` alias calls the type the alias names. leveldb's
    /// `table_.Insert(buf)` on `Table table_;` beside `typedef SkipList<const
    /// char*, KeyComparator> Table;` reaches `SkipList::Insert`, not
    /// `HandleTable::Insert` guessed by the receiver's name; `Table::Iterator`
    /// constructs and calls `SkipList::Iterator`; `Table fresh(cmp, …)`
    /// constructs `SkipList`, never the outer `leveldb::Table` the alias
    /// hides; an alias of an alias is followed.
    #[test]
    fn pr2399_a_receiver_declared_through_an_alias_calls_the_aliased_type() {
        let project = leveldb_alias_project(&[]).index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "leveldb::MemTable::Add"),
            ["leveldb::SkipList::Insert (db/skiplist.h)"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callers(graph, "leveldb::HandleTable::Insert"),
            Vec::<String>::new()
        );
        assert_eq!(
            callees(graph, "leveldb::MemTable::Get"),
            [
                "leveldb::SkipList::Iterator::Iterator (db/skiplist.h)",
                "leveldb::SkipList::Iterator::Seek (db/skiplist.h)",
            ]
        );
        // `Table stale(&arena[1])` fits no SkipList constructor, and
        // `leveldb::Table(int*)` is not the `Table` this scope names.
        assert_eq!(
            callees(graph, "leveldb::MemTable::Rebuild"),
            ["leveldb::SkipList::SkipList (db/skiplist.h)"]
        );
        assert_eq!(
            callers(graph, "leveldb::Table::Table"),
            Vec::<String>::new()
        );
        assert_eq!(
            callees(graph, "leveldb::MemTable::Index"),
            ["leveldb::SkipList::Insert (db/skiplist.h)"]
        );
    }

    /// Upstream #2399: an alias spelled from outside its class
    /// (`MemTable::Table* t`), and one declared in the calling function.
    #[test]
    fn pr2399_an_alias_spelled_from_outside_its_class_or_declared_in_the_function() {
        let project = leveldb_alias_project(&[(
            "db/builder.cc",
            "#include \"db/memtable.h\"\nnamespace leveldb {\nvoid Fill(MemTable::Table* t) {\n  t->Insert(\"k\");\n}\nvoid Build(int* arena) {\n  typedef SkipList<const char*, KeyComparator> List;\n  KeyComparator cmp;\n  List list(cmp, &arena[0]);\n  list.Insert(\"k\");\n}\n}  // namespace leveldb\n",
        )])
        .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "leveldb::Fill"),
            ["leveldb::SkipList::Insert (db/skiplist.h)"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callees(graph, "leveldb::Build"),
            [
                "leveldb::SkipList::Insert (db/skiplist.h)",
                "leveldb::SkipList::SkipList (db/skiplist.h)",
            ]
        );
    }

    /// Upstream #2399: no method is guessed by the receiver's name when the
    /// aliased type lacks it: `files_` is a `std::vector`, not `Slice::clear`,
    /// the one project `clear`, also when the call continues on the next line
    /// (protobuf's `symbols_by_parent_` / `.insert(…)`).
    #[test]
    fn pr2399_no_guess_when_the_aliased_type_lacks_the_method() {
        let project = leveldb_alias_project(&[
            (
                "include/leveldb/slice.h",
                "namespace leveldb {\nclass Slice {\n public:\n  void clear() { size_ = 0; }\n private:\n  int size_;\n};\n}  // namespace leveldb\n",
            ),
            (
                "db/version_set.h",
                "#include <vector>\nnamespace leveldb {\nclass VersionSet {\n public:\n  void Clear();\n private:\n  using Table = std::vector<int>;\n  Table files_;\n};\n}  // namespace leveldb\n",
            ),
            (
                "db/version_set.cc",
                "#include \"db/version_set.h\"\nnamespace leveldb {\nvoid VersionSet::Clear() {\n  files_.clear();\n}\n}  // namespace leveldb\n",
            ),
        ])
        .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "leveldb::VersionSet::Clear"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );

        let protobuf = Project::new()
            .file(
                "src/descriptor.cc",
                "#include \"absl/container/flat_hash_set.h\"\nnamespace google { namespace protobuf {\nclass Arena {\n public:\n  void insert(int block) {}\n};\nclass FileDescriptorTables {\n public:\n  bool AddAliasUnderParent(int symbol);\n private:\n  typedef absl::flat_hash_set<int> SymbolsByParentSet;\n  SymbolsByParentSet symbols_by_parent_;\n};\nbool FileDescriptorTables::AddAliasUnderParent(int symbol) {\n  return symbols_by_parent_\n      .insert(symbol)\n      .second;\n}\n} }\n",
            )
            .index();
        let graph = protobuf.graph();
        assert_eq!(
            callees(
                graph,
                "google::protobuf::FileDescriptorTables::AddAliasUnderParent"
            ),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399: a class declared in an inner scope hides an outer
    /// alias of the same name; outside it, the alias applies.
    #[test]
    fn pr2399_an_inner_class_hides_an_outer_alias_of_its_name() {
        let project = Project::new()
            .file(
                "app/holder.h",
                "namespace app {\nclass Gadget {\n public:\n  void Spin();\n};\nusing Widget = Gadget;\nclass Holder {\n public:\n  class Widget {\n   public:\n    void Spin();\n  };\n  void Use();\n private:\n  Widget w_;\n};\n}  // namespace app\n",
            )
            .file(
                "app/holder.cc",
                "#include \"app/holder.h\"\nnamespace app {\nvoid Gadget::Spin() {}\nvoid Holder::Widget::Spin() {}\nvoid Holder::Use() {\n  w_.Spin();\n}\nvoid Twirl(Widget& w) {\n  w.Spin();\n}\n}  // namespace app\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "app::Holder::Use"),
            ["app::Holder::Widget::Spin (app/holder.cc)"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callees(graph, "app::Twirl"),
            ["app::Gadget::Spin (app/holder.cc)"]
        );
    }

    /// Upstream #2399: an alias a class inherits is found in the base class
    /// before an enclosing namespace's alias template, and a dependent alias
    /// (`typename Traits::Field`) is not followed (protobuf's json traits).
    #[test]
    fn pr2399_an_inherited_alias_is_found_before_the_namespace() {
        let project = Project::new()
            .file(
                "protobuf/descriptor.h",
                "namespace google { namespace protobuf {\nclass FieldDescriptor {\n public:\n  bool is_repeated() const;\n};\ninline bool FieldDescriptor::is_repeated() const { return false; }\nclass OneofDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\n} }\n",
            )
            .file(
                "protobuf/json/traits.h",
                "#include \"protobuf/descriptor.h\"\nnamespace google { namespace protobuf { namespace json_internal {\ntemplate <typename Traits>\nusing Field = typename Traits::Field;\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\nstruct ParseProto2Descriptor : Proto2Descriptor {\n  static bool NewMsg(Field f) { return f->is_repeated(); }\n};\n} } }\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(
                graph,
                "google::protobuf::json_internal::ParseProto2Descriptor::NewMsg"
            ),
            ["google::protobuf::FieldDescriptor::is_repeated (protobuf/descriptor.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399: an alias of a template parameter names no type, so a
    /// class that shares the alias's name is not it (protobuf's
    /// `GenericTypeHandler` `using Type = GenericType;` beside the generated
    /// `Type` message).
    #[test]
    fn pr2399_an_alias_of_a_template_parameter_names_no_type() {
        let project = Project::new()
            .file(
                "protobuf/type.pb.h",
                "namespace google { namespace protobuf {\nclass Type {\n public:\n  void Clear();\n};\ninline void Type::Clear() {}\n} }\n",
            )
            .file(
                "protobuf/repeated_ptr_field.h",
                "namespace google { namespace protobuf { namespace internal {\ntemplate <typename GenericType>\nclass GenericTypeHandler {\n public:\n  using Type = GenericType;\n  static void Reset(Type* value) { value->Clear(); }\n};\n} } }\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(
                graph,
                "google::protobuf::internal::GenericTypeHandler::Reset"
            ),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399: `->` through an alias of an iterator reaches the
    /// element type, which the alias does not name, so the alias rule leaves
    /// the call to the guesses (gmock's `ExpectationSet::const_iterator`, a
    /// `std::set<Expectation>` iterator). Upstream's receiver is `it`; here
    /// it shares a word with the method's class, since a lone same-named
    /// method is guessed only then (KEEP-RUST #226).
    #[test]
    fn pr2399_an_arrow_through_an_iterator_alias_still_reaches_a_guess() {
        let project = Project::new()
            .file(
                "gmock/expectation.h",
                "#include <set>\nnamespace testing {\nclass Expectation {\n public:\n  int expectation_base() const { return 0; }\n};\nclass ExpectationSet {\n public:\n  typedef ::std::set<Expectation> Set;\n  typedef Set::const_iterator const_iterator;\n};\nint Describe(const ExpectationSet& set) {\n  ExpectationSet::const_iterator expectation_it;\n  return expectation_it->expectation_base();\n}\n}  // namespace testing\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "testing::Describe"),
            ["testing::Expectation::expectation_base (gmock/expectation.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399: a class template's specialization can declare a member
    /// the template itself does not (rocksdb's `omt_node_templated<omtdata_t,
    /// true>` adds `get_marked`), so an alias of a project class template
    /// does not rule a call out. The specialization is indexed as
    /// `<anonymous>`, as upstream indexes it, so the call is a guess by the
    /// method's name; upstream's receiver is `node`, and here it shares a word
    /// with the method, since a lone same-named method is guessed only then
    /// (KEEP-RUST #226).
    #[test]
    fn pr2399_an_alias_of_a_class_template_does_not_rule_a_call_out() {
        let project = Project::new()
            .file(
                "util/omt.h",
                "namespace toku {\nnamespace omt_internal {\ntemplate <typename omtdata_t, bool subtree_supports_marks>\nclass omt_node_templated {\n public:\n  void clear_stolen_bits() {}\n};\ntemplate <typename omtdata_t>\nclass omt_node_templated<omtdata_t, true> {\n public:\n  bool get_marked() const { return false; }\n};\n}  // namespace omt_internal\ntemplate <typename omtdata_t, bool supports_marks = false>\nclass omt {\n public:\n  bool has_marks() const;\n private:\n  typedef omt_internal::omt_node_templated<omtdata_t, supports_marks> omt_node;\n  omt_node root_;\n};\ntemplate <typename omtdata_t, bool supports_marks>\nbool omt<omtdata_t, supports_marks>::has_marks() const {\n  const omt_node &marked_node = root_;\n  return marked_node.get_marked();\n}\n}  // namespace toku\n",
            )
            .index();
        let graph = project.graph();
        let names: Vec<String> = callees(graph, "toku::omt::has_marks")
            .into_iter()
            .map(|callee| {
                callee
                    .split(" (")
                    .next()
                    .and_then(|qualified| qualified.rsplit("::").next())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(names, ["get_marked"], "{}", graph.report());
    }

    /// Upstream #2399: a declaration read from another class is not known to
    /// be the receiver's (fmt's scan.h: `auto data = scan_data<T...>()`
    /// deduces nothing the reader can see, and the `data` it then finds is
    /// another class's `const scan_arg* data;`).
    #[test]
    fn pr2399_a_declaration_read_from_another_class_is_not_the_receivers() {
        let project = Project::new()
            .file(
                "test/scan.h",
                "namespace fmt {\nclass basic_scan_arg {\n public:\n  int type() const { return 0; }\n};\nusing scan_arg = basic_scan_arg;\nclass scan_args {\n  const scan_arg* data;\n};\ntemplate <typename... T> class scan_data {\n public:\n  int make_args() { return 0; }\n};\ntemplate <typename... T> int scan() {\n  auto data = scan_data<T...>();\n  return data.make_args();\n}\n}  // namespace fmt\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "fmt::scan"),
            ["fmt::scan_data::make_args (test/scan.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399's "no guess" cases hold here before the port too, since
    /// a lone same-named method is guessed only when the receiver shares a
    /// word with it (KEEP-RUST #226). With a decoy that does share one
    /// (`FilesIndex::clear` for `files_`, `ParentSymbols::insert` for
    /// `symbols_by_parent_`), the base guessed it; the aliased type lacking
    /// the method now rules the guess out.
    #[test]
    fn pr2399_no_guess_even_when_a_decoy_shares_the_receivers_word() {
        let project = Project::new()
            .file(
                "include/leveldb/files_index.h",
                "namespace leveldb {\nclass FilesIndex {\n public:\n  void clear() { size_ = 0; }\n private:\n  int size_;\n};\n}  // namespace leveldb\n",
            )
            .file(
                "db/version_set.h",
                "#include <vector>\nnamespace leveldb {\nclass VersionSet {\n public:\n  void Clear();\n private:\n  using Table = std::vector<int>;\n  Table files_;\n};\n}  // namespace leveldb\n",
            )
            .file(
                "db/version_set.cc",
                "#include \"db/version_set.h\"\nnamespace leveldb {\nvoid VersionSet::Clear() {\n  files_.clear();\n}\n}  // namespace leveldb\n",
            )
            .file(
                "src/descriptor.cc",
                "#include \"absl/container/flat_hash_set.h\"\nnamespace google { namespace protobuf {\nclass ParentSymbols {\n public:\n  void insert(int block) {}\n};\nclass FileDescriptorTables {\n public:\n  bool AddAliasUnderParent(int symbol);\n private:\n  typedef absl::flat_hash_set<int> SymbolsByParentSet;\n  SymbolsByParentSet symbols_by_parent_;\n};\nbool FileDescriptorTables::AddAliasUnderParent(int symbol) {\n  return symbols_by_parent_\n      .insert(symbol)\n      .second;\n}\n} }\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "leveldb::VersionSet::Clear"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            callees(
                graph,
                "google::protobuf::FileDescriptorTables::AddAliasUnderParent"
            ),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2399's alias choice: two headers declaring the same alias in
    /// the same scope for different types, neither the calling file's nor its
    /// header's, name no type to follow (an `#if` arm each, as rocksdb
    /// writes); the calling file's own declaration of it is the one its
    /// translation unit sees.
    #[test]
    fn pr2399_an_alias_two_headers_declare_differently_is_not_followed() {
        let ambiguous = Project::new()
            .file(
                "store/kinds.h",
                "namespace app {\nclass MemStore {\n public:\n  void Save() {}\n};\nclass DiskStore {\n public:\n  void Save() {}\n};\n}  // namespace app\n",
            )
            .file(
                "config/fast.h",
                "#include \"store/kinds.h\"\nnamespace app {\nusing Store = MemStore;\n}  // namespace app\n",
            )
            .file(
                "config/safe.h",
                "#include \"store/kinds.h\"\nnamespace app {\nusing Store = DiskStore;\n}  // namespace app\n",
            )
            .file(
                "src/use.cc",
                "#include \"config/fast.h\"\nnamespace app {\nvoid Run() {\n  Store store;\n  store.Save();\n}\n}  // namespace app\n",
            )
            .index();
        let graph = ambiguous.graph();
        assert_eq!(
            callees(graph, "app::Run"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );

        let own = Project::new()
            .file(
                "store/kinds.h",
                "namespace app {\nclass MemStore {\n public:\n  void Save() {}\n};\nclass DiskStore {\n public:\n  void Save() {}\n};\n}  // namespace app\n",
            )
            .file(
                "config/fast.h",
                "#include \"store/kinds.h\"\nnamespace app {\nusing Store = MemStore;\n}  // namespace app\n",
            )
            .file(
                "src/use.cc",
                "#include \"store/kinds.h\"\nnamespace app {\nusing Store = DiskStore;\nvoid Run() {\n  Store store;\n  store.Save();\n}\n}  // namespace app\n",
            )
            .index();
        let graph = own.graph();
        assert_eq!(
            callees(graph, "app::Run"),
            ["app::DiskStore::Save (store/kinds.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2407's fixture (`cpp-base-class-lookup.test.ts`): protobuf's
    /// `Message` beside a nested namesake and the PHP extension's C struct,
    /// leveldb's iterators and test comparator, rocksdb's nested bases,
    /// aliases and using-directive, googletest's global `::testing::Test`, a
    /// template parameter, and a using-directive only a header writes.
    const BASE_LOOKUP: &[(&str, &str)] = &[
        (
            "src/google/protobuf/message_lite.h",
            "namespace google {\nnamespace protobuf {\nnamespace internal {\nclass MessageGlobalsBase {\n public:\n  int default_instance() const { return 0; }\n};\n}  // namespace internal\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "src/google/protobuf/message.h",
            "#include \"google/protobuf/message_lite.h\"\nnamespace google {\nnamespace protobuf {\nclass Message {\n public:\n  virtual ~Message();\n  virtual int ByteSizeLong() const = 0;\n};\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "src/google/protobuf/json/internal/untyped_message.h",
            "namespace google {\nnamespace protobuf {\nnamespace json_internal {\nclass ResolverPool {\n public:\n  class Message {\n   public:\n    int ByteSizeLong() const { return 0; }\n  };\n};\n}  // namespace json_internal\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "php/ext/google/protobuf/message.c",
            "typedef struct {\n  int std;\n} Message;\n",
        ),
        (
            "src/google/protobuf/dynamic_message.cc",
            "#include \"google/protobuf/message.h\"\nnamespace google {\nnamespace protobuf {\nclass DynamicMessage final : public Message {\n public:\n  int ByteSizeLong() const override { return 1; }\n};\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "src/google/protobuf/generated_message_bases.h",
            "#include \"google/protobuf/message.h\"\nnamespace google {\nnamespace protobuf {\nnamespace internal {\nclass ZeroFieldsBase : public Message {\n public:\n  int ByteSizeLong() const override { return 0; }\n};\n}  // namespace internal\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "src/google/protobuf/any.pb.cc",
            "#include \"google/protobuf/message_lite.h\"\nnamespace _pbi = ::google::protobuf::internal;\nnamespace google {\nnamespace protobuf {\nstruct AnyGlobalsTypeInternal : ::_pbi::MessageGlobalsBase {\n  int x;\n};\n}  // namespace protobuf\n}  // namespace google\n",
        ),
        (
            "include/leveldb/iterator.h",
            "namespace leveldb {\nclass Iterator {\n public:\n  virtual ~Iterator();\n  virtual bool Valid() const = 0;\n  virtual void Next() = 0;\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/skiplist.h",
            "namespace leveldb {\ntemplate <typename Key, class Comparator>\nclass SkipList {\n public:\n  class Iterator {\n   public:\n    bool Valid() const { return node_ != nullptr; }\n    void Next() { node_ = nullptr; }\n    void SeekToFirst() { node_ = nullptr; }\n   private:\n    const char* node_;\n  };\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/db_iter.cc",
            "#include \"include/leveldb/iterator.h\"\nnamespace leveldb {\nclass DBIter : public Iterator {\n public:\n  bool Valid() const override { return valid_; }\n  void Next() override { valid_ = false; }\n private:\n  bool valid_;\n};\n}  // namespace leveldb\n",
        ),
        (
            "include/leveldb/comparator.h",
            "namespace leveldb {\nclass Comparator {\n public:\n  virtual int Compare(const char* a, const char* b) const = 0;\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/skiplist_test.cc",
            "namespace leveldb {\nstruct Comparator {\n  int operator()(const int& a, const int& b) const { return a - b; }\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/db_test.cc",
            "#include \"include/leveldb/comparator.h\"\nnamespace leveldb {\nclass NumberComparator : public Comparator {\n public:\n  int Compare(const char* a, const char* b) const override { return 0; }\n};\n}  // namespace leveldb\n",
        ),
        (
            "include/leveldb/filter_policy.h",
            "namespace leveldb {\nclass FilterPolicy {\n public:\n  virtual const char* Name() const = 0;\n};\n}  // namespace leveldb\n",
        ),
        (
            "db/c.cc",
            "#include \"include/leveldb/filter_policy.h\"\nusing leveldb::FilterPolicy;\nstruct leveldb_filterpolicy_t : public FilterPolicy {\n  const char* Name() const override { return \"c\"; }\n};\n",
        ),
        (
            "cache/clock_cache.h",
            "namespace rocksdb {\nnamespace clock_cache {\nclass BaseClockTable {\n public:\n  struct BaseOpts {\n    int eviction_effort_cap;\n  };\n};\nclass AutoHyperClockTable : public BaseClockTable {\n public:\n  struct Opts : public BaseOpts {\n    int min_avg_value_size;\n  };\n};\n}  // namespace clock_cache\n}  // namespace rocksdb\n",
        ),
        (
            "cache/lru_cache.h",
            "namespace rocksdb {\nnamespace lru_cache {\nstruct BaseOpts {\n  int capacity;\n};\n}  // namespace lru_cache\n}  // namespace rocksdb\n",
        ),
        (
            "table/internal_iterator.h",
            "namespace rocksdb {\nclass Slice {};\ntemplate <class TValue>\nclass InternalIteratorBase {\n public:\n  virtual void Next() = 0;\n  void SeekToFirst() {}\n};\nusing InternalIterator = InternalIteratorBase<Slice>;\ntemplate <bool B> using bool_constant = std::integral_constant<bool, B>;\n}  // namespace rocksdb\n",
        ),
        (
            "table/merging_iterator.cc",
            "#include \"table/internal_iterator.h\"\nnamespace rocksdb {\nclass MergingIterator : public InternalIterator {\n public:\n  void Next() override {}\n};\nvoid Rewind(MergingIterator* iter) {\n  iter->SeekToFirst();\n}\ntemplate <typename T>\nstruct is_fast_float : bool_constant<sizeof(T) <= sizeof(double)> {};\n}  // namespace rocksdb\n",
        ),
        (
            "include/rocksdb/listener.h",
            "namespace rocksdb {\nclass EventListener {\n public:\n  virtual void OnFlushCompleted() {}\n};\n}  // namespace rocksdb\n",
        ),
        (
            "examples/compact_files_example.cc",
            "#include \"include/rocksdb/listener.h\"\nusing namespace rocksdb;\nclass Compactor : public EventListener {\n public:\n  void OnFlushCompleted() override {}\n};\n",
        ),
        (
            "test/gtest/gtest.h",
            "namespace testing {\nclass Test {\n public:\n  virtual void TestBody() = 0;\n};\nnamespace internal {\nclass SuiteApiResolver {\n public:\n  typedef int Test;\n};\n}  // namespace internal\n}  // namespace testing\n",
        ),
        (
            "test/gtest-extra-test.cc",
            "#include \"test/gtest/gtest.h\"\nclass single_evaluation_test : public ::testing::Test {\n protected:\n  void TestBody() override {}\n};\n",
        ),
        (
            "util/mixin.h",
            "class Base {\n public:\n  void Run() {}\n};\ntemplate <class Base>\nclass Logged : public Base {\n public:\n  void Log() {}\n};\n",
        ),
        (
            "util/testharness.h",
            "#include \"include/leveldb/env.h\"\nusing namespace leveldb;\n",
        ),
        (
            "include/leveldb/env.h",
            "namespace leveldb {\nclass EnvWrapper {\n public:\n  virtual int NowMicros() { return 0; }\n};\n}  // namespace leveldb\n",
        ),
        (
            "util/env_test.cc",
            "#include \"util/testharness.h\"\nclass SleepingEnv : public EnvWrapper {\n public:\n  int NowMicros() override { return 1; }\n};\n",
        ),
    ];

    fn base_lookup_project() -> super::support::Indexed {
        BASE_LOOKUP
            .iter()
            .fold(Project::new(), |project, (path, text)| {
                project.file(path, text)
            })
            .index()
    }

    /// The one node qualified `qualified` in `file` (no import).
    #[track_caller]
    fn the_class<'g>(graph: &'g Graph, qualified: &str, file: &str) -> &'g super::support::NodeRow {
        let found: Vec<&super::support::NodeRow> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.qualified_name == qualified && node.file_path == file && node.kind != "import"
            })
            .collect();
        assert_eq!(found.len(), 1, "{qualified} in {file}: {found:?}");
        found[0]
    }

    /// The supertypes of a class, as `kind qualified (file) resolvedBy`.
    #[track_caller]
    fn supertypes(graph: &Graph, qualified: &str, file: &str) -> Vec<String> {
        let id = &the_class(graph, qualified, file).id;
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| {
                &edge.source == id && (edge.kind == "extends" || edge.kind == "implements")
            })
            .map(|edge| {
                let target = graph.node(&edge.target);
                format!(
                    "{} {} ({}) {}",
                    edge.kind,
                    target.map_or("?", |node| node.qualified_name.as_str()),
                    target.map_or("?", |node| node.file_path.as_str()),
                    edge.resolved_by().unwrap_or("?")
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2407 (`4040eab9`): a C++ base class is the one name lookup
    /// finds from the deriving class: in the namespaces around it, never a
    /// class another source file defines for itself, a nested class's base in
    /// its outer class and what that one inherits, through the namespace
    /// aliases, using-declarations and using-directives the file writes,
    /// `::testing::Test` from the global scope, an alias followed to the class
    /// it names (and an alias of an outside type linked itself), a template
    /// parameter left unresolved, and the only class of that name the file can
    /// see when the lookup finds none. No C/C++ base reaches another language.
    #[test]
    fn pr2407_a_base_class_is_the_class_name_lookup_finds() {
        let project = base_lookup_project();
        let graph = project.graph();
        let report = || graph.report();
        assert_eq!(
            supertypes(
                graph,
                "google::protobuf::DynamicMessage",
                "src/google/protobuf/dynamic_message.cc"
            ),
            ["extends google::protobuf::Message (src/google/protobuf/message.h) qualified-name"],
            "{}",
            report()
        );
        assert_eq!(
            supertypes(
                graph,
                "google::protobuf::internal::ZeroFieldsBase",
                "src/google/protobuf/generated_message_bases.h"
            ),
            ["extends google::protobuf::Message (src/google/protobuf/message.h) qualified-name"]
        );
        assert_eq!(
            supertypes(graph, "leveldb::DBIter", "db/db_iter.cc"),
            ["extends leveldb::Iterator (include/leveldb/iterator.h) qualified-name"]
        );
        assert_eq!(
            supertypes(graph, "leveldb::NumberComparator", "db/db_test.cc"),
            ["extends leveldb::Comparator (include/leveldb/comparator.h) qualified-name"]
        );
        assert_eq!(
            supertypes(
                graph,
                "rocksdb::clock_cache::AutoHyperClockTable::Opts",
                "cache/clock_cache.h"
            ),
            [
                "extends rocksdb::clock_cache::BaseClockTable::BaseOpts (cache/clock_cache.h) qualified-name"
            ]
        );
        assert_eq!(
            supertypes(
                graph,
                "google::protobuf::AnyGlobalsTypeInternal",
                "src/google/protobuf/any.pb.cc"
            ),
            [
                "extends google::protobuf::internal::MessageGlobalsBase (src/google/protobuf/message_lite.h) qualified-name"
            ]
        );
        assert_eq!(
            supertypes(graph, "leveldb_filterpolicy_t", "db/c.cc"),
            ["extends leveldb::FilterPolicy (include/leveldb/filter_policy.h) qualified-name"]
        );
        assert_eq!(
            supertypes(graph, "Compactor", "examples/compact_files_example.cc"),
            ["extends rocksdb::EventListener (include/rocksdb/listener.h) qualified-name"]
        );
        assert_eq!(
            supertypes(graph, "single_evaluation_test", "test/gtest-extra-test.cc"),
            ["extends testing::Test (test/gtest/gtest.h) qualified-name"]
        );
        assert_eq!(
            supertypes(
                graph,
                "rocksdb::MergingIterator",
                "table/merging_iterator.cc"
            ),
            ["extends rocksdb::InternalIteratorBase (table/internal_iterator.h) qualified-name"]
        );
        // `bool_constant<sizeof(T) <= sizeof(double)>`: the comparison is no
        // template bracket.
        assert_eq!(
            supertypes(graph, "rocksdb::is_fast_float", "table/merging_iterator.cc"),
            ["extends rocksdb::bool_constant (table/internal_iterator.h) qualified-name"]
        );
        assert_eq!(
            supertypes(graph, "Logged", "util/mixin.h"),
            Vec::<String>::new()
        );
        let logged = &the_class(graph, "Logged", "util/mixin.h").id;
        assert_eq!(
            graph
                .unresolved
                .iter()
                .filter(|reference| &reference.from_node_id == logged
                    && reference.reference_kind == "extends")
                .map(|reference| reference.reference_name.as_str())
                .collect::<Vec<_>>(),
            ["Base"]
        );
        // `using namespace leveldb;` is in a header the file includes.
        assert_eq!(
            supertypes(graph, "SleepingEnv", "util/env_test.cc"),
            ["extends leveldb::EnvWrapper (include/leveldb/env.h) exact-match"]
        );
        // No C or C++ base reaches a framework resolver or another language.
        let mut bases = 0;
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "extends" || edge.kind == "implements")
        {
            let (Some(source), Some(target)) = (graph.node(&edge.source), graph.node(&edge.target))
            else {
                continue;
            };
            if source.language != "cpp" && source.language != "c" {
                continue;
            }
            bases += 1;
            assert_ne!(
                edge.resolved_by(),
                Some("framework"),
                "{} -> {}",
                source.qualified_name,
                target.qualified_name
            );
            assert!(
                target.language == "cpp" || target.language == "c",
                "{} -> {} ({})",
                source.qualified_name,
                target.qualified_name,
                target.language
            );
        }
        assert!(bases > 0);
        let c_message = &the_class(graph, "Message", "php/ext/google/protobuf/message.c").id;
        assert!(
            !graph
                .edges
                .iter()
                .any(|edge| &edge.target == c_message && edge.kind == "extends"),
            "{}",
            report()
        );
    }

    /// Upstream #2407: a call to a method a class inherits from the base an
    /// alias names reaches that base's method (rocksdb's `MergingIterator :
    /// public InternalIterator`, `using InternalIterator =
    /// InternalIteratorBase<Slice>;`).
    #[test]
    fn pr2407_an_inherited_method_is_found_on_the_base_an_alias_names() {
        let project = base_lookup_project();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "rocksdb::Rewind"),
            ["rocksdb::InternalIteratorBase::SeekToFirst (table/internal_iterator.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2426's last case, which needs #2407's lookup: protobuf's
    /// `FieldDescriptor`, walked in brace scopes after an unknown attribute
    /// macro broke the tree, keeps its namespaces, and its base
    /// `internal::SymbolBase` is `google::protobuf::internal::SymbolBase`, not
    /// the `other::internal::SymbolBase` namesake (W4-CPP-02 carried it here).
    #[test]
    fn pr2426_a_class_that_lost_its_namespace_finds_its_base_by_scope_lookup() {
        let project = Project::new()
            .file(
                "src/descriptor.h",
                "namespace google {\nnamespace protobuf {\n\nclass Names {\n public:\n  NODISCARD absl::string_view name() const {\n    return absl::string_view(payload_);\n  }\n\n private:\n  const char* payload_;\n};\n\nclass FieldDescriptor : private internal::SymbolBase {\n public:\n  int index() const;\n};\n\n}  // namespace protobuf\n}  // namespace google\n",
            )
            .file(
                "src/symbol_base.h",
                "namespace google {\nnamespace protobuf {\nnamespace internal {\nclass SymbolBase {\n public:\n  int kind() const;\n};\n}  // namespace internal\n}  // namespace protobuf\n}  // namespace google\n",
            )
            .file(
                "src/other.h",
                "namespace other {\nnamespace internal {\nclass SymbolBase {\n public:\n  int kind() const;\n};\n}  // namespace internal\n}  // namespace other\n",
            )
            .index();
        let graph = project.graph();
        let derived = &the_class(
            graph,
            "google::protobuf::FieldDescriptor",
            "src/descriptor.h",
        )
        .id;
        let bases: Vec<&str> = graph
            .edges
            .iter()
            .filter(|edge| &edge.source == derived && edge.kind == "extends")
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert_eq!(
            bases,
            ["google::protobuf::internal::SymbolBase"],
            "{}",
            graph.report()
        );
    }

    /// Source lines joined as a file, with its last newline (upstream's
    /// `lines(…)`).
    fn lines(text: &[&str]) -> String {
        let mut out = text.join("\n");
        out.push('\n');
        out
    }

    /// The qualified names of the `calls` callees of `caller`, sorted.
    fn callee_names(graph: &Graph, caller: &str) -> Vec<String> {
        callees(graph, caller)
            .into_iter()
            .map(|callee| {
                callee
                    .rsplit_once(" (")
                    .map_or(callee.clone(), |(name, _)| name.to_string())
            })
            .collect()
    }

    /// The `calls` callees of `caller` with how each was resolved, as
    /// `qualified resolvedBy@confidence`, sorted.
    fn resolved_callees(graph: &Graph, caller: &str) -> Vec<String> {
        let ids: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.qualified_name == caller && (node.kind == "method" || node.kind == "function")
            })
            .map(|node| node.id.as_str())
            .collect();
        assert!(!ids.is_empty(), "no function or method {caller}");
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" && ids.contains(&edge.source.as_str()))
            .filter_map(|edge| {
                graph.node(&edge.target).map(|node| {
                    format!(
                        "{} {}@{}",
                        node.qualified_name,
                        edge.resolved_by().unwrap_or("?"),
                        edge.confidence()
                            .map_or_else(|| "?".to_string(), |c| c.to_string())
                    )
                })
            })
            .collect();
        out.sort();
        out
    }

    /// leveldb's Slice and its namesakes elsewhere (upstream #2413's
    /// `PROJECT`): every method a library type's call could be mistaken for.
    fn library_type_project(extra: &[(&str, String)]) -> super::support::Indexed {
        let project = Project::new()
            .file(
                "include/leveldb/slice.h",
                &lines(&[
                    "namespace leveldb {",
                    "class Slice {",
                    " public:",
                    "  const char* data() const { return data_; }",
                    "  unsigned size() const { return size_; }",
                    "  void clear() { size_ = 0; }",
                    "  const char* begin() const { return data_; }",
                    "  void remove_prefix(unsigned n) { size_ -= n; }",
                    " private:",
                    "  const char* data_;",
                    "  unsigned size_;",
                    "};",
                    "}  // namespace leveldb",
                ]),
            )
            .file(
                "util/buffers.h",
                &lines(&[
                    "namespace leveldb {",
                    "class LeftoverBuffer {",
                    " public:",
                    "  void append(const char* s, unsigned n) {}",
                    "};",
                    "class Arena {",
                    " public:",
                    "  void insert(int block) {}",
                    "};",
                    "class Cache {",
                    " public:",
                    "  void reset() {}",
                    "};",
                    "class Iterator {",
                    " public:",
                    "  void SeekToFirst() {}",
                    "};",
                    "}  // namespace leveldb",
                ]),
            );
        extra
            .iter()
            .fold(project, |project, (path, text)| project.file(path, text))
            .index()
    }

    /// Upstream #2413 (`61f8b09c`): a C++ receiver the calling function or
    /// its class declares as a type from outside the project with a
    /// lowercase name calls that type's own member. leveldb's `std::string
    /// saved_key_; saved_key_.clear()` reached `Slice::clear`, the one
    /// project `clear`. The receiver here is `saved_slice_`: Rust's guess of a
    /// lone candidate needs a word the receiver shares with it (#226,
    /// KEEP-RUST), and this one would reach `Slice::clear` without the stop.
    #[test]
    fn pr2413_a_std_string_member_calls_its_own_member() {
        let project = library_type_project(&[(
            "db/db_iter.cc",
            lines(&[
                "#include <string>",
                "#include \"include/leveldb/slice.h\"",
                "namespace leveldb {",
                "class DBIter {",
                " public:",
                "  void Next();",
                " private:",
                "  std::string saved_slice_;    // == current key when direction_==kReverse",
                "};",
                "void DBIter::Next() {",
                "  saved_slice_.clear();",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::DBIter::Next"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            callers(graph, "leveldb::Slice::clear"),
            Vec::<String>::new()
        );
    }

    /// Upstream #2413: a local, a parameter, a raw pointer, nested template
    /// arguments and an abseil container; `value` is a `Slice`, the others
    /// are not. The receivers are `buffer`, `proto_buffer`, `arena` and
    /// `slice_files` (upstream's `dst`, `proto`, `seen`, `files`), sharing a
    /// word with a decoy Rust's lone-candidate guess (#226, KEEP-RUST) would
    /// take without the stop.
    #[test]
    fn pr2413_locals_parameters_and_raw_pointers_of_library_types() {
        let project = library_type_project(&[(
            "db/encode.cc",
            lines(&[
                "#include <string>",
                "#include <vector>",
                "#include \"absl/container/flat_hash_set.h\"",
                "#include \"include/leveldb/slice.h\"",
                "#include \"util/buffers.h\"",
                "namespace leveldb {",
                "void Encode(std::string* buffer, const Slice& value) {",
                "  buffer->clear();",
                "  buffer->append(value.data(), value.size());",
                "}",
                "std::string Render(const std::vector<std::pair<int, Slice>>& slice_files) {",
                "  std::string proto_buffer;",
                "  proto_buffer.append(\"x\", 1);",
                "  absl::flat_hash_set<int> arena;",
                "  arena.insert(1);",
                "  const char* first = proto_buffer.data();",
                "  return slice_files.begin() == slice_files.end() ? proto_buffer : std::string(first);",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Encode"),
            ["leveldb::Slice::data", "leveldb::Slice::size"],
            "{}",
            graph.report()
        );
        assert_eq!(callee_names(graph, "leveldb::Render"), Vec::<String>::new());
    }

    /// Upstream #2413: a `std::` name beside a project type of that name in
    /// another namespace (rocksdb vendors googletest, whose `typedef
    /// ::std::string string;` in `testing::internal` is a project `string`).
    /// The receiver is `slice` (upstream's `text`), which Rust's lone-candidate
    /// guess (#226, KEEP-RUST) would take to `Slice::clear` without the stop.
    #[test]
    fn pr2413_a_std_name_beside_a_project_namesake_elsewhere() {
        let project = library_type_project(&[
            (
                "third_party/gtest.h",
                lines(&[
                    "#include <string>",
                    "namespace testing {",
                    "namespace internal {",
                    "typedef ::std::string string;",
                    "}  // namespace internal",
                    "}  // namespace testing",
                ]),
            ),
            (
                "tools/trim.cc",
                lines(&[
                    "#include <string>",
                    "#include \"include/leveldb/slice.h\"",
                    "void Trim(std::string& slice) {",
                    "  slice.clear();",
                    "}",
                ]),
            ),
        ]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "Trim"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: on a smart pointer, `.` is its own member and `->`
    /// reaches the element type: `reset()` is `unique_ptr`'s, not
    /// `Cache::reset`. The receiver is `cache_iterator` (upstream's `iter`),
    /// which shares a word with both `Iterator` and the decoy `Cache` for
    /// Rust's lone-candidate guess (#226, KEEP-RUST).
    #[test]
    fn pr2413_a_smart_pointer_dot_call_is_its_own_member() {
        let project = library_type_project(&[(
            "db/scan.cc",
            lines(&[
                "#include <memory>",
                "#include \"util/buffers.h\"",
                "namespace leveldb {",
                "void Scan(Iterator* raw) {",
                "  std::unique_ptr<Iterator> cache_iterator(raw);",
                "  cache_iterator->SeekToFirst();",
                "  cache_iterator.reset();",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Scan"),
            ["leveldb::Iterator::SeekToFirst"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a std trait or member type names another type, maybe
    /// the project's (rocksdb's `std::conditional_t<kIsDigestedDict, void*,
    /// Slice> dict` is a `Slice` when the flag is off). The receivers are
    /// `dict_slice` and `first_slice` (upstream's `dict`, `first`), which
    /// Rust's lone-candidate guess (#226, KEEP-RUST) takes to `Slice`.
    #[test]
    fn pr2413_a_std_trait_or_member_type_may_name_a_project_type() {
        let project = library_type_project(&[(
            "util/compression.cc",
            lines(&[
                "#include <type_traits>",
                "#include <vector>",
                "#include \"include/leveldb/slice.h\"",
                "namespace leveldb {",
                "template <bool kIsDigestedDict>",
                "unsigned DictSize(std::conditional_t<kIsDigestedDict, void*, Slice> dict_slice) {",
                "  return dict_slice.size();",
                "}",
                "void Drop(const std::vector<Slice>& slices) {",
                "  std::vector<Slice>::value_type first_slice = slices[0];",
                "  first_slice.remove_prefix(1);",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::DictSize"),
            ["leveldb::Slice::size"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callee_names(graph, "leveldb::Drop"),
            ["leveldb::Slice::remove_prefix"]
        );
    }

    /// Upstream #2413: a project type with a lowercase name is still the
    /// project's.
    #[test]
    fn pr2413_a_lowercase_project_type_keeps_its_method() {
        let project = library_type_project(&[
            (
                "util/arena.h",
                lines(&[
                    "namespace util {",
                    "class arena {",
                    " public:",
                    "  void* allocate(unsigned n) { return 0; }",
                    "};",
                    "}  // namespace util",
                ]),
            ),
            (
                "util/use.cc",
                lines(&[
                    "#include \"util/arena.h\"",
                    "void Use(util::arena& a) {",
                    "  a.allocate(8);",
                    "}",
                ]),
            ),
        ]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "Use"),
            ["util::arena::allocate"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a range-for variable, or an `auto` one, hides an
    /// earlier library-typed one of the same name, so that declaration says
    /// nothing about the call; the block's `clear()` is the string's. The
    /// receivers are `slice` and `cache` (upstream's `key`, `buf`), which
    /// Rust's lone-candidate guess (#226, KEEP-RUST) takes to `Slice` and
    /// `Cache`.
    #[test]
    fn pr2413_a_range_for_or_auto_variable_hides_a_library_typed_one() {
        let project = library_type_project(&[(
            "db/shadow.cc",
            lines(&[
                "#include <string>",
                "#include <vector>",
                "#include \"include/leveldb/slice.h\"",
                "#include \"util/buffers.h\"",
                "namespace leveldb {",
                "void Trim(const std::vector<Slice>& slices) {",
                "  std::string slice;",
                "  for (const Slice& slice : slices) {",
                "    slice.remove_prefix(1);",
                "  }",
                "}",
                "void Flush(Cache* caches) {",
                "  { std::string cache; cache.clear(); }",
                "  auto cache = caches[0];",
                "  cache.reset();",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Trim"),
            ["leveldb::Slice::remove_prefix"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callee_names(graph, "leveldb::Flush"),
            ["leveldb::Cache::reset"]
        );
    }

    /// Upstream #2413: a call that contradicts the declaration the scan
    /// found (`.` on what reads as a pointer, `absl::kWeightScale * key[0]`)
    /// keeps its guess: that declaration is another variable's. The receiver
    /// is `slice_key` (upstream's `key`), which Rust's lone-candidate guess
    /// (#226, KEEP-RUST) takes to `Slice::size`.
    #[test]
    fn pr2413_a_call_contradicting_its_declaration_keeps_its_guess() {
        let project = library_type_project(&[(
            "util/weight.cc",
            lines(&[
                "#include \"include/leveldb/slice.h\"",
                "namespace leveldb {",
                "unsigned Weight(const Slice& slice_key) {",
                "  unsigned w = absl::kWeightScale * slice_key[0];",
                "  return w + slice_key.size();",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Weight"),
            ["leveldb::Slice::size"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a constant of a project class read as a type
    /// (`Slice::kMask & key[0]`), and a `_t` scalar, are no library types.
    /// The receiver is `slice_key` (upstream's `key`), which Rust's
    /// lone-candidate guess (#226, KEEP-RUST) takes to `Slice`.
    #[test]
    fn pr2413_a_project_class_constant_and_a_scalar_are_no_library_types() {
        let project = library_type_project(&[(
            "util/mask.cc",
            lines(&[
                "#include <cstddef>",
                "#include \"include/leveldb/slice.h\"",
                "#define SLICE_AT(var, list, i) const Slice& var = list[i]",
                "namespace leveldb {",
                "unsigned Mask(const Slice& slice_key) {",
                "  unsigned m = Slice::kMask & slice_key[0];",
                "  return m + slice_key.size();",
                "}",
                "void Visit(const Slice* slices) {",
                "  std::size_t slice_key = 0;",
                "  {",
                "    SLICE_AT(slice_key, slices, 0);",
                "    slice_key.remove_prefix(1);",
                "  }",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Mask"),
            ["leveldb::Slice::size"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callee_names(graph, "leveldb::Visit"),
            ["leveldb::Slice::remove_prefix"]
        );
    }

    /// Upstream #2413: a member of another class in the file (leveldb's
    /// `db/log_test.cc`: `StringDest`'s `std::string contents_` sits above
    /// `StringSource`, whose own `Slice contents_` follows its methods). The
    /// members are `slice_` here (upstream's `contents_`), which Rust's
    /// lone-candidate guess (#226, KEEP-RUST) takes to `Slice`.
    #[test]
    fn pr2413_a_member_of_another_class_in_the_file() {
        let project = library_type_project(&[(
            "db/log_test.cc",
            lines(&[
                "#include <string>",
                "#include \"include/leveldb/slice.h\"",
                "namespace leveldb {",
                "class StringDest {",
                " public:",
                "  std::string slice_;",
                "};",
                "class StringSource {",
                " public:",
                "  void Skip(unsigned n) { slice_.remove_prefix(n); }",
                " private:",
                "  Slice slice_;",
                "};",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::StringSource::Skip"),
            ["leveldb::Slice::remove_prefix"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: the last line of a declaration that starts on an
    /// earlier one (rocksdb's `BlobFileReader`: `autovector<std::pair<…,`
    /// ends one line and `std::unique_ptr<…>>>& blob_reqs` starts the next).
    /// The parameter is `autovector_reqs` (upstream's `blob_reqs`), which
    /// Rust's lone-candidate guess (#226, KEEP-RUST) takes to `autovector`.
    #[test]
    fn pr2413_the_last_line_of_a_declaration_begun_above() {
        let project = Project::new()
            .file(
                "util/autovector.h",
                &lines(&[
                    "namespace rocksdb {",
                    "template <class T>",
                    "class autovector {",
                    " public:",
                    "  unsigned size() const { return 0; }",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            )
            .file(
                "db/blob/blob_file_reader.cc",
                &lines(&[
                    "#include <memory>",
                    "#include \"util/autovector.h\"",
                    "namespace rocksdb {",
                    "class BlobFileReader {",
                    " public:",
                    "  unsigned MultiGetBlob(autovector<std::pair<int*,",
                    "                                 std::unique_ptr<int>>>& autovector_reqs);",
                    "};",
                    "unsigned BlobFileReader::MultiGetBlob(autovector<std::pair<int*,",
                    "                                      std::unique_ptr<int>>>& autovector_reqs) {",
                    "  return autovector_reqs.size();",
                    "}",
                    "}  // namespace rocksdb",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "rocksdb::BlobFileReader::MultiGetBlob"),
            ["rocksdb::autovector::size"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a member of a class nested in the caller's (rocksdb's
    /// `MultiScan::MultiScanIterator` declares a `scan_opts_` of its own,
    /// above `MultiScan`'s).
    #[test]
    fn pr2413_a_member_of_a_class_nested_in_the_callers() {
        let project = Project::new()
            .file(
                "include/rocksdb/multi_scan.h",
                &lines(&[
                    "#include <vector>",
                    "namespace rocksdb {",
                    "struct ScanOptions {};",
                    "class MultiScanArgs {",
                    " public:",
                    "  int GetScanRanges() const { return 0; }",
                    "};",
                    "class MultiScan {",
                    " public:",
                    "  class MultiScanIterator {",
                    "   private:",
                    "    const std::vector<ScanOptions>& scan_opts_;",
                    "  };",
                    "  int begin() { return scan_opts_.GetScanRanges(); }",
                    " private:",
                    "  const MultiScanArgs scan_opts_;",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "rocksdb::MultiScan::begin"),
            ["rocksdb::MultiScanArgs::GetScanRanges"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a comment that mentions the receiver is not its
    /// declaration (leveldb's `// … since there is a non-null imm_` above
    /// `imm_->…` read as `imm_`'s declaration, with the type `null`).
    #[test]
    fn pr2413_a_comment_mentioning_the_receiver_is_no_declaration() {
        let project = Project::new()
            .file(
                "db/memtable.h",
                &lines(&[
                    "namespace leveldb {",
                    "class MemTable {",
                    " public:",
                    "  unsigned ApproximateMemoryUsage() { return 0; }",
                    "};",
                    "class Arena {",
                    " public:",
                    "  unsigned ApproximateMemoryUsage() { return 0; }",
                    "};",
                    "}  // namespace leveldb",
                ]),
            )
            .file(
                "db/db_impl.h",
                &lines(&[
                    "#include \"db/memtable.h\"",
                    "namespace leveldb {",
                    "class DBImpl {",
                    " public:",
                    "  unsigned MemoryUsage();",
                    " private:",
                    "  MemTable* imm_;",
                    "};",
                    "}  // namespace leveldb",
                ]),
            )
            .file(
                "db/db_impl.cc",
                &lines(&[
                    "#include \"db/db_impl.h\"",
                    "namespace leveldb {",
                    "unsigned DBImpl::MemoryUsage() {",
                    "  // Count the memtable being compacted, since there is a non-null imm_",
                    "  return imm_->ApproximateMemoryUsage();",
                    "}",
                    "}  // namespace leveldb",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::DBImpl::MemoryUsage"),
            ["leveldb::MemTable::ApproximateMemoryUsage"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2413: a parameter after a leading `/* static */` is the
    /// caller's own. The parameter is `slice_msg` (upstream's `msg`), which
    /// Rust's lone-candidate guess (#226, KEEP-RUST) would take to
    /// `Slice::begin` without the stop.
    #[test]
    fn pr2413_a_parameter_after_a_leading_block_comment_is_the_callers() {
        let project = library_type_project(&[(
            "util/status.cc",
            lines(&[
                "#include <string>",
                "#include \"include/leveldb/slice.h\"",
                "namespace leveldb {",
                "class Status {",
                " public:",
                "  static bool Check(const std::string& slice_msg);",
                "};",
                "/* static */ bool Status::Check(const std::string& slice_msg) {",
                "  return slice_msg.begin() != nullptr;",
                "}",
                "}  // namespace leveldb",
            ]),
        )]);
        let graph = project.graph();
        assert_eq!(
            callee_names(graph, "leveldb::Status::Check"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// The declaration scan reads a receiver's own namespace qualifier as no
    /// type (kept from before upstream #2413's port; upstream reads `leveldb`
    /// as the type): leveldb's `db_bench.cc` assigns `leveldb::g_env =
    /// leveldb::Env::Default();` above `leveldb::g_env->GetTestDirectory(…)`,
    /// and the call is `Env`'s, the type `leveldb::Env* g_env` declares, not
    /// `InMemoryEnv::GetTestDirectory` guessed by name.
    #[test]
    fn pr2413_a_qualified_receivers_namespace_is_no_declaration() {
        let project = Project::new()
            .file(
                "include/leveldb/env.h",
                &lines(&[
                    "namespace leveldb {",
                    "class Env {",
                    " public:",
                    "  static Env* Default();",
                    "  virtual int GetTestDirectory(int* path) = 0;",
                    "};",
                    "class EnvWrapper : public Env {",
                    " public:",
                    "  int GetTestDirectory(int* path) override { return 0; }",
                    "};",
                    "}  // namespace leveldb",
                ]),
            )
            .file(
                "helpers/memenv/memenv.cc",
                &lines(&[
                    "#include \"include/leveldb/env.h\"",
                    "namespace leveldb {",
                    "class InMemoryEnv : public EnvWrapper {",
                    " public:",
                    "  int GetTestDirectory(int* path) override { return 1; }",
                    "};",
                    "}  // namespace leveldb",
                ]),
            )
            .file(
                "benchmarks/db_bench.cc",
                &lines(&[
                    "#include \"include/leveldb/env.h\"",
                    "static const char* FLAGS_db = nullptr;",
                    "namespace leveldb {",
                    "namespace {",
                    "leveldb::Env* g_env = nullptr;",
                    "}  // namespace",
                    "}  // namespace leveldb",
                    "int main(int argc, char** argv) {",
                    "  int default_db_path = 0;",
                    "  leveldb::g_env = leveldb::Env::Default();",
                    "  // Choose a location for the test database if none given with --db=<path>",
                    "  if (FLAGS_db == nullptr) {",
                    "    leveldb::g_env->GetTestDirectory(&default_db_path);",
                    "  }",
                    "  return 0;",
                    "}",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "main"),
            ["leveldb::Env::GetTestDirectory (include/leveldb/env.h)"],
            "{}",
            graph.report()
        );
    }

    /// facebook/rocksdb's shapes, trimmed (upstream #2443's `ROCKSDB`): the
    /// small vector, and the class whose words the receivers share.
    fn nested_template_files() -> Vec<(&'static str, String)> {
        vec![
            (
                "util/autovector.h",
                lines(&[
                    "namespace rocksdb {",
                    "template <class T, size_t kSize = 8>",
                    "class autovector {",
                    " public:",
                    "  size_t size() const { return 0; }",
                    "  void clear() {}",
                    "  void push_back(const T& item) {}",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            ),
            (
                "db/compaction/compaction.h",
                lines(&[
                    "namespace rocksdb {",
                    "struct FileMetaData {};",
                    "struct CompactionInputFiles {",
                    "  size_t size() const { return 0; }",
                    "  void clear() {}",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            ),
            (
                "db/version_set.h",
                lines(&[
                    "#include \"util/autovector.h\"",
                    "#include \"db/compaction/compaction.h\"",
                    "namespace rocksdb {",
                    "class VersionEdit {};",
                    "class VersionStorageInfo {",
                    " public:",
                    "  void ComputeFilesMarkedForCompaction();",
                    "  size_t CountEditLists(VersionEdit* edit);",
                    " private:",
                    "  autovector<std::pair<int, FileMetaData*>> files_marked_for_compaction_;",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            ),
            (
                "db/version_set.cc",
                lines(&[
                    "#include \"db/version_set.h\"",
                    "namespace rocksdb {",
                    "void VersionStorageInfo::ComputeFilesMarkedForCompaction() {",
                    "  files_marked_for_compaction_.clear();",
                    "}",
                    "size_t VersionStorageInfo::CountEditLists(VersionEdit* edit) {",
                    "  autovector<autovector<VersionEdit*>> edit_lists;",
                    "  edit_lists.push_back(autovector<VersionEdit*>());",
                    "  return edit_lists.size();",
                    "}",
                    "}  // namespace rocksdb",
                ]),
            ),
        ]
    }

    fn nested_template_project(extra: &[(&'static str, String)]) -> super::support::Indexed {
        nested_template_files()
            .iter()
            .chain(extra)
            .fold(Project::new(), |project, (path, text)| {
                project.file(path, text)
            })
            .index()
    }

    /// Upstream #2443 (`cdf06812`): a C++ receiver declared with template
    /// arguments nested in its template arguments calls its own class's
    /// method. rocksdb's `files_marked_for_compaction_.clear()` on an
    /// `autovector<std::pair<int, FileMetaData*>>` was guessed by the
    /// receiver's words to be `CompactionInputFiles::clear`.
    #[test]
    fn pr2443_a_member_with_nested_template_arguments_calls_its_class() {
        let project = nested_template_project(&[]);
        let graph = project.graph();
        assert_eq!(
            callees(
                graph,
                "rocksdb::VersionStorageInfo::ComputeFilesMarkedForCompaction"
            ),
            ["rocksdb::autovector::clear (util/autovector.h)"],
            "{}",
            graph.report()
        );
        assert_eq!(
            callers(graph, "rocksdb::CompactionInputFiles::clear"),
            Vec::<String>::new()
        );
    }

    /// Upstream #2443: a local of a class template nested in its own
    /// template arguments; both calls are the declared type's.
    #[test]
    fn pr2443_a_local_nested_in_its_own_template_arguments_is_typed() {
        let project = nested_template_project(&[]);
        let graph = project.graph();
        assert_eq!(
            resolved_callees(graph, "rocksdb::VersionStorageInfo::CountEditLists"),
            [
                "rocksdb::autovector::push_back instance-method@0.9",
                "rocksdb::autovector::size instance-method@0.9",
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2443: a capitalized class template is the project's own,
    /// not a type from outside it (`Striped<CacheAlignedWrapper<port::Mutex>>
    /// mutex_; mutex_.Get(key)` got no edge).
    #[test]
    fn pr2443_a_capitalized_class_template_is_the_projects() {
        let project = Project::new()
            .file(
                "util/mutexlock.h",
                &lines(&[
                    "namespace rocksdb {",
                    "namespace port {",
                    "class Mutex {",
                    " public:",
                    "  void Lock() {}",
                    "};",
                    "}  // namespace port",
                    "template <class T>",
                    "struct CacheAlignedWrapper {",
                    "  T obj_;",
                    "};",
                    "template <class T, class Key = int>",
                    "class Striped {",
                    " public:",
                    "  T& Get(const Key& key) { return *data_; }",
                    " private:",
                    "  T* data_;",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            )
            .file(
                "db/blob/blob_file_cache.h",
                &lines(&[
                    "#include \"util/mutexlock.h\"",
                    "namespace rocksdb {",
                    "class BlobFileCache {",
                    " public:",
                    "  void Evict(int key);",
                    " private:",
                    "  Striped<CacheAlignedWrapper<port::Mutex>> mutex_;",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            )
            .file(
                "db/blob/blob_file_cache.cc",
                &lines(&[
                    "#include \"db/blob/blob_file_cache.h\"",
                    "namespace rocksdb {",
                    "void BlobFileCache::Evict(int key) {",
                    "  mutex_.Get(key);",
                    "}",
                    "}  // namespace rocksdb",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "rocksdb::BlobFileCache::Evict"),
            ["rocksdb::Striped::Get (util/mutexlock.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2443: the end of a declaration begun on the line above
    /// names no type, not even its template argument's
    /// (`CachableEntry<BlobContents>>>& blob_reqs` under
    /// `autovector<std::pair<…,`).
    #[test]
    fn pr2443_a_declaration_tail_from_the_line_above_names_no_type() {
        let project = nested_template_project(&[
            (
                "table/block_based/cachable_entry.h",
                lines(&[
                    "namespace rocksdb {",
                    "template <class T>",
                    "class CachableEntry {",
                    " public:",
                    "  size_t size() const { return 0; }",
                    "};",
                    "}  // namespace rocksdb",
                ]),
            ),
            (
                "db/blob/blob_file_reader.cc",
                lines(&[
                    "#include \"util/autovector.h\"",
                    "#include \"table/block_based/cachable_entry.h\"",
                    "namespace rocksdb {",
                    "struct BlobContents {};",
                    "struct BlobReadRequest {};",
                    "size_t MultiGetBlob(",
                    "    autovector<std::pair<BlobReadRequest*,",
                    "                         CachableEntry<BlobContents>>>& blob_reqs) {",
                    "  return blob_reqs.size();",
                    "}",
                    "}  // namespace rocksdb",
                ]),
            ),
        ]);
        let graph = project.graph();
        assert_eq!(
            callees(graph, "rocksdb::MultiGetBlob"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2443: a declaration begun on the line above still hides a
    /// member of the same name: the class's `std::string` member says nothing
    /// about the call on the parameter. Both are `autovector_reqs` (upstream's
    /// `blob_reqs`), which Rust's lone-candidate guess (#226, KEEP-RUST)
    /// takes to `autovector`.
    #[test]
    fn pr2443_a_declaration_begun_above_still_hides_a_member() {
        let project = Project::new()
            .file("util/autovector.h", &nested_template_files()[0].1)
            .file(
                "db/blob/blob_file_reader.cc",
                &lines(&[
                    "#include <string>",
                    "#include \"util/autovector.h\"",
                    "namespace rocksdb {",
                    "struct BlobContents {};",
                    "class BlobFileReader {",
                    " public:",
                    "  size_t MultiGetBlob(autovector<std::pair<int*,",
                    "                                           std::unique_ptr<BlobContents>>>& autovector_reqs) const;",
                    " private:",
                    "  std::string autovector_reqs;",
                    "};",
                    "size_t BlobFileReader::MultiGetBlob(autovector<std::pair<int*,",
                    "                                           std::unique_ptr<BlobContents>>>& autovector_reqs) const {",
                    "  return autovector_reqs.size();",
                    "}",
                    "}  // namespace rocksdb",
                ]),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "rocksdb::BlobFileReader::MultiGetBlob"),
            ["rocksdb::autovector::size (util/autovector.h)"],
            "{}",
            graph.report()
        );
    }

    /// `kind target-qualified (target file)` of every edge leaving a
    /// function or method named `name` in `file`, sorted.
    fn edges_from_named(graph: &Graph, file: &str, name: &str) -> Vec<String> {
        let ids: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.file_path == file
                    && node.name == name
                    && (node.kind == "function" || node.kind == "method")
            })
            .map(|node| node.id.as_str())
            .collect();
        assert!(!ids.is_empty(), "no function or method {name} in {file}");
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains" && ids.contains(&edge.source.as_str()))
            .filter_map(|edge| {
                graph.node(&edge.target).map(|target| {
                    format!(
                        "{} {} ({})",
                        edge.kind, target.qualified_name, target.file_path
                    )
                })
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2176 (`5844eb89`): a library that opens its namespace with a
    /// macro (fmt's `FMT_BEGIN_NAMESPACE`, pybind11's
    /// `PYBIND11_NAMESPACE_BEGIN(PYBIND11_NAMESPACE)`) has its declarations
    /// indexed without that namespace; a qualified name now reaches the
    /// declaration the project's own namespace macros put there, through a
    /// namespace alias (`namespace py = pybind11;`) too.
    #[test]
    fn pr2176_a_qualified_name_reaches_a_declaration_in_a_macro_opened_namespace() {
        let project = Project::new()
            .file(
                "include/fmt/base.h",
                "#pragma once\n#define FMT_BEGIN_NAMESPACE \\\n  namespace fmt {           \\\n  inline namespace v12 {\n#define FMT_END_NAMESPACE \\\n  }                       \\\n  }\n\nFMT_BEGIN_NAMESPACE\nnamespace detail {\ninline int to_unsigned(int value) { return value; }\n}\n\ninline int format(const char* spec) { return detail::to_unsigned(0); }\nFMT_END_NAMESPACE\n",
            )
            .file(
                "include/pybind11/detail/macros.h",
                "#pragma once\n#define PYBIND11_NAMESPACE pybind11\n#define PYBIND11_NAMESPACE_BEGIN(name) \\\n    namespace name {                   \\\n    PYBIND11_WARNING_PUSH\n#define PYBIND11_NAMESPACE_END(name) \\\n    PYBIND11_WARNING_POP             \\\n    }\n\nPYBIND11_NAMESPACE_BEGIN(PYBIND11_NAMESPACE)\ninline void print(const char* text) {}\nPYBIND11_NAMESPACE_END(PYBIND11_NAMESPACE)\n",
            )
            .file(
                "test/format-test.cc",
                "#include \"fmt/base.h\"\n#include \"pybind11/detail/macros.h\"\n\nnamespace py = pybind11;\n\nint main() {\n  fmt::format(\"{}\");\n  fmt::detail::to_unsigned(1);\n  py::print(\"hi\");\n  return 0;\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from_named(graph, "test/format-test.cc", "main"),
            [
                "calls detail::to_unsigned (include/fmt/base.h)",
                "calls format (include/fmt/base.h)",
                "calls print (include/pybind11/detail/macros.h)",
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2201 (`116feb46`): `lib::format("{}", x)` among overloads a
    /// namespace macro opens picks the one the arguments fit: a narrow
    /// literal the `format_string` overload, a wide one (`L"{}"`) the
    /// `wformat_string` one, a style argument the style one.
    #[test]
    fn pr2201_a_macro_namespaced_call_picks_the_overload_its_arguments_fit() {
        let project = Project::new()
            .file(
                "include/lib/base.h",
                "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\n\nLIB_BEGIN_NAMESPACE\ntemplate <typename... T> struct format_string {};\ntemplate <typename... T> struct wformat_string {};\nLIB_END_NAMESPACE\n",
            )
            .file(
                "include/lib/color.h",
                "#pragma once\n#include \"base.h\"\n\nLIB_BEGIN_NAMESPACE\nstruct text_style {};\n\ntemplate <typename... T>\ninline auto format(const text_style& ts, format_string<T...> fmt, T&&... args) -> int {\n  return 0;\n}\nLIB_END_NAMESPACE\n",
            )
            .file(
                "include/lib/format.h",
                "#pragma once\n#include \"base.h\"\n\nLIB_BEGIN_NAMESPACE\ntemplate <typename... T>\ninline auto format(format_string<T...> fmt, T&&... args) -> int {\n  return 1;\n}\nLIB_END_NAMESPACE\n",
            )
            .file(
                "include/lib/xchar.h",
                "#pragma once\n#include \"base.h\"\n\nLIB_BEGIN_NAMESPACE\ntemplate <typename... T>\ninline auto format(wformat_string<T...> fmt, T&&... args) -> int {\n  return 2;\n}\nLIB_END_NAMESPACE\n",
            )
            .file(
                "test/use.cc",
                "#include \"lib/color.h\"\n#include \"lib/format.h\"\n#include \"lib/xchar.h\"\n\nint narrow() { return lib::format(\"{}\", 1); }\nint wide() { return lib::format(L\"{}\", 2); }\nint styled() { return lib::format(lib::text_style{}, \"{}\", 3); }\n",
            )
            .index();
        let graph = project.graph();
        let format_from = |caller: &str| -> Vec<String> {
            edges_from_named(graph, "test/use.cc", caller)
                .into_iter()
                .filter(|edge| edge.starts_with("calls format "))
                .collect()
        };
        assert_eq!(
            format_from("narrow"),
            ["calls format (include/lib/format.h)"],
            "{}",
            graph.report()
        );
        assert_eq!(format_from("wide"), ["calls format (include/lib/xchar.h)"]);
        assert_eq!(
            format_from("styled"),
            ["calls format (include/lib/color.h)"]
        );
    }

    const PB_INTERNAL: &str = "#pragma once\nnamespace pb {\nnamespace internal {\ninline int Prefetch(const void* ptr) { return ptr != nullptr; }\n}  // namespace internal\n}  // namespace pb\n\nnamespace _pbi = ::pb::internal;\n";

    /// Upstream #2445 (`7a796541`): a C or C++ name written from the global
    /// scope (`::store::Repair(…)`, `::_pbi::Prefetch(…)` through `namespace
    /// _pbi = ::pb::internal;`) gets past the name pre-filter and is the
    /// declaration of exactly that qualified name: never one nested in
    /// another namespace or class (the wrapper `int close(int fd) { return
    /// ::close(fd); }`), nor one a namespace macro opens (`struct pipe` in
    /// `LIB_BEGIN_NAMESPACE`), nor an outside `::testing::` name.
    #[test]
    fn pr2445_a_global_scope_name_is_the_declaration_of_exactly_that_name() {
        let project = Project::new()
            .file(
                "src/store/repair.cc",
                "namespace store {\nint Repair(const char* name) { return name != nullptr; }\nint move(int value) { return value; }\n\nnamespace testing {\nconst char* TempDir() { return \"/tmp/store\"; }\n}  // namespace testing\n}  // namespace store\n",
            )
            .file("src/pb/internal.h", PB_INTERNAL)
            .file(
                "src/store/posix.cc",
                "#include <unistd.h>\n\nnamespace store {\nnamespace posix {\nint close(int fd) { return ::close(fd); }\n}  // namespace posix\n}  // namespace store\n",
            )
            .file(
                "tools/repair_tool.cc",
                "#include <utility>\n#include \"pb/internal.h\"\n\nnamespace tools {\nnamespace store {\nint Repair(const char* name) { return 0; }\n}  // namespace store\n\nint Run(const char* name) {\n  int fixed = ::store::Repair(name);\n  int warmed = ::_pbi::Prefetch(name);\n  return ::std::move(fixed) + warmed;\n}\n}  // namespace tools\n",
            )
            .file(
                "include/lib/os.h",
                "#pragma once\n#define LIB_BEGIN_NAMESPACE \\\n  namespace lib {           \\\n  inline namespace v1 {\n#define LIB_END_NAMESPACE \\\n  }                       \\\n  }\n\nLIB_BEGIN_NAMESPACE\nstruct pipe {\n  int read_end;\n  int write_end;\n};\nLIB_END_NAMESPACE\n",
            )
            .file(
                "tests/posix-mock.cc",
                "#include <unistd.h>\n#include \"lib/os.h\"\n\nnamespace test {\nint pipe(int fds[2]) { return ::pipe(fds); }\n}  // namespace test\n",
            )
            .file(
                "tests/repair_test.cc",
                "#include <gtest/gtest.h>\n\nclass RepairTest : public ::testing::Test {};\n\nint RunAll(int argc, char** argv) {\n  ::testing::InitGoogleTest(&argc, argv);\n  return ::testing::TempDir() != nullptr;\n}\n",
            )
            .index();
        let graph = project.graph();
        // Inside `namespace tools`, `store::Repair` would be tools' own;
        // `::store::Repair` is the global one.
        assert_eq!(
            edges_from_named(graph, "tools/repair_tool.cc", "Run"),
            [
                "calls pb::internal::Prefetch (src/pb/internal.h)",
                "calls store::Repair (src/store/repair.cc)",
            ],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from_named(graph, "src/store/posix.cc", "close"),
            Vec::<String>::new()
        );
        assert_eq!(
            edges_from_named(graph, "tests/posix-mock.cc", "pipe"),
            Vec::<String>::new()
        );
        assert_eq!(
            edges_from_named(graph, "tests/repair_test.cc", "RunAll"),
            Vec::<String>::new()
        );
        assert_eq!(
            supertypes(graph, "RepairTest", "tests/repair_test.cc"),
            Vec::<String>::new()
        );
    }

    /// Upstream #2445: protobuf's C++ `::_pbi::` call beside Swift and
    /// Objective-C files still links (upstream's Swift ↔ Objective-C bridge
    /// no longer claims it; no framework claims C++ names here).
    #[test]
    fn pr2445_a_global_scope_call_beside_swift_and_objective_c() {
        let project = Project::new()
            .file(
                "objectivec/GPBLegacy.m",
                "#import <Foundation/Foundation.h>\n\n@interface GPBLegacy : NSObject\n- (void)start;\n@end\n\n@implementation GPBLegacy\n- (void)start {}\n@end\n",
            )
            .file(
                "swift/Bridge.swift",
                "import Foundation\n\nfinal class Bridge: NSObject {\n  func run() {}\n}\n",
            )
            .file("src/pb/internal.h", PB_INTERNAL)
            .file(
                "src/message.cc",
                "#include \"pb/internal.h\"\n\nint Parse(const void* data) { return ::_pbi::Prefetch(data); }\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from_named(graph, "src/message.cc", "Parse"),
            ["calls pb::internal::Prefetch (src/pb/internal.h)"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2407's final shape with #2176's tables (W4-CPP-07): a base
    /// named through a namespace alias another header writes, and a base a
    /// namespace macro puts in its namespace (`fmt::detail::buffer`, indexed
    /// as `detail::buffer` inside `FMT_BEGIN_NAMESPACE`).
    #[test]
    fn pr2407_a_base_through_a_project_alias_or_a_macro_opened_namespace() {
        let project = Project::new()
            .file(
                "include/fmt/base.h",
                "#pragma once\n#define FMT_BEGIN_NAMESPACE namespace fmt {\n#define FMT_END_NAMESPACE }\nFMT_BEGIN_NAMESPACE\nnamespace detail {\ntemplate <typename T> class buffer {\n public:\n  void push_back(const T& value) {}\n};\n}  // namespace detail\nFMT_END_NAMESPACE\n",
            )
            .file(
                "include/other/buffer.h",
                "namespace other {\nnamespace detail {\nclass buffer {};\n}  // namespace detail\n}  // namespace other\n",
            )
            .file(
                "src/memory.cc",
                "#include \"fmt/base.h\"\nclass memory_buffer : public fmt::detail::buffer<char> {};\n",
            )
            .file(
                "src/google/protobuf/message.h",
                "namespace google {\nnamespace protobuf {\nclass Message {};\n}  // namespace protobuf\n}  // namespace google\nnamespace pb = ::google::protobuf;\n",
            )
            .file(
                "src/generated.cc",
                "#include \"google/protobuf/message.h\"\nclass Generated : public pb::Message {};\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            supertypes(graph, "memory_buffer", "src/memory.cc"),
            ["extends detail::buffer (include/fmt/base.h) qualified-name"],
            "{}",
            graph.report()
        );
        assert_eq!(
            supertypes(graph, "Generated", "src/generated.cc"),
            ["extends google::protobuf::Message (src/google/protobuf/message.h) qualified-name"]
        );
    }

    /// Upstream #2234 (`d9d457d5`): C has no methods and cannot call a C++
    /// one: redis' POSIX `read(fd, buf, n)` calls and hiredis' function
    /// pointer `c->funcs->read(c, buf, …)` all went to the Qt adapter's
    /// `RedisQtAdapter::read` in a C++ header.
    #[test]
    fn pr2234_a_c_call_never_reaches_a_cpp_method() {
        let project = Project::new()
            .file(
                "deps/hiredis/adapters/qt.hpp",
                "class RedisQtAdapter {\npublic:\n    void read() { }\n};\n",
            )
            .file(
                "src/util.c",
                "#include <unistd.h>\n\nlong slurp(int fd, char *buf, long n) {\n    return read(fd, buf, n);\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callers(graph, "RedisQtAdapter::read"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2150 (`b238e240`), its C-call half: Objective-C C call
    /// syntax (`completionBlock()`) calls a block, a function or a function
    /// pointer, never a method or property (SDWebImage's block parameter
    /// reached `SDPrefetchToken`'s `completionBlock` property).
    #[test]
    fn pr2150_objective_c_c_call_syntax_never_reaches_a_method_or_property() {
        let project = Project::new()
            .file(
                "Core/SDPrefetchToken.h",
                "#import <Foundation/Foundation.h>\n@interface SDPrefetchToken : NSObject\n@property (nonatomic, copy) void (^completionBlock)(void);\n@end\n",
            )
            .file(
                "Core/SDOperation.h",
                "#import <Foundation/Foundation.h>\n@interface SDOperation : NSObject\n- (void)completionBlock;\n@end\n",
            )
            .file(
                "Core/SDOperation.m",
                "#import \"SDOperation.h\"\n@implementation SDOperation\n- (void)completionBlock {\n}\n@end\n",
            )
            .file(
                "Core/SDCache.h",
                "#import <Foundation/Foundation.h>\n@interface SDCache : NSObject\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image;\n@end\n",
            )
            .file(
                "Core/SDCache.m",
                "#import \"SDCache.h\"\n@implementation SDCache\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image {\n    completionBlock();\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        let caller: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.file_path == "Core/SDCache.m" && node.name.starts_with("storeWithCompletion")
            })
            .map(|node| node.id.as_str())
            .collect();
        assert!(!caller.is_empty(), "{}", graph.report());
        let targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" && caller.contains(&edge.source.as_str()))
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{} {}", node.kind, node.qualified_name))
            .collect();
        assert_eq!(targets, Vec::<String>::new(), "{}", graph.report());
    }
}

mod java {
    use super::support::{Graph, Project};

    /// `qualified` of every method of `file`, sorted.
    fn methods_in(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == "method" && node.file_path == file)
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// `source -> target` of every `calls` edge out of `file`, by qualified
    /// name, sorted.
    fn calls_in(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                (source.file_path == file)
                    .then(|| format!("{} -> {}", source.qualified_name, target.qualified_name))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2194 (`ad77e5fc`): a Java enum constant or Kotlin enum entry
    /// with a body of its own declares members, and the calls in them are
    /// calls; jsoup's tokenizer (an enum whose every state implements `read`)
    /// was absent from the graph.
    #[test]
    fn pr2194_enum_constant_bodies_declare_methods_and_calls() {
        let project = Project::new()
            .file(
                "src/main/java/app/Op.java",
                "package app;\n\npublic enum Op {\n  PLUS {\n    @Override\n    int apply(int a, int b) {\n      return helper(a) + b;\n    }\n  },\n  MINUS {\n    @Override\n    int apply(int a, int b) { return a - b; }\n  };\n\n  abstract int apply(int a, int b);\n\n  static int helper(int x) { return x; }\n}\n",
            )
            .file(
                "src/main/kotlin/app/Factory.kt",
                "package app\n\nenum class Factory {\n  NewBuffer {\n    override val isOneByteAtATime: Boolean\n      get() = false\n\n    override fun pipe(): Int {\n      val buffer = 1\n      return helper(buffer)\n    }\n  },\n  Other {\n    override val isOneByteAtATime: Boolean = true\n    override fun pipe(): Int = 2\n  };\n\n  abstract val isOneByteAtATime: Boolean\n  abstract fun pipe(): Int\n\n  fun helper(x: Int): Int = x\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            methods_in(graph, "src/main/java/app/Op.java"),
            [
                "app::Op::MINUS::apply",
                "app::Op::PLUS::apply",
                "app::Op::apply",
                "app::Op::helper",
            ],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_in(graph, "src/main/java/app/Op.java"),
            ["app::Op::PLUS::apply -> app::Op::helper"],
            "{}",
            graph.report()
        );
        assert_eq!(
            methods_in(graph, "src/main/kotlin/app/Factory.kt"),
            [
                "app::Factory::NewBuffer::pipe",
                "app::Factory::Other::pipe",
                "app::Factory::helper",
                "app::Factory::pipe",
            ],
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_in(graph, "src/main/kotlin/app/Factory.kt"),
            ["app::Factory::NewBuffer::pipe -> app::Factory::helper"],
            "{}",
            graph.report()
        );
    }

    /// `kind qualified-name` of every edge but `contains` and `imports` out
    /// of `file`'s nodes, sorted.
    fn edges_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains" && edge.kind != "imports")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| {
                graph
                    .node(&edge.target)
                    .map(|target| format!("{} {}", edge.kind, target.qualified_name))
            })
            .collect();
        out.sort();
        out
    }

    /// The names of what the method `qualified` calls, sorted.
    fn calls_of(graph: &Graph, qualified: &str) -> Vec<String> {
        let ids: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == "method" && node.qualified_name == qualified)
            .map(|node| node.id.as_str())
            .collect();
        assert!(!ids.is_empty(), "no method {qualified}");
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" && ids.contains(&edge.source.as_str()))
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2126 (`8987be42`): a bare Java call reaches a method of a
    /// class around it, of one of that class's supertypes (read from the
    /// declarations' heads), or one the file imports statically; not another
    /// class's method of that name (halo's Mockito `verify(…)` bound 1,038
    /// calls to an `EmailVerificationService.verify`).
    #[test]
    fn pr2126_a_bare_java_call_reaches_its_class_supertypes_or_static_imports() {
        let project = Project::new()
            .file(
                "src/main/java/app/EmailVerificationService.java",
                "package app;\npublic class EmailVerificationService {\n  public boolean verify(String token) { return true; }\n}\n",
            )
            .file(
                "src/main/java/app/Util.java",
                "package app;\npublic class Util {\n  public static String format(String s) { return s; }\n}\n",
            )
            .file(
                "src/main/java/app/Base.java",
                "package app;\npublic class Base {\n  protected void helper() {}\n}\n",
            )
            .file(
                "src/main/java/app/Child.java",
                "package app;\nimport static app.Util.format;\npublic class Child extends Base {\n  public void run() {\n    helper();\n    format(\"x\");\n    own();\n  }\n  private void own() {}\n}\n",
            )
            .file(
                "src/test/java/app/ChildTest.java",
                "package app;\nimport static org.mockito.Mockito.verify;\npublic class ChildTest {\n  public void sends(Object mock) {\n    verify(mock);\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "app::ChildTest::sends"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            calls_of(graph, "app::Child::run"),
            ["app::Base::helper", "app::Child::own", "app::Util::format"],
            "{}",
            graph.report()
        );
    }

    /// #2126 beyond upstream's fixture: the walk follows supertypes through
    /// a type that only names another (two levels up, an interface in an
    /// `implements` clause, a qualified and generic base), and an on-demand
    /// static import opens its owner's methods; a same-named method of a
    /// class nothing reaches stays out, and so does a lone one (the edge
    /// would be the only candidate's).
    #[test]
    fn pr2126_the_scope_walk_follows_heads_and_on_demand_static_imports() {
        let project = Project::new()
            .file(
                "src/app/Root.java",
                "package app;\npublic class Root {\n  protected void rootHelper() {}\n}\n",
            )
            .file(
                "src/app/Mid.java",
                "package app;\npublic class Mid<T> extends app.Root {\n}\n",
            )
            .file(
                "src/app/Greeter.java",
                "package app;\npublic interface Greeter {\n  default void greet() {}\n}\n",
            )
            .file(
                "src/app/Asserts.java",
                "package app;\npublic class Asserts {\n  public static void check(boolean b) {}\n}\n",
            )
            .file(
                "src/app/Other.java",
                "package app;\npublic class Other {\n  public void greet() {}\n  public void rootHelper() {}\n  public void lonely() {}\n}\n",
            )
            .file(
                "src/app/Leaf.java",
                "package app;\nimport static app.Asserts.*;\npublic class Leaf extends Mid<String>\n    implements Greeter {\n  void go() {\n    rootHelper();\n    greet();\n    check(true);\n    lonely();\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "app::Leaf::go"),
            [
                "app::Asserts::check",
                "app::Greeter::greet",
                "app::Root::rootHelper"
            ],
            "{}",
            graph.report()
        );
    }

    /// #2126 with an anonymous class (KEEP-RUST): its bare calls reach the
    /// methods of what it instantiates and of that type's supertypes, as in
    /// gson's `new LinkedTreeMapIterator<…>() { … nextNode() … }`; upstream
    /// reads only the anonymous class's own head, which names nothing.
    #[test]
    fn pr2126_an_anonymous_class_reaches_what_it_instantiates() {
        let project = Project::new()
            .file(
                "src/app/Root.java",
                "package app;\npublic class Root {\n  protected Object nextNode() { return null; }\n}\n",
            )
            .file(
                "src/app/Walker.java",
                "package app;\npublic abstract class Walker<T> extends Root {\n  public abstract T next();\n}\n",
            )
            .file(
                "src/app/Other.java",
                "package app;\npublic class Other {\n  public Object nextNode() { return null; }\n}\n",
            )
            .file(
                "src/app/Tree.java",
                "package app;\npublic class Tree {\n  Walker<String> walk() {\n    return new Walker<String>() {\n      public String next() {\n        return (String) nextNode();\n      }\n    };\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_of(graph, "app::Tree::walk::<Walker$anon@4>::next"),
            ["app::Root::nextNode"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2195 (`81ce7e56`): a single-type or static import whose
    /// package prefix is none of the project's packages makes the name mean
    /// the outside class: gson's production code bound
    /// `java.lang.reflect.Field` to a test's nested
    /// `ParameterizedTypesTest.Field`. The project's own imports, nested
    /// classes included, resolve as before.
    #[test]
    fn pr2195_an_import_from_outside_the_project_owns_its_name() {
        let project = Project::new()
            .file(
                "src/test/java/app/functional/TypesTest.java",
                "package app.functional;\n\npublic class TypesTest {\n  static class Field {\n    String name;\n  }\n\n  static void assertEquals(Object a, Object b) { }\n}\n",
            )
            .file(
                "src/main/java/app/model/Outer.java",
                "package app.model;\n\npublic class Outer {\n  public static class Inner {\n    public static Inner create() { return new Inner(); }\n  }\n}\n",
            )
            .file(
                "src/main/java/app/Reflect.java",
                "package app;\n\nimport java.lang.reflect.Field;\nimport app.model.Outer;\nimport app.model.Outer.Inner;\n\npublic class Reflect {\n  String nameOf(Field field) {\n    return field.getName();\n  }\n\n  Outer.Inner make() {\n    return Inner.create();\n  }\n}\n",
            )
            .file(
                "src/test/java/app/ReflectTest.java",
                "package app;\n\nimport static org.junit.Assert.assertEquals;\n\npublic class ReflectTest {\n  void names() {\n    assertEquals(\"a\", \"a\");\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let reflect = edges_from(graph, "src/main/java/app/Reflect.java");
        assert!(
            !reflect.contains(&"references app.functional::TypesTest::Field".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            reflect.contains(&"calls app.model::Outer::Inner::create".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            !edges_from(graph, "src/test/java/app/ReflectTest.java")
                .contains(&"calls app.functional::TypesTest::assertEquals".to_string()),
            "{}",
            graph.report()
        );
    }

    /// #2195 beyond upstream's fixture: a package a Kotlin file declares is
    /// the project's, so a Java import from it still resolves; a class
    /// imported from outside owns its name through a qualified call too
    /// (`Mockito.verify(…)`).
    #[test]
    fn pr2195_a_kotlin_package_is_the_projects_and_a_qualified_use_is_owned() {
        let project = Project::new()
            .file(
                "src/kt/Shapes.kt",
                "package app.shapes\n\nclass Circle {\n  fun area(): Double = 1.0\n}\n",
            )
            .file(
                "src/app/Mockito.java",
                "package app;\n\npublic class Mockito {\n  public static void verify(Object o) {}\n}\n",
            )
            .file(
                "src/app/Area.java",
                "package app;\n\nimport app.shapes.Circle;\nimport org.mockito.Mockito;\n\npublic class Area {\n  double of(Circle c) {\n    Mockito.verify(c);\n    return c.area();\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let area = edges_from(graph, "src/app/Area.java");
        assert!(
            area.contains(&"references app.shapes::Circle".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            !area.contains(&"calls app::Mockito::verify".to_string()),
            "{}",
            graph.report()
        );
    }

    /// #2195 with a package only Scala files declare: Scala files index no
    /// namespace node (neither upstream nor here), so the package is no
    /// project package and the import owns its name, as upstream decides it.
    #[test]
    fn pr2195_a_package_only_scala_declares_is_outside() {
        let project = Project::new()
            .file(
                "src/scala/Geometry.scala",
                "package app.geometry\n\nclass Circle {\n  def area(): Double = 1.0\n}\n",
            )
            .file(
                "src/app/shapes/Circle.java",
                "package app.shapes;\n\npublic class Circle { }\n",
            )
            .file(
                "src/app/Area.java",
                "package app;\n\nimport app.geometry.Circle;\n\npublic class Area {\n  double of(Circle c) {\n    return 0;\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert!(
            !graph.nodes.iter().any(
                |node| node.kind == "namespace" && node.file_path == "src/scala/Geometry.scala"
            ),
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/app/Area.java"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2236 (`ef62f023`): a bare Java type name is a type in reach
    /// by package, import or nesting. retrofit's `new Retrofit.Builder()`
    /// went to `RequestFactory.Builder`, halo's micrometer `Counter` to its
    /// own `core.extension.Counter`; a constructor never stands in for its
    /// type, and a nested type of what an anonymous class implements is in
    /// reach inside it.
    #[test]
    fn pr2236_a_bare_java_type_name_is_a_type_in_reach() {
        let project = Project::new()
            .file(
                "src/main/java/retrofit2/RequestFactory.java",
                "package retrofit2;\n\nfinal class RequestFactory {\n  static final class Builder {\n    Builder() { }\n  }\n}\n",
            )
            .file(
                "src/main/java/retrofit2/Retrofit.java",
                "package retrofit2;\n\npublic final class Retrofit {\n  public static final class Builder {\n    public Builder() { }\n  }\n}\n",
            )
            .file(
                "src/test/java/retrofit2/CallTest.java",
                "package retrofit2;\n\nclass CallTest {\n  Object make() {\n    return new Retrofit.Builder();\n  }\n}\n",
            )
            .file(
                "src/main/java/run/halo/core/extension/Counter.java",
                "package run.halo.core.extension;\n\npublic class Counter { }\n",
            )
            .file(
                "src/main/java/run/halo/core/counter/MeterUtils.java",
                "package run.halo.core.counter;\n\nimport io.micrometer.core.instrument.Counter;\n\nclass MeterUtils {\n  static Counter counter() { return null; }\n}\n",
            )
            .file(
                "src/main/java/org/jsoup/select/NodeFilter.java",
                "package org.jsoup.select;\n\npublic interface NodeFilter {\n  enum FilterResult { CONTINUE }\n  FilterResult head(Object node);\n}\n",
            )
            .file(
                "src/test/java/org/jsoup/select/TraversorTest.java",
                "package org.jsoup.select;\n\nclass TraversorTest {\n  void filter() {\n    run(new NodeFilter() {\n      public FilterResult head(Object node) { return null; }\n    });\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let call_test = edges_from(graph, "src/test/java/retrofit2/CallTest.java");
        assert!(
            call_test.contains(&"instantiates retrofit2::Retrofit::Builder".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            !call_test
                .iter()
                .any(|edge| edge.contains("retrofit2::RequestFactory")
                    || edge.ends_with("::Builder::Builder")),
            "{}",
            graph.report()
        );
        assert!(
            !edges_from(graph, "src/main/java/run/halo/core/counter/MeterUtils.java")
                .iter()
                .any(|edge| edge.ends_with(" run.halo.core.extension::Counter")),
            "{}",
            graph.report()
        );
        assert!(
            edges_from(graph, "src/test/java/org/jsoup/select/TraversorTest.java")
                .contains(&"references org.jsoup.select::NodeFilter::FilterResult".to_string()),
            "{}",
            graph.report()
        );
    }

    /// #2236 beyond upstream's fixture: a top-level type of another package
    /// is in reach through a single-type or on-demand import and not
    /// without one; a nested type from a subclass of its owner (two levels
    /// down, the heads read from source) and through its owner's import; a
    /// nested type of an unrelated class is not, whichever ranks first.
    #[test]
    fn pr2236_packages_imports_and_subtypes_put_a_type_in_reach() {
        let project = Project::new()
            .file(
                "src/lib/Widget.java",
                "package lib;\n\npublic class Widget {\n  public static class Part { }\n}\n",
            )
            .file(
                "src/lib/Gadget.java",
                "package lib;\n\npublic class Gadget { }\n",
            )
            .file(
                "src/other/Holder.java",
                "package other;\n\npublic class Holder {\n  public static class Part { }\n}\n",
            )
            .file(
                "src/app/Base.java",
                "package app;\n\npublic class Base extends lib.Widget { }\n",
            )
            .file(
                "src/app/Sub.java",
                "package app;\n\npublic class Sub extends Base {\n  Part part;\n}\n",
            )
            .file(
                "src/app/Single.java",
                "package app;\n\nimport lib.Gadget;\n\npublic class Single {\n  Gadget gadget;\n}\n",
            )
            .file(
                "src/app/Demand.java",
                "package app;\n\nimport lib.*;\n\npublic class Demand {\n  Gadget gadget;\n}\n",
            )
            .file(
                "src/app/Nothing.java",
                "package app;\n\npublic class Nothing {\n  Gadget gadget;\n  Part part;\n}\n",
            )
            .file(
                "src/app/Members.java",
                "package app;\n\nimport lib.Widget.*;\n\npublic class Members {\n  Part part;\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "src/app/Sub.java"),
            ["extends app::Base", "references lib::Widget::Part"],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/app/Single.java"),
            ["references lib::Gadget"],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/app/Demand.java"),
            ["references lib::Gadget"],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/app/Nothing.java"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/app/Members.java"),
            ["references lib::Widget::Part"],
            "{}",
            graph.report()
        );
    }

    /// Whether `node` is one Lombok generates here: it carries the `lombok`
    /// decorator.
    fn is_lombok(node: &super::support::NodeRow) -> bool {
        node.decorators
            .as_array()
            .is_some_and(|decorators| decorators.iter().any(|d| d == "lombok"))
    }

    /// Upstream #912 (`826810f1`): the members Lombok generates are nodes,
    /// marked with a `lombok` decorator and a docstring naming the
    /// annotation, and calls to them resolve; a static field gets no
    /// accessor. Upstream's fixture, verbatim.
    #[test]
    fn pr912_lombok_members_are_synthesized_and_resolve() {
        let project = Project::new()
            .file(
                "model/User.java",
                "package model;\nimport lombok.Data;\nimport lombok.Builder;\nimport lombok.extern.slf4j.Slf4j;\n\n@Data\n@Builder\n@Slf4j\npublic class User {\n    private String name;\n    private boolean active;\n    private static final int MAX = 10;\n}\n",
            )
            .file(
                "svc/UserService.java",
                "package svc;\nimport model.User;\n\nclass UserService {\n    String describe(User user) {\n        user.setActive(true);\n        return user.getName();\n    }\n    User make() {\n        return User.builder();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let synthesized = |name: &str| {
            graph
                .nodes
                .iter()
                .find(|node| node.name == name && is_lombok(node))
                .unwrap_or_else(|| panic!("no synthesized {name}\n{}", graph.report()))
        };
        for name in [
            "getName",
            "setName",
            "isActive",
            "setActive",
            "builder",
            "equals",
            "hashCode",
            "toString",
        ] {
            let node = synthesized(name);
            assert_eq!(node.kind, "method", "{name}");
            assert_eq!(node.file_path, "model/User.java", "{name}");
            assert_eq!(node.qualified_name, format!("model::User::{name}"));
        }
        let get_name = synthesized("getName");
        assert_eq!(
            get_name.docstring.as_deref(),
            Some("Lombok-generated (@Data)")
        );
        assert_eq!(get_name.signature.as_deref(), Some("String getName()"));
        assert_eq!(get_name.return_type.as_deref(), Some("String"));
        assert_eq!(get_name.visibility.as_deref(), Some("public"));
        assert_eq!(get_name.start_line, 10);
        assert_eq!(
            synthesized("isActive").signature.as_deref(),
            Some("boolean isActive()")
        );
        assert_eq!(
            synthesized("setName").signature.as_deref(),
            Some("void setName(String name)")
        );
        let builder = synthesized("builder");
        assert_eq!(
            builder.signature.as_deref(),
            Some("static User.UserBuilder builder()")
        );
        assert!(builder.is_static);
        assert_eq!(builder.return_type.as_deref(), Some("UserBuilder"));
        assert_eq!(
            builder.docstring.as_deref(),
            Some("Lombok-generated (@Builder)")
        );
        let log = synthesized("log");
        assert_eq!(log.kind, "field");
        assert!(log.is_static);
        assert_eq!(log.visibility.as_deref(), Some("private"));
        assert_eq!(log.signature.as_deref(), Some("Logger log"));
        assert_eq!(log.docstring.as_deref(), Some("Lombok-generated (@Slf4j)"));
        assert!(
            !graph
                .nodes
                .iter()
                .any(|node| node.name == "getMAX" || node.name == "getMax"),
            "{}",
            graph.report()
        );
        let calls = calls_in(graph, "svc/UserService.java");
        for call in [
            "svc::UserService::describe -> model::User::getName",
            "svc::UserService::describe -> model::User::setActive",
            "svc::UserService::make -> model::User::builder",
        ] {
            assert!(
                calls.contains(&call.to_string()),
                "{call}\n{}",
                graph.report()
            );
        }
    }

    /// Upstream #912: a member the source declares is never generated
    /// again; the other fields still get theirs.
    #[test]
    fn pr912_lombok_never_overrides_a_hand_written_accessor() {
        let project = Project::new()
            .file(
                "model/Account.java",
                "package model;\nimport lombok.Getter;\n\n@Getter\npublic class Account {\n    private int balance;\n    private String owner;\n\n    // explicit getter — Lombok skips it, so must we\n    public int getBalance() { return balance < 0 ? 0 : balance; }\n}\n",
            )
            .index();
        let graph = project.graph();
        let balance: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.name == "getBalance")
            .collect();
        assert_eq!(balance.len(), 1, "{}", graph.report());
        assert!(!is_lombok(balance[0]), "{}", graph.report());
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.name == "getOwner" && is_lombok(node)),
            "{}",
            graph.report()
        );
    }

    /// Upstream #912: field-level `@Getter` / `@Setter`, a `final` field
    /// gets no setter, and a field without either gets nothing.
    #[test]
    fn pr912_lombok_field_annotations_and_final_fields() {
        let project = Project::new()
            .file(
                "model/Box.java",
                "package model;\nimport lombok.Getter;\nimport lombok.Setter;\n\npublic class Box {\n    @Getter @Setter private String label;\n    @Getter private final long id;     // final → getter only, no setter\n    private int hidden;                // no annotation → nothing\n}\n",
            )
            .index();
        let graph = project.graph();
        let lombok = |name: &str| {
            graph
                .nodes
                .iter()
                .any(|node| node.name == name && is_lombok(node))
        };
        assert!(lombok("getLabel"), "{}", graph.report());
        assert!(lombok("setLabel"), "{}", graph.report());
        assert!(lombok("getId"), "{}", graph.report());
        assert!(
            !graph.nodes.iter().any(|node| node.name == "setId"),
            "{}",
            graph.report()
        );
        assert!(
            !graph.nodes.iter().any(|node| node.name == "getHidden"),
            "{}",
            graph.report()
        );
    }

    /// Upstream #912: a plain Java class gets nothing.
    #[test]
    fn pr912_a_plain_java_class_gets_no_synthesized_members() {
        let project = Project::new()
            .file(
                "model/Plain.java",
                "package model;\n\npublic class Plain {\n    private int value;\n    public int getValue() { return value; }\n    public void setValue(int v) { this.value = v; }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert!(!graph.nodes.iter().any(is_lombok), "{}", graph.report());
    }
}

mod kotlin {
    use super::support::{Graph, Project};

    /// `target qualified-name` of every `calls` edge out of `file`'s nodes,
    /// sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// The files the `calls` edges out of `file` named `name` land in.
    fn call_targets_named(graph: &Graph, file: &str, name: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .filter(|node| node.name == name)
            .map(|node| node.file_path.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2146 (`75976f99`): a top-level Kotlin function, an extension
    /// function included, is visible from its own package and from a file
    /// that imports it by name or imports its package with `*`; a file
    /// importing kotlin.test's `assertEquals` reaches neither project helper
    /// (Exposed's 3,075 calls). A function type with a qualified receiver
    /// keeps the class around it: tree-sitter-kotlin-ng parses it, so
    /// upstream's pre-parse is not needed (N/A).
    #[test]
    fn pr2146_kotlin_top_level_functions_are_visible_where_imported() {
        let project = Project::new()
            .file(
                "jdbc/src/main/kotlin/app/jdbc/tests/Assert.kt",
                "package app.jdbc.tests\n\nclass JdbcTransaction\n\nfun withTables(statement: JdbcTransaction.() -> Unit) {}\n\nfun <T> JdbcTransaction.assertEquals(exp: T, act: T) {}\n",
            )
            .file(
                "r2dbc/src/main/kotlin/app/r2dbc/tests/Assert.kt",
                "package app.r2dbc.tests\n\nclass R2dbcTransaction\n\nfun withTables(statement: R2dbcTransaction.() -> Unit) {}\n\nfun <T> R2dbcTransaction.assertEquals(exp: T, act: T) {}\n",
            )
            .file(
                "jdbc/src/test/kotlin/app/jdbc/JdbcTest.kt",
                "package app.jdbc\n\nimport app.jdbc.tests.assertEquals\nimport app.jdbc.tests.withTables\n\nclass JdbcTest {\n  fun works() {\n    withTables { assertEquals(1, 1) }\n  }\n}\n",
            )
            .file(
                "r2dbc/src/test/kotlin/app/r2dbc/R2dbcTest.kt",
                "package app.r2dbc\n\nimport app.r2dbc.tests.*\n\nclass R2dbcTest {\n  fun works() {\n    withTables { assertEquals(1, 1) }\n  }\n}\n",
            )
            .file(
                "core/src/test/kotlin/app/core/PlainTest.kt",
                "package app.core\n\nimport kotlin.test.assertEquals\n\nclass PlainTest {\n  fun works() {\n    assertEquals(1, 1)\n  }\n}\n",
            )
            .file(
                "jdbc/src/main/kotlin/app/jdbc/tests/Local.kt",
                "package app.jdbc.tests\n\nfun check() {\n  withTables { assertEquals(1, 1) }\n}\n",
            )
            .file(
                "jdbc/src/main/kotlin/app/jdbc/tests/DatabaseTestsBase.kt",
                "package app.jdbc.tests\n\nclass DatabaseConfig {\n  class Builder\n}\n\n@MethodSource(\"data\")\nabstract class DatabaseTestsBase {\n  fun withTables(configure: (DatabaseConfig.Builder.() -> Unit)? = null, statement: () -> Unit) {\n    statement()\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            call_targets_named(
                graph,
                "jdbc/src/test/kotlin/app/jdbc/JdbcTest.kt",
                "assertEquals"
            ),
            ["jdbc/src/main/kotlin/app/jdbc/tests/Assert.kt"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets_named(
                graph,
                "r2dbc/src/test/kotlin/app/r2dbc/R2dbcTest.kt",
                "assertEquals"
            ),
            ["r2dbc/src/main/kotlin/app/r2dbc/tests/Assert.kt"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets_named(
                graph,
                "core/src/test/kotlin/app/core/PlainTest.kt",
                "assertEquals"
            ),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets_named(
                graph,
                "jdbc/src/main/kotlin/app/jdbc/tests/Local.kt",
                "assertEquals"
            ),
            ["jdbc/src/main/kotlin/app/jdbc/tests/Assert.kt"],
            "{}",
            graph.report()
        );
        let with_tables: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.file_path == "jdbc/src/main/kotlin/app/jdbc/tests/DatabaseTestsBase.kt"
                    && node.name == "withTables"
            })
            .map(|node| (node.kind.as_str(), node.qualified_name.as_str()))
            .collect();
        assert_eq!(
            with_tables,
            [("method", "app.jdbc.tests::DatabaseTestsBase::withTables")],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2196 (`da1312e8`): Kotlin imports (no `;`, an optional
    /// `as`) are read, an outside import owns its name, a member the file
    /// declares itself comes first, and a renaming import binds its alias by
    /// the imported FQN.
    #[test]
    fn pr2196_kotlin_imports_outside_names_and_aliases() {
        let project = Project::new()
            .file(
                "src/test/java/app/functional/TypesTest.java",
                "package app.functional;\n\npublic class TypesTest {\n  static class Field {\n    String name;\n  }\n\n  static void assertEquals(Object a, Object b) { }\n}\n",
            )
            .file(
                "src/main/java/app/model/Outer.java",
                "package app.model;\n\npublic class Outer {\n  public static class Inner {\n    public static Inner create() { return new Inner(); }\n  }\n}\n",
            )
            .file(
                "src/main/kotlin/app/Dates.kt",
                "package app\n\nimport java.lang.reflect.Field\nimport kotlinx.datetime.toLocalDateTime\nimport app.model.Outer.Inner as Made\n\nclass Dates {\n    private fun toLocalDateTime(value: Long): Long = value\n\n    fun parse(value: Long, field: Field): Long {\n        field.getName()\n        Made.create()\n        return toLocalDateTime(value)\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains" && edge.kind != "imports")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "src/main/kotlin/app/Dates.kt")
            })
            .filter_map(|edge| {
                graph
                    .node(&edge.target)
                    .map(|target| format!("{} {}", edge.kind, target.qualified_name))
            })
            .collect();
        assert!(
            !out.contains(&"references app.functional::TypesTest::Field".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            out.contains(&"calls app::Dates::toLocalDateTime".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            out.contains(&"calls app.model::Outer::Inner::create".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2172 (`2e2b98c9`): a bare Kotlin call reaches a member of a
    /// type around it (an anonymous `object : Table()` included), of a type
    /// the project's function types take as a lambda receiver, or of an
    /// object member the file imports; never another class's member by name
    /// (koin's `error("…")` went to a Logger's `error`). `require(n >= 0) {
    /// … }` is Kotlin's precondition, not a member `require`.
    #[test]
    fn pr2172_bare_kotlin_calls_reach_only_members_in_reach() {
        let project = Project::new()
            .file(
                "src/main/kotlin/app/dsl/Module.kt",
                "package app.dsl\n\nclass Scope {\n    fun get(): Any = 1\n}\n\nclass Koin {\n    fun get(): Any = 2\n}\n\nclass Module {\n    fun single(definition: Scope.() -> Any) { }\n}\n\nfun module(declaration: Module.() -> Unit): Module = Module().apply(declaration)\n",
            )
            .file(
                "src/main/kotlin/app/log/Logger.kt",
                "package app.log\n\nclass Logger {\n    fun error(message: String) { }\n}\n",
            )
            .file(
                "src/main/kotlin/app/db/Table.kt",
                "package app.db\n\nabstract class Table {\n    fun integer(name: String): Int = 0\n}\n",
            )
            .file(
                "src/main/kotlin/app/io/Source.kt",
                "package app.io\n\ninterface Source {\n    fun require(byteCount: Long)\n}\n\nclass RealSource : Source {\n    override fun require(byteCount: Long) { }\n\n    fun read(byteCount: Long) {\n        require(byteCount >= 0L) { \"byteCount < 0: $byteCount\" }\n        require(byteCount)\n    }\n}\n",
            )
            .file(
                "src/test/kotlin/app/TestUtil.kt",
                "package app\n\nobject TestUtil {\n    fun deepCopy(value: Int): Int = value\n}\n",
            )
            .file(
                "src/test/kotlin/app/AppTest.kt",
                "package app\n\nimport app.TestUtil.deepCopy\nimport app.db.Table\nimport app.dsl.module\n\nclass AppTest {\n    fun wires() {\n        val m = module {\n            single { get() }\n        }\n        val table = object : Table() {\n            val id = integer(\"id\")\n        }\n        val copy = deepCopy(1)\n        if (copy < 0) error(\"negative\")\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let calls = calls_from(graph, "src/test/kotlin/app/AppTest.kt");
        for wanted in [
            "app.dsl::Module::single",
            "app.dsl::Scope::get",
            "app.db::Table::integer",
            "app::TestUtil::deepCopy",
        ] {
            assert!(
                calls.contains(&wanted.to_string()),
                "missing {wanted}: {calls:?}\n{}",
                graph.report()
            );
        }
        for unwanted in ["app.dsl::Koin::get", "app.log::Logger::error"] {
            assert!(
                !calls.contains(&unwanted.to_string()),
                "unexpected {unwanted}: {calls:?}\n{}",
                graph.report()
            );
        }
        let read: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.file_path == "src/main/kotlin/app/io/Source.kt" && node.name == "read"
            })
            .map(|node| node.id.as_str())
            .collect();
        let mut lines: Vec<i64> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" && read.contains(&edge.source.as_str()))
            .filter_map(|edge| edge.line)
            .collect();
        lines.sort();
        assert_eq!(lines, [12], "{}", graph.report());
    }

    /// Upstream #2168 (`b7b2a44c`): a primary constructor on the line after
    /// its class name keeps the class (tree-sitter-kotlin-ng parses it, so
    /// the pre-parse is N/A), and a standard-library name on an untyped chain
    /// link needs a receiver named after the owner: `.apply { }` never goes to
    /// `ConnectionSpec.apply`.
    #[test]
    fn pr2168_split_constructors_keep_their_class_and_std_chain_names_stay_std() {
        let project = Project::new()
            .file(
                "src/commonMain/kotlin/okio/ByteString.kt",
                "package okio\n\nexpect open class ByteString\n// Trusted internal constructor doesn't clone data.\ninternal constructor(data: ByteArray) : Comparable<ByteString> {\n  fun hex(): String\n  fun toByteArray(): ByteArray\n}\n",
            )
            .file(
                "src/main/kotlin/app/ConnectionSpec.kt",
                "package app\n\nclass ConnectionSpec {\n  fun apply(sslSocket: Any, isFallback: Boolean) {}\n}\n",
            )
            .file(
                "src/main/kotlin/app/Builder.kt",
                "package app\n\nclass Builder {\n  var name = \"\"\n  fun build(): String = StringBuilder().apply { append(name) }.toString()\n}\n",
            )
            .file(
                "src/main/kotlin/app/Chains.kt",
                "package app\n\nclass Chains {\n  fun names(items: List<String>): List<String> =\n    items.filter { it.isNotEmpty() }.map { it.trim() }\n}\n",
            )
            .file(
                "src/main/kotlin/app/Mapper.kt",
                "package app\n\nclass Mapper {\n  fun map(value: String): String = value\n}\n",
            )
            .index();
        let graph = project.graph();
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.name == "hex" && node.qualified_name == "okio::ByteString::hex"),
            "{}",
            graph.report()
        );
        assert!(
            !calls_from(graph, "src/main/kotlin/app/Builder.kt")
                .iter()
                .any(|target| target.ends_with("ConnectionSpec::apply")),
            "{}",
            graph.report()
        );
        assert!(
            !calls_from(graph, "src/main/kotlin/app/Chains.kt")
                .contains(&"app::Mapper::map".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2206 (`fc73204f`): a lambda runs on the receiver its
    /// function's parameter type names, directly or through a typealias, and
    /// the members the code around a bare call reaches come before those
    /// only another lambda type in the project could: koin's `single {
    /// C51(get(), get()) }` and `Scope.new(…)`'s `get()` are Scope's.
    #[test]
    fn pr2206_a_kotlin_lambda_runs_on_its_own_receiver() {
        let project = Project::new()
            .file(
                "src/main/kotlin/org/koin/core/Koin.kt",
                "package org.koin.core\n\nclass Koin {\n    fun <T> get(): T = TODO()\n}\n\nfun koinTest(block: Koin.() -> Unit) { }\n",
            )
            .file(
                "src/main/kotlin/org/koin/core/scope/Scope.kt",
                "package org.koin.core.scope\n\nclass Scope {\n    fun <T> get(): T = TODO()\n}\n",
            )
            .file(
                "src/main/kotlin/org/koin/core/definition/Definition.kt",
                "package org.koin.core.definition\n\nimport org.koin.core.scope.Scope\n\ntypealias Definition<T> = Scope.(Int) -> T\n",
            )
            .file(
                "src/main/kotlin/org/koin/core/module/Module.kt",
                "package org.koin.core.module\n\nimport org.koin.core.definition.Definition\n\nclass Module {\n    fun <T> single(definition: Definition<T>) { }\n}\n",
            )
            .file(
                "src/main/kotlin/org/koin/dsl/New.kt",
                "package org.koin.dsl\n\nimport org.koin.core.scope.Scope\n\ninline fun <reified R, reified T1, reified T2> Scope.new(\n    constructor: (T1, T2) -> R,\n): R = constructor(get(), get())\n",
            )
            .file(
                "src/test/kotlin/app/Perfs.kt",
                "package app\n\nimport org.koin.core.module.Module\n\nclass C51(a: Int, b: Int)\n\nfun perfModule(m: Module) {\n    m.single { C51(get(), get()) }\n}\n",
            )
            .index();
        let graph = project.graph();
        let gets = |file: &str| {
            let mut out: Vec<String> = calls_from(graph, file)
                .into_iter()
                .filter(|target| target.ends_with("::get"))
                .collect();
            out.dedup();
            out
        };
        assert_eq!(
            gets("src/test/kotlin/app/Perfs.kt"),
            ["org.koin.core.scope::Scope::get"],
            "{}",
            graph.report()
        );
        assert_eq!(
            gets("src/main/kotlin/org/koin/dsl/New.kt"),
            ["org.koin.core.scope::Scope::get"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2232 (`27da2563`): a Gradle script's bare calls run on the
    /// build tool's own types, never on a class the project declares
    /// (nowinandroid's `Graph.plugins()`); build logic's extension functions
    /// on Gradle's types stay.
    #[test]
    fn pr2232_a_gradle_script_bare_call_is_never_a_project_class_member() {
        let project = Project::new()
            .file(
                "build-logic/src/main/kotlin/com/example/Graph.kt",
                "package com.example\n\nclass Graph {\n    fun plugins(): List<String> = emptyList()\n    fun dependencies(): List<String> = emptyList()\n}\n",
            )
            .file(
                "build-support/src/main/kotlin/SourceSets.kt",
                "import org.gradle.api.NamedDomainObjectContainer\n\nfun NamedDomainObjectContainer<String>.createSourceSet(name: String) {\n}\n",
            )
            .file(
                "app/build.gradle.kts",
                "plugins {\n    id(\"com.android.application\")\n}\n\ndependencies {\n    implementation(libs.core)\n}\n\nkotlin {\n    sourceSets {\n        createSourceSet(\"nonJvm\")\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let names: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "app/build.gradle.kts")
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.name.clone())
            .collect();
        assert!(
            !names.contains(&"plugins".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            !names.contains(&"dependencies".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            names.contains(&"createSourceSet".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2174 (`c6491ff6`): a Kotlin infix call, `id eq id1` or
    /// koin's `single { … } bind Ops::class`, is a call of the infix
    /// function (`id.eq` when the left operand is a plain name, else bare
    /// `bind`); a standard infix name on an expression receiver,
    /// `alias(…) apply false` in a build script, is not a project method's.
    /// Upstream's fixture, verbatim. Upstream's `id.eq` reaches `Column::eq`
    /// through the type `id` is declared with in the class, #2171, which
    /// W4-CS-03 ports; until then it is recorded and left unresolved.
    #[test]
    fn pr2174_kotlin_infix_calls_call_the_infix_function() {
        let project = Project::new()
            .file(
                "src/main/kotlin/app/Ops.kt",
                "package app\n\nclass Column {\n    infix fun eq(other: Int): Boolean = true\n}\n\nclass Definition {\n    infix fun bind(type: Any): Definition = this\n}\n\nfun single(block: () -> Any): Definition = Definition()\n\nclass Ops(private val id: Column) {\n    fun same(id1: Int): Boolean = id eq id1\n\n    fun wire() {\n        single { Ops(Column()) } bind Ops::class\n    }\n}\n\nclass Spec {\n    fun apply(block: Spec.() -> Unit): Spec = this\n}\n",
            )
            .file(
                "build.gradle.kts",
                "plugins {\n    alias(libs.plugins.kotlin.jvm) apply false\n}\n",
            )
            .index();
        let graph = project.graph();
        let calls = calls_from(graph, "src/main/kotlin/app/Ops.kt");
        assert!(
            graph
                .unresolved
                .iter()
                .any(|r| r.file_path == "src/main/kotlin/app/Ops.kt" && r.reference_name == "id.eq"),
            "{}",
            graph.report()
        );
        assert!(
            calls.contains(&"app::Definition::bind".to_string()),
            "{}",
            graph.report()
        );
        let script = calls_from(graph, "build.gradle.kts");
        assert!(
            !script.contains(&"app::Spec::apply".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            graph
                .unresolved
                .iter()
                .any(|r| r.file_path == "build.gradle.kts" && r.reference_name == "apply"),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2174: a project's bitwise extension on a number type,
    /// okio's `infix fun Byte.and(mask: Int)`, is never chosen, by name, on a
    /// typed receiver (`b: Int`) or guessed from the receiver's name
    /// (`byte shr 4`): the standard library's `Int.and` / `Long.shr` members
    /// take `x and 0xff`. The calls are still recorded.
    #[test]
    fn pr2174_a_project_bitwise_extension_on_a_number_type_is_never_chosen() {
        let project = Project::new()
            .file(
                "src/main/kotlin/okio/Util.kt",
                "package okio\n\ninfix fun Byte.and(mask: Int): Int = toInt() and mask\n\ninfix fun Int.and(mask: Long): Long = toLong() and mask\n\ninfix fun Byte.shr(bitCount: Int): Int = toInt() shr bitCount\n\nclass Buffer {\n    fun readInt(): Int {\n        val b: Int = 5\n        val medium = 7\n        return (b and 0xff) or (medium shr 8)\n    }\n\n    fun high(bytes: List<Byte>): List<Int> = bytes.map { byte -> byte shr 4 }\n}\n",
            )
            .index();
        let graph = project.graph();
        let bitwise: Vec<&str> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter_map(|edge| graph.node(&edge.target))
            .filter(|node| node.name == "and" || node.name == "shr")
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert!(bitwise.is_empty(), "{}", graph.report());
        let mut recorded: Vec<&str> = graph
            .unresolved
            .iter()
            .filter(|r| r.reference_kind == "calls")
            .map(|r| r.reference_name.as_str())
            .filter(|name| name.ends_with("and") || name.ends_with("shr"))
            .collect();
        recorded.sort_unstable();
        assert_eq!(
            recorded,
            ["and", "and", "b.and", "byte.shr", "medium.shr", "shr"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2182 (`fcea0222`), the part tree-sitter-kotlin-ng 1.1.0
    /// still lacks: a `when` guard (`is String if mode == 2 ->`) and a
    /// multi-dollar string (`$$"…"`) are rewritten, offsets intact, before
    /// the parse; either one made the file contribute nothing. The grammar
    /// parses `..<` and a nullable function-type receiver itself (N/A).
    /// Upstream's fixture, verbatim.
    #[test]
    fn pr2182_newer_kotlin_syntax_keeps_the_class_around_it() {
        let project = Project::new()
            .file(
                "src/Dialect.kt",
                "package app\n\nclass Dialect(private val mode: Int) {\n    fun sql(value: Any?): String = when (value) {\n        is String if mode == 2 -> \"text\"\n        is Int if (\n            mode > 0\n        ) -> \"int\"\n        is Long -> if (mode == 1) \"long\" else \"big\"\n        else -> \"other\"\n    }\n\n    fun ranges(n: Int) {\n        for (i in 0..<n) println(i)\n    }\n\n    fun path(team: String) = $$\"?(@.user.team == $team)\"\n\n    fun adjust(body: Op<Boolean>?.() -> Op<Boolean>): Dialect = this\n\n    fun after(): Int = 1\n}\n\nclass Op<T>\n",
            )
            .index();
        let graph = project.graph();
        let mut methods: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == "src/Dialect.kt" && node.kind == "method")
            .map(|node| node.qualified_name.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(
            methods,
            [
                "app::Dialect::adjust",
                "app::Dialect::after",
                "app::Dialect::path",
                "app::Dialect::ranges",
                "app::Dialect::sql",
            ],
            "{}",
            graph.report()
        );
    }
}

mod scala {
    use super::support::{Graph, Project};

    /// `target qualified-name:line` of every edge of `kinds` out of `file`'s
    /// nodes, sorted.
    fn edges_from(graph: &Graph, file: &str, kinds: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| kinds.contains(&edge.kind.as_str()))
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| {
                let target = graph.node(&edge.target)?;
                Some(format!(
                    "{}:{}",
                    target.qualified_name,
                    edge.line.unwrap_or_default()
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// The files the edges of `kinds` out of `file` land in, sorted.
    fn target_files(graph: &Graph, file: &str, kinds: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| kinds.contains(&edge.kind.as_str()))
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.file_path.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2160 (`7aa124c0`): a bare Scala name reached by name alone
    /// is in reach: a parameter binds `f`, a later chain link reaches a
    /// member of what its receiver is named after, and otherwise a member of
    /// the types around it and their supertypes. Upstream's fixture, with two
    /// changes: its `val parser = new OptionParser[Int]("scopt") { head(…) }`
    /// sits in a `def`, as the extractor does not walk a Scala `val`'s
    /// initializer (pre-existing, upstream #1802, recorded for the owner), and
    /// an `Other` class declares `head` and `map` too, so each assertion has a
    /// namesake to reject. A `More` object adds a bare call and a chain link
    /// whose only same-named candidates are an unrelated class's.
    #[test]
    fn pr2160_bare_scala_calls_stay_in_reach() {
        let project = Project::new()
            .file(
                "src/main/scala/app/FreeT.scala",
                "package app\n\nfinal case class FlatMapped[A](f: A => A)\n",
            )
            .file(
                "src/main/scala/app/LazyOps.scala",
                "package app\n\nclass LazyOps[A](xs: List[A]) {\n  def map[B](g: A => B): List[B] = xs.map(g)\n}\n",
            )
            .file(
                "src/main/scala/app/OptionParser.scala",
                "package app\n\nabstract class OptionParser[C](name: String) {\n  def head(xs: String*): Unit = ()\n}\n",
            )
            .file(
                "src/main/scala/app/Base.scala",
                "package app\n\ntrait Base {\n  def helper(): Int = 1\n}\n",
            )
            .file(
                "src/main/scala/app/Request.scala",
                "package app\n\nclass Request {\n  def send(backend: String): String = backend\n}\n",
            )
            .file(
                "src/main/scala/app/Use.scala",
                "package app\n\nobject Use extends Base {\n  def tabulate[A](f: Boolean => A): (A, A) = (f(true), f(false))\n  def twice(fa: List[Int]): List[Int] = fa.iterator.map(_ + 1).toList\n  def run(basicRequest: Request): String = basicRequest.send(\"b\")\n  def local(): Int = helper()\n  def parser(): OptionParser[Int] = new OptionParser[Int](\"scopt\") {\n    head(\"scopt\", \"3.x\")\n  }\n}\n",
            )
            .file(
                "src/main/scala/app/Other.scala",
                "package app\n\nclass Other {\n  def head(xs: String*): Unit = ()\n  def map(i: Int): Int = i\n}\n",
            )
            .file(
                "src/main/scala/app/Unrelated.scala",
                "package app\n\nclass Unrelated {\n  def helper2(): Int = 1\n  def size2(): Int = 2\n}\n",
            )
            .file(
                "src/main/scala/app/More.scala",
                "package app\n\nobject More extends Base {\n  def other(): Int = helper2()\n  def chain(): Int = List(1).size2()\n}\n",
            )
            .index();
        let graph = project.graph();
        // The one `helper2` and the one `size2` are an unrelated class's: out
        // of reach of a bare call and of a chain link on `List(1)`.
        let more = edges_from(graph, "src/main/scala/app/More.scala", &["calls"]);
        assert!(
            !more.iter().any(|call| call.starts_with("Unrelated::")),
            "{}",
            graph.report()
        );
        let calls = edges_from(graph, "src/main/scala/app/Use.scala", &["calls"]);
        let has = |suffix: &str| calls.iter().any(|call| call.contains(suffix));
        assert!(!has("FlatMapped::f:"), "{}", graph.report());
        assert!(!has("LazyOps::map:"), "{}", graph.report());
        assert!(!has("Other::map:"), "{}", graph.report());
        assert!(has("Request::send:"), "{}", graph.report());
        assert!(has("Base::helper:"), "{}", graph.report());
        assert!(has("OptionParser::head:"), "{}", graph.report());
        assert!(!has("Other::head:"), "{}", graph.report());
    }

    /// Upstream #2193 (`8410db42`): a `val` written in a block of a class
    /// body is that block's, a class-body binder counts only in a block still
    /// open at the site, and a package object's supertypes are in scope where
    /// it is imported and in its own package. Upstream's fixture, verbatim.
    #[test]
    fn pr2193_block_local_vals_and_package_objects() {
        let project = Project::new()
            .file(
                "src/test/scala/app/ASuite.scala",
                "package app\n\nclass ASuite {\n  def test(name: String)(body: => Unit): Unit = ()\n\n  test(\"a\") {\n    val f = (i: Int) => i\n    f(1)\n  }\n\n  test(\"c\") {\n    f(2)\n  }\n}\n",
            )
            .file(
                "src/test/scala/app/BSuite.scala",
                "package app\n\nimport app.syntax.all._\n\nclass BSuite {\n  def test(name: String)(body: => Unit): Unit = ()\n  def forAll(p: (Int, Int => Int) => Boolean): Unit = ()\n\n  test(\"b\") {\n    forAll { (e: Int, f: Int => Int) =>\n      f(e) == 1\n    }\n  }\n}\n",
            )
            .file(
                "src/main/scala/app/syntax/all.scala",
                "package app.syntax\n\nobject all\n",
            )
            .file(
                "src/main/scala/app/Api.scala",
                "package app\n\ntrait Api {\n  def multipart(s: String): String = s\n}\n",
            )
            .file(
                "src/main/scala/app/api/package.scala",
                "package app\n\npackage object api extends Api\n",
            )
            .file(
                "src/main/scala/app/client/Client.scala",
                "package app.client\n\nimport app.api._\n\nclass Client {\n  def send(): String = multipart(\"x\")\n}\n",
            )
            .file(
                "src/main/scala/app/api/Inner.scala",
                "package app.api\n\nclass Inner {\n  def send(): String = multipart(\"y\")\n}\n",
            )
            .index();
        let graph = project.graph();
        let a_suite: Vec<String> = edges_from(graph, "src/test/scala/app/ASuite.scala", &["calls"])
            .into_iter()
            .filter(|call| call.starts_with("ASuite::f"))
            .collect();
        assert_eq!(a_suite, ["ASuite::f:8"], "{}", graph.report());
        assert!(
            !edges_from(graph, "src/test/scala/app/BSuite.scala", &["calls"])
                .iter()
                .any(|call| call.contains("::f:")),
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/main/scala/app/client/Client.scala", &["calls"]),
            ["Api::multipart:6"],
            "{}",
            graph.report()
        );
        assert_eq!(
            edges_from(graph, "src/main/scala/app/api/Inner.scala", &["calls"]),
            ["Api::multipart:4"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2199 (`7493fdef`): a chain link is never an object named
    /// like the member, and reaches a type the receiver's package path names
    /// (`app.client.Response(200)`, recorded as `client.Response` here, KEEP-RUST
    /// G14). Upstream's fixture, verbatim.
    #[test]
    fn pr2199_a_chain_link_is_never_an_object_named_like_the_member() {
        let project = Project::new()
            .file(
                "src/main/scala/alleycats/std/map.scala",
                "package alleycats.std\n\nobject map {\n  val instances: Int = 1\n}\n",
            )
            .file(
                "src/main/scala/app/client/Response.scala",
                "package app.client\n\ncase class Response(code: Int)\n",
            )
            .file(
                "src/main/scala/app/Use.scala",
                "package app\n\nclass Use {\n  def gen[A](a: A): List[A] = List(a)\n  def run(): List[Int] = gen[Int](1).map { x => x + 1 }\n  def make(): Any = app.client.Response(200)\n}\n",
            )
            .index();
        let graph = project.graph();
        let files = target_files(
            graph,
            "src/main/scala/app/Use.scala",
            &["calls", "instantiates"],
        );
        assert!(
            !files.contains(&"src/main/scala/alleycats/std/map.scala".to_string()),
            "{}",
            graph.report()
        );
        assert!(
            files.contains(&"src/main/scala/app/client/Response.scala".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2217 (`fa43e259`): a kind-projector placeholder in a type is
    /// no reference to a `*` method. Upstream's fixture, verbatim.
    #[test]
    fn pr2217_a_symbolic_name_in_a_type_is_never_an_operator() {
        let project = Project::new()
            .file(
                "src/main/scala/algebra/Signed.scala",
                "package algebra\n\nobject Signed {\n  sealed abstract class Sign(val toInt: Int) {\n    def *(that: Sign): Sign = this\n  }\n}\n",
            )
            .file(
                "src/main/scala/cats/Align.scala",
                "package cats\n\ntrait Align[F[_]]\n\nobject Align {\n  implicit def catsAlignForEither[A]: Align[Either[A, *]] = null\n}\n",
            )
            .index();
        let graph = project.graph();
        let names: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "references")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "src/main/scala/cats/Align.scala")
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.name.clone())
            .collect();
        assert!(!names.contains(&"*".to_string()), "{}", graph.report());
    }

    /// Upstream #2230 (`3878123f`): a package object's member is in scope in
    /// its package and those under it, or through an import of its package.
    /// Upstream's fixture, verbatim.
    #[test]
    fn pr2230_a_package_object_member_is_in_scope_in_its_package_or_by_import() {
        let project = Project::new()
            .file(
                "algebra-core/src/main/scala/algebra/package.scala",
                "package object algebra {\n  type Eq[A] = cats.kernel.Eq[A]\n}\n",
            )
            .file(
                "core/src/main/scala/cats/package.scala",
                "package object cats {\n  type Eq[A] = cats.kernel.Eq[A]\n}\n",
            )
            .file(
                "laws/src/main/scala/cats/laws/FunctorLaws.scala",
                "package cats\npackage laws\n\ntrait FunctorLaws {\n  def identity[A](eq: Eq[A]): Boolean = true\n}\n",
            )
            .file(
                "algebra-laws/src/main/scala/algebra/laws/RingLaws.scala",
                "package algebra.laws\n\nimport algebra._\n\ntrait RingLaws {\n  def eqv[A](eq: Eq[A]): Boolean = true\n}\n",
            )
            .index();
        let graph = project.graph();
        let eq_files = |file: &str| {
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|edge| edge.kind == "references")
                .filter(|edge| {
                    graph
                        .node(&edge.source)
                        .is_some_and(|node| node.file_path == file)
                })
                .filter_map(|edge| graph.node(&edge.target))
                .filter(|node| node.name == "Eq")
                .map(|node| node.file_path.clone())
                .collect();
            out.sort();
            out.dedup();
            out
        };
        assert_eq!(
            eq_files("laws/src/main/scala/cats/laws/FunctorLaws.scala"),
            ["core/src/main/scala/cats/package.scala"],
            "{}",
            graph.report()
        );
        assert_eq!(
            eq_files("algebra-laws/src/main/scala/algebra/laws/RingLaws.scala"),
            ["algebra-core/src/main/scala/algebra/package.scala"],
            "{}",
            graph.report()
        );
    }
}

mod csharp {
    use super::support::{Graph, Project};

    /// `kind qualified-name` of every node of `file` but its file node.
    fn qualified_names(graph: &Graph, file: &str) -> Vec<String> {
        graph
            .nodes
            .iter()
            .filter(|node| node.file_path == file && node.kind != "file")
            .map(|node| format!("{} {}", node.kind, node.qualified_name))
            .collect()
    }

    /// `kind file:qualified-name` of what the edges out of `file`'s nodes at
    /// `line` (of `kind`, when given) reach, sorted.
    fn targets_at(graph: &Graph, file: &str, line: i64, kind: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.line == Some(line))
            .filter(|edge| kind.is_none_or(|kind| edge.kind == kind))
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{} {}:{}", node.kind, node.file_path, node.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2121 (`e453e820`), its C# half: a type position names a
    /// type. `Type sourceType`, `List<int>` and `new TypeMap()` bound to
    /// whichever same-named member was nearest: an attribute's `Type`
    /// property, a test class's `List` property, the `TypeMap` property beside
    /// the `TypeMap` class. Upstream's fixture.
    #[test]
    fn pr2121_a_csharp_type_position_names_a_type_never_a_member() {
        let project = Project::new()
            .file(
                "src/TypeMap.cs",
                "namespace App;\npublic class TypeMap {\n    public string Name { get; set; }\n}\n\
                 public class MemberMap {\n    public MemberMap() { }\n}\n",
            )
            .file(
                "src/Attrs.cs",
                "namespace App;\npublic class ValueConverterAttribute {\n    public System.Type Type { get; set; }\n}\n\
                 public class Source {\n    public System.Collections.Generic.List<int> List { get; set; }\n}\n",
            )
            .file(
                "src/Profile.cs",
                "namespace App;\nusing System;\nusing System.Collections.Generic;\npublic class Profile {\n\
                 \x20   public TypeMap TypeMap { get; set; }\n\
                 \x20   public void Configure(TypeMap typeMap, MemberMap memberMap, Type sourceType) {\n\
                 \x20       var items = new List<int>();\n\
                 \x20       var copy = new TypeMap();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();

        let configure = graph
            .nodes
            .iter()
            .find(|node| node.kind == "method" && node.name == "Configure")
            .expect("Configure is indexed");
        let targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == configure.id)
            .filter(|edge| ["references", "instantiates", "type_of"].contains(&edge.kind.as_str()))
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{}:{}", node.kind, node.name))
            .collect();
        assert!(
            targets.contains(&"class:TypeMap".to_owned()),
            "{}",
            graph.report()
        );
        assert!(
            targets.contains(&"class:MemberMap".to_owned()),
            "{}",
            graph.report()
        );
        // A property, a constructor, or a type the project only has a member
        // named after.
        assert!(
            !targets
                .iter()
                .any(|target| target.starts_with("property:") || target.starts_with("method:")),
            "{targets:?}\n{}",
            graph.report()
        );
    }

    /// Upstream #2216 (`a8b578f7`): a bare C# type name means a type its file
    /// can see: one in its own namespace or an enclosing one, one a `using`
    /// imports, or the one an alias names. Newtonsoft's `async Task` tests
    /// (`using System.Threading.Tasks;`) bound `Task` to a test class of that
    /// name. Upstream's fixture.
    #[test]
    fn pr2216_a_bare_csharp_type_name_is_one_its_namespaces_see() {
        let project = Project::new()
            .file(
                "src/App.Tests/Schema/Generator.cs",
                "namespace App.Tests.Schema\n{\n    public class Task\n    {\n    }\n}\n",
            )
            .file(
                "src/App.Tests/Reader/ReadTests.cs",
                "using System.Threading.Tasks;\n\nnamespace App.Tests.Reader\n{\n    public class ReadTests\n    {\n\
                 \x20       public async Task Read()\n        {\n\
                 \x20           await System.Threading.Tasks.Task.Yield();\n        }\n    }\n}\n",
            )
            .file(
                "src/App.Tests/Schema/Planner.cs",
                "namespace App.Tests.Schema\n{\n    public class Planner\n    {\n\
                 \x20       public Task Next() { return new Task(); }\n    }\n}\n",
            )
            .file(
                "src/App.Tests/Other/Aliased.cs",
                "using Task = App.Tests.Schema.Task;\n\nnamespace App.Tests.Other\n{\n    public class Aliased\n    {\n\
                 \x20       public Task Make() { return new Task(); }\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let reaches_task = |file: &str| {
            graph.edges.iter().any(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == file)
                    && graph
                        .node(&edge.target)
                        .is_some_and(|node| node.qualified_name == "App.Tests.Schema::Task")
            })
        };

        assert!(
            !reaches_task("src/App.Tests/Reader/ReadTests.cs"),
            "{}",
            graph.report()
        );
        assert!(
            reaches_task("src/App.Tests/Schema/Planner.cs"),
            "{}",
            graph.report()
        );
        assert!(
            reaches_task("src/App.Tests/Other/Aliased.cs"),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2220 (`ff75a6ca`): C# names follow the language's scopes. A
    /// block `namespace X { … }` holds only its body, and a file's second
    /// namespace, or one nested in another (`Beta.Inner`), is its own; `using
    /// X;` names the project's namespace X; a `global using` is its own
    /// project's (serilog's two test projects each `global using` their own
    /// `Support`, and both define `Some`); a nested type is reached by bare
    /// name only from inside its owner or a type deriving from it, through any
    /// partial part. Upstream's fixture.
    #[test]
    fn pr2220_csharp_namespaces_usings_and_nested_types_follow_their_scopes() {
        let project = Project::new()
            .file("src/Lib/Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n")
            .file(
                "src/Lib/Guard.cs",
                "using JetBrains.Annotations;\n\nnamespace JetBrains.Annotations\n{\n\
                 \x20   sealed class NoEnumerationAttribute : System.Attribute { }\n}\n\n\
                 static class Guard\n{\n\
                 \x20   public static T AgainstNull<T>(T value) where T : class => value;\n}\n",
            )
            .file(
                "src/Lib/Multi.cs",
                "namespace Alpha\n{\n    class X { }\n}\n\nnamespace Beta\n{\n    class Y { }\n\n\
                 \x20   namespace Inner\n    {\n        class Z { }\n    }\n}\n",
            )
            .file(
                "src/Lib/Uses.cs",
                "using Beta;\n\nnamespace Gamma\n{\n    class Consumer { }\n}\n",
            )
            .file(
                "src/Lib/Reader.cs",
                "namespace Lib\n{\n    public abstract class Reader\n    {\n\
                 \x20       internal enum State { Start, Done }\n    }\n\n\
                 \x20   public partial class TextReader : Reader\n    {\n    }\n}\n",
            )
            .file(
                "src/Lib/TextReader.Async.cs",
                "namespace Lib\n{\n    public partial class TextReader\n    {\n\
                 \x20       object Peek() => State.Start;\n    }\n}\n",
            )
            .file(
                "src/Lib/Writer.cs",
                "namespace Lib\n{\n    public abstract class Writer\n    {\n\
                 \x20       internal enum State { Start, Done }\n\n\
                 \x20       object Current() => State.Done;\n    }\n}\n",
            )
            .file("test/Unit/Unit.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n")
            .file("test/Unit/GlobalUsings.cs", "global using Unit.Support;\n")
            .file(
                "test/Unit/Support/Some.cs",
                "namespace Unit.Support;\n\nstatic class Some\n{\n\
                 \x20   public static int InformationEvent() => 1;\n}\n",
            )
            .file(
                "test/Unit/LogTests.cs",
                "namespace Unit.Context;\n\nclass LogTests\n{\n    int Run() => Some.InformationEvent();\n}\n",
            )
            .file(
                "test/Unit/MappingTests.cs",
                "namespace Unit.Mapping;\n\nclass FirstCase\n{\n    class Source { }\n\
                 \x20   object Make() => new Source();\n}\n\nclass SecondCase\n{\n    class Source { }\n\
                 \x20   object Make() => new Source();\n}\n",
            )
            .file("test/Perf/Perf.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n")
            .file("test/Perf/GlobalUsings.cs", "global using Perf.Support;\n")
            .file(
                "test/Perf/Support/Some.cs",
                "namespace Perf.Support;\n\nstatic class Some\n{\n\
                 \x20   public static int InformationEvent() => 2;\n}\n",
            )
            .index();
        let graph = project.graph();

        // A block namespace qualifies only its own body.
        let guard = qualified_names(graph, "src/Lib/Guard.cs");
        for expected in [
            "class JetBrains.Annotations::NoEnumerationAttribute",
            "class Guard",
            "method Guard::AgainstNull",
        ] {
            assert!(
                guard.iter().any(|name| name == expected),
                "{expected}: {guard:?}"
            );
        }
        let multi = qualified_names(graph, "src/Lib/Multi.cs");
        for expected in ["class Alpha::X", "class Beta::Y", "class Beta.Inner::Z"] {
            assert!(
                multi.iter().any(|name| name == expected),
                "{expected}: {multi:?}"
            );
        }

        // A using names the project's namespace.
        assert_eq!(
            targets_at(graph, "src/Lib/Uses.cs", 1, Some("imports")),
            ["namespace src/Lib/Multi.cs:Beta"],
            "{}",
            graph.report()
        );

        // A global using is its own project's.
        assert_eq!(
            targets_at(graph, "test/Unit/LogTests.cs", 5, Some("calls")),
            ["method test/Unit/Support/Some.cs:Unit.Support::Some::InformationEvent"],
            "{}",
            graph.report()
        );

        // A nested type is its owner's, or a derived type's through any
        // partial part.
        assert_eq!(
            targets_at(graph, "test/Unit/MappingTests.cs", 12, Some("instantiates")),
            ["class test/Unit/MappingTests.cs:Unit.Mapping::SecondCase::Source"],
            "{}",
            graph.report()
        );
        let peek = targets_at(graph, "src/Lib/TextReader.Async.cs", 5, None);
        assert!(
            peek.iter()
                .any(|target| target == "enum src/Lib/Reader.cs:Lib::Reader::State"),
            "{peek:?}\n{}",
            graph.report()
        );
        let current = targets_at(graph, "src/Lib/Writer.cs", 7, None);
        assert!(
            current
                .iter()
                .any(|target| target == "enum src/Lib/Writer.cs:Lib::Writer::State"),
            "{current:?}\n{}",
            graph.report()
        );
    }
}
