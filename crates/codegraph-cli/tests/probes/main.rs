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

    /// The qualified name of what every `calls` edge out of `file` reaches,
    /// sorted (upstream's `callsFrom`).
    fn call_targets(graph: &Graph, file: &str) -> Vec<String> {
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
    /// Upstream #2171 (`f9530d7d`), its Java half: a receiver's declared
    /// type is read through generics and for-each loops (`final Pair<L, R>
    /// pair`, `for (Element el : this)`), from a field of the class (`private
    /// DateFormat dateTimeParser;`, outside the project, so no edge), or as
    /// the bound of a type parameter (`T extends ExceptionContext`); a
    /// constant's receiver is its type (`Phase.CONTROLLERS.getPhase()`).
    /// Upstream's fixture.
    #[test]
    fn pr2171_java_calls_through_declared_receivers_use_their_type() {
        let project = Project::new()
            .file(
                "src/main/java/app/Pair.java",
                "package app;\n\npublic abstract class Pair<L, R> {\n    public abstract L getKey();\n}\n",
            )
            .file(
                "src/main/java/app/MutablePair.java",
                "package app;\n\npublic class MutablePair<L, R> extends Pair<L, R> {\n    private L left;\n\n    public L getKey() { return left; }\n\n    /**\n     * Copies the key of each element into this pair.\n     */\n    public void setValue(final Pair<L, R> pair) {\n        left = pair.getKey();\n    }\n}\n",
            )
            .file(
                "src/main/java/app/DateParser.java",
                "package app;\n\npublic interface DateParser {\n    Object parse(String source);\n}\n",
            )
            .file(
                "src/main/java/app/Element.java",
                "package app;\n\npublic class Element {\n    public void val(String value) { }\n}\n",
            )
            .file(
                "src/main/java/app/Elements.java",
                "package app;\n\nimport java.util.ArrayList;\n\npublic class Elements extends ArrayList<Element> {\n    public Elements val(String value) {\n        for (Element element : this)\n            element.val(value);\n        return this;\n    }\n}\n",
            )
            .file(
                "src/main/java/app/Phase.java",
                "package app;\n\npublic enum Phase {\n    CONTROLLERS;\n\n    public int getPhase() { return 0; }\n}\n",
            )
            .file(
                "src/main/java/app/ExceptionContext.java",
                "package app;\n\npublic interface ExceptionContext {\n    int getContextEntries();\n}\n",
            )
            .file(
                "src/test/java/app/DateUtilsTest.java",
                "package app;\n\nimport java.text.DateFormat;\n\npublic abstract class DateUtilsTest<T extends ExceptionContext> {\n    private DateFormat dateTimeParser;\n    protected T exceptionContext;\n\n    void rounds() throws Exception {\n        dateTimeParser.parse(\"January 1, 2008\");\n        exceptionContext.getContextEntries();\n        Phase.CONTROLLERS.getPhase();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            call_targets(graph, "src/main/java/app/MutablePair.java"),
            ["app::Pair::getKey"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets(graph, "src/main/java/app/Elements.java"),
            ["app::Element::val"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets(graph, "src/test/java/app/DateUtilsTest.java"),
            [
                "app::ExceptionContext::getContextEntries",
                "app::Phase::getPhase"
            ],
            "{}",
            graph.report()
        );
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
    /// Upstream's fixture, verbatim: `id.eq` reaches `Column::eq` through the
    /// type the class declares `id` with (`class Ops(private val id:
    /// Column)`, upstream #2171, W4-CS-03).
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
            calls.contains(&"app::Column::eq".to_string()),
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
    use super::support::{Graph, Project, assert_edge, assert_no_edge};

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

    const PR2151_FILES: [(&str, &str); 5] = [
        (
            "tests/OrderProcessorTests.cs",
            "namespace App.Tests;\n\npublic class OrderProcessorTests\n{\n    public TestContext TestContext { get; set; } = null!;\n}\n",
        ),
        (
            "src/Animations/AnimationBase.cs",
            "namespace App.Animations;\n\npublic abstract class AnimationBase\n{\n    public EasingType Easing { get; set; }\n}\n",
        ),
        (
            "src/Execution/ExpressionBuilder.cs",
            "namespace App.Execution;\n\npublic static class ExpressionBuilder\n{\n    public static object Call(object target) => target;\n}\n",
        ),
        (
            "src/Specs/SpecBase.cs",
            "namespace App.Specs;\n\npublic abstract class SpecBase\n{\n    protected void AssertValid() { }\n}\n",
        ),
        (
            "src/Specs/CatalogSpec.cs",
            "namespace App.Specs;\n\npublic class CatalogSpec : SpecBase\n{\n    public void Runs()\n    {\n        var token = TestContext.Current.CancellationToken;\n        var easing = Easing.Linear;\n        AssertValid();\n        Call(token);\n    }\n}\n",
        ),
    ];

    /// What the `calls` and `references` edges out of `CatalogSpec.cs` reach.
    fn catalog_targets(graph: &Graph) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls" || edge.kind == "references")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "src/Specs/CatalogSpec.cs")
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2151 (`6eb08d68`): a bare C# name (`TestContext`, `Easing`,
    /// `Helper()`) is a member of the types around it, of their bases, or of
    /// a type a static using brings in, never another class's property:
    /// eShop's `TestContext.Current` reached another test class's
    /// `TestContext` property. Upstream's fixture, where a project file's
    /// `<Using … Static="true"/>` brings `ExpressionBuilder` in; then the
    /// same tree without it (`Call` is out of reach), with a `global using
    /// static` in another file, and with the file's own `using static`.
    #[test]
    fn pr2151_a_bare_csharp_name_reaches_only_members_in_scope() {
        let with = |extra: &[(&str, &str)], catalog: Option<&str>| {
            let mut project = Project::new();
            for (path, contents) in PR2151_FILES {
                let contents = match (path, catalog) {
                    ("src/Specs/CatalogSpec.cs", Some(catalog)) => catalog,
                    _ => contents,
                };
                project = project.file(path, contents);
            }
            for (path, contents) in extra {
                project = project.file(path, contents);
            }
            project.index()
        };
        let csproj = (
            "src/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Using Include=\"App.Execution.ExpressionBuilder\" Static=\"true\"/>\n  </ItemGroup>\n</Project>\n",
        );
        let upstream = with(&[csproj], None);
        let targets = catalog_targets(upstream.graph());
        assert!(
            !targets
                .iter()
                .any(|t| t == "App.Tests::OrderProcessorTests::TestContext"),
            "{targets:?}"
        );
        assert!(
            !targets
                .iter()
                .any(|t| t == "App.Animations::AnimationBase::Easing"),
            "{targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|t| t == "App.Specs::SpecBase::AssertValid"),
            "{targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|t| t == "App.Execution::ExpressionBuilder::Call"),
            "{targets:?}"
        );

        let call = "App.Execution::ExpressionBuilder::Call";
        let bare = with(&[], None);
        assert!(
            !catalog_targets(bare.graph()).iter().any(|t| t == call),
            "{}",
            bare.graph().report()
        );
        let global = with(
            &[(
                "src/GlobalUsings.cs",
                "global using static App.Execution.ExpressionBuilder;\n",
            )],
            None,
        );
        assert!(
            catalog_targets(global.graph()).iter().any(|t| t == call),
            "{}",
            global.graph().report()
        );
        let own = with(
            &[],
            Some(
                "using static App.Execution.ExpressionBuilder;\nnamespace App.Specs;\n\npublic class CatalogSpec : SpecBase\n{\n    public void Runs()\n    {\n        var token = TestContext.Current.CancellationToken;\n        var easing = Easing.Linear;\n        AssertValid();\n        Call(token);\n    }\n}\n",
            ),
        );
        assert!(
            catalog_targets(own.graph()).iter().any(|t| t == call),
            "{}",
            own.graph().report()
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

    /// The qualified name of what every `calls` edge out of `file` reaches,
    /// sorted (upstream's `callsFrom`).
    fn call_targets(graph: &Graph, file: &str) -> Vec<String> {
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

    /// Upstream #2171 (`f9530d7d`): a C# call through a field, property or
    /// parameter is a call on the type it is declared with, read from the
    /// class's member declarations, those it inherits (with the type
    /// arguments the subclass gives), or a using alias, never a guess at a
    /// same-named method; a C# interface's members are public and a type in a
    /// namespace is internal; a standard .NET name on an untyped receiver
    /// (`table.Columns.Add(…)`) is no project method unless the receiver
    /// names its owner. Upstream's fixture.
    #[test]
    fn pr2171_csharp_calls_through_declared_members_use_the_member_type() {
        let project = Project::new()
            .file(
                "src/Json/JsonWriter.cs",
                "namespace App.Json;\n\npublic abstract class JsonWriter\n{\n    public virtual void WriteValue(string value) { }\n}\n",
            )
            .file(
                "src/Json/TraceJsonWriter.cs",
                "using System.IO;\n\nnamespace App.Json;\n\ninternal class TraceJsonWriter : JsonWriter\n{\n    private readonly JsonWriter _innerWriter;\n    /* a block comment naming the _textWriter field */\n    private readonly TextWriter _textWriter;\n\n    public override void WriteValue(string value)\n    {\n        _innerWriter.WriteValue(value);\n        _textWriter.Write(value);\n    }\n}\n",
            )
            .file(
                "tests/ThrowingWriter.cs",
                "using System.IO;\n\nnamespace App.Tests;\n\npublic class ThrowingWriter : TextWriter\n{\n    public override void Write(string value) { }\n}\n",
            )
            .file(
                "tests/XUnitAssert.cs",
                "namespace App.Tests;\n\npublic static class XUnitAssert\n{\n    public static void AreEqual(object a, object b) { }\n}\n",
            )
            .file(
                "src/DefaultJsonNameTable.cs",
                "namespace App;\n\npublic class DefaultJsonNameTable\n{\n    public void Add(string key) { }\n}\n",
            )
            .file(
                "src/Services/IFixUriService.cs",
                "namespace App.Services;\n\npublic interface IFixUriService\n{\n    void FixBasketItemPictureUri(string uri);\n}\n",
            )
            .file(
                "src/Services/FixUriService.cs",
                "namespace App.Services;\n\npublic class FixUriService : IFixUriService\n{\n    public void FixBasketItemPictureUri(string uri) { }\n}\n",
            )
            .file(
                "src/Services/BasketService.cs",
                "namespace App.Services;\n\npublic class BasketService\n{\n    private readonly IFixUriService _fixUriService;\n\n    public void Load(string uri)\n    {\n        _fixUriService.FixBasketItemPictureUri(uri);\n    }\n}\n",
            )
            .file(
                "tests/Integration/IntegrationTest.cs",
                "namespace App.Tests.Integration;\n\npublic abstract class IntegrationTest<TFixture> where TFixture : new()\n{\n    protected TFixture Fixture { get; private set; }\n}\n\npublic class DropCreateDatabaseAlways\n{\n    public object CreateContext() => null;\n}\n",
            )
            .file(
                "tests/Integration/QueryTests.cs",
                "using Assert = App.Tests.XUnitAssert;\n\nnamespace App.Tests.Integration;\n\npublic class QueryTests : IntegrationTest<QueryTests.DatabaseInitializer>\n{\n    public class DatabaseInitializer : DropCreateDatabaseAlways { }\n\n    public void Runs()\n    {\n        var context = Fixture.CreateContext();\n        Assert.AreEqual(1, 1);\n        var table = MakeTable();\n        table.Columns.Add(\"price\");\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            call_targets(graph, "src/Json/TraceJsonWriter.cs"),
            ["App.Json::JsonWriter::WriteValue"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets(graph, "src/Services/BasketService.cs"),
            ["App.Services::IFixUriService::FixBasketItemPictureUri"],
            "{}",
            graph.report()
        );
        assert_eq!(
            call_targets(graph, "tests/Integration/QueryTests.cs"),
            [
                "App.Tests.Integration::DropCreateDatabaseAlways::CreateContext",
                "App.Tests::XUnitAssert::AreEqual"
            ],
            "{}",
            graph.report()
        );
        let visibility = |file: &str, kind: &str| {
            graph
                .nodes
                .iter()
                .find(|node| node.file_path == file && node.kind == kind)
                .and_then(|node| node.visibility.clone())
        };
        assert_eq!(
            visibility("src/Services/IFixUriService.cs", "method").as_deref(),
            Some("public")
        );
        assert_eq!(
            visibility("src/Json/TraceJsonWriter.cs", "class").as_deref(),
            Some("internal")
        );
    }

    /// KEEP-RUST (W3-05c, adapted to upstream #2171): a C# receiver declared
    /// with a keyword type (`string message`) reaches the project's extension
    /// method for it now that the receiver-word guess no longer reads the
    /// method's own name. When several overloads of one type fit, C#'s rules
    /// pick one, also across a `partial static class` split over files (an
    /// array creation at a `params` position takes the `params` overload); a
    /// standard .NET name (`message.StartsWith(".")`) stays the keyword
    /// type's own method.
    #[test]
    fn keyword_typed_receivers_reach_extension_overloads_across_partial_parts() {
        let project = Project::new()
            .file(
                "src/Utilities/StringUtils.cs",
                "namespace App.Utilities\n{\n    internal static partial class StringUtils\n    {\n        public static string FormatWith(this string format, IFormatProvider provider, object? arg0)\n        {\n            return format;\n        }\n    }\n}\n",
            )
            .file(
                "src/Utilities/StringUtils.Params.cs",
                "namespace App.Utilities\n{\n    internal static partial class StringUtils\n    {\n        public static string FormatWith(this string format, IFormatProvider provider, params object?[] args)\n        {\n            return format;\n        }\n\n        public static bool StartsWith(this string source, char value)\n        {\n            return false;\n        }\n    }\n}\n",
            )
            .file(
                "src/App/Report.cs",
                "using App.Utilities;\n\nnamespace App\n{\n    public class Report\n    {\n        public string Render(string message, object? a, object? b)\n        {\n            if (message.StartsWith(\".\")) { }\n            return message.FormatWith(null, new object?[] { a, b });\n        }\n    }\n}\n",
            )
            .index();
        assert_edge!(
            project,
            "method App::Report::Render" => calls => "method App.Utilities::StringUtils::FormatWith @src/Utilities/StringUtils.Params.cs:5"
        );
        assert_no_edge!(
            project,
            "method App::Report::Render" => calls => "method App.Utilities::StringUtils::FormatWith @src/Utilities/StringUtils.cs:5"
        );
        assert_no_edge!(
            project,
            "method App::Report::Render" => calls => "method App.Utilities::StringUtils::StartsWith"
        );
    }

    /// The qualified name of what every `kind` edge out of the node of `file`
    /// named `qualified_name` reaches, sorted (upstream's
    /// `targets(member(…), kind)`).
    fn member_targets(graph: &Graph, file: &str, qualified_name: &str, kind: &str) -> Vec<String> {
        let member = graph
            .nodes
            .iter()
            .find(|node| node.file_path == file && node.qualified_name == qualified_name)
            .unwrap_or_else(|| panic!("{qualified_name} is indexed\n{}", graph.report()));
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == member.id && edge.kind == kind)
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| node.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2345 (`489b4749`), its C# half: a property's accessor bodies
    /// and expression body are walked as the property, so the calls,
    /// instantiations and static reads written there belong to it, and a
    /// method passed as a value there is the property's reference, captured
    /// once. Upstream's fixture.
    #[test]
    fn pr2345_csharp_accessor_bodies_are_walked_as_the_property() {
        let project = Project::new()
            .file(
                "Helper.cs",
                "public static class Helper {\n\
                \x20   public static int Compute(int n) => n;\n\
                \x20   public static void Store(int n) {}\n\
                \x20   public static int Arrow() => 1;\n\
                \x20   public static int GetA() => 1;\n\
                \x20   public static void SetA(int v) {}\n\
                }\n\
                public class Widget {}\n\
                public static class Defaults { public const string Name = \"x\"; }\n",
            )
            .file(
                "Box.cs",
                "public class Box {\n\
                \x20   private int _x;\n\
                \x20   public int Value {\n\
                \x20       get { return Helper.Compute(_x); }\n\
                \x20       set { Helper.Store(value); _x = value; }\n\
                \x20   }\n\
                \x20   public int Arrow => Helper.Arrow();\n\
                \x20   public int Both { get => Helper.GetA(); set => Helper.SetA(value); }\n\
                \x20   public Widget Made { get { return new Widget(); } }\n\
                \x20   public string Label { get { return Defaults.Name; } }\n\
                \x20   private void Handle(int v) { }\n\
                \x20   public System.Action<int> Handler { get { return Pick(Handle); } }\n\
                }\n",
            )
            .index();
        let graph = project.graph();
        let targets = |name: &str, kind: &str| member_targets(graph, "Box.cs", name, kind);

        assert_eq!(
            targets("Box::Value", "calls"),
            ["Helper::Compute", "Helper::Store"]
        );
        assert_eq!(targets("Box::Arrow", "calls"), ["Helper::Arrow"]);
        assert_eq!(
            targets("Box::Both", "calls"),
            ["Helper::GetA", "Helper::SetA"]
        );
        assert_eq!(targets("Box::Made", "instantiates"), ["Widget"]);
        assert!(targets("Box::Label", "references").contains(&"Defaults".to_owned()));
        // A method passed as a value inside an accessor is the property's
        // reference, captured once, not the class's as well.
        assert!(targets("Box::Handler", "references").contains(&"Box::Handle".to_owned()));
        assert!(!targets("Box", "references").contains(&"Box::Handle".to_owned()));
        assert!(targets("Box", "calls").is_empty(), "{}", graph.report());
    }

    /// Upstream #2348 (`355afc70`): a C# field declarator and a property's
    /// `= initializer` are walked as the member they initialize, so `private
    /// readonly ILogger _log = LogManager.GetLogger(typeof(Box));` calls
    /// `GetLogger` from `_log`; a target-typed `new()` there instantiates the
    /// declared type; lambdas and function values belong to the member;
    /// attributes are not walked. Upstream's fixture.
    #[test]
    fn pr2348_csharp_initializers_are_walked_as_their_member() {
        let project = Project::new()
            .file(
                "Lib.cs",
                "namespace Lib\n\
                {\n\
                \x20   public class Crate { }\n\
                }\n\
                public interface ILogger { }\n\
                public static class LogManager\n\
                {\n\
                \x20   public static ILogger GetLogger(System.Type t) => null;\n\
                }\n\
                public static class Helper\n\
                {\n\
                \x20   public static int Compute(int n) => n;\n\
                \x20   public static int Seed() => 1;\n\
                \x20   public static void Register(System.Action a) { }\n\
                \x20   public static System.Action Wrap(System.Action a) => a;\n\
                }\n\
                public class Widget\n\
                {\n\
                \x20   public Widget() { }\n\
                \x20   public Widget(int n) { }\n\
                \x20   public int Size { get; set; }\n\
                }\n\
                public class Bag<T> { }\n\
                public static class Defaults\n\
                {\n\
                \x20   public const string Name = \"x\";\n\
                \x20   public static readonly Widget Empty = new Widget();\n\
                }\n",
            )
            .file(
                "Box.cs",
                "using Lib;\n\
                \n\
                public class Box\n\
                {\n\
                \x20   private readonly ILogger _log = LogManager.GetLogger(typeof(Box));\n\
                \x20   private int _a = 1, _b = Helper.Compute(2);\n\
                \x20   private static readonly int Max = Helper.Seed();\n\
                \x20   private readonly Widget _made = new Widget(3);\n\
                \x20   private readonly Widget _typed = new() { Size = Helper.Seed() };\n\
                \x20   private Widget? _maybe = new();\n\
                \x20   private readonly Bag<Widget> _bag = new();\n\
                \x20   private readonly Lib.Crate _crate = new();\n\
                \x20   private readonly string _label = Defaults.Name;\n\
                \x20   private readonly System.Func<int, int> _square = x => Helper.Compute(x * x);\n\
                \x20   private readonly System.Action _later = () => Helper.Register(Handle);\n\
                \x20   private readonly System.Action _direct = Handle;\n\
                \x20   [System.Obsolete(\"use Helper.Seed()\")] private int _flagged;\n\
                \x20   public Bag<Widget> Items { get; } = new Bag<Widget>();\n\
                \x20   public Widget Made { get; set; } = new();\n\
                \x20   public Widget Copy { get; } = Defaults.Empty;\n\
                \x20   public System.Action Wrapped { get; } = Helper.Wrap(Handle);\n\
                \x20   public int Seeded { get; } = Helper.Seed();\n\
                \x20   private static void Handle() { }\n\
                }\n",
            )
            .index();
        let graph = project.graph();
        let targets = |name: &str, kind: &str| member_targets(graph, "Box.cs", name, kind);
        let kind_of = |name: &str| {
            graph
                .nodes
                .iter()
                .find(|node| node.file_path == "Box.cs" && node.qualified_name == name)
                .map(|node| node.kind.clone())
        };

        // Calls, per declarator.
        assert_eq!(targets("Box::_log", "calls"), ["LogManager::GetLogger"]);
        assert!(targets("Box::_a", "calls").is_empty());
        assert_eq!(targets("Box::_b", "calls"), ["Helper::Compute"]);
        assert_eq!(kind_of("Box::Max").as_deref(), Some("constant"));
        assert_eq!(targets("Box::Max", "calls"), ["Helper::Seed"]);
        assert_eq!(targets("Box::Seeded", "calls"), ["Helper::Seed"]);

        // Instantiations, including a target-typed `new()` of the declared
        // type.
        assert_eq!(targets("Box::_made", "instantiates"), ["Widget"]);
        assert_eq!(targets("Box::_typed", "instantiates"), ["Widget"]);
        assert_eq!(targets("Box::_typed", "calls"), ["Helper::Seed"]);
        assert_eq!(targets("Box::_maybe", "instantiates"), ["Widget"]);
        assert_eq!(targets("Box::_bag", "instantiates"), ["Bag"]);
        assert_eq!(targets("Box::_crate", "instantiates"), ["Lib::Crate"]);
        assert_eq!(targets("Box::Items", "instantiates"), ["Bag"]);
        assert_eq!(targets("Box::Made", "instantiates"), ["Widget"]);

        // Static reads.
        assert!(targets("Box::_label", "references").contains(&"Defaults".to_owned()));
        assert!(targets("Box::Copy", "references").contains(&"Defaults".to_owned()));

        // Lambda bodies belong to the member the lambda initializes.
        assert_eq!(targets("Box::_square", "calls"), ["Helper::Compute"]);
        assert_eq!(targets("Box::_later", "calls"), ["Helper::Register"]);

        // A method passed as a value is the member's reference, captured
        // once, not the class's as well.
        assert!(targets("Box::_later", "references").contains(&"Box::Handle".to_owned()));
        assert!(targets("Box::_direct", "references").contains(&"Box::Handle".to_owned()));
        assert_eq!(targets("Box::Wrapped", "calls"), ["Helper::Wrap"]);
        assert!(targets("Box::Wrapped", "references").contains(&"Box::Handle".to_owned()));
        assert!(!targets("Box", "references").contains(&"Box::Handle".to_owned()));

        // Attribute arguments are not an initializer, and the class itself
        // calls and creates nothing.
        assert!(targets("Box::_flagged", "calls").is_empty());
        assert!(targets("Box", "calls").is_empty(), "{}", graph.report());
        assert!(
            targets("Box", "instantiates").is_empty(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2348 (`355afc70`, issue #2337): a C# type position never
    /// names a constant, the kind `const` and `static readonly` fields get:
    /// `new Version(5, 18)` bound to a claim-name `const string Version`, and
    /// `static readonly Meter Meter = new(…)` instantiated itself. Upstream's
    /// fixture.
    #[test]
    fn pr2348_a_csharp_type_position_never_names_a_constant() {
        let project = Project::new()
            .file(
                "src/Claims.cs",
                "namespace App;\n\
                public static class ClaimTypes {\n\
                \x20   public const string Version = \"v\";\n\
                }\n",
            )
            .file(
                "src/Encoder.cs",
                "namespace App;\n\
                using System;\n\
                public class ValueFormatter { }\n\
                public class Encoder {\n\
                \x20   private static readonly Version MinVersion = new Version(5, 18);\n\
                \x20   private static readonly Meter Meter = new(\"app\");\n\
                \x20   private static readonly ValueFormatter ValueFormatter = new();\n\
                \x20   public void Check() { var v = new Version(1, 0); }\n\
                }\n",
            )
            .index();
        let graph = project.graph();
        let mut targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| ["references", "instantiates", "type_of"].contains(&edge.kind.as_str()))
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                (source.file_path == "src/Encoder.cs" && source.kind != "file").then(|| {
                    format!(
                        "{} {} {}:{}",
                        source.name, edge.kind, target.kind, target.qualified_name
                    )
                })
            })
            .collect();
        targets.sort();
        assert!(
            !targets.iter().any(|target| target.contains(" constant:")),
            "{targets:?}"
        );
        assert!(
            targets.contains(&"ValueFormatter instantiates class:App::ValueFormatter".to_owned()),
            "{targets:?}\n{}",
            graph.report()
        );
    }
}

mod ruby {
    use super::support::{Graph, Indexed, Project};

    /// The qualified name of what every `calls` edge out of `file` reaches,
    /// sorted (upstream's `callsFrom`).
    fn call_targets(graph: &Graph, file: &str) -> Vec<String> {
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

    /// Upstream's #2147 fixture (`df761e45`).
    fn project() -> Indexed {
        Project::new()
            .file(
                "lib/app/ext/regexp.rb",
                "module App\n\
                \x20 module Ext\n\
                \x20   module Expression\n\
                \x20     def loc\n\
                \x20       @loc\n\
                \x20     end\n\
                \x20   end\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/lsp/runtime.rb",
                "module App\n\
                \x20 module LSP\n\
                \x20   class Runtime\n\
                \x20     def format(text)\n\
                \x20       text\n\
                \x20     end\n\
                \x20   end\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/web_push_request.rb",
                "class WebPushRequest\n\
                \x20 def legacy_encrypt(payload)\n\
                \x20   payload\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/cop/base.rb",
                "module App\n\
                \x20 module Cop\n\
                \x20   module RangeHelp\n\
                \x20     def range_between(a, b)\n\
                \x20       [a, b]\n\
                \x20     end\n\
                \x20   end\n\
                \n\
                \x20   class Base\n\
                \x20     def add_offense(node)\n\
                \x20       node\n\
                \x20     end\n\
                \x20   end\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/cop/style/foo.rb",
                "module App\n\
                \x20 module Cop\n\
                \x20   module Style\n\
                \x20     class Foo < Base\n\
                \x20       include RangeHelp\n\
                \n\
                \x20       MSG = 'Use %<x>s.'\n\
                \n\
                \x20       def on_send(node, web_push_request)\n\
                \x20         selector = node.loc\n\
                \x20         web_push_request.legacy_encrypt(selector)\n\
                \x20         add_offense(node)\n\
                \x20         range_between(1, 2)\n\
                \x20         format(MSG, x: 1)\n\
                \x20       end\n\
                \x20     end\n\
                \x20   end\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/greeting.rb",
                "module Greeting\n\
                \x20 def greet\n\
                \x20   \"Hello, \" + display_name()\n\
                \x20 end\n\
                end\n",
            )
            .file(
                "lib/app/person.rb",
                "class Person\n\
                \x20 include Greeting\n\
                \n\
                \x20 def display_name\n\
                \x20   'Ada'\n\
                \x20 end\n\
                end\n",
            )
            .index()
    }

    /// Upstream #2147: a bare Ruby call written inside a class is a call on
    /// `self`, so it reaches only the class's own methods, its superclasses'
    /// and the modules mixed into any of them (rubocop's `format(…)`,
    /// Kernel's, is no LSP runtime's `format`); a call through an untyped
    /// receiver takes the one method of its name only when the receiver is
    /// named after its owner (`web_push_request.legacy_encrypt`, but not
    /// `node.loc`). Upstream's fixture.
    #[test]
    fn pr2147_ruby_calls_by_name_alone_keep_the_ancestry_and_named_receivers() {
        let project = project();
        assert_eq!(
            call_targets(project.graph(), "lib/app/cop/style/foo.rb"),
            [
                "App::Cop::Base::add_offense",
                "App::Cop::RangeHelp::range_between",
                "WebPushRequest::legacy_encrypt",
            ],
            "{}",
            project.graph().report()
        );
    }

    /// Upstream #2147's receiver gate is about the method's owner, not the
    /// method: `user_name.name` shares `name` with `Person::name` itself
    /// (enough for the receiver-word rule every language has here, #226) but
    /// nothing with `Person`, so the one `name` is no guess; `person.name` is.
    #[test]
    fn pr2147_an_untyped_ruby_receiver_must_name_the_methods_owner() {
        let project = Project::new()
            .file(
                "lib/person.rb",
                "class Person\n  def name\n    'Ada'\n  end\nend\n",
            )
            .file(
                "lib/report.rb",
                "class Report\n  def run(user_name, person)\n    user_name.name\n    person.name\n  end\nend\n",
            )
            .index();
        let graph = project.graph();
        let calls: Vec<(i64, String)> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "lib/report.rb")
            })
            .filter_map(|edge| {
                graph
                    .node(&edge.target)
                    .map(|node| (edge.line.unwrap_or_default(), node.qualified_name.clone()))
            })
            .collect();
        assert_eq!(
            calls,
            [(4, "Person::name".to_string())],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2147: a call in a module body is not judged, as a module's
    /// methods run on whatever includes it. Upstream's fixture.
    #[test]
    fn pr2147_a_call_in_a_ruby_module_body_is_not_judged() {
        let project = project();
        assert_eq!(
            call_targets(project.graph(), "lib/app/greeting.rb"),
            ["Person::display_name"],
            "{}",
            project.graph().report()
        );
    }
}

mod cfml {
    use super::support::{Graph, Project};

    /// A case: its files, as (path, contents), and the edges it must hold.
    type Fixture<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str]);

    /// The qualified name of what every `calls` edge out of `file` reaches,
    /// sorted.
    fn call_targets(graph: &Graph, file: &str) -> Vec<String> {
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

    /// `source-name kind target-qualified-name @target-file` of every edge of
    /// `kinds`, sorted.
    fn edges_of(graph: &Graph, kinds: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| kinds.contains(&edge.kind.as_str()))
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                Some(format!(
                    "{} {} {} @{}",
                    source.name, edge.kind, target.qualified_name, target.file_path
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2296 (issue #2091): calls written in `<cfset>`, `<cfif>` and
    /// `<cfreturn>` reach the methods they call; a `<cffunction>` wrapped in
    /// `<cfprocessingdirective>` (the Application.cfc shape) is indexed with
    /// its calls; `<cfinterface>` is an interface `implements` resolves to.
    /// Upstream's fixture.
    #[test]
    fn pr2296_tag_based_cfml_calls_wrapped_functions_and_interfaces_are_indexed() {
        let project = Project::new()
            .file(
                "model/Svc.cfc",
                "<cfcomponent>\n\t<cffunction name=\"a\">\n\t\t<cfset x = b(1)>\n\t\t<cfif c(2)></cfif>\n\t\t<cfreturn d()>\n\t</cffunction>\n\t<cffunction name=\"b\"></cffunction>\n\t<cffunction name=\"c\"></cffunction>\n\t<cffunction name=\"d\"></cffunction>\n</cfcomponent>\n",
            )
            .file(
                "Application.cfc",
                "<cfcomponent>\n\t<cfprocessingdirective suppresswhitespace=\"true\">\n\t\t<cffunction name=\"onRequestStart\">\n\t\t\t<cfset loadConfig()>\n\t\t</cffunction>\n\t\t<cffunction name=\"loadConfig\"></cffunction>\n\t</cfprocessingdirective>\n</cfcomponent>\n",
            )
            .file(
                "model/ISearchable.cfc",
                "<cfinterface>\n\t<cffunction name=\"search\" access=\"public\"></cffunction>\n</cfinterface>\n",
            )
            .file(
                "model/Catalog.cfc",
                "<cfcomponent implements=\"ISearchable\">\n\t<cffunction name=\"search\" access=\"public\"></cffunction>\n</cfcomponent>\n",
            )
            .index();
        let graph = project.graph();
        let calls = edges_of(graph, &["calls"]);
        for expected in [
            "a calls Svc::b @model/Svc.cfc",
            "a calls Svc::c @model/Svc.cfc",
            "a calls Svc::d @model/Svc.cfc",
            "onRequestStart calls Application::loadConfig @Application.cfc",
        ] {
            assert!(
                calls.contains(&expected.to_string()),
                "{calls:#?}\n{}",
                graph.report()
            );
        }
        assert_eq!(calls.len(), 4, "{calls:#?}");
        let iface = graph
            .nodes
            .iter()
            .find(|node| node.file_path == "model/ISearchable.cfc" && node.kind == "interface")
            .expect("the interface is indexed");
        assert_eq!(iface.name, "ISearchable");
        assert_eq!(
            edges_of(graph, &["implements"]),
            ["Catalog implements model/ISearchable.cfc::ISearchable @model/ISearchable.cfc"]
        );
    }

    /// Upstream #1152 / #1154: a CFML supertype written as a dotted component
    /// path resolves to the class whose directories corroborate the path,
    /// right to left and in any case, even when the mapping root is not in
    /// the repository (the ColdBox shape); an uncorroborated namesake (an
    /// out-of-repository mxunit supertype) and a corroboration tie are no
    /// edge. Upstream's fixtures.
    #[test]
    fn pr1152_a_dotted_cfml_component_path_is_corroborated_by_its_directories() {
        let cases: [Fixture; 5] = [
            (
                &[
                    (
                        "system/web/Controller.cfc",
                        "component {\n  function handle() { return 1; }\n}\n",
                    ),
                    (
                        "handlers/Main.cfc",
                        "component extends=\"coldbox.system.web.Controller\" {\n  function index() { return 1; }\n}\n",
                    ),
                ],
                &["Main extends system/web/Controller.cfc::Controller @system/web/Controller.cfc"],
            ),
            (
                &[
                    ("system/web/Controller.cfc", "component {}\n"),
                    ("other/Controller.cfc", "component {}\n"),
                    (
                        "handlers/Main.cfc",
                        "component extends=\"coldbox.system.web.Controller\" {}\n",
                    ),
                ],
                &["Main extends system/web/Controller.cfc::Controller @system/web/Controller.cfc"],
            ),
            (
                &[
                    ("system/web/Controller.cfc", "component {}\n"),
                    (
                        "handlers/Main.cfc",
                        "component extends=\"COLDBOX.System.Web.Controller\" {}\n",
                    ),
                ],
                &["Main extends system/web/Controller.cfc::Controller @system/web/Controller.cfc"],
            ),
            (
                &[
                    ("lib/TestCase.cfc", "component {}\n"),
                    (
                        "tests/MyTest.cfc",
                        "component extends=\"mxunit.framework.TestCase\" {}\n",
                    ),
                ],
                &[],
            ),
            (
                &[
                    ("a/models/User.cfc", "component {}\n"),
                    ("b/models/User.cfc", "component {}\n"),
                    (
                        "handlers/Main.cfc",
                        "component extends=\"models.User\" {}\n",
                    ),
                ],
                &[],
            ),
        ];
        for (files, expected) in cases {
            let mut project = Project::new();
            for (path, contents) in files {
                project = project.file(path, contents);
            }
            let project = project.index();
            assert_eq!(
                edges_of(project.graph(), &["extends", "implements"]),
                expected,
                "{}",
                project.graph().report()
            );
        }
    }

    /// Upstream #1152 / #1154: a relative component path (`../base`,
    /// `./app`, the FW/1 shape) resolves against the referencing file; a
    /// dotted `implements` reaches an interface; the tag-based `extends`
    /// attribute and a lowercase path (`framework.one`) resolve the same
    /// way; and a dotted `calls` reference (a member chain) is never taken
    /// for a component path. Upstream's fixtures.
    #[test]
    fn pr1152_relative_tag_based_and_implements_component_paths_resolve() {
        let cases: [Fixture; 5] = [
            (
                &[
                    (
                        "examples/base.cfc",
                        "component {\n  function shared() { return 1; }\n}\n",
                    ),
                    ("examples/sub/app.cfc", "component extends=\"../base\" {}\n"),
                    (
                        "examples/sub/sibling.cfc",
                        "component extends=\"./app\" {}\n",
                    ),
                ],
                &[
                    "app extends examples/base.cfc::base @examples/base.cfc",
                    "sibling extends examples/sub/app.cfc::app @examples/sub/app.cfc",
                ],
            ),
            (
                &[
                    (
                        "app/interfaces/IService.cfc",
                        "interface {\n  public string function getName();\n}\n",
                    ),
                    (
                        "app/services/Greeter.cfc",
                        "component implements=\"app.interfaces.IService\" {\n  public string function getName() { return \"hi\"; }\n}\n",
                    ),
                ],
                &[
                    "Greeter implements app/interfaces/IService.cfc::IService @app/interfaces/IService.cfc",
                ],
            ),
            (
                &[
                    ("system/Base.cfc", "component {}\n"),
                    (
                        "legacy/Old.cfc",
                        "<cfcomponent extends=\"app.system.Base\">\n<cffunction name=\"run\"><cfreturn 1></cffunction>\n</cfcomponent>\n",
                    ),
                ],
                &["Old extends system/Base.cfc::Base @system/Base.cfc"],
            ),
            (
                &[
                    (
                        "framework/one.cfc",
                        "component {\n  function onRequest() { return 1; }\n}\n",
                    ),
                    (
                        "Application.cfc",
                        "component extends=\"framework.one\" {}\n",
                    ),
                ],
                &["Application extends framework/one.cfc::one @framework/one.cfc"],
            ),
            (
                &[
                    ("util/getName.cfc", "component {}\n"),
                    (
                        "svc/Caller.cfc",
                        "component {\n  function go() { return variables.dsn.getName(); }\n}\n",
                    ),
                ],
                &[],
            ),
        ];
        for (files, expected) in cases {
            let mut project = Project::new();
            for (path, contents) in files {
                project = project.file(path, contents);
            }
            let project = project.index();
            let graph = project.graph();
            let mut edges = edges_of(graph, &["extends", "implements"]);
            edges.extend(
                edges_of(graph, &["calls"])
                    .into_iter()
                    .filter(|edge| edge.starts_with("go ") || edge.starts_with("Caller ")),
            );
            assert_eq!(edges, expected, "{}", graph.report());
        }
    }

    /// Upstream #1155: a CFML receiver is typed by its declaration (`new`,
    /// a dotted path included; `createObject` in both forms; a typed cfscript
    /// parameter; a `<cfargument>` read inside a `<cfscript>` body), and the
    /// call goes to that component's method, never the same-named one of
    /// another; a type that does not declare the method is no edge. The
    /// methods are scoped under their component in all three extraction
    /// paths. Upstream's fixtures.
    #[test]
    fn pr1155_cfml_receivers_are_typed_by_their_declarations() {
        let user = "component {\n  function save(any u) { return u; }\n}\n";
        let order = "component {\n  function save(any o) { return o; }\n}\n";
        let project = Project::new()
            .file("svc/UserService.cfc", user)
            .file("svc/OrderService.cfc", order)
            .file(
                "tag/TagService.cfc",
                "<cfcomponent>\n<cffunction name=\"save\"><cfreturn 1></cffunction>\n</cfcomponent>\n",
            )
            .file(
                "mod/ModuleConfig.cfc",
                "<cfcomponent>\n<cfscript>\nfunction configure() { return 1; }\n</cfscript>\n</cfcomponent>\n",
            )
            .file(
                "handlers/Main.cfc",
                "component {\n  function bare() {\n    var svc = new UserService();\n    return svc.save(1);\n  }\n  function dotted() {\n    var svc2 = new svc.UserService();\n    return svc2.save(2);\n  }\n}\n",
            )
            .file(
                "handlers/Legacy.cfc",
                "component {\n  function classic() {\n    var svc = createObject(\"component\", \"svc.UserService\");\n    return svc.save(1);\n  }\n  function modern() {\n    var svc2 = CreateObject(\"svc.OrderService\");\n    return svc2.save(2);\n  }\n}\n",
            )
            .file(
                "handlers/Typed.cfc",
                "component {\n  function process(required UserService svc) {\n    return svc.save(1);\n  }\n}\n",
            )
            .file(
                "handlers/TagTyped.cfc",
                "<cfcomponent>\n<cffunction name=\"tagged\">\n  <cfargument name=\"svc\" type=\"svc.UserService\">\n  <cfscript>\n    return svc.save(1);\n  </cfscript>\n</cffunction>\n</cfcomponent>\n",
            )
            .file(
                "handlers/Wrong.cfc",
                "component {\n  function go() {\n    var svc = new UserService();\n    return svc.destroyEverything();\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let methods: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == "method")
            .map(|node| node.qualified_name.clone())
            .collect();
        for scoped in [
            "UserService::save",
            "TagService::save",
            "ModuleConfig::configure",
        ] {
            assert!(methods.contains(&scoped.to_string()), "{methods:?}");
        }
        let calls = edges_of(graph, &["calls"]);
        assert_eq!(
            calls,
            [
                "bare calls UserService::save @svc/UserService.cfc",
                "classic calls UserService::save @svc/UserService.cfc",
                "dotted calls UserService::save @svc/UserService.cfc",
                "modern calls OrderService::save @svc/OrderService.cfc",
                "process calls UserService::save @svc/UserService.cfc",
                "tagged calls UserService::save @svc/UserService.cfc",
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #1155: a component field reached through `variables.` is
    /// typed by its pseudo-constructor assignment even below the call, or by
    /// a WireBox-injected `property` (the ColdBox DI shape). Upstream's
    /// fixtures.
    #[test]
    fn pr1155_cfml_component_fields_are_typed_from_anywhere_in_the_file() {
        let user = "component {\n  function save(any u) { return u; }\n}\n";
        let order = "component {\n  function save(any o) { return o; }\n}\n";
        let project = Project::new()
            .file("svc/UserService.cfc", user)
            .file("svc/OrderService.cfc", order)
            .file(
                "handlers/Fielded.cfc",
                "component {\n  function handle() {\n    return variables.svc.save(1);\n  }\n  function init() {\n    variables.svc = new UserService();\n    return this;\n  }\n}\n",
            )
            .file(
                "handlers/Injected.cfc",
                "component {\n  property name=\"svc\" inject=\"UserService\";\n\n  function handleInjected() {\n    return variables.svc.save(1);\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_of(graph, &["calls"])
                .into_iter()
                .filter(|edge| edge.contains("::save"))
                .collect::<Vec<_>>(),
            [
                "handle calls UserService::save @svc/UserService.cfc",
                "handleInjected calls UserService::save @svc/UserService.cfc",
            ],
            "{}",
            graph.report()
        );
    }

    /// W4-CF-02 (KEEP-RUST over upstream #1155, which takes the first): when
    /// same-named components in several files declare the method, a typed
    /// receiver's written path decides, by the directories its file
    /// corroborates, and a bare type name by the call's own directory; no
    /// corroboration, a tie, or namesakes of a supertype reached by name are
    /// no edge. A lone component of the name is the type's, as upstream has
    /// it.
    #[test]
    fn pr1155_same_named_cfml_components_are_told_apart_by_their_path() {
        let save = "component {\n  function save(any u) { return u; }\n}\n";
        let cases: [Fixture; 4] = [
            (
                &[
                    ("a/UserService.cfc", save),
                    ("b/UserService.cfc", save),
                    (
                        "handlers/Main.cfc",
                        "component {\n  function dottedA() {\n    var svc = new a.UserService();\n    return svc.save(1);\n  }\n  function dottedB() {\n    var svc = createObject(\"component\", \"b.UserService\");\n    return svc.save(2);\n  }\n  function bare() {\n    var svc = new UserService();\n    return svc.save(3);\n  }\n  function uncorroborated() {\n    var svc = new zz.UserService();\n    return svc.save(4);\n  }\n}\n",
                    ),
                    (
                        "a/Caller.cfc",
                        "component {\n  function beside() {\n    var svc = new UserService();\n    return svc.save(5);\n  }\n}\n",
                    ),
                ],
                &[
                    "beside calls UserService::save @a/UserService.cfc",
                    "dottedA calls UserService::save @a/UserService.cfc",
                    "dottedB calls UserService::save @b/UserService.cfc",
                ],
            ),
            (
                &[
                    ("p/a/UserService.cfc", save),
                    ("q/a/UserService.cfc", save),
                    (
                        "handlers/Main.cfc",
                        "component {\n  function tied() {\n    var svc = new a.UserService();\n    return svc.save(1);\n  }\n  function deeper() {\n    var svc = new p.a.UserService();\n    return svc.save(2);\n  }\n}\n",
                    ),
                ],
                &["deeper calls UserService::save @p/a/UserService.cfc"],
            ),
            (
                &[
                    ("a/Base.cfc", save),
                    ("b/Base.cfc", save),
                    ("b/Child.cfc", "component extends=\"b.Base\" {\n}\n"),
                    (
                        "handlers/Main.cfc",
                        "component {\n  function inherited() {\n    var base = new Child();\n    return base.save(1);\n  }\n}\n",
                    ),
                ],
                &[],
            ),
            (
                &[
                    ("svc/UserService.cfc", save),
                    (
                        "handlers/Main.cfc",
                        "component {\n  function lone() {\n    var svc = new zz.UserService();\n    return svc.save(1);\n  }\n}\n",
                    ),
                ],
                &["lone calls UserService::save @svc/UserService.cfc"],
            ),
        ];
        for (files, expected) in cases {
            let mut project = Project::new();
            for (path, contents) in files {
                project = project.file(path, contents);
            }
            let project = project.index();
            assert_eq!(
                edges_of(project.graph(), &["calls"]),
                expected,
                "{}",
                project.graph().report()
            );
        }
    }

    /// Upstream #2148: a bare call in a component reaches only its own
    /// methods and those of the components it `extends` (read from source,
    /// the path matched by its longest suffix), so coldbox's `now()`, the
    /// built-in, is no date helper's `now`; a call through an untyped
    /// receiver takes the one method of its name only when the receiver is
    /// named after its component (`server.keyExists` is the struct member
    /// function). Upstream's fixture.
    #[test]
    fn pr2148_cfml_calls_by_name_alone_keep_the_extends_chain_and_named_receivers() {
        let project = Project::new()
            .file(
                "system/core/DateTimeHelper.cfc",
                "component {\n\
                \x20 function now() {\n\
                \x20   return createObject( \"java\", \"java.time.LocalDateTime\" ).now();\n\
                \x20 }\n\
                }\n",
            )
            .file(
                "system/core/InterceptorBuffer.cfc",
                "component {\n\
                \x20 function keyExists( required key ) {\n\
                \x20   return false;\n\
                \x20 }\n\
                }\n",
            )
            .file(
                "system/Base.cfc",
                "component {\n\
                \x20 function getSetting( required name ) {\n\
                \x20   return \"\";\n\
                \x20 }\n\
                }\n",
            )
            .file(
                "system/models/UserService.cfc",
                "component {\n\
                \x20 function list() {\n\
                \x20   return [];\n\
                \x20 }\n\
                }\n",
            )
            .file(
                "system/Handler.cfc",
                "component extends=\"app.system.Base\" {\n\
                \x20 function index( event, rc, prc, userService ) {\n\
                \x20   var stamp = now();\n\
                \x20   var lucee = server.keyExists( \"lucee\" );\n\
                \x20   var name = getSetting( \"appName\" );\n\
                \x20   var users = userService.list();\n\
                \x20   return stamp;\n\
                \x20 }\n\
                }\n",
            )
            .index();
        assert_eq!(
            call_targets(project.graph(), "system/Handler.cfc"),
            ["Base::getSetting", "UserService::list"],
            "{}",
            project.graph().report()
        );
    }

    /// Upstream #2148's receiver relaxations: the receiver's whole name
    /// spelling its component (`cbsecurity` -> CBSecurity) or a word with a
    /// trailing number dropped (`executor1` -> Executor) names the owner, so
    /// the one method of that name is taken.
    #[test]
    fn pr2148_a_cfml_receiver_named_after_its_component_takes_its_method() {
        let project = Project::new()
            .file(
                "system/security/CBSecurity.cfc",
                "component {\n  function secure( event ) {\n    return true;\n  }\n}\n",
            )
            .file(
                "system/Executor.cfc",
                "component {\n  function run() {\n    return 1;\n  }\n}\n",
            )
            .file(
                "system/Guard.cfc",
                "component {\n  function guard( cbsecurity, executor1, scheduler ) {\n    cbsecurity.secure( \"x\" );\n    executor1.run();\n    scheduler.run();\n  }\n}\n",
            )
            .index();
        // `scheduler` names neither owner: the one `run` is no guess for it.
        assert_eq!(
            call_targets(project.graph(), "system/Guard.cfc"),
            ["CBSecurity::secure", "Executor::run"],
            "{}",
            project.graph().report()
        );
    }

    /// Upstream #2148: a dotted `extends` path picks, among the components of
    /// its last segment's name, the one ending in the longest suffix of the
    /// path, so `app.system.Base` is `system/Base.cfc`, not `other/Base.cfc`,
    /// and the bare call reaches only that base's method.
    #[test]
    fn pr2148_a_dotted_cfml_extends_takes_the_longest_matching_suffix() {
        let project = Project::new()
            .file(
                "system/Base.cfc",
                "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
            )
            .file(
                "other/Base.cfc",
                "component {\n  function getSetting( name ) {\n    return 2;\n  }\n}\n",
            )
            .file(
                "app/Handler.cfc",
                "component extends=\"app.system.Base\" {\n  function index() {\n    return getSetting( \"x\" );\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let calls: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .filter(|edge| {
                graph
                    .node(&edge.source)
                    .is_some_and(|node| node.file_path == "app/Handler.cfc")
            })
            .filter_map(|edge| graph.node(&edge.target))
            .map(|node| format!("{}:{}", node.file_path, node.qualified_name))
            .collect();
        assert_eq!(
            calls,
            ["system/Base.cfc:Base::getSetting"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2148: a bare call in a `.cfm` template is not judged (a
    /// ColdBox view runs inside the renderer's scope), so the one project
    /// function of its name is still taken.
    #[test]
    fn pr2148_a_bare_call_in_a_cfml_template_is_not_judged() {
        let project = Project::new()
            .file(
                "system/Helpers.cfc",
                "component {\n  function formatStamp( value ) {\n    return value;\n  }\n}\n",
            )
            .file(
                "views/main.cfm",
                "<cfoutput>#formatStamp( now() )#</cfoutput>\n<cfscript>\n  formatStamp( 1 );\n</cfscript>\n",
            )
            .index();
        let targets = call_targets(project.graph(), "views/main.cfm");
        assert!(
            targets.contains(&"Helpers::formatStamp".to_string()),
            "{targets:?}\n{}",
            project.graph().report()
        );
    }
}

mod swift {
    use super::support::{Graph, Project};

    /// Every non-`contains` edge as `kind source -> target (file:line)`,
    /// upstream's `edges()` shape, sorted.
    fn edges(graph: &Graph) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains")
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                let file = target
                    .file_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&target.file_path);
                Some(format!(
                    "{} {} -> {} ({file}:{})",
                    edge.kind, source.qualified_name, target.qualified_name, target.start_line
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2107: an extension is not the type it extends. A reference to
    /// an SDK type the project only extends is no edge; one that reached a
    /// type's extension moves to its declaration; a nested type is reached
    /// bare only from its parent's scope; and a conforming type still reaches
    /// the members the SDK type's extensions add. Upstream's fixture.
    #[test]
    fn pr2107_an_extension_is_not_the_type_it_extends() {
        let project = Project::new()
            .file(
                "App/DesignSystem/Card.swift",
                "import SwiftUI\nextension View {\n    func cardStyle() -> some View { padding() }\n}\n",
            )
            .file(
                "App/DesignSystem/Text+Style.swift",
                "import SwiftUI\nextension Text {\n    func headline() -> Text { bold() }\n}\n",
            )
            .file(
                "App/Models/Account.swift",
                "struct Account {\n    let id: String\n}\n",
            )
            .file(
                "App/Other/Account+Display.swift",
                "extension Account {\n    var display: String { id }\n}\nstruct AccountRow {\n    let account: Account\n}\n",
            )
            .file(
                "App/Home/HomeView.swift",
                "import SwiftUI\nstruct HomeView: View {\n    @State private var count = 0\n    var last: Result<Int, LoadError>?\n    var body: some View {\n        Text(\"Hello\").headline()\n    }\n}\nfunc makeHome() -> some View {\n    HomeView().cardStyle()\n}\nenum LoadError: Error {\n    case offline\n}\n",
            )
            .file(
                "App/Home/ListViewModel.swift",
                "public final class ListViewModel {\n    public enum State { case loading, loaded }\n    public struct Result { let ok: Bool }\n    var state: State = .loading\n    var latest: Result?\n}\n",
            )
            .index();
        let all = edges(project.graph());
        let report = project.graph().report();
        // `struct HomeView: View`, `-> some View`, `Text("Hello")`: SwiftUI's,
        // not the extension files'.
        assert!(
            !all.iter()
                .any(|e| e.contains(" -> View (") || e.contains(" -> Text (")),
            "{all:#?}\n{report}"
        );
        // `AccountRow` sits in the extension's own file; proximity used to
        // pick the extension.
        assert_eq!(
            all.iter()
                .filter(|e| e.contains(" -> Account ("))
                .collect::<Vec<_>>(),
            ["references AccountRow -> Account (Account.swift:1)"],
            "{report}"
        );
        // HomeView's `Result<Int, LoadError>` is Swift's and its `@State`
        // SwiftUI's; inside ListViewModel, `State` and `Result` are its own.
        assert_eq!(
            all.iter()
                .filter(
                    |e| e.contains("ListViewModel::State") || e.contains("ListViewModel::Result")
                )
                .collect::<Vec<_>>(),
            [
                "references ListViewModel -> ListViewModel::Result (ListViewModel.swift:3)",
                "references ListViewModel -> ListViewModel::State (ListViewModel.swift:2)",
            ],
            "{report}"
        );
        // `HomeView().cardStyle()`: `cardStyle` comes from `extension View`,
        // and HomeView conforms to View.
        for expected in [
            "calls makeHome -> View::cardStyle (Card.swift:3)",
            "calls HomeView::body -> Text::headline (Text+Style.swift:3)",
        ] {
            assert!(all.contains(&expected.to_string()), "{all:#?}\n{report}");
        }
    }

    /// Upstream #2107: a subclass names its superclass's nested type bare, a
    /// generic qualifier is not part of a type's path, and inside `extension
    /// Socket.Option` the bare `Option` is the type itself (Rust also keeps
    /// the property's type annotation, W2-03c, so a `references` edge joins
    /// upstream's `instantiates`). Upstream's fixture.
    #[test]
    fn pr2107_the_scopes_that_can_name_a_nested_type_bare() {
        let project = Project::new()
            .file(
                "Sources/Core/Channel.swift",
                "class BaseChannel {\n    enum ReadResult { case some, none }\n}\nfinal class StreamChannel: BaseChannel {\n    var last: ReadResult?\n}\n",
            )
            .file(
                "Sources/Core/Future.swift",
                "struct Future<Value> {\n    struct Isolated { let id: Int }\n}\nstruct Waiter {\n    var isolated: Future<Int>.Isolated?\n}\n",
            )
            .file(
                "Sources/Core/Socket.swift",
                "enum Socket {\n    struct Option { let raw: Int }\n}\nextension Socket.Option {\n    static let none: Option = Option(raw: 0)\n}\n",
            )
            .index();
        let all = edges(project.graph());
        let report = project.graph().report();
        for expected in [
            "references StreamChannel -> BaseChannel::ReadResult (Channel.swift:2)",
            "references Waiter -> Future::Isolated (Future.swift:2)",
        ] {
            assert!(all.contains(&expected.to_string()), "{all:#?}\n{report}");
        }
        assert_eq!(
            all.iter()
                .filter(|e| e.contains(" -> Socket::Option ")
                    || e.ends_with("-> Option (Socket.swift:4)"))
                .collect::<Vec<_>>(),
            [
                "instantiates Option -> Socket::Option (Socket.swift:2)",
                "references Option -> Socket::Option (Socket.swift:2)",
            ],
            "{report}"
        );
    }

    /// Upstream #2107: the index keeps `Build.Id` and `Package.Id` as refs
    /// named `Id`; the line says which nested type each is. Upstream's
    /// fixture; its qualified construction (`API.PackageController.Model(…)`)
    /// is a type-path call here, W4-SW-02's.
    #[test]
    fn pr2107_a_qualified_type_name_lands_on_the_type_on_that_path() {
        let project = Project::new()
            .file(
                "Sources/App/Models/Build.swift",
                "import Foundation\nfinal class Build {\n    typealias Id = UUID\n}\n",
            )
            .file(
                "Sources/App/Models/Package.swift",
                "import Foundation\nfinal class Package {\n    typealias Id = UUID\n}\n",
            )
            .file(
                "Sources/App/Commands/Trigger.swift",
                "struct Trigger {\n    let build: Build.Id\n    let package: Package.Id\n}\n",
            )
            .index();
        let all = edges(project.graph());
        assert_eq!(
            all.iter()
                .filter(|e| e.starts_with("references Trigger -> "))
                .collect::<Vec<_>>(),
            [
                "references Trigger -> Build (Build.swift:2)",
                "references Trigger -> Build::Id (Build.swift:3)",
                "references Trigger -> Package (Package.swift:2)",
                "references Trigger -> Package::Id (Package.swift:3)",
            ],
            "{}",
            project.graph().report()
        );
    }

    /// Upstream #2118: a call through a type path lands on the type the path
    /// names (declared nested, in `extension API.PackageController`, in
    /// `extension A.B.C`, shortened inside its namespace, or module-qualified),
    /// a nested type's initializer, a case with associated values or the case
    /// a TCA `@Reducer enum` generates its `State` from, or nowhere. Upstream's
    /// fixture.
    #[test]
    fn pr2118_a_call_through_a_type_path_lands_on_the_type_it_names() {
        let project = Project::new()
            .file(
                "Sources/App/API.swift",
                "enum API {}\nextension API {\n    enum PackageController {\n        enum GetRoute {\n            static func query(on db: Int) -> Int { db }\n            static func query(on db: Int, limit: Int) -> Int { db + limit }\n        }\n        struct Model { let name: String }\n    }\n}\nextension API.PackageController {\n    enum ShowRoute {\n        static func query(on db: Int) -> Int { db }\n    }\n}\nextension API.DependencyController.GetRoute {\n    static func query(on db: Int) -> Int { db }\n}\nextension API {\n    enum DependencyController {\n        enum GetRoute {}\n        static func inside() -> Int { PackageController.GetRoute.query(on: 0) }\n    }\n}\n",
            )
            .file(
                "Sources/App/Other.swift",
                "enum Other { static func query(on db: Int) -> Int { db } }\nenum Gitlab { enum Error: Swift.Error { case requestFailed(status: Int) } }\nenum Remote { enum Error: Swift.Error { case requestFailed(status: Int) } }\nstruct Detail {}\nenum Screens {\n    @Reducer\n    enum Path { case detail(Detail) }\n}\nenum Zed { enum GetRoute { static func query(on db: Int) -> Int { db } } }\nenum Yon { enum GetRoute { static func query(on db: Int) -> Int { db } } }\n",
            )
            .file(
                "Sources/App/Basics.swift",
                "struct BasicsView {}\nstruct ObservableBasicsView {}\nextension BasicsView { struct Feature {} }\nextension ObservableBasicsView { struct Feature {} }\nextension BasicsView.Feature { struct State {} }\nextension ObservableBasicsView.Feature { struct State {} }\n",
            )
            .file(
                "Sources/App/Health.swift",
                "import Foundation\nenum Health {\n    static func one() -> Int { API.PackageController.GetRoute.query(on: 1) }\n    static func two() -> Int {\n        API.PackageController.GetRoute\n            .query(on: 2)\n    }\n    static func three() -> Int { Other.query(on: 3) }\n    static func four() -> Int { API.PackageController.ShowRoute.query(on: 4) }\n    static func five() -> Int { API.DependencyController.GetRoute.query(on: 5) }\n    static func six() -> API.PackageController.Model { API.PackageController.Model(name: \"x\") }\n    static func seven() -> Int { App.Zed.GetRoute.query(on: 7) }\n    static func eight() -> Int { Missing.Thing.query(on: 8) }\n    static func nine() -> Int { API.PackageController.GetRoute.nothing(on: 9) }\n    static func ten() throws { throw Gitlab.Error.requestFailed(status: 10) }\n    static func eleven() { _ = Screens.Path.State.detail(Detail()) }\n    static func twelve() { _ = ObservableBasicsView.Feature.State() }\n}\n",
            )
            .index();
        let graph = project.graph();
        let reached = |caller: &str| -> Vec<String> {
            let from = graph
                .nodes
                .iter()
                .find(|n| n.name == caller && n.kind == "method")
                .unwrap_or_else(|| panic!("{caller}\n{}", graph.report()));
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|e| e.source == from.id && (e.kind == "calls" || e.kind == "instantiates"))
                .filter_map(|e| graph.node(&e.target))
                .filter(|n| n.language == "swift")
                .map(|n| format!("{}:{}", n.qualified_name, n.start_line))
                .collect();
            out.sort();
            out
        };
        let report = graph.report();
        // The first declared of GetRoute's two overloads.
        assert_eq!(
            reached("one"),
            ["API::PackageController::GetRoute::query:5"],
            "{report}"
        );
        assert_eq!(
            reached("two"),
            ["API::PackageController::GetRoute::query:5"],
            "{report}"
        );
        assert_eq!(reached("three"), ["Other::query:1"], "{report}");
        // Declared in `extension API.PackageController { enum ShowRoute … }`.
        assert_eq!(
            reached("four"),
            ["PackageController::ShowRoute::query:13"],
            "{report}"
        );
        // Declared in `extension API.DependencyController.GetRoute { … }`.
        assert_eq!(reached("five"), ["GetRoute::query:17"], "{report}");
        // A nested type's initializer.
        assert_eq!(
            reached("six"),
            ["API::PackageController::Model:8"],
            "{report}"
        );
        // A module qualifier.
        assert_eq!(reached("seven"), ["Zed::GetRoute::query:9"], "{report}");
        // No such type, and no such member: nothing, not some other type's
        // `query`.
        assert!(reached("eight").is_empty(), "{report}");
        assert!(reached("nine").is_empty(), "{report}");
        // A case with associated values.
        assert_eq!(
            reached("ten"),
            ["Gitlab::Error::requestFailed:2"],
            "{report}"
        );
        // The case a TCA `@Reducer enum` generates its State from.
        assert!(
            reached("eleven").contains(&"Screens::Path::detail:7".to_string()),
            "{report}"
        );
        // Two types share the qualified name `Feature::State`; the extension
        // each is declared in tells them apart.
        assert_eq!(reached("twelve"), ["Feature::State:6"], "{report}");
        // Shortened inside its namespace.
        assert_eq!(
            reached("inside"),
            ["API::PackageController::GetRoute::query:5"],
            "{report}"
        );
    }

    /// The calls out of `file`, as their targets' qualified names, sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2170: a bare, `self.` or `super.` Swift call reaches the
    /// nearest member of the types around it and what they inherit (UIKit
    /// ancestry included), `super.` the inherited only; down a longer chain
    /// a standard-library name needs the receiver to name the owner or a
    /// label only the project's method takes; a playground's global is no
    /// call target. Upstream's fixture.
    #[test]
    fn pr2170_a_swift_call_reached_by_name_follows_how_it_is_written() {
        let project = Project::new()
            .file(
                "Sources/Charts/Utils/Sequence+KeyPath.swift",
                "extension Sequence {\n    func max<T: Comparable>(by keyPath: KeyPath<Element, T>) -> Element? {\n        return nil\n    }\n}\n",
            )
            .file(
                "Sources/Charts/Renderers/Renderer.swift",
                "open class Renderer {\n    public struct XBounds {\n        public var min: Int = 0\n    }\n}\n",
            )
            .file(
                "Sources/Charts/Data/ChartData.swift",
                "open class ChartData {\n    open func removeAll() { }\n}\n",
            )
            .file(
                "Sources/Charts/Formatters/Formatter.swift",
                "open class Formatter {\n    open func reset() { }\n}\n",
            )
            .file(
                "Sources/Charts/Charts/ChartViewBase.swift",
                "open class ChartViewBase: NSObject {\n    open func highlightValue(_ h: Int) { }\n    open func reset() { }\n}\n",
            )
            .file(
                "Sources/Charts/Charts/BarChartView.swift",
                "open class BarChartView: ChartViewBase {\n    open override func highlightValue(_ h: Int) { }\n}\n",
            )
            .file(
                "Sources/Charts/Charts/BarLineChartViewBase.swift",
                "open class BarLineChartViewBase: ChartViewBase {\n    open func tapped(axis: Axis) {\n        highlightValue(1)\n        let widest = max(1, 2)\n        let narrowest = min(3, 4)\n        super.reset()\n        axis.entries.removeAll()\n    }\n}\n",
            )
            .file(
                "Sources/Charts/Wrapper/KingfisherWrapper.swift",
                "public struct KingfisherWrapper<Base> {\n    public let base: Base\n}\n\nextension KingfisherWrapper where Base == Data {\n    func contains(jpeg marker: Int) -> Bool {\n        return false\n    }\n}\n",
            )
            .file(
                "Sources/Charts/Wrapper/Decoder.swift",
                "final class Decoder {\n    func scan(data: Data) -> Bool {\n        return data.kf.contains(jpeg: 0xC2)\n    }\n}\n",
            )
            .file(
                "Demo/Extensions/UIViewController+Bar.swift",
                "extension UIViewController {\n    func setupOperationBar() { }\n}\n",
            )
            .file(
                "Demo/Controllers/ListController.swift",
                "class ListController: UICollectionViewController {\n    override func viewDidLoad() {\n        setupOperationBar()\n    }\n}\n",
            )
            .file("Demo/Chart.playground/Contents.swift", "let min = 20.0\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "Sources/Charts/Charts/BarLineChartViewBase.swift"),
            ["ChartViewBase::highlightValue", "ChartViewBase::reset"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "Sources/Charts/Wrapper/Decoder.swift"),
            ["KingfisherWrapper::contains"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "Demo/Controllers/ListController.swift"),
            ["UIViewController::setupOperationBar"],
            "{report}"
        );
    }

    /// Upstream #2205: a call on a property the type around it declares with
    /// a type reaches that type's method, not a subclass override elsewhere
    /// (upstream's fixture), even when the property's name shares no word
    /// with its type, and a function's own `let` shadows the property.
    #[test]
    fn pr2205_a_call_on_a_typed_swift_property_is_a_member_of_its_type() {
        let project = Project::new()
            .file(
                "Sources/Cache/ImageCache.swift",
                "open class ImageCache {\n    open func imageCachedType(forKey key: String) -> Int {\n        return 0\n    }\n}\n",
            )
            .file(
                "Sources/Cache/Disk.swift",
                "open class DiskBackend {\n    open func imageCachedType(forKey key: String) -> Int {\n        return 2\n    }\n}\n",
            )
            .file(
                "Tests/KingfisherTests/ImagePrefetcherTests.swift",
                "class EvictingPrefetchCache: ImageCache {\n    override func imageCachedType(forKey key: String) -> Int {\n        return 1\n    }\n}\n",
            )
            .file(
                "Tests/KingfisherTests/ImageCacheTests.swift",
                "class ImageCacheTests {\n    var cache: ImageCache!\n\n    func testCachedType() {\n        _ = cache.imageCachedType(forKey: \"a\")\n        _ = self.cache.imageCachedType(forKey: \"b\")\n    }\n}\n",
            )
            .file(
                "Tests/KingfisherTests/StoreTests.swift",
                "class StoreTests {\n    var store: ImageCache!\n\n    func testCachedType() {\n        _ = store.imageCachedType(forKey: \"a\")\n        _ = self.store.imageCachedType(forKey: \"b\")\n    }\n    func testShadowed() {\n        let store = DiskBackend()\n        _ = store.imageCachedType(forKey: \"c\")\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "Tests/KingfisherTests/ImageCacheTests.swift"),
            ["ImageCache::imageCachedType", "ImageCache::imageCachedType"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "Tests/KingfisherTests/StoreTests.swift"),
            [
                "DiskBackend::imageCachedType",
                "ImageCache::imageCachedType",
                "ImageCache::imageCachedType",
            ],
            "{report}"
        );
    }
}

mod objc {
    use super::support::{Graph, Project};

    /// The calls out of `file`, as their targets' qualified names, sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2150: a message to `self` or `super` (the extractor drops
    /// the receiver) reaches a method of the sender's hierarchy, never a
    /// proxy class's `class`; C call syntax never names a method or
    /// property; a message to an untyped local needs its declaration
    /// (`FMResultSet *rs`) or a receiver named after the class. Upstream's
    /// fixture.
    #[test]
    fn pr2150_an_objc_message_reached_by_name_alone_must_be_in_reach() {
        let project = Project::new()
            .file(
                "Core/SDWeakProxy.h",
                "#import <Foundation/Foundation.h>\n@interface SDWeakProxy : NSProxy\n- (Class)class;\n- (BOOL)respondsToSelector:(SEL)aSelector;\n@end\n",
            )
            .file(
                "Core/SDWeakProxy.m",
                "#import \"SDWeakProxy.h\"\n@implementation SDWeakProxy\n- (Class)class {\n    return nil;\n}\n- (BOOL)respondsToSelector:(SEL)aSelector {\n    return NO;\n}\n@end\n",
            )
            .file(
                "Core/SDPrefetchToken.h",
                "#import <Foundation/Foundation.h>\n@interface SDPrefetchToken : NSObject\n@property (nonatomic, copy) void (^completionBlock)(void);\n@end\n",
            )
            .file(
                "Core/FMResultSet.h",
                "#import <Foundation/Foundation.h>\n@interface FMResultSet : NSObject\n- (BOOL)next;\n@end\n",
            )
            .file(
                "Core/FMResultSet.m",
                "#import \"FMResultSet.h\"\n@implementation FMResultSet\n- (BOOL)next {\n    return NO;\n}\n@end\n",
            )
            .file(
                "Core/SDBase.h",
                "#import <Foundation/Foundation.h>\n@interface SDBase : NSObject\n- (void)prepare;\n@end\n",
            )
            .file(
                "Core/SDBase.m",
                "#import \"SDBase.h\"\n@implementation SDBase\n- (void)prepare {\n}\n@end\n",
            )
            .file(
                "Core/SDCache.h",
                "#import \"SDBase.h\"\n@interface SDCache : SDBase\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image;\n@end\n",
            )
            .file(
                "Core/SDCache.m",
                "#import \"SDCache.h\"\n#import \"FMResultSet.h\"\n@implementation SDCache\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image {\n    Class cls = [self class];\n    [super prepare];\n    [self helper];\n    FMResultSet *rs = [self query];\n    while ([rs next]) {\n    }\n    if ([image respondsToSelector:@selector(copy)]) {\n    }\n    completionBlock();\n}\n- (void)helper {\n}\n- (FMResultSet *)query {\n    return nil;\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let method = graph
            .nodes
            .iter()
            .find(|n| n.qualified_name == "SDCache::storeWithCompletion:image:")
            .unwrap_or_else(|| panic!("{report}"));
        let mut targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls" && e.source == method.id)
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        targets.sort();
        assert_eq!(
            targets,
            [
                "FMResultSet::next",
                "SDBase::prepare",
                "SDCache::helper",
                "SDCache::query"
            ],
            "{report}"
        );
    }

    /// Upstream #2150: an untyped receiver keeps the one method of its name
    /// when it ends in a property whose declared type has the method's
    /// owner (`managed.anyDataObj`, an `RLMSet *`), and not when it names
    /// nothing the owner is (`image`). A class receiver reaches the class
    /// method it inherits.
    #[test]
    fn pr2150_a_class_or_typed_property_receiver_keeps_its_inherited_method() {
        let project = Project::new()
            .file(
                "RLMObject.h",
                "#import <Foundation/Foundation.h>\n@interface RLMObject : NSObject\n+ (id)objectsInRealm:(id)realm;\n@end\n",
            )
            .file(
                "RLMObject.m",
                "#import \"RLMObject.h\"\n@implementation RLMObject\n+ (id)objectsInRealm:(id)realm {\n    return nil;\n}\n@end\n",
            )
            .file(
                "RLMSet.h",
                "#import <Foundation/Foundation.h>\n@interface RLMSet<T> : NSObject\n- (BOOL)containsObject:(id)object;\n@end\n",
            )
            .file(
                "RLMSet.m",
                "#import \"RLMSet.h\"\n@implementation RLMSet\n- (BOOL)containsObject:(id)object {\n    return NO;\n}\n@end\n",
            )
            .file(
                "AllTypesObject.h",
                "#import \"RLMObject.h\"\n#import \"RLMSet.h\"\n@interface AllTypesObject : RLMObject\n@property (nonatomic) RLMSet<NSString *> *anyDataObj;\n@end\n",
            )
            .file(
                "Tests.m",
                "#import \"AllTypesObject.h\"\n@interface Tests : NSObject\n@end\n@implementation Tests\n- (void)run:(AllTypesObject *)managed image:(id)image {\n    [AllTypesObject objectsInRealm:nil];\n    [managed.anyDataObj containsObject:@\"x\"];\n    [image containsObject:@\"y\"];\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "Tests.m"),
            ["RLMObject::objectsInRealm:", "RLMSet::containsObject:"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2150: a method parameter's declaration
    /// (`(nullable FMResultSet *)rs`) types the messages to it, and a
    /// function the parameter's name shadows (`rs(void)`) types nothing.
    #[test]
    fn pr2150_a_typed_parameter_types_its_messages() {
        let project = Project::new()
            .file(
                "FMResultSet.m",
                "@interface FMResultSet : NSObject\n@end\n@implementation FMResultSet\n- (BOOL)next {\n    return NO;\n}\n@end\n",
            )
            .file(
                "FMCursor.m",
                "@interface FMCursor : NSObject\n@end\n@implementation FMCursor\n- (BOOL)next {\n    return NO;\n}\n@end\n",
            )
            .file(
                "Reader.m",
                "@interface Reader : NSObject\n@end\n@implementation Reader\n- (void)read:(nullable FMResultSet *)rs {\n    [rs next];\n}\n- (void)scan {\n    FMCursor *rs(void);\n    [rs next];\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let calls_of = |method: &str| -> Vec<String> {
            let id = &graph
                .nodes
                .iter()
                .find(|n| n.qualified_name == method)
                .unwrap_or_else(|| panic!("{report}"))
                .id;
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|e| e.kind == "calls" && &e.source == id)
                .filter_map(|e| graph.node(&e.target))
                .map(|n| n.qualified_name.clone())
                .collect();
            out.sort();
            out
        };
        assert_eq!(calls_of("Reader::read:"), ["FMResultSet::next"], "{report}");
        assert!(calls_of("Reader::scan").is_empty(), "{report}");
    }

    /// Upstream #2150's system table: a category on UIImageView sending
    /// `[self sd_internalSetImageWithURL:]` reaches the UIView category's
    /// method, UIView being UIImageView's superclass, and not another
    /// class's method of that name.
    #[test]
    fn pr2150_a_category_on_a_system_class_reaches_its_superclass_category() {
        let project = Project::new()
            .file(
                "UIView+WebCache.m",
                "#import <UIKit/UIKit.h>\n@implementation UIView (WebCache)\n- (void)sd_internalSetImageWithURL:(NSURL *)url {\n}\n@end\n",
            )
            .file(
                "SDDecoy.m",
                "#import <Foundation/Foundation.h>\n@interface SDDecoy : NSObject\n@end\n@implementation SDDecoy\n- (void)sd_internalSetImageWithURL:(NSURL *)url {\n}\n@end\n",
            )
            .file(
                "UIImageView+WebCache.m",
                "#import <UIKit/UIKit.h>\n@implementation UIImageView (WebCache)\n- (void)sd_setImageWithURL:(NSURL *)url {\n    [self sd_internalSetImageWithURL:url];\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "UIImageView+WebCache.m"),
            ["UIView::sd_internalSetImageWithURL:"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2179: `[super init]` goes past the class it is written in,
    /// to the superclass's `init`, or for an NSObject subclass to no project
    /// method at all. Upstream's fixture.
    #[test]
    fn pr2179_a_message_to_super_reaches_the_superclass() {
        let project = Project::new()
            .file(
                "Base.h",
                "#import <Foundation/Foundation.h>\n@interface Base : NSObject\n- (instancetype)init;\n@end\n",
            )
            .file(
                "Base.m",
                "#import \"Base.h\"\n@implementation Base\n- (instancetype)init {\n    self = [super init];\n    return self;\n}\n@end\n",
            )
            .file("Child.h", "#import \"Base.h\"\n@interface Child : Base\n@end\n")
            .file(
                "Child.m",
                "#import \"Child.h\"\n@implementation Child\n- (instancetype)init {\n    self = [super init];\n    return self;\n}\n@end\n",
            )
            .file(
                "Other.m",
                "#import <Foundation/Foundation.h>\n@interface Other : NSObject\n@end\n@implementation Other\n- (instancetype)init {\n    self = [super init];\n    return self;\n}\n@end\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(calls_from(graph, "Child.m"), ["Base::init"], "{report}");
        assert!(calls_from(graph, "Base.m").is_empty(), "{report}");
        assert!(calls_from(graph, "Other.m").is_empty(), "{report}");
    }

    /// Upstream #2242: a class conforming to a protocol of its own name
    /// (SDWebImage's `@interface SDDiskCache : NSObject <SDDiskCache>`,
    /// its header copied into `include/`) implements the protocol in its
    /// own header. Upstream's fixture.
    #[test]
    fn pr2242_a_class_conforming_to_its_namesake_protocol_implements_it() {
        let header = "#import <Foundation/Foundation.h>\n\n@protocol SDDiskCache <NSObject>\n- (nullable NSData *)dataForKey:(nonnull NSString *)key;\n@end\n\n@interface SDDiskCache : NSObject <SDDiskCache>\n@end\n";
        let project = Project::new()
            .file("SDWebImage/Core/SDDiskCache.h", header)
            .file("SDWebImage/include/SDWebImage/SDDiskCache.h", header)
            .file(
                "Examples/SwiftUIDemo/ContentView.swift",
                "import SwiftUI\n\nstruct ContentView: View {\n    var body: some View { Text(\"x\") }\n}\n",
            )
            .index();
        let graph = project.graph();
        let class = graph
            .nodes
            .iter()
            .find(|n| n.kind == "class" && n.file_path == "SDWebImage/Core/SDDiskCache.h")
            .unwrap_or_else(|| panic!("{}", graph.report()));
        let targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "implements" && e.source == class.id)
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{} {}", n.kind, n.file_path))
            .collect();
        assert_eq!(
            targets,
            ["protocol SDWebImage/Core/SDDiskCache.h"],
            "{}",
            graph.report()
        );
    }
}

mod go {
    use super::support::{Graph, NodeRow, Project, codegraph};

    /// `file::qualifiedName` of every `kind` target of the symbol `name` in
    /// `file`, sorted (upstream's `targets` / `callTargets`).
    fn targets(graph: &Graph, file: &str, name: &str, kind: &str) -> Vec<String> {
        let source = graph
            .nodes
            .iter()
            .find(|n| n.file_path == file && n.name == name)
            .unwrap_or_else(|| panic!("{name} in {file}\n{}", graph.report()));
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == kind && e.source == source.id)
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{}::{}", n.file_path, n.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// `kind name` of every node of `file` but the file and its imports, in
    /// source order (upstream's `declarations`).
    fn declarations(graph: &Graph, file: &str) -> Vec<String> {
        let mut nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == file && n.kind != "file" && n.kind != "import")
            .collect();
        nodes.sort_by_key(|n| n.start_line);
        nodes
            .iter()
            .map(|n| format!("{} {}", n.kind, n.name))
            .collect()
    }

    /// `source name` of every `references` reference out of a type
    /// declaration of `file`, resolved or not, sorted (upstream's `typeRefs`).
    fn type_refs(graph: &Graph, file: &str) -> Vec<String> {
        let is_type = |kind: &str| matches!(kind, "type_alias" | "struct" | "interface");
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references")
            .filter_map(|e| Some((graph.node(&e.source)?, graph.node(&e.target)?)))
            .filter(|(source, _)| source.file_path == file && is_type(&source.kind))
            .map(|(source, target)| format!("{} {}", source.name, target.name))
            .chain(
                graph
                    .unresolved
                    .iter()
                    .filter(|r| r.reference_kind == "references")
                    .filter_map(|r| Some((graph.node(&r.from_node_id)?, r)))
                    .filter(|(source, _)| source.file_path == file && is_type(&source.kind))
                    .map(|(source, r)| format!("{} {}", source.name, r.reference_name)),
            )
            .collect();
        out.sort();
        out
    }

    fn docstring_of(graph: &Graph, file: &str, kind: &str, name: &str) -> Option<String> {
        let found: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == file && n.kind == kind && n.name == name)
            .collect();
        assert_eq!(found.len(), 1, "{kind} {name}\n{}", graph.report());
        found[0].docstring.clone()
    }

    const ALIAS_SOURCE: &str = "package clientv3\n\nimport (\n\t\"context\"\n\n\t\"go.etcd.io/etcd/api/v3/mvccpb\"\n)\n\n// Event is the event a watcher reports.\ntype Event = mvccpb.Event\n\ntype WatchChan <-chan WatchResponse\n\ntype WatchResponse struct {\n\tEvents []*Event\n}\n\ntype (\n\t// Local names a type of this package.\n\tLocal   = WatchResponse\n\tPtr     = *WatchResponse\n\tSlice   = []WatchResponse\n\tHandler = func(ctx context.Context, r WatchResponse) error\n\tIndex   = map[string]*mvccpb.KeyValue\n\tDefined WatchResponse\n)\n\ntype Plain = List[WatchResponse]\n\ntype Anon = struct {\n\tKey string\n}\n\ntype Closer = interface {\n\tClose() error\n}\n\ntype Type = string\n\ntype Word = uint\n";

    const STORE: &str = "package store\n\ntype Manager struct{}\n\nfunc New() *Manager { return &Manager{} }\n\nfunc (m *Manager) CreateItem(name string) error { return nil }\n";
    const CLOCK: &str = "package clock\n\nfunc Now() int { return 0 }\n";

    fn service(module: &str) -> String {
        format!(
            "package domain\n\nimport (\n\t\"{module}/internal/store\"\n\t\"{module}/pkg/clock\"\n)\n\ntype Service struct {{\n\tdb *store.Manager\n}}\n\nfunc NewService() *Service {{\n\treturn &Service{{db: store.New()}}\n}}\n\nfunc (s *Service) AddItem(name string) error {{\n\treturn s.db.CreateItem(name)\n}}\n\nfunc Stamp() int {{\n\treturn clock.Now()\n}}\n"
        )
    }

    /// Upstream #2323 (via #2361): a call through a method receiver of an
    /// unexported type, through a field of one, and through a type it
    /// embeds, all within the receiver's own package; another package's
    /// same-named types are never targets. Upstream's fixture.
    #[test]
    fn pr2361_calls_through_an_unexported_receiver_stay_in_its_package() {
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file(
                "internal/domain/service.go",
                "package domain\n\ntype Service struct{}\n\nfunc (s *Service) AddItem(name string) error { return nil }\n\nfunc (s *Service) Count() int { return 0 }\n",
            )
            .file(
                "internal/handlers/server.go",
                "package handlers\n\nimport \"example.com/app/internal/domain\"\n\ntype server struct {\n\tBaseAPI\n\tservice *domain.Service\n\tstore   *store\n}\n\nfunc (s *server) Create(name string) error {\n\ts.store.Put(name)\n\ts.Close()\n\ts.SendError()\n\treturn s.service.AddItem(name)\n}\n\ntype cache[T any] struct {\n\tservice *domain.Service\n}\n\nfunc (c *cache[T]) Size() int {\n\treturn c.service.Count()\n}\n",
            )
            .file(
                "internal/handlers/store.go",
                "package handlers\n\ntype store struct{}\n\nfunc (st *store) Put(name string) {}\n\nfunc (s *server) Close() {}\n",
            )
            .file(
                "internal/handlers/base.go",
                "package handlers\n\ntype BaseAPI struct{}\n\nfunc (b *BaseAPI) SendError() {}\n",
            )
            .file(
                "internal/admin/server.go",
                "package admin\n\ntype server struct {\n\tBaseAPI\n\tstore *store\n}\n\ntype store struct{}\n\nfunc (st *store) Put(name string) {}\n\nfunc (s *server) Close() {}\n\ntype BaseAPI struct{}\n\nfunc (b *BaseAPI) SendError() {}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            targets(graph, "internal/handlers/server.go", "Create", "calls"),
            [
                "internal/domain/service.go::Service::AddItem",
                "internal/handlers/base.go::BaseAPI::SendError",
                "internal/handlers/store.go::server::Close",
                "internal/handlers/store.go::store::Put",
            ],
            "{report}"
        );
        assert_eq!(
            targets(graph, "internal/handlers/server.go", "Size", "calls"),
            ["internal/domain/service.go::Service::Count"],
            "{report}"
        );
        let admin: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == "internal/admin/server.go" && n.kind == "method")
            .map(|n| n.id.as_str())
            .collect();
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && admin.contains(&e.target.as_str())),
            "{report}"
        );
    }

    /// Upstream #2322 (via #2361): a module in a subdirectory resolves its
    /// package-qualified calls, and a call through a struct field typed with
    /// one of its packages. Upstream's fixture.
    #[test]
    fn pr2361_a_go_module_in_a_subdirectory() {
        let project = Project::new()
            .file("svc/go.mod", "module example.com/app/svc\n\ngo 1.22\n")
            .file("svc/internal/store/store.go", STORE)
            .file("svc/pkg/clock/clock.go", CLOCK)
            .file(
                "svc/internal/domain/service.go",
                &service("example.com/app/svc"),
            )
            .file("web/package.json", "{ \"name\": \"web\" }\n")
            .index();
        let graph = project.graph();
        let file = "svc/internal/domain/service.go";
        assert_eq!(
            targets(graph, file, "NewService", "calls"),
            ["svc/internal/store/store.go::New"]
        );
        assert_eq!(
            targets(graph, file, "Stamp", "calls"),
            ["svc/pkg/clock/clock.go::Now"]
        );
        assert_eq!(
            targets(graph, file, "AddItem", "calls"),
            ["svc/internal/store/store.go::Manager::CreateItem"]
        );
    }

    /// Upstream #2322: modules side by side, etcd's shape. Upstream's
    /// fixture.
    #[test]
    fn pr2361_go_modules_side_by_side() {
        let project = Project::new()
            .file("go.mod", "module example.com/etcd/v3\n\ngo 1.22\n")
            .file(
                "etcdutl/main.go",
                "package main\n\nimport \"example.com/etcd/server/v3/storage/wal\"\n\nfunc main() {\n\twal.OpenForRead(\"dir\")\n}\n",
            )
            .file("server/go.mod", "module example.com/etcd/server/v3\n\ngo 1.22\n")
            .file(
                "server/storage/wal/wal.go",
                "package wal\n\nfunc OpenForRead(dir string) error { return nil }\n",
            )
            .file("server/api/api.go", "package api\n\nfunc Start() int { return 1 }\n")
            .file(
                "server/cmd/main.go",
                "package main\n\nimport (\n\t\"example.com/etcd/server/v3/api\"\n\t\"example.com/tools/lint\"\n)\n\ntype App struct {\n\tlinter *lint.Linter\n}\n\nfunc main() {\n\tapi.Start()\n\tlint.Run()\n}\n\nfunc (a *App) check() {\n\ta.linter.Check()\n}\n",
            )
            .file(
                "server/cmd/ext.go",
                "package main\n\nimport (\n\t\"example.com/toolsx/api\"\n)\n\nfunc external() {\n\tapi.Start()\n}\n",
            )
            .file("tools/go.mod", "module example.com/tools\n\ngo 1.22\n")
            .file("tools/api/api.go", "package api\n\nfunc Start() int { return 2 }\n")
            .file(
                "tools/lint/lint.go",
                "package lint\n\ntype Linter struct{}\n\nfunc Run() {}\n\nfunc (l *Linter) Check() {}\n",
            )
            .file(
                "tools/gen/gen.go",
                "package main\n\nimport \"example.com/tools/api\"\n\nfunc generate() {\n\tapi.Start()\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            targets(graph, "etcdutl/main.go", "main", "calls"),
            ["server/storage/wal/wal.go::OpenForRead"]
        );
        assert_eq!(
            targets(graph, "tools/gen/gen.go", "generate", "calls"),
            ["tools/api/api.go::Start"]
        );
        assert_eq!(
            targets(graph, "server/cmd/main.go", "main", "calls"),
            ["server/api/api.go::Start", "tools/lint/lint.go::Run"]
        );
        assert_eq!(
            targets(graph, "server/cmd/main.go", "check", "calls"),
            ["tools/lint/lint.go::Linter::Check"]
        );
        assert!(targets(graph, "server/cmd/ext.go", "external", "calls").is_empty());
    }

    /// Upstream #2322: a name written through a package of a nested module
    /// is that package's; a module under testdata never answers for the
    /// module it imitates. Upstream's fixture.
    #[test]
    fn pr2361_names_written_through_a_package_of_a_nested_module() {
        let project = Project::new()
            .file("src/go.mod", "module example.com/harbor/src\n\ngo 1.22\n")
            .file("src/jobservice/job/op.go", "package job\n\ntype OPCommand string\n")
            .file(
                "src/jobservice/impl/context.go",
                "package impl\n\nimport \"example.com/harbor/src/jobservice/job\"\n\ntype Context struct{}\n\nfunc (c *Context) OPCommand() (job.OPCommand, bool) {\n\treturn \"\", false\n}\n",
            )
            .file(
                "src/pkg/artifact/manager.go",
                "package artifact\n\ntype Manager interface {\n\tCount() int64\n}\n",
            )
            .file(
                "src/pkg/cached/redis/manager.go",
                "package redis\n\nimport \"example.com/harbor/src/pkg/artifact\"\n\ntype Manager struct {\n\tdelegator artifact.Manager\n}\n\nfunc (m *Manager) Count() int64 {\n\treturn m.delegator.Count()\n}\n",
            )
            .file(
                "internal/testdata/stub/go.mod",
                "module example.com/harbor/src\n\ngo 1.22\n",
            )
            .file(
                "internal/testdata/stub/pkg/artifact/manager.go",
                "package artifact\n\ntype Manager interface {\n\tCount() int64\n}\n",
            )
            .file("tools/go.mod", "module example.com/harbor/tools\n\ngo 1.22\n")
            .file(
                "tools/report/report.go",
                "package report\n\nimport \"example.com/harbor/src/pkg/artifact\"\n\ntype Reporter struct {\n\tmgr artifact.Manager\n}\n\nfunc (r *Reporter) Total() int64 {\n\treturn r.mgr.Count()\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            targets(
                graph,
                "src/jobservice/impl/context.go",
                "OPCommand",
                "references"
            ),
            ["src/jobservice/job/op.go::OPCommand"]
        );
        assert_eq!(
            targets(graph, "src/pkg/cached/redis/manager.go", "Count", "calls"),
            ["src/pkg/artifact/manager.go::Manager::Count"]
        );
        assert_eq!(
            targets(graph, "tools/report/report.go", "Total", "calls"),
            ["src/pkg/artifact/manager.go::Manager::Count"]
        );
    }

    /// Upstream #2322: two copies declaring one module path resolve each
    /// one's imports into its own copy. Upstream's fixture.
    #[test]
    fn pr2361_two_go_modules_declaring_the_same_path() {
        let mut project = Project::new();
        for copy in ["v1", "v2"] {
            project = project
                .file(
                    &format!("{copy}/go.mod"),
                    "module example.com/app/svc\n\ngo 1.22\n",
                )
                .file(&format!("{copy}/internal/store/store.go"), STORE)
                .file(&format!("{copy}/pkg/clock/clock.go"), CLOCK)
                .file(
                    &format!("{copy}/internal/domain/service.go"),
                    &service("example.com/app/svc"),
                );
        }
        let project = project.index();
        let graph = project.graph();
        for copy in ["v1", "v2"] {
            let file = format!("{copy}/internal/domain/service.go");
            assert_eq!(
                targets(graph, &file, "NewService", "calls"),
                [format!("{copy}/internal/store/store.go::New")]
            );
            assert_eq!(
                targets(graph, &file, "Stamp", "calls"),
                [format!("{copy}/pkg/clock/clock.go::Now")]
            );
        }
    }

    /// Upstream #2322: a root-level go.mod resolves as before, the field
    /// chain included. Upstream's fixture.
    #[test]
    fn pr2361_a_go_module_at_the_project_root() {
        let project = Project::new()
            .file("go.mod", "module example.com/app/svc\n\ngo 1.22\n")
            .file("internal/store/store.go", STORE)
            .file("pkg/clock/clock.go", CLOCK)
            .file(
                "internal/domain/service.go",
                &service("example.com/app/svc"),
            )
            .index();
        let graph = project.graph();
        let file = "internal/domain/service.go";
        assert_eq!(
            targets(graph, file, "NewService", "calls"),
            ["internal/store/store.go::New"]
        );
        assert_eq!(
            targets(graph, file, "Stamp", "calls"),
            ["pkg/clock/clock.go::Now"]
        );
        assert_eq!(
            targets(graph, file, "AddItem", "calls"),
            ["internal/store/store.go::Manager::CreateItem"]
        );
    }

    /// Every `references` reference out of a type declaration of `file`
    /// sits on the name it writes, where resolution reads a package
    /// qualifier back (upstream's `expectRefsOnTheirNames`).
    #[track_caller]
    fn assert_type_refs_on_their_names(graph: &Graph, file: &str, source: &str) {
        let lines: Vec<&str> = source.lines().collect();
        let is_type = |id: &str| {
            graph.node(id).is_some_and(|n| {
                n.file_path == file
                    && matches!(n.kind.as_str(), "type_alias" | "struct" | "interface")
            })
        };
        let resolved = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references" && is_type(&e.source))
            .filter_map(|e| Some((graph.node(&e.target)?.name.as_str(), e.line?, e.col?)));
        let unresolved = graph
            .unresolved
            .iter()
            .filter(|r| r.reference_kind == "references" && is_type(&r.from_node_id))
            .map(|r| (r.reference_name.as_str(), r.line, r.col));
        let mut seen = 0;
        for (name, line, col) in resolved.chain(unresolved) {
            let text = lines[usize::try_from(line - 1).unwrap()];
            assert!(
                text.get(usize::try_from(col).unwrap()..)
                    .is_some_and(|rest| rest.starts_with(name)),
                "{name} at {line}:{col} of {file}: {text:?}"
            );
            seen += 1;
        }
        assert!(seen > 0, "no type references in {file}");
    }

    /// Each LF fixture as written, then with CRLF line ends.
    fn with_line_ends(source: &str) -> [(&'static str, String); 2] {
        [
            ("LF", source.to_string()),
            ("CRLF", source.replace('\n', "\r\n")),
        ]
    }

    /// Upstream #2417: an alias is a node of the kind a declaration of its
    /// type gets, nested under its file, documented as any type declaration
    /// is, and references the types it names, each on its name; a defined
    /// type references its own (#2441). Upstream's fixtures, LF and CRLF.
    /// Its generic aliases need no fallback here: the grammar has their rule.
    #[test]
    fn pr2417_an_alias_is_a_node_that_references_the_types_it_names() {
        let generic = "package clientv3\n\ntype List[T any] struct {\n\titems []T\n}\n\n// Set is a set of comparable values.\ntype Set[T comparable] = map[T]struct{}\n\ntype Items[T any] = List[T]\n\ntype Pairs[K comparable, V any] = map[K]List[V]\n";
        for ((label, aliases), (_, generic)) in with_line_ends(ALIAS_SOURCE)
            .into_iter()
            .zip(with_line_ends(generic))
        {
            let project = Project::new()
                .file("client/v3/watch.go", &aliases)
                .file("client/v3/set.go", &generic)
                .index();
            let graph = project.graph();
            let report = graph.report();
            let file = "client/v3/watch.go";
            assert_eq!(
                declarations(graph, file),
                [
                    "type_alias Event",
                    "type_alias WatchChan",
                    "struct WatchResponse",
                    "type_alias Local",
                    "type_alias Ptr",
                    "type_alias Slice",
                    "type_alias Handler",
                    "type_alias Index",
                    "type_alias Defined",
                    "type_alias Plain",
                    "struct Anon",
                    "interface Closer",
                    "method Close",
                    "type_alias Type",
                    "type_alias Word",
                ],
                "{label}\n{report}"
            );
            let event = node_in(graph, file, "Event");
            assert_eq!(event.kind, "type_alias", "{label}");
            assert!(event.is_exported, "{label}");
            assert_eq!(event.start_line, 10, "{label}");
            assert!(
                graph.edges.iter().any(|e| e.kind == "contains"
                    && e.source == format!("file:{file}")
                    && e.target == event.id),
                "{label}\n{report}"
            );
            assert_eq!(
                docstring_of(graph, file, "type_alias", "Local").as_deref(),
                Some("Local names a type of this package."),
                "{label}"
            );
            // #2438: a type declared on its own keeps the comment above it.
            assert_eq!(
                event.docstring.as_deref(),
                Some("Event is the event a watcher reports."),
                "{label}"
            );
            assert_eq!(
                type_refs(graph, file),
                [
                    "Defined WatchResponse",
                    "Event Event",
                    "Handler Context",
                    "Handler WatchResponse",
                    "Index KeyValue",
                    "Local WatchResponse",
                    "Plain List",
                    "Plain WatchResponse",
                    "Ptr WatchResponse",
                    "Slice WatchResponse",
                    "WatchChan WatchResponse",
                ],
                "{label}\n{report}"
            );
            assert_type_refs_on_their_names(graph, file, &aliases);
            let set = "client/v3/set.go";
            assert_eq!(
                declarations(graph, set),
                [
                    "struct List",
                    "type_alias Set",
                    "type_alias Items",
                    "type_alias Pairs"
                ],
                "{label}"
            );
            assert!(node_in(graph, set, "Set").is_exported, "{label}");
            assert_eq!(
                type_refs(graph, set),
                ["Items List", "Pairs List"],
                "{label}"
            );
            assert_type_refs_on_their_names(graph, set, &generic);
        }
    }

    /// The one node named `name` in `file`, imports aside.
    #[track_caller]
    fn node_in<'g>(graph: &'g Graph, file: &str, name: &str) -> &'g NodeRow {
        let found: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == file && n.name == name && n.kind != "import")
            .collect();
        assert_eq!(found.len(), 1, "{name} in {file}\n{}", graph.report());
        found[0]
    }

    /// `kind file:qualifiedName` of each edge but `contains` out of the one
    /// node named `name` in `file`, sorted and deduplicated (upstream's
    /// `linksFrom`).
    #[track_caller]
    fn links_from(graph: &Graph, file: &str, name: &str) -> Vec<String> {
        let id = &node_in(graph, file, name).id;
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains" && &e.source == id)
            .filter_map(|e| graph.node(&e.target).map(|t| (e.kind.as_str(), t)))
            .map(|(kind, t)| format!("{kind} {}:{}", t.file_path, t.qualified_name))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// `kind name` of every unresolved reference out of the one node named
    /// `name` in `file`.
    #[track_caller]
    fn unresolved_from(graph: &Graph, file: &str, name: &str) -> Vec<String> {
        let id = &node_in(graph, file, name).id;
        graph
            .unresolved
            .iter()
            .filter(|r| &r.from_node_id == id)
            .map(|r| format!("{} {}", r.reference_kind, r.reference_name))
            .collect()
    }

    /// Upstream #2417: an indexed module resolves through its aliases — an
    /// alias references the type it names in the package it is written
    /// through, a bare name in the alias's package is the alias, a method
    /// called on an alias is the aliased type's (through two aliases, one of
    /// a pointer, too), and an alias written through a package the index does
    /// not know links to nothing. Upstream's fixture; its `implements` half
    /// (an alias owning the methods written on it) is W7-15's.
    #[test]
    fn pr2417_an_indexed_go_module_resolves_through_its_aliases() {
        let project = Project::new()
            .file("go.mod", "module go.etcd.io/etcd\n\ngo 1.24\n")
            .file(
                "api/v3/mvccpb/kv.go",
                "package mvccpb\n\ntype Event struct {\n\tType int\n}\n\nfunc (e *Event) IsCreate() bool { return e.Type == 0 }\n",
            )
            .file(
                "alpha/event.go",
                "package alpha\n\ntype Event struct{}\n\nfunc (e *Event) IsCreate() bool { return false }\n",
            )
            .file(
                "client/v3/watch.go",
                "package clientv3\n\nimport \"go.etcd.io/etcd/api/v3/mvccpb\"\n\ntype Event = mvccpb.Event\n\ntype WatchResponse struct {\n\tEvents []*Event\n}\n\nfunc (wr *WatchResponse) Err() error { return nil }\n\ntype (\n\tLocal = WatchResponse\n\tPtr   = *Local\n)\n\nfunc newEvent() Event { return Event{} }\n\nfunc isCreate(e *Event) bool { return e.IsCreate() }\n\nfunc check(p Ptr) error { return p.Err() }\n",
            )
            .file(
                "client/v3/list.go",
                "package clientv3\n\ntype List[T any] struct{ items []T }\n\nfunc (l *List[T]) Len() int { return len(l.items) }\n\ntype Items[T any] = List[T]\n\nfunc count(xs *Items[int]) int { return xs.Len() }\n",
            )
            .file(
                "discovery/kuma.go",
                "package discovery\n\ntype Config interface {\n\tName() string\n}\n\ntype SDConfig struct{}\n\ntype KumaSDConfig = SDConfig\n\nfunc (*KumaSDConfig) Name() string { return \"kuma\" }\n",
            )
            .file(
                "watcher/watcher.go",
                "package watcher\n\nimport \"go.etcd.io/etcd/client/v3\"\n\n// The package is clientv3, though the index knows the import as v3 or client.\ntype Event = clientv3.Event\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let links = |file: &str, name: &str| links_from(graph, file, name);
        assert_eq!(
            links("client/v3/watch.go", "Event"),
            ["references api/v3/mvccpb/kv.go:Event"],
            "{report}"
        );
        assert_eq!(
            links("client/v3/watch.go", "Local"),
            ["references client/v3/watch.go:WatchResponse"]
        );
        assert_eq!(
            links("client/v3/watch.go", "Ptr"),
            ["references client/v3/watch.go:Local"]
        );
        assert_eq!(
            links("client/v3/list.go", "Items"),
            ["references client/v3/list.go:List"]
        );
        assert_eq!(
            links("client/v3/watch.go", "newEvent"),
            [
                "instantiates client/v3/watch.go:Event",
                "references client/v3/watch.go:Event"
            ]
        );
        assert_eq!(
            links("client/v3/watch.go", "isCreate"),
            [
                "calls api/v3/mvccpb/kv.go:Event::IsCreate",
                "references client/v3/watch.go:Event"
            ]
        );
        assert_eq!(
            links("client/v3/watch.go", "check"),
            [
                "calls client/v3/watch.go:WatchResponse::Err",
                "references client/v3/watch.go:Ptr"
            ]
        );
        assert_eq!(
            links("client/v3/list.go", "count"),
            [
                "calls client/v3/list.go:List::Len",
                "references client/v3/list.go:Items"
            ]
        );
        // Upstream also has `implements discovery/kuma.go:Config` here: W7-15.
        assert!(
            links("discovery/kuma.go", "KumaSDConfig")
                .contains(&"references discovery/kuma.go:SDConfig".to_string()),
            "{report}"
        );
        // Not to itself, nor to client/v3's, mvccpb's or alpha's `Event`.
        assert!(links("watcher/watcher.go", "Event").is_empty(), "{report}");
        assert_eq!(
            unresolved_from(graph, "watcher/watcher.go", "Event"),
            ["references Event"]
        );
    }

    /// Upstream #2441: a defined type is still one `type_alias` node, and
    /// references each type its right-hand side names, on the name: not the
    /// predeclared types, its own type parameters, or its own name written
    /// bare. Upstream's fixture, LF and CRLF.
    #[test]
    fn pr2441_a_defined_type_references_each_type_it_names() {
        let source = "package gin\n\nimport (\n\t\"context\"\n\n\t\"github.com/gin-gonic/gin/render\"\n)\n\n// HandlerFunc defines the handler used by gin middleware as return value.\ntype HandlerFunc func(*Context)\n\n// HandlersChain defines a HandlerFunc slice.\ntype HandlersChain []HandlerFunc\n\ntype Context struct {\n\thandlers HandlersChain\n}\n\ntype WatchChan <-chan WatchResponse\n\ntype WatchResponse struct{}\n\ntype (\n\t// Render names another package's type of its own name.\n\tRender   render.Render\n\tLookup   map[Key]*WatchResponse\n\tGrid     [4][4]Cell\n\tPipeline func(ctx context.Context, in <-chan Cell) (Key, error)\n\tWrapped  (Cell)\n\tNamed    Cell\n\tTables   map[string]struct {\n\t\tcell Cell\n\t\t*Key\n\t}\n)\n\ntype Generic[T any] List[T]\n\ntype Pair[K comparable, V any] map[K]List[V]\n\ntype stateFn func(*Lexer) stateFn\n\ntype Tree []*Tree\n\ntype List[T any] struct {\n\titems []T\n}\n\ntype Cell struct{}\n\ntype Key string\n\ntype Dur int\n\ntype Mode uint8\n\ntype Lexer struct{}\n";
        for (label, text) in with_line_ends(source) {
            let project = Project::new().file("gin.go", &text).index();
            let graph = project.graph();
            let report = graph.report();
            assert_eq!(
                declarations(graph, "gin.go"),
                [
                    "type_alias HandlerFunc",
                    "type_alias HandlersChain",
                    "struct Context",
                    "type_alias WatchChan",
                    "struct WatchResponse",
                    "type_alias Render",
                    "type_alias Lookup",
                    "type_alias Grid",
                    "type_alias Pipeline",
                    "type_alias Wrapped",
                    "type_alias Named",
                    "type_alias Tables",
                    "type_alias Generic",
                    "type_alias Pair",
                    "type_alias stateFn",
                    "type_alias Tree",
                    "struct List",
                    "struct Cell",
                    "type_alias Key",
                    "type_alias Dur",
                    "type_alias Mode",
                    "struct Lexer",
                ],
                "{label}\n{report}"
            );
            assert_eq!(
                type_refs(graph, "gin.go"),
                [
                    "Generic List",
                    "Grid Cell",
                    "HandlerFunc Context",
                    "HandlersChain HandlerFunc",
                    "Lookup Key",
                    "Lookup WatchResponse",
                    "Named Cell",
                    "Pair List",
                    "Pipeline Cell",
                    "Pipeline Context",
                    "Pipeline Key",
                    "Render Render",
                    "Tables Cell",
                    "Tables Key",
                    "WatchChan WatchResponse",
                    "Wrapped Cell",
                    "stateFn Lexer",
                ],
                "{label}\n{report}"
            );
            assert_type_refs_on_their_names(graph, "gin.go", &text);
            // `render.Render` is written through its package: the reference
            // sits on the second `Render`.
            let render = &node_in(graph, "gin.go", "Render").id;
            let lines: Vec<&str> = text.lines().collect();
            let before: Vec<String> = graph
                .unresolved
                .iter()
                .filter(|r| &r.from_node_id == render && r.reference_name == "Render")
                .map(|r| {
                    let line = lines[usize::try_from(r.line - 1).unwrap()];
                    line[..usize::try_from(r.col).unwrap()]
                        .trim_start()
                        .to_string()
                })
                .collect();
            assert_eq!(before, ["Render   render."], "{label}\n{report}");
        }
    }

    /// Upstream #2441: an indexed module links its defined types — a bare
    /// name is its own package's type, `pb.PutResponse` the import's, a name
    /// written through a package the index does not know links to nothing,
    /// a recursive type has no edge or row for its own name, a defined type
    /// has none of its underlying type's methods, and impact on a type
    /// reaches the defined types built from it. Upstream's fixture, LF and
    /// CRLF.
    #[test]
    fn pr2441_an_indexed_go_module_links_its_defined_types() {
        let module: [(&str, &str); 6] = [
            ("go.mod", "module go.etcd.io/etcd\n\ngo 1.24\n"),
            (
                "api/v3/etcdserverpb/rpc.pb.go",
                "package etcdserverpb\n\ntype PutResponse struct {\n\tHeader *ResponseHeader\n}\n\ntype ResponseHeader struct{}\n\nfunc (m *PutResponse) GetHeader() *ResponseHeader { return m.Header }\n",
            ),
            (
                "alpha/names.go",
                "package alpha\n\ntype PutResponse struct{}\n\nfunc (m *PutResponse) GetHeader() int { return 0 }\n\ntype WatchResponse struct{}\n\ntype Context struct{}\n\ntype Op struct{}\n\ntype watcher struct{}\n",
            ),
            (
                "client/v3/watch.go",
                "package clientv3\n\nimport pb \"go.etcd.io/etcd/api/v3/etcdserverpb\"\n\ntype PutResponse pb.PutResponse\n\ntype WatchChan <-chan WatchResponse\n\ntype WatchResponse struct {\n\tCanceled bool\n}\n\ntype Op struct{}\n\ntype watcher struct{}\n\nfunc (w *watcher) Watch() WatchChan { return nil }\n\nfunc header(r *PutResponse) { r.GetHeader() }\n\ntype stateFn func(*watcher) stateFn\n",
            ),
            (
                "client/v3/ordering/util.go",
                "package ordering\n\nimport \"go.etcd.io/etcd/client/v3\"\n\ntype OrderViolationFunc func(op clientv3.Op) error\n",
            ),
            (
                "gin/gin.go",
                "package gin\n\ntype HandlerFunc func(*Context)\n\ntype HandlersChain []HandlerFunc\n\ntype Context struct {\n\thandlers HandlersChain\n}\n\ntype Engine struct{}\n\nfunc (engine *Engine) Use(middleware ...HandlerFunc) {}\n",
            ),
        ];
        for crlf in [false, true] {
            let label = if crlf { "CRLF" } else { "LF" };
            let project = module
                .iter()
                .fold(Project::new(), |project, (path, text)| {
                    let text = if crlf {
                        text.replace('\n', "\r\n")
                    } else {
                        (*text).to_string()
                    };
                    project.file(path, &text)
                })
                .index();
            let graph = project.graph();
            let report = graph.report();
            let links = |file: &str, name: &str| links_from(graph, file, name);
            assert_eq!(
                links("client/v3/watch.go", "WatchChan"),
                ["references client/v3/watch.go:WatchResponse"],
                "{label}\n{report}"
            );
            assert_eq!(
                links("gin/gin.go", "HandlerFunc"),
                ["references gin/gin.go:Context"],
                "{label}"
            );
            assert_eq!(
                links("gin/gin.go", "HandlersChain"),
                ["references gin/gin.go:HandlerFunc"],
                "{label}"
            );
            assert_eq!(
                links("client/v3/watch.go", "PutResponse"),
                ["references api/v3/etcdserverpb/rpc.pb.go:PutResponse"],
                "{label}\n{report}"
            );
            assert_eq!(
                links("client/v3/watch.go", "stateFn"),
                ["references client/v3/watch.go:watcher"],
                "{label}"
            );
            assert!(
                unresolved_from(graph, "client/v3/watch.go", "stateFn").is_empty(),
                "{label}"
            );
            assert!(
                links("client/v3/ordering/util.go", "OrderViolationFunc").is_empty(),
                "{label}"
            );
            assert_eq!(
                unresolved_from(graph, "client/v3/ordering/util.go", "OrderViolationFunc"),
                ["references Op"],
                "{label}"
            );
            // Go has no `GetHeader` on a type defined from pb.PutResponse.
            assert_eq!(
                links("client/v3/watch.go", "header"),
                ["references client/v3/watch.go:PutResponse"],
                "{label}\n{report}"
            );
            let impact = |file: &str, name: &str| -> Vec<String> {
                let output = codegraph(
                    project.root(),
                    [
                        "impact", name, "-p", ".", "--depth", "3", "--file", file, "--json",
                    ],
                );
                let value: serde_json::Value =
                    serde_json::from_slice(&output.stdout).expect("impact --json output");
                let mut out: Vec<String> = value["definitions"]
                    .as_array()
                    .expect("impact definitions")
                    .iter()
                    .flat_map(|d| d["affected"].as_array().expect("affected").iter())
                    .filter_map(|a| graph.node(a["id"].as_str()?))
                    .map(|n| format!("{}:{}", n.file_path, n.qualified_name))
                    .collect();
                out.sort();
                out.dedup();
                out
            };
            assert_eq!(
                impact("client/v3/watch.go", "WatchResponse"),
                [
                    "client/v3/watch.go:WatchChan",
                    "client/v3/watch.go:WatchResponse",
                    "client/v3/watch.go:watcher::Watch",
                ],
                "{label}"
            );
            assert_eq!(
                impact("gin/gin.go", "Context"),
                [
                    "gin/gin.go:Context",
                    "gin/gin.go:Engine::Use",
                    "gin/gin.go:HandlerFunc",
                    "gin/gin.go:HandlersChain",
                ],
                "{label}"
            );
        }
    }

    /// Upstream #2233: a Go name written through an import from outside the
    /// module (`context.Context`, testify's `require.Contains`) is that
    /// package's, never a project symbol of its name, not even the same
    /// file's method; a name through one of the module's own packages still
    /// resolves. Upstream's fixture.
    #[test]
    fn pr2233_a_go_name_through_an_outside_package_is_never_a_project_symbol() {
        let project = Project::new()
            .file("go.mod", "module github.com/gofiber/fiber/v3\n\ngo 1.22\n")
            .file(
                "middleware/sse/sse.go",
                "package sse\n\nimport (\n\t\"context\"\n\n\t\"github.com/gofiber/fiber/v3/extractors\"\n)\n\ntype Stream struct{}\n\nfunc (s *Stream) Context() context.Context {\n\treturn nil\n}\n\nfunc run(ctx context.Context) bool {\n\treturn extractors.Contains(\"a\", \"b\")\n}\n",
            )
            .file(
                "extractors/extractors.go",
                "package extractors\n\nfunc Contains(haystack, needle string) bool {\n\treturn true\n}\n",
            )
            .file(
                "middleware/sse/sse_test.go",
                "package sse\n\nimport (\n\t\"testing\"\n\n\t\"github.com/stretchr/testify/require\"\n)\n\nfunc TestStream(t *testing.T) {\n\trequire.Contains(t, \"abc\", \"a\")\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let targets_from = |file: &str| -> Vec<String> {
            graph
                .edges
                .iter()
                .filter(|e| e.kind != "contains")
                .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
                .filter_map(|e| graph.node(&e.target))
                .map(|n| n.qualified_name.clone())
                .collect()
        };
        let sse = targets_from("middleware/sse/sse.go");
        assert!(!sse.contains(&"Stream::Context".to_string()), "{report}");
        assert!(
            !targets_from("middleware/sse/sse_test.go").contains(&"Contains".to_string()),
            "{report}"
        );
        assert!(sse.contains(&"Contains".to_string()), "{report}");
    }

    /// Upstream #2448's fixture (`go-ref-qualifier.test.ts`): project
    /// methods named like outside packages' functions, and locals and
    /// parameters named like imports.
    const GO_REF_QUALIFIER: [(&str, &str); 17] = [
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "logging/wrapper.go",
            "package logging\n\n// An adapter with klog's method names, like kubernetes' etcd3 logger.\ntype klogWrapper struct{}\n\nfunc (klogWrapper) Infof(format string, args ...interface{})  {}\nfunc (klogWrapper) Errorf(format string, args ...interface{}) {}\n",
        ),
        (
            "registry/suite.go",
            "package registry\n\ntype Suite struct{}\n\nfunc (s *Suite) Digest(v string) string { return v }\n",
        ),
        (
            "diff/differ.go",
            "package diff\n\ntype Differ struct{}\n\nfunc (d *Differ) Diff(a, b string) string { return a + b }\n",
        ),
        (
            "example/example.go",
            "package example\n\nimport (\n\t\"errors\"\n\t\"testing\"\n\n\t\"github.com/distribution/distribution/v3\"\n\t\"github.com/google/go-cmp/cmp\"\n\t\"github.com/opencontainers/go-digest\"\n\t\"github.com/patrickmn/go-cache\"\n\t\"k8s.io/klog/v2\"\n)\n\ntype parseError struct{}\n\nfunc (e *parseError) Error() string { return \"parse\" }\n\ntype lru struct{}\n\nfunc (l *lru) Get(key string) string { return key }\n\ntype Server struct {\n\tcache *lru\n}\n\nfunc (s *Server) Lookup(key string) string {\n\treturn s.cache.Get(key)\n}\n\nfunc NewServer() *Server {\n\t_ = cache.New(0, 0)\n\treturn &Server{cache: &lru{}}\n}\n\nfunc Run(dig string) {\n\tklog.Infof(\"Log using Infof, key: %s\", \"value\")\n\terr := errors.New(\"fail\")\n\tklog.Errorf(\"Log using Errorf, err: %v\", err)\n\tdesc := &distribution.Descriptor{Digest: digest.Digest(dig)}\n\t_ = desc\n\tperr := &parseError{}\n\tklog.Error(perr.Error())\n}\n\nfunc TestParser(t *testing.T) {\n\tt.Errorf(\"Unexpected policy! Diff:\\n%s\", cmp.Diff(\"a\", \"b\"))\n}\n",
        ),
        (
            "auth/jwt.go",
            "package auth\n\nimport \"github.com/golang-jwt/jwt/v5\"\n\ntype tokenJWT struct{ key string }\n\nfunc newTokenProviderJWT(key string) (*tokenJWT, error) { return &tokenJWT{key: key}, nil }\n\nfunc (t *tokenJWT) assign(user string) (string, error) { return user, nil }\n\nfunc (t *tokenJWT) info(token string) (string, bool) { return token, true }\n\n// Named like the package's Parse.\ntype jwtOptions struct{}\n\nfunc (o *jwtOptions) Parse(token string) error { return nil }\n\nfunc parse(token string) {\n\t_, _ = jwt.Parse(token, nil)\n}\n",
        ),
        (
            "auth/jwt_test.go",
            "package auth\n\nimport (\n\t\"testing\"\n\n\t\"github.com/golang-jwt/jwt/v5\"\n)\n\nfunc TestJWTInfo(t *testing.T) {\n\tjwt, err := newTokenProviderJWT(\"key\")\n\tif err != nil {\n\t\tt.Fatal(err)\n\t}\n\ttoken, _ := jwt.assign(\"abc\")\n\t_, ok := jwt.info(token)\n\t_ = ok\n}\n\nfunc TestJWTParse(t *testing.T) {\n\tjwt, err := jwt.Parse(\"x\", nil)\n\t_, _ = jwt, err\n}\n\nfunc TestJWTClosure(t *testing.T) {\n\tcheck := func(jwt *tokenJWT) {\n\t\tjwt.info(\"t\")\n\t}\n\tcheck(nil)\n\t_, _ = jwt.Parse(\"y\", nil)\n}\n",
        ),
        (
            "compare/compare.go",
            "package compare\n\nimport \"github.com/google/go-cmp/cmp\"\n\ntype Comparer interface {\n\tCompare(a, b int) int\n}\n\nfunc comparers() []Comparer { return nil }\n\nfunc Check(cmp Comparer) int {\n\treturn cmp.Compare(1, 2)\n}\n\nfunc Report(a, b string) string {\n\tfor _, cmp := range comparers() {\n\t\t_ = cmp.Compare(3, 4)\n\t}\n\treturn cmp.Diff(a, b)\n}\n\nfunc Report2(a, b string) string {\n\tcmp := cmp.Diff(a, b)\n\treturn cmp\n}\n",
        ),
        (
            "clock/clock.go",
            "package clock\n\nimport \"time\"\n\n// Named like k8s.io/utils/clock's interface.\ntype Clock interface {\n\tNow() time.Time\n}\n",
        ),
        (
            "cache/store.go",
            "package cache\n\ntype Store interface {\n\tAdd(obj interface{}) error\n}\n",
        ),
        (
            "jobs/logger/logger.go",
            "package logger\n\ntype Interface interface {\n\tError(v ...interface{})\n\tString() string\n}\n\nfunc Error(v ...interface{}) {}\n",
        ),
        (
            "jobs/transfer/transfer.go",
            "package transfer\n\ntype Logger struct{}\n\nfunc (l *Logger) Error(v ...interface{}) {}\n",
        ),
        (
            "jobs/job.go",
            "package jobs\n\nimport (\n\t\"sync/atomic\"\n\t\"time\"\n\n\t\"example.com/app/cache\"\n\t\"example.com/app/jobs/logger\"\n\t\"k8s.io/utils/clock\"\n\tclocktesting \"k8s.io/utils/clock/testing\"\n)\n\nfunc logError(logger logger.Interface, err error) {\n\tlogger.Error(err)\n}\n\nfunc nextCheck(clock clock.PassiveClock) int64 {\n\treturn clock.Now().Unix()\n}\n\nfunc cachedHasSynced(store cache.Store) func() bool {\n\tcache := &atomic.Bool{}\n\tcache.Store(false)\n\treturn cache.Load\n}\n\nfunc describe(logger logger.Interface) string {\n\treturn logger.Kind.String()\n}\n\nfunc elapsed() time.Time {\n\tclock := clocktesting.NewFakePassiveClock(time.Unix(1, 0))\n\tnow := clock.Now()\n\treturn now\n}\n",
        ),
        (
            "robot/model/model.go",
            "package model\n\ntype Robot struct{ Name string }\n\nfunc (r *Robot) ToJSON() (string, error) { return r.Name, nil }\n",
        ),
        (
            "robot/robot.go",
            "package robot\n\nimport \"example.com/app/robot/model\"\n\ntype Robot struct {\n\tmodel.Robot\n\tLevel string\n}\n",
        ),
        (
            "scan/job_test.go",
            "package scan\n\nimport (\n\t\"testing\"\n\n\t\"example.com/app/robot\"\n)\n\nfunc TestJob(t *testing.T) {\n\trobot := &robot.Robot{Level: \"system\"}\n\tdata, _ := robot.ToJSON()\n\t_ = data\n}\n",
        ),
        (
            "store/store.go",
            "package store\n\nimport \"github.com/patrickmn/go-cache\"\n\ntype store struct{}\n\nfunc (cache *store) Refresh() {\n\tcache.load()\n}\n\nfunc (s *store) load() {}\n\nfunc New() *cache.Cache {\n\treturn cache.New(0, 0)\n}\n",
        ),
    ];

    /// Lines beside upstream's: chi's `r.Use(middleware.Logger)` (a method
    /// value recorded at its member), etcd's `(*v3.DeleteResponse)(resp)` (a
    /// conversion recorded at its parenthesis), etcd's `zap.String("…",
    /// p.id.String())`,
    /// chi's `Chain(…).Handler(h)`, and a field of a call's result named like
    /// an outside import.
    const GO_REF_QUALIFIER_EXTRA: [(&str, &str); 6] = [
        (
            "app/convert.go",
            "package app\n\nimport \"example.com/app/mw\"\n\nfunc convert(r any) {\n\t_ = (*mw.Response)(nil)\n}\n",
        ),
        (
            "mw/logger.go",
            "package mw\n\nfunc Logger(next func()) func() { return next }\n\ntype Response struct{}\n",
        ),
        (
            "app/router.go",
            "package app\n\nimport \"example.com/app/mw\"\n\ntype router struct{}\n\nfunc (r *router) Use(f func(func()) func()) {}\n\nfunc route(r *router) {\n\tr.Use(mw.Logger)\n}\n",
        ),
        (
            "rafthttp/peer.go",
            "package rafthttp\n\nimport \"go.uber.org/zap\"\n\ntype ID uint64\n\nfunc (i ID) String() string { return \"\" }\n\ntype peer struct {\n\tid ID\n}\n\nfunc (p *peer) send(lg *zap.Logger) {\n\tlg.Info(\"sent\", zap.String(\"remote-peer-id\", p.id.String()))\n}\n",
        ),
        (
            "chain/chain.go",
            "package chain\n\nimport \"net/http\"\n\ntype Middlewares []func(http.Handler) http.Handler\n\nfunc Chain(middlewares ...func(http.Handler) http.Handler) Middlewares { return middlewares }\n\nfunc (mws Middlewares) Handler(h http.Handler) http.Handler { return h }\n\nfunc mount(h http.Handler) http.Handler {\n\treturn Chain(func(next http.Handler) http.Handler { return next }).Handler(h)\n}\n",
        ),
        (
            "app/start.go",
            "package app\n\nimport \"github.com/acme/bar\"\n\ntype barRunner struct{}\n\nfunc (b *barRunner) Run() {}\n\ntype outer struct {\n\tbar *barRunner\n}\n\nfunc makeOuter() *outer { return &outer{} }\n\nfunc start() {\n\t_ = bar.Version\n\tmakeOuter().bar.Run()\n}\n",
        ),
    ];

    /// What the edges out of `file`'s nodes on the `nth` line holding
    /// `snippet` reach, as qualified names (upstream's `reachedAt`).
    fn reached_at(graph: &Graph, file: &str, snippet: &str, nth: usize) -> Vec<String> {
        let text = GO_REF_QUALIFIER
            .iter()
            .chain(GO_REF_QUALIFIER_EXTRA.iter())
            .find(|(path, _)| *path == file)
            .map(|(_, text)| *text)
            .unwrap_or_else(|| panic!("no {file}"));
        let line = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(snippet))
            .nth(nth - 1)
            .map(|(index, _)| i64::try_from(index + 1).unwrap())
            .unwrap_or_else(|| panic!("no {snippet} in {file}"));
        graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains" && e.line == Some(line))
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect()
    }

    fn go_ref_qualifier_project() -> super::support::Indexed {
        GO_REF_QUALIFIER
            .iter()
            .chain(GO_REF_QUALIFIER_EXTRA.iter())
            .fold(Project::new(), |project, (path, text)| {
                project.file(path, text)
            })
            .index()
    }

    /// Upstream #2448's first half: a dotted Go call is written through its
    /// own operand, the only part that can be a package, whatever else its
    /// line spells — neither a second spelling of the name (`"Log using
    /// Infof"`, `Digest:`, `Diff:`) nor another selector before it
    /// (`klog.Error(perr.Error())`). Upstream's fixture, with etcd's, chi's
    /// and a call result's field's lines beside it.
    #[test]
    fn pr2448_a_dotted_go_call_is_written_through_its_own_receiver() {
        let project = go_ref_qualifier_project();
        let graph = project.graph();
        let report = graph.report();
        let at = |file: &str, snippet: &str| reached_at(graph, file, snippet, 1);
        let ex = "example/example.go";
        assert!(
            !at(ex, "klog.Infof(").contains(&"klogWrapper::Infof".to_string()),
            "{report}"
        );
        assert!(
            !at(ex, "klog.Errorf(").contains(&"klogWrapper::Errorf".to_string()),
            "{report}"
        );
        assert!(
            !at(ex, "digest.Digest(dig)").contains(&"Suite::Digest".to_string()),
            "{report}"
        );
        assert!(
            !at(ex, "cmp.Diff(\"a\", \"b\")").contains(&"Differ::Diff".to_string()),
            "{report}"
        );
        assert!(
            at(ex, "klog.Error(perr.Error())").contains(&"parseError::Error".to_string()),
            "{report}"
        );
        assert!(
            at(ex, "s.cache.Get(key)").contains(&"lru::Get".to_string()),
            "{report}"
        );
        assert_eq!(
            at("rafthttp/peer.go", "p.id.String()"),
            ["ID::String"],
            "{report}"
        );
        // A conversion through a project package, recorded at its parenthesis.
        assert!(
            at("app/convert.go", "(*mw.Response)(nil)").contains(&"Response".to_string()),
            "{report}"
        );
        // A value written through a project package, recorded at its member.
        assert!(
            at("app/router.go", "r.Use(mw.Logger)").contains(&"Logger".to_string()),
            "{report}"
        );
        assert!(
            at("chain/chain.go", "}).Handler(h)").contains(&"Middlewares::Handler".to_string()),
            "{report}"
        );
        assert!(
            at("app/start.go", "makeOuter().bar.Run()").contains(&"barRunner::Run".to_string()),
            "{report}"
        );
    }

    /// Upstream #2448's second half: a parameter or local named like an
    /// import is that variable where it is in scope — a closure's parameter,
    /// a function's, a range variable, a method receiver — and the package
    /// everywhere else (another function, the declaration's own right-hand
    /// side, past a closure or a loop that bound the name). Its declared type
    /// says whose methods it has: an outside package's type or a value an
    /// outside function hands out has none of the project's, a project
    /// package's type its own (an embedded type's method too), and a field
    /// read through the local keeps its own type. Upstream's fixture, less
    /// the untyped range variable's guessed callee (KEEP-RUST OD-8).
    #[test]
    fn pr2448_a_go_local_named_like_an_import_is_that_variable() {
        let project = go_ref_qualifier_project();
        let graph = project.graph();
        let report = graph.report();
        let at = |file: &str, snippet: &str| reached_at(graph, file, snippet, 1);
        let has = |reached: Vec<String>, target: &str| reached.contains(&target.to_string());
        assert!(
            has(at("auth/jwt_test.go", "jwt.assign("), "tokenJWT::assign"),
            "{report}"
        );
        assert!(
            has(at("auth/jwt_test.go", "jwt.info(token)"), "tokenJWT::info"),
            "{report}"
        );
        assert!(
            has(at("auth/jwt_test.go", "jwt.info(\"t\")"), "tokenJWT::info"),
            "{report}"
        );
        assert!(
            has(
                at("compare/compare.go", "cmp.Compare(1, 2)"),
                "Comparer::Compare"
            ),
            "{report}"
        );
        // A range variable is that variable too (`go_scope`'s unit tests), but
        // nothing types it: upstream's lone-candidate guess takes
        // `Comparer::Compare`, which the receiver-word gate keeps from `cmp`
        // here (#226, KEEP-RUST OD-8).
        assert!(
            has(at("store/store.go", "cache.load()"), "store::load"),
            "{report}"
        );
        assert!(
            !has(
                at("auth/jwt.go", "jwt.Parse(token, nil)"),
                "jwtOptions::Parse"
            ),
            "{report}"
        );
        assert!(
            !has(
                at("auth/jwt_test.go", "jwt.Parse(\"x\", nil)"),
                "jwtOptions::Parse"
            ),
            "{report}"
        );
        assert!(
            !has(
                at("auth/jwt_test.go", "jwt.Parse(\"y\", nil)"),
                "jwtOptions::Parse"
            ),
            "{report}"
        );
        assert!(
            !has(
                at("compare/compare.go", "return cmp.Diff(a, b)"),
                "Differ::Diff"
            ),
            "{report}"
        );
        assert!(
            !has(
                at("compare/compare.go", "cmp := cmp.Diff(a, b)"),
                "Differ::Diff"
            ),
            "{report}"
        );
        assert!(
            !has(at("jobs/job.go", "clock.Now()"), "Clock::Now"),
            "{report}"
        );
        assert!(
            at("jobs/job.go", "cache.Store(false)").is_empty(),
            "{report}"
        );
        assert!(
            at("jobs/job.go", "now := clock.Now()").is_empty(),
            "{report}"
        );
        assert_eq!(
            at("jobs/job.go", "logger.Error(err)"),
            ["Interface::Error"],
            "{report}"
        );
        assert_eq!(
            at("scan/job_test.go", "robot.ToJSON()"),
            ["Robot::ToJSON"],
            "{report}"
        );
        assert!(
            !has(
                at("jobs/job.go", "logger.Kind.String()"),
                "Interface::String"
            ),
            "{report}"
        );
    }

    /// Upstream #2448: the local is read from the file as a sync leaves it —
    /// the same lines either way, so a stale reading would still see it.
    #[test]
    fn pr2448_a_go_local_is_read_from_the_file_a_sync_leaves() {
        let jwt = GO_REF_QUALIFIER
            .iter()
            .find(|(path, _)| *path == "auth/jwt.go")
            .map(|(_, text)| *text)
            .unwrap();
        let use_file = |decl: &str| {
            format!(
                "package auth\nimport \"github.com/golang-jwt/jwt/v5\"\nfunc use() {{\n\t{decl}\n\tjwt.assign(\"user\")\n}}\n"
            )
        };
        let mut project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file("auth/jwt.go", jwt)
            .file(
                "auth/use.go",
                &use_file("jwt, _ := newTokenProviderJWT(\"key\")"),
            )
            .index();
        let assigns = |graph: &Graph| -> Vec<String> {
            graph
                .edges
                .iter()
                .filter(|e| e.kind != "contains" && e.line == Some(5))
                .filter(|e| {
                    graph
                        .node(&e.source)
                        .is_some_and(|n| n.file_path == "auth/use.go")
                })
                .filter_map(|e| graph.node(&e.target))
                .map(|n| n.qualified_name.clone())
                .collect()
        };
        assert_eq!(
            assigns(project.graph()),
            ["tokenJWT::assign"],
            "{}",
            project.graph().report()
        );
        project.write("auth/use.go", &use_file("_, _ = jwt.Parse(\"key\", nil)"));
        let graph = project.sync();
        assert!(assigns(graph).is_empty(), "{}", graph.report());
    }

    /// `kind qualifiedName@file` of each edge but `contains` out of the
    /// nodes of `file`, sorted (upstream's `targetsFrom`).
    fn targets_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target).map(|t| (e.kind.as_str(), t)))
            .map(|(kind, t)| format!("{kind} {}@{}", t.qualified_name, t.file_path))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2410: a name written through an unaliased versioned or `go-`
    /// import (`yaml.Node`, `klog.V(2)`, `semver.Version`, `sqlite3.Error`) is
    /// that package's, never a project symbol sharing the name, and a project
    /// import whose path ends in a major version (`example.com/kit/v2`, package
    /// `kit`) or names a package `v1` still resolves. Upstream's fixture.
    #[test]
    fn pr2410_go_references_through_a_versioned_or_go_import() {
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file(
                "other/other.go",
                "package other\n\ntype Node struct{}\n\ntype Version struct{}\n\ntype Error struct{}\n\nfunc Unmarshal(b []byte, v any) error { return nil }\n\ntype klogWrapper struct{}\n\nfunc (k *klogWrapper) V(level int) bool { return true }\n\nfunc (k *klogWrapper) Infof(format string, args ...any) {}\n",
            )
            .file(
                "store/store.go",
                "package store\n\nimport (\n\t\"github.com/Masterminds/semver/v3\"\n\t\"github.com/mattn/go-sqlite3\"\n\t\"go.yaml.in/yaml/v3\"\n\t\"k8s.io/klog/v2\"\n)\n\nfunc Load(b []byte, root *yaml.Node, min *semver.Version) error {\n\tif klog.V(2) {\n\t\tklog.Infof(\"loading %d bytes\", len(b))\n\t}\n\tvar e sqlite3.Error\n\t_ = e\n\treturn yaml.Unmarshal(b, root)\n}\n",
            )
            .file("kit/go.mod", "module example.com/kit/v2\n\ngo 1.22\n")
            .file(
                "kit/kit.go",
                "package kit\n\ntype Kit struct{}\n\nfunc New() *Kit { return &Kit{} }\n",
            )
            .file(
                "decoy/decoy.go",
                "package decoy\n\ntype Kit struct{}\n\nfunc New() *Kit { return &Kit{} }\n\nfunc Find(id string) string { return id }\n",
            )
            .file("api/core/v1/types.go", "package v1\n\ntype Pod struct{}\n")
            .file("pkg/apis/core/types.go", "package core\n\ntype Pod struct{}\n")
            .file(
                "repo/repo.go",
                "package repo\n\nfunc Find(id string) string { return id }\n",
            )
            .file(
                "app/app.go",
                "package app\n\nimport (\n\t\"fmt\" // for printing\n\t\"example.com/app/repo\"\n\t\"example.com/app/api/core/v1\"\n\t\"example.com/app/pkg/apis/core\"\n\t\"example.com/kit/v2\"\n)\n\nfunc Run(p *v1.Pod, q *core.Pod) *kit.Kit {\n\tfmt.Println(repo.Find(\"x\"))\n\treturn kit.New()\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let store: Vec<String> = targets_from(graph, "store/store.go")
            .into_iter()
            .filter(|t| t.contains("@other/"))
            .collect();
        assert!(store.is_empty(), "{store:?}\n{report}");
        assert_eq!(
            targets_from(graph, "app/app.go"),
            [
                "calls Find@repo/repo.go",
                "calls New@kit/kit.go",
                "references Kit@kit/kit.go",
                "references Pod@api/core/v1/types.go",
                "references Pod@pkg/apis/core/types.go",
            ],
            "{report}"
        );
    }

    /// Upstream #2397 with #2410: an embedded interface or struct links the
    /// type its package declares — a bare name its own package's, `pkg.T` the
    /// import's — and nothing for a type from outside the project (`io.Closer`,
    /// `sync.Mutex`, `yaml.Node` under `go.yaml.in/yaml/v3`) or through a
    /// package none of the file's imports is named (`clientv3.KV`), whose
    /// references stay unresolved. Upstream's fixture, less its viewer half.
    #[test]
    fn pr2397_an_indexed_go_module_links_embedded_types_in_their_package() {
        let gin = "package gin\n\ntype HandlerFunc func(*Context)\n\ntype IRoutes interface {\n\tUse(...HandlerFunc) IRoutes\n\tHandle(string, string, ...HandlerFunc) IRoutes\n}\n\ntype IRouter interface {\n\tIRoutes\n\tGroup(string, ...HandlerFunc) *RouterGroup\n}\n";
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file(
                "storage/storage.go",
                "package storage\n\ntype LabelQuerier interface {\n\tLabelValues(name string) []string\n}\n\ntype Base struct{}\n\nfunc (b *Base) Close() error { return nil }\n",
            )
            .file(
                "other/other.go",
                "package other\n\ntype Closer interface{ Close() error }\n\ntype Mutex struct{}\n\ntype Reader interface{ Read(p []byte) (int, error) }\n\ntype Node struct{}\n\ntype KV interface{ Get(key string) string }\n",
            )
            .file(
                "store/store.go",
                "package store\n\nimport (\n\t\"io\"\n\t\"sync\"\n\n\t\"example.com/app/storage\"\n\t\"go.etcd.io/etcd/client/v3\"\n\t\"go.yaml.in/yaml/v3\"\n)\n\n// The package is yaml, though its import path ends in v3.\ntype RuleGroupNode struct {\n\tyaml.Node\n\tName string\n}\n\n// The package is clientv3: not v3, nor the client goimports would assume.\ntype kvPrefix struct {\n\tclientv3.KV\n}\n\ntype Queryable interface{ Q() }\n\ntype Querier interface {\n\tQueryable\n\tio.Closer\n\tstorage.LabelQuerier\n\tSelect(sorted bool) int\n}\n\ntype Number interface {\n\t~int | ~float64\n}\n\ntype Engine struct{}\n\ntype Reader interface{ Read() }\n\ntype List[T any] struct{ items []T }\n\ntype Store struct {\n\t*Engine\n\tReader\n\tsync.Mutex\n\t*storage.Base\n\tList[int]\n}\n",
            )
            .file(
                "wrap/wrap.go",
                "package wrap\n\nimport \"io\"\n\ntype Reader interface {\n\tio.Reader\n\tPeek() byte\n}\n",
            )
            .file("gin/routergroup.go", gin)
            .file(
                "parser/ast.go",
                "package parser\n\ntype Node interface{ String() string }\n\ntype Expr interface {\n\tNode\n\tType() string\n}\n\ntype exprWrapper struct {\n\tNode\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let sources: Vec<&str> = [
            ("Querier", "store/store.go"),
            ("Number", "store/store.go"),
            ("Store", "store/store.go"),
            ("RuleGroupNode", "store/store.go"),
            ("kvPrefix", "store/store.go"),
            ("Reader", "wrap/wrap.go"),
            ("IRouter", "gin/routergroup.go"),
            ("Expr", "parser/ast.go"),
            ("exprWrapper", "parser/ast.go"),
        ]
        .iter()
        .map(|(name, file)| node_in(graph, file, name).id.as_str())
        .collect();
        let mut supertypes: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "extends" | "implements"))
            .filter(|e| e.provenance.as_deref() != Some("heuristic"))
            .filter(|e| sources.contains(&e.source.as_str()))
            .filter_map(|e| Some((graph.node(&e.source)?, e, graph.node(&e.target)?)))
            .map(|(s, e, t)| format!("{} {} {}@{}", s.name, e.kind, t.name, t.file_path))
            .collect();
        supertypes.sort();
        assert_eq!(
            supertypes,
            [
                "Expr extends Node@parser/ast.go",
                "IRouter extends IRoutes@gin/routergroup.go",
                "Querier extends LabelQuerier@storage/storage.go",
                "Querier extends Queryable@store/store.go",
                "Store extends Base@storage/storage.go",
                "Store extends Engine@store/store.go",
                "Store extends List@store/store.go",
                "Store implements Reader@store/store.go",
                "exprWrapper implements Node@parser/ast.go",
            ],
            "{report}"
        );
        let unresolved = |file: &str, name: &str| -> Vec<String> {
            let id = &node_in(graph, file, name).id;
            let mut out: Vec<String> = graph
                .unresolved
                .iter()
                .filter(|r| &r.from_node_id == id && r.reference_kind == "extends")
                .map(|r| r.reference_name.clone())
                .collect();
            out.sort();
            out
        };
        assert_eq!(unresolved("store/store.go", "Querier"), ["Closer"]);
        assert_eq!(unresolved("store/store.go", "Store"), ["Mutex"]);
        assert_eq!(unresolved("store/store.go", "RuleGroupNode"), ["Node"]);
        assert_eq!(unresolved("store/store.go", "kvPrefix"), ["KV"]);
        assert_eq!(unresolved("wrap/wrap.go", "Reader"), ["Reader"]);
        let other: Vec<&str> = ["Closer", "Mutex", "Reader", "Node", "KV"]
            .iter()
            .map(|name| node_in(graph, "other/other.go", name).id.as_str())
            .collect();
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| matches!(e.kind.as_str(), "extends" | "implements")
                    && e.provenance.as_deref() != Some("heuristic")
                    && other.contains(&e.target.as_str())),
            "{report}"
        );
    }

    /// Upstream #2408: Go reads a name from one package only — a bare one
    /// from the reference's own, `pkg.Name` from that import's, `x[i].Name()`
    /// from whatever type `x[i]` has. A bare type is its own package's
    /// interface, a type through an outside import none of the project's, a
    /// method called through an expression no instantiation of a namesake
    /// struct, a result type its package's type and not the method the line
    /// declares, a variadic `...pkg.T` that import's type, and a conversion or
    /// a composite literal still its own package's struct. Upstream's fixture,
    /// less the route handler (a Go framework resolver's route: W7-19i) and
    /// `var _ error = (*Error)(nil)`'s conversion (a package-level `var`
    /// initializer's calls are not extracted here: pre-existing).
    #[test]
    fn pr2408_a_go_name_is_read_from_the_package_go_reads_it_from() {
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file(
                "promql/parser/ast.go",
                "package parser\n\ntype Node interface {\n\tString() string\n}\n\ntype Visitor interface {\n\tVisit(node Node, path []Node) (Visitor, error)\n}\n\nfunc Walk(v Visitor, node Node, path []Node) error {\n\treturn nil\n}\n",
            )
            .file(
                "discovery/kubernetes/node.go",
                "package kubernetes\n\nimport apiv1 \"k8s.io/api/core/v1\"\n\ntype Node struct {\n\tname string\n}\n\nfunc nodeSource(n *apiv1.Node) string {\n\treturn n.Name\n}\n",
            )
            .file(
                "storage/interface.go",
                "package storage\n\ntype Appender interface {\n\tAppend(ref uint64) error\n}\n\ntype Filter interface {\n\tAccept(value string) bool\n}\n",
            )
            .file(
                "storage/fanout.go",
                "package storage\n\ntype fanout struct{}\n\nfunc (f *fanout) Appender() Appender {\n\treturn nil\n}\n",
            )
            .file("discovery/aws/aws.go", "package aws\n\ntype Filter struct {\n\tName string\n}\n")
            .file(
                "web/api/v1/search.go",
                "package v1\n\nimport \"example.com/app/storage\"\n\ntype ChainFilter struct{}\n\nfunc NewChainFilter(filters ...storage.Filter) *ChainFilter {\n\treturn &ChainFilter{}\n}\n",
            )
            .file(
                "errors.go",
                "package gin\n\ntype Error struct {\n\tErr error\n}\n\nfunc (msg *Error) Error() string {\n\treturn msg.Err.Error()\n}\n\nvar _ error = (*Error)(nil)\n\nfunc newError(err error) *Error {\n\treturn &Error{Err: err}\n}\n",
            )
            .file(
                "binding/validator.go",
                "package binding\n\nimport \"strings\"\n\ntype SliceValidationError []error\n\nfunc (err SliceValidationError) Error() string {\n\tvar b strings.Builder\n\tfor i := range err {\n\t\tb.WriteString(err[i].Error())\n\t}\n\treturn b.String()\n}\n",
            )
            .file(
                "handler/handler.go",
                "package handler\n\ntype Handler struct{}\n\nfunc (h *Handler) Follow(c Context) error {\n\treturn nil\n}\n\ntype Context interface{}\n",
            )
            .file("model/user.go", "package model\n\ntype Follow struct {\n\tFollowerID uint\n}\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        // `kind file:qualifiedName` of each edge out of `file`'s nodes (named
        // `from` when given) to a target named `name` (upstream's `linksFrom`).
        let links = |file: &str, name: &str, from: Option<&str>| -> Vec<String> {
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|e| e.kind != "contains")
                .filter(|e| {
                    graph.node(&e.source).is_some_and(|n| {
                        n.file_path == file && from.is_none_or(|from| n.name == from)
                    })
                })
                .filter_map(|e| graph.node(&e.target).map(|t| (e.kind.as_str(), t)))
                .filter(|(_, t)| t.name == name)
                .map(|(kind, t)| format!("{kind} {}:{}", t.file_path, t.qualified_name))
                .collect();
            out.sort();
            out.dedup();
            out
        };
        assert_eq!(
            links("promql/parser/ast.go", "Node", None),
            ["references promql/parser/ast.go:Node"],
            "{report}"
        );
        assert!(
            links("discovery/kubernetes/node.go", "Node", None).is_empty(),
            "{report}"
        );
        let errors = links("binding/validator.go", "Error", None);
        assert!(
            !errors.iter().any(|l| l.starts_with("instantiates")),
            "{errors:?}"
        );
        assert!(
            !errors.contains(&"calls errors.go:Error".to_string()),
            "{errors:?}"
        );
        assert_eq!(
            links("storage/fanout.go", "Appender", None),
            ["references storage/interface.go:Appender"],
            "{report}"
        );
        assert_eq!(
            links("web/api/v1/search.go", "Filter", None),
            ["references storage/interface.go:Filter"],
            "{report}"
        );
        assert_eq!(
            links("errors.go", "Error", Some("newError")),
            ["instantiates errors.go:Error", "references errors.go:Error"],
            "{report}"
        );
    }

    /// Upstream #2416: a Go type position — a parameter or result type, a
    /// composite literal's type — names a type of the package Go reads it
    /// from, never a method or a function of that name; a generic receiver's
    /// `[T]` declares `T`. Upstream's fixture.
    #[test]
    fn pr2416_a_go_type_position_resolves_to_a_type() {
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.22\n")
            .file(
                "mvcc/key_index.go",
                "package mvcc\n\ntype keyIndex struct {\n\tkey []byte\n}\n\ntype revisions []int64\n",
            )
            .file(
                "mvcc/index.go",
                "package mvcc\n\ntype treeIndex struct {\n\ttree map[string]*keyIndex\n}\n\nfunc (ti *treeIndex) KeyIndex(keyi *keyIndex) *keyIndex {\n\treturn ti.keyIndex(keyi)\n}\n\nfunc (ti *treeIndex) keyIndex(keyi *keyIndex) *keyIndex {\n\treturn &keyIndex{key: keyi.key}\n}\n\nfunc (ti *treeIndex) revisions() revisions {\n\treturn revisions{1, 2}\n}\n",
            )
            .file(
                "mvcc/watchable_store.go",
                "package mvcc\n\ntype contains interface {\n\tcontains(rev int64) bool\n}\n\nfunc kvsToEvents(c contains, revs [][]byte) {}\n",
            )
            .file(
                "discovery/aws/ec2.go",
                "package aws\n\nimport \"context\"\n\ntype ec2Client interface {\n\tDescribeInstances(ctx context.Context) error\n}\n\ntype EC2Discovery struct {\n\tclient ec2Client\n}\n\nfunc (d *EC2Discovery) ec2Client(ctx context.Context) (ec2Client, error) {\n\treturn d.client, nil\n}\n",
            )
            .file(
                "util/zeropool/pool.go",
                "package zeropool\n\ntype Pool[T any] struct {\n\titems []T\n}\n\nfunc (p *Pool[T]) Get() T {\n\tvar zero T\n\treturn zero\n}\n\nfunc (p *Pool[T]) Put(item T) {}\n\ntype LazyLoader[K comparable, V any] struct{}\n\nfunc (l *LazyLoader[K, V]) Load(key K) (V, error) {\n\tvar v V\n\treturn v, nil\n}\n",
            )
            .file(
                "prompb/types.pb.go",
                "package prompb\n\ntype Histogram struct {\n\tCount isHistogram_Count\n}\n\ntype isHistogram_Count interface {\n\tisHistogram_Count()\n}\n\ntype Histogram_CountInt struct {\n\tCountInt uint64\n}\n\nfunc (*Histogram_CountInt) isHistogram_Count() {}\n\nfunc (m *Histogram) GetCount() isHistogram_Count {\n\treturn m.Count\n}\n",
            )
            .file(
                "prompb/codec.go",
                "package prompb\n\nfunc FromIntHistogram(count uint64) Histogram {\n\treturn Histogram{\n\t\tCount: &Histogram_CountInt{CountInt: count},\n\t}\n}\n",
            )
            .file(
                "prompb/io/prometheus/write/v2/types.pb.go",
                "package writev2\n\ntype Histogram_CountInt struct {\n\tCountInt uint64\n}\n\ntype Sample struct {\n\tts int64\n}\n\nfunc (s Sample) T() int64 { return s.ts }\n",
            )
            .file("model/value/value.go", "package value\n\ntype T struct{}\n\ntype K struct{}\n")
            .file(
                "storage/remote/client_test.go",
                "package remote\n\nimport (\n\t\"testing\"\n\n\tconfig_util \"github.com/prometheus/common/config\"\n)\n\ntype ClientConfig struct {\n\tURL *config_util.URL\n}\n\nfunc TestStoreHTTPErrorHandling(t *testing.T) {\n\tconf := &ClientConfig{\n\t\tURL: &config_util.URL{URL: nil},\n\t}\n\t_ = conf\n}\n",
            )
            .file(
                "scrape/target.go",
                "package scrape\n\ntype Target struct{}\n\nfunc (t *Target) URL() string { return \"\" }\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        // `kind file:qualifiedName` of each `references` or `instantiates`
        // edge out of `file`'s nodes (the one named `from` when given) to a
        // target named `name` (upstream's `typeLinksFrom`).
        let type_links = |file: &str, name: &str, from: Option<&str>| -> Vec<String> {
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|e| matches!(e.kind.as_str(), "references" | "instantiates"))
                .filter(|e| {
                    graph.node(&e.source).is_some_and(|n| {
                        n.file_path == file && from.is_none_or(|from| n.name == from)
                    })
                })
                .filter_map(|e| graph.node(&e.target).map(|t| (e.kind.as_str(), t)))
                .filter(|(_, t)| t.name == name)
                .map(|(kind, t)| format!("{kind} {}:{}", t.file_path, t.qualified_name))
                .collect();
            out.sort();
            out.dedup();
            out
        };
        assert_eq!(
            type_links("mvcc/index.go", "keyIndex", Some("KeyIndex")),
            ["references mvcc/key_index.go:keyIndex"],
            "{report}"
        );
        assert_eq!(
            type_links("mvcc/index.go", "keyIndex", Some("keyIndex")),
            [
                "instantiates mvcc/key_index.go:keyIndex",
                "references mvcc/key_index.go:keyIndex"
            ],
            "{report}"
        );
        let calls: Vec<&str> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "mvcc/index.go" && n.name == "KeyIndex")
            })
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.as_str())
            .collect();
        assert_eq!(calls, ["treeIndex::keyIndex"], "{report}");
        assert_eq!(
            type_links("mvcc/index.go", "revisions", Some("revisions")),
            [
                "instantiates mvcc/key_index.go:revisions",
                "references mvcc/key_index.go:revisions"
            ],
            "{report}"
        );
        assert_eq!(
            type_links("mvcc/watchable_store.go", "contains", Some("kvsToEvents")),
            ["references mvcc/watchable_store.go:contains"]
        );
        assert_eq!(
            type_links("prompb/types.pb.go", "isHistogram_Count", Some("GetCount")),
            ["references prompb/types.pb.go:isHistogram_Count"]
        );
        assert_eq!(
            type_links("discovery/aws/ec2.go", "ec2Client", Some("ec2Client")),
            ["references discovery/aws/ec2.go:ec2Client"],
            "{report}"
        );
        assert_eq!(
            type_links("prompb/codec.go", "Histogram_CountInt", None),
            ["instantiates prompb/types.pb.go:Histogram_CountInt"],
            "{report}"
        );
        for (name, from) in [("T", "Get"), ("T", "Put"), ("K", "Load"), ("V", "Load")] {
            assert!(
                type_links("util/zeropool/pool.go", name, Some(from)).is_empty(),
                "{name} from {from}\n{report}"
            );
        }
        assert!(
            type_links("storage/remote/client_test.go", "URL", None).is_empty(),
            "{report}"
        );
    }

    /// `qualifiedName` of every `calls` target out of the node named `name`
    /// in `file`, sorted.
    fn calls_from(graph: &Graph, file: &str, name: &str) -> Vec<String> {
        let id = &node_in(graph, file, name).id;
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls" && &e.source == id)
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2167 (its Go half) and #2177: a call on an expression
    /// arrives by its bare name, so its shape is read at the call. A
    /// standard-library method name chained on a receiver not named after a
    /// project type is the library's (cobra's `Flags().String(…)`, etcd's
    /// `c.GetLogger().Error(…)`, net/http's `w.Header().Get(…)`); a
    /// project-specific one keeps its match (`c.Root().Name()`); a chained
    /// call is never a package function, nor a bare call a method; a link on a
    /// later line is chained. Upstream's fixtures, with etcd's and cobra's
    /// lines beside them.
    #[test]
    fn pr2167_a_go_call_on_an_expression_is_read_by_its_shape() {
        let project = Project::new()
            .file("go.mod", "module example.com/app\n\ngo 1.21\n")
            .file(
                "flags.go",
                "package app\n\ntype customString struct{}\n\nfunc (c customString) String() string { return \"\" }\n\ntype FlagSet struct{}\n\nfunc Flags() *FlagSet { return nil }\n",
            )
            .file("cmd.go", "package app\n\nfunc run() {\n\tFlags().String(\"name\", \"\", \"\")\n}\n")
            .file(
                "context.go",
                "package gin\n\ntype Context struct{}\n\nfunc (c *Context) Get(key string) (any, bool) { return nil, false }\n\nfunc handler(w Writer) string {\n\treturn w.Header().Get(\"Content-Type\")\n}\n",
            )
            .file(
                "client/lease.go",
                "package client\n\ntype ErrKeepAliveHalted struct{}\n\nfunc (e ErrKeepAliveHalted) Error() string { return \"\" }\n\ntype Command struct{}\n\nfunc (c *Command) Name() string { return \"\" }\n\nfunc (c *Command) Root() *Command { return c }\n\nfunc helper() int { return 0 }\n\ntype Client struct{}\n\nfunc (c *Client) GetLogger() *Client { return c }\n\nfunc (c *Client) Info(msg string) {}\n",
            )
            .file(
                "client/retry.go",
                "package client\n\nfunc retry(c *Client, cmd *Command) {\n\tc.GetLogger().Error(\"getToken failed\")\n\t_ = cmd.Root().Name()\n\t_ = c.GetLogger().helper()\n\tc.GetLogger().\n\t\tInfo(\"done\")\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert!(
            !calls_from(graph, "cmd.go", "run")
                .iter()
                .any(|t| t.ends_with("customString::String")),
            "{report}"
        );
        assert!(
            !calls_from(graph, "context.go", "handler").contains(&"Context::Get".to_string()),
            "{report}"
        );
        let retry = calls_from(graph, "client/retry.go", "retry");
        assert!(
            !retry.contains(&"ErrKeepAliveHalted::Error".to_string()),
            "{retry:?}\n{report}"
        );
        assert!(
            retry.contains(&"Command::Name".to_string()),
            "{retry:?}\n{report}"
        );
        assert!(
            !retry.contains(&"helper".to_string()),
            "{retry:?}\n{report}"
        );
        assert!(
            retry.contains(&"Client::Info".to_string()),
            "{retry:?}\n{report}"
        );
    }

    /// The module upstream's #2444 resolution tests index (the gRPC handlers
    /// with CRLF line ends), with namesakes where name matching looked first.
    const ASSERTED_CALLS: [(&str, &str); 8] = [
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "pb/rpc_grpc.pb.go",
            "package pb\r\n\r\nimport \"context\"\r\n\r\ntype KVServer interface {\r\n\tRange(context.Context, *RangeRequest) (*RangeResponse, error)\r\n\tPut(context.Context, *RangeRequest) (*RangeResponse, error)\r\n}\r\n\r\ntype UnimplementedKVServer struct{}\r\n\r\nfunc (UnimplementedKVServer) Range(context.Context, *RangeRequest) (*RangeResponse, error) {\r\n\treturn nil, nil\r\n}\r\n\r\nfunc (UnimplementedKVServer) Put(context.Context, *RangeRequest) (*RangeResponse, error) {\r\n\treturn nil, nil\r\n}\r\n\r\ntype RangeRequest struct{}\r\n\r\ntype RangeResponse struct{}\r\n\r\nfunc _KV_Range_Handler(srv interface{}, ctx context.Context, dec func(interface{}) error, interceptor func(context.Context, interface{}, func(context.Context, interface{}) (interface{}, error)) (interface{}, error)) (interface{}, error) {\r\n\tin := new(RangeRequest)\r\n\tif err := dec(in); err != nil {\r\n\t\treturn nil, err\r\n\t}\r\n\tif interceptor == nil {\r\n\t\treturn srv.(KVServer).Range(ctx, in)\r\n\t}\r\n\thandler := func(ctx context.Context, req interface{}) (interface{}, error) {\r\n\t\treturn srv.(KVServer).Range(ctx, req.(*RangeRequest))\r\n\t}\r\n\treturn interceptor(ctx, in, handler)\r\n}\r\n\r\nfunc _KV_Put_Handler(srv interface{}, ctx context.Context, in *RangeRequest) (interface{}, error) {\r\n\treturn srv.(KVServer).\r\n\t\tPut(ctx, in)\r\n}\r\n",
        ),
        (
            "internal/bufconn/bufconn.go",
            "package bufconn\n\nimport \"io\"\n\ntype pipe struct{}\n\nfunc (p *pipe) Close() error { return nil }\n\nfunc (p *pipe) closeWrite() error { return nil }\n\nfunc (p *pipe) close() {}\n\ntype conn struct {\n\tio.Reader\n\tio.Writer\n}\n\nfunc (c *conn) Close() error {\n\terr1 := c.Reader.(*pipe).Close()\n\terr2 := c.Writer.(*pipe).closeWrite()\n\tc.Reader.(*pipe).close()\n\tif err1 != nil {\n\t\treturn err1\n\t}\n\treturn err2\n}\n",
        ),
        (
            "alpha/alpha.go",
            "package alpha\n\ntype Store interface {\n\tFetch(key string) string\n}\n\ntype sink struct{}\n\nfunc (s *sink) Fetch(key string) string { return \"\" }\n\nfunc (s *sink) Flush() error { return nil }\n\nfunc (s *sink) Read(p []byte) (int, error) { return 0, nil }\n\ntype Ctx struct{}\n\nfunc (c *Ctx) Done() <-chan struct{} { return nil }\n",
        ),
        (
            "storage/storage.go",
            "package storage\n\ntype Store interface {\n\tFetch(key string) string\n}\n\ntype Base struct{}\n\nfunc (b *Base) Flush() error { return nil }\n",
        ),
        (
            "store/store.go",
            "package store\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/storage\"\n)\n\ntype Reader interface {\n\tRead(p []byte) (int, error)\n}\n\ntype ReadCloser interface {\n\tReader\n\tClose() error\n}\n\ntype Wrapper struct {\n\t*storage.Base\n\tname string\n}\n\ntype Job struct{}\n\nfunc (j *Job) Start() {}\n\ntype Factory interface {\n\tNew() *Job\n}\n\ntype Builder struct{}\n\nfunc (b *Builder) Add(n int) *Builder { return b }\n\ntype Ctx = context.Context\n\ntype Keeper = storage.Store\n",
        ),
        (
            "store/use.go",
            "package store\n\nimport (\n\t\"net/http\"\n\n\t\"example.com/app/storage\"\n)\n\ntype sink struct{}\n\nfunc (s *sink) Flush() error { return nil }\n\nfunc (s *sink) Read(p []byte) (int, error) { return 0, nil }\n\nfunc (s *sink) Fetch(key string) string { return \"\" }\n\nfunc Use(v any, w http.ResponseWriter, p []byte) string {\n\tv.(*Wrapper).Flush()\n\tv.(ReadCloser).Read(p)\n\tw.(http.Flusher).Flush()\n\treturn v.(storage.Store).Fetch(\"k\")\n}\n\nfunc Anon(v any) string {\n\treturn v.(interface{ Fetch(key string) string }).Fetch(\"k\")\n}\n\nfunc Run(v any) {\n\tv.(Factory).New().Start()\n}\n\nfunc lookup(key string) any { return nil }\n\nfunc Quoted() string {\n\treturn lookup(\")\").(storage.Store).Fetch(\"k\")\n}\n\nfunc (s *sink) Add(n int) *sink { return s }\n\nfunc Build(v any) {\n\tv.(*Builder).Add(1).Add(2)\n}\n\nfunc Wait(v any) {\n\tv.(Ctx).Done()\n}\n\nfunc Keep(v any) string {\n\treturn v.(Keeper).Fetch(\"k\")\n}\n",
        ),
        (
            "dot/dot.go",
            "package dot\n\nimport . \"example.com/app/storage\"\n\nfunc Get(v any) string {\n\treturn v.(Store).Fetch(\"k\")\n}\n",
        ),
    ];

    /// Upstream #2444: a Go call through a type assertion is a call of the
    /// asserted type's method — its own, the one its interface declares, or
    /// one embedding promotes into it; written bare, as a pointer, through a
    /// project package or a dot import, or through an alias — and links
    /// nothing for a type from outside the project, a type literal or an
    /// alias of an outside type. A later link of the chain is left to the type
    /// the call before it returns. Upstream's fixture.
    #[test]
    fn pr2444_a_go_call_through_a_type_assertion_reaches_the_asserted_types_method() {
        let project = ASSERTED_CALLS
            .iter()
            .fold(Project::new(), |project, (path, text)| {
                project.file(path, text)
            })
            .index();
        let graph = project.graph();
        let report = graph.report();
        // `line file::qualifiedName` of every call the function or method
        // `qualified` in `file` makes, sorted (upstream's `calls`).
        let calls = |file: &str, qualified: &str| -> Vec<String> {
            let caller = graph
                .nodes
                .iter()
                .find(|n| {
                    n.file_path == file
                        && n.qualified_name == qualified
                        && matches!(n.kind.as_str(), "function" | "method")
                })
                .unwrap_or_else(|| panic!("{qualified} in {file}\n{report}"));
            let mut out: Vec<String> = graph
                .edges
                .iter()
                .filter(|e| e.kind == "calls" && e.source == caller.id)
                .filter_map(|e| {
                    let target = graph.node(&e.target)?;
                    Some(format!(
                        "{} {}::{}",
                        e.line?, target.file_path, target.qualified_name
                    ))
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            calls("pb/rpc_grpc.pb.go", "_KV_Range_Handler"),
            [
                "30 pb/rpc_grpc.pb.go::KVServer::Range",
                "33 pb/rpc_grpc.pb.go::KVServer::Range",
            ],
            "{report}"
        );
        // A chain continued on the next line.
        assert_eq!(
            calls("pb/rpc_grpc.pb.go", "_KV_Put_Handler"),
            ["39 pb/rpc_grpc.pb.go::KVServer::Put"],
            "{report}"
        );
        assert_eq!(
            calls("internal/bufconn/bufconn.go", "conn::Close"),
            [
                "19 internal/bufconn/bufconn.go::pipe::Close",
                "20 internal/bufconn/bufconn.go::pipe::closeWrite",
                "21 internal/bufconn/bufconn.go::pipe::close",
            ],
            "{report}"
        );
        // *Wrapper's Flush is promoted from *storage.Base; ReadCloser's Read
        // from Reader; http.Flusher's Flush links nothing.
        assert_eq!(
            calls("store/use.go", "Use"),
            [
                "18 storage/storage.go::Base::Flush",
                "19 store/store.go::Reader::Read",
                "21 storage/storage.go::Store::Fetch",
            ],
            "{report}"
        );
        assert_eq!(
            calls("dot/dot.go", "Get"),
            ["6 storage/storage.go::Store::Fetch"],
            "{report}"
        );
        // An alias is the type it names.
        assert_eq!(
            calls("store/use.go", "Keep"),
            ["49 storage/storage.go::Store::Fetch"],
            "{report}"
        );
        // A string argument holding a parenthesis is not the end of the call.
        assert_eq!(
            calls("store/use.go", "Quoted"),
            [
                "35 storage/storage.go::Store::Fetch",
                "35 store/use.go::lookup",
            ],
            "{report}"
        );
        assert!(calls("store/use.go", "Anon").is_empty(), "{report}");
        // Ctx is an alias of context.Context.
        assert!(calls("store/use.go", "Wait").is_empty(), "{report}");
        let flush = &node_in(graph, "store/use.go", "Flush").id;
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && &e.target == flush),
            "{report}"
        );
        assert_eq!(
            calls("store/use.go", "Run"),
            [
                "29 store/store.go::Factory::New",
                "29 store/store.go::Job::Start",
            ],
            "{report}"
        );
        // Both `Add` calls start at `v`, so nothing tells them apart.
        let mut build = calls("store/use.go", "Build");
        build.dedup();
        assert_eq!(build, ["41 store/store.go::Builder::Add"], "{report}");
        let decoys: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == "method")
            .filter(|n| {
                n.file_path == "alpha/alpha.go"
                    || (n.file_path == "store/use.go" && n.qualified_name.starts_with("sink::"))
                    || (n.file_path == "pb/rpc_grpc.pb.go"
                        && n.qualified_name.starts_with("UnimplementedKVServer::"))
            })
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(decoys.len(), 11, "{report}");
        for decoy in decoys {
            assert!(
                !graph.edges.iter().any(|e| e.kind == "calls"
                    && e.target == decoy
                    && e.provenance.as_deref() != Some("heuristic")),
                "{}\n{report}",
                graph.describe(decoy)
            );
        }
    }

    /// Upstream #2438: a type declared on its own takes the comment above
    /// `type`; a group's comment is the group's unless the group holds one
    /// type without a comment of its own; a comment after code on its line is
    /// that line's, never the next declaration's. Upstream's fixture, less its
    /// `const sides` (a Go constant carries no docstring here: pre-existing).
    #[test]
    fn pr2438_a_go_type_declared_on_its_own_keeps_its_doc_comment() {
        let source = "// Copyright 2026 The Shapes Authors.\n\n// Package shapes is documented.\npackage shapes\n\ntype Bare struct{}\n\n// Point is documented on its own.\ntype Point struct{ X, Y int }\n\n// Shape is an interface documented on its own.\ntype Shape interface {\n\tArea() float64\n}\n\n// ID is a defined type documented on its own.\ntype ID int\n\n// Set is a generic type documented on its own.\ntype Set[T comparable] map[T]struct{}\n\n// Alias is an alias documented on its own.\ntype Alias = Point\n\n/*\nBox is documented in a block comment.\n*/\ntype Box struct{}\n\n// Kinds of shapes. The comment documents the group.\ntype (\n\tCircle struct{}\n\t// Square is documented inside the group.\n\tSquare struct{}\n)\n\n// Lone is the only member of its group.\ntype (\n\tLone int\n)\n\n// A group of one whose member has its own comment.\ntype (\n\t// Own is documented inside its group.\n\tOwn int\n)\n\n// Pair groups a type with its alias.\ntype (\n\tFirst  int\n\tSecond = First\n)\n\ntype (\n\tCelsius float64 // degrees C.\n\tKelvin  float64\n)\n\n// sides is documented above its line.\nconst sides = 4 /* four */ // sides of a square.\n\n// Rect is documented below a constant's line.\ntype Rect struct{}\n\nvar origin = Point{} // the origin.\ntype Polygon struct{}\n\n// Area is documented as before.\nfunc Area(s Shape) float64 { return s.Area() }\n\n// Norm is documented as before.\nfunc (p Point) Norm() int { return p.X } // Norm's line comment.\nfunc Next() int { return 0 }\n";
        for (label, text) in with_line_ends(source) {
            let project = Project::new().file("shapes/shapes.go", &text).index();
            let graph = project.graph();
            let file = "shapes/shapes.go";
            let doc = |kind: &str, name: &str| docstring_of(graph, file, kind, name);
            let found: Vec<(&str, Option<String>)> = [
                ("struct", "Bare"),
                ("struct", "Point"),
                ("interface", "Shape"),
                ("type_alias", "ID"),
                ("type_alias", "Set"),
                ("type_alias", "Alias"),
                ("struct", "Box"),
                ("struct", "Circle"),
                ("struct", "Square"),
                ("type_alias", "Lone"),
                ("type_alias", "Own"),
                ("type_alias", "First"),
                ("type_alias", "Second"),
                ("type_alias", "Celsius"),
                ("type_alias", "Kelvin"),
                ("struct", "Rect"),
                ("struct", "Polygon"),
                ("function", "Area"),
                ("method", "Norm"),
                ("function", "Next"),
            ]
            .into_iter()
            .map(|(kind, name)| (name, doc(kind, name)))
            .collect();
            let want: Vec<(&str, Option<String>)> = [
                ("Bare", None),
                ("Point", Some("Point is documented on its own.")),
                (
                    "Shape",
                    Some("Shape is an interface documented on its own."),
                ),
                ("ID", Some("ID is a defined type documented on its own.")),
                ("Set", Some("Set is a generic type documented on its own.")),
                ("Alias", Some("Alias is an alias documented on its own.")),
                ("Box", Some("Box is documented in a block comment.")),
                ("Circle", None),
                ("Square", Some("Square is documented inside the group.")),
                ("Lone", Some("Lone is the only member of its group.")),
                ("Own", Some("Own is documented inside its group.")),
                ("First", None),
                ("Second", None),
                ("Celsius", None),
                ("Kelvin", None),
                ("Rect", Some("Rect is documented below a constant's line.")),
                ("Polygon", None),
                ("Area", Some("Area is documented as before.")),
                ("Norm", Some("Norm is documented as before.")),
                ("Next", None),
            ]
            .into_iter()
            .map(|(name, doc)| (name, doc.map(str::to_string)))
            .collect();
            assert_eq!(found, want, "{label}");
        }
    }
}

mod rust {
    use super::support::{Graph, Project};

    /// `kind:qualifiedName` of every `calls` / `references` / `instantiates`
    /// target out of the functions named `name` (upstream's `targets`).
    fn targets(graph: &Graph, name: &str) -> Vec<String> {
        let from: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == "function" && n.name == name)
            .map(|n| n.id.as_str())
            .collect();
        graph
            .edges
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "calls" | "references" | "instantiates"))
            .filter(|e| from.contains(&e.source.as_str()))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{}:{}", n.kind, n.qualified_name))
            .collect()
    }

    /// The files every edge but `contains` out of `file`'s nodes lands in
    /// (upstream's `targetsFrom`).
    fn target_files(graph: &Graph, file: &str) -> Vec<String> {
        graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.file_path.clone())
            .collect()
    }

    /// Upstream #2124: a bare Rust name reaches only what is in scope —
    /// `Some(x)` / `Ok(x)` the prelude's, not a project variant or a struct
    /// another file declares; a variant through a `use` of its enum's `*`; a
    /// path (`crate::error::Result`) is no prelude lookup; a same-file struct
    /// shadows the prelude. Upstream's fixture, its hygiene structs `pub` (a
    /// private one is out of reach already), with the audit's `drop(v)`: with
    /// no `use` it is the prelude's, not a `pub fn drop` in `util.rs`; a `use`
    /// of it, or a glob over its module, shadows the prelude.
    #[test]
    fn pr2124_a_bare_rust_name_reaches_what_is_in_scope() {
        let project = Project::new()
            .file("Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n")
            .file(
                "src/lowargs.rs",
                "pub enum EncodingMode {\n    Auto,\n    Some(u8),\n    Disabled,\n}\n\npub enum ParseResult<T> {\n    Ok(T),\n    Err(String),\n}\n",
            )
            .file(
                "src/hygiene.rs",
                "pub struct Ok;\npub struct Result;\n\nfn local() -> Ok { Ok }\n",
            )
            .file(
                "src/decompress.rs",
                "pub fn find(cmd: u8) -> Option<u8> {\n    if cmd > 0 { return Some(cmd); }\n    let r: std::result::Result<u8, ()> = Ok(cmd);\n    None\n}\n",
            )
            .file(
                "src/error.rs",
                "pub struct Error;\npub type Result<T> = std::result::Result<T, Error>;\n",
            )
            .file(
                "src/api.rs",
                "pub fn load() -> crate::error::Result<u8> { unimplemented!() }\n",
            )
            .file(
                "src/modes.rs",
                "use crate::lowargs::EncodingMode::*;\n\npub fn pick() -> crate::lowargs::EncodingMode { Some(1) }\n",
            )
            .file("src/util.rs", "pub fn drop<T>(_value: T) {}\n")
            .file(
                "src/release.rs",
                "pub fn release(v: Vec<u8>) {\n    drop(v);\n}\n",
            )
            .file(
                "src/flush.rs",
                "use crate::util::drop;\n\npub fn flush(v: Vec<u8>) {\n    drop(v);\n}\n",
            )
            .file(
                "src/tidy.rs",
                "use crate::util::*;\n\npub fn tidy(v: Vec<u8>) {\n    drop(v);\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let find: Vec<String> = targets(graph, "find")
            .into_iter()
            .filter(|t| t.contains("Some") || t.contains("Ok") || t.contains("Result"))
            .collect();
        assert!(find.is_empty(), "{find:?}\n{report}");
        assert!(
            !targets(graph, "release").contains(&"function:drop".to_string()),
            "{report}"
        );
        assert!(
            targets(graph, "flush").contains(&"function:drop".to_string()),
            "{report}"
        );
        assert!(
            targets(graph, "tidy").contains(&"function:drop".to_string()),
            "{report}"
        );
        assert!(
            targets(graph, "pick").contains(&"enum_member:EncodingMode::Some".to_string()),
            "{report}"
        );
        // A path names its module: `crate::error::Result` is `error.rs`'s
        // alias, not `hygiene.rs`'s struct.
        let load = targets(graph, "load");
        assert!(
            load.contains(&"type_alias:Result".to_string())
                && !load.contains(&"struct:Result".to_string()),
            "{load:?}\n{report}"
        );
        assert!(
            targets(graph, "local").contains(&"struct:Ok".to_string()),
            "{report}"
        );
    }

    /// Upstream #2214: a bare Rust type or function from another file is in
    /// scope only through a `use` that binds it or a glob over its module; one
    /// the file imports from outside the project (`use
    /// std::task::{Context, Poll}`) is that outside item; a path names its
    /// module (`io::Result` under `use std::io` is std's, `std::io::Error`
    /// too); a crate-local `use` and an inline test module still reach the
    /// project's item. Upstream's fixture, with two more: an outside import
    /// shadows a project glob (`poll.rs`), and a crate `Cargo.toml` depends on
    /// is outside the project (`use futures::Stream`).
    #[test]
    fn pr2214_a_rust_name_is_in_scope_only_through_its_module_or_a_use() {
        let project = Project::new()
            .file(
                "Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfutures = \"0.3\"\n",
            )
            .file(
                "src/lib.rs",
                "pub mod runtime;\npub mod io;\npub mod net;\npub mod stream;\npub mod poll;\npub mod source;\npub mod sink;\npub mod handle;\n",
            )
            .file("src/stream.rs", "pub trait Stream {}\n")
            .file(
                "src/poll.rs",
                "use crate::runtime::context::*;\nuse std::task::Context;\n\npub fn wake(cx: &mut Context<'_>) {\n    let _ = cx;\n}\n",
            )
            .file(
                "src/source.rs",
                "use futures::Stream;\n\npub fn drain(s: impl Stream) {\n    let _ = s;\n}\n",
            )
            .file(
                "src/sink.rs",
                "use crate::stream::Stream;\n\npub fn fill(s: impl Stream) {\n    let _ = s;\n}\n",
            )
            .file(
                "src/runtime/mod.rs",
                "pub mod context;\npub mod task;\npub mod driver;\n",
            )
            .file(
                "src/runtime/context.rs",
                "pub struct Context {\n    pub depth: u8,\n}\n\npub fn try_current() -> u8 {\n    0\n}\n",
            )
            .file(
                "src/handle.rs",
                "pub fn current() -> u8 {\n    try_current()\n}\n",
            )
            .file(
                "src/runtime/task/mod.rs",
                "pub type Result<T> = std::result::Result<T, ()>;\n",
            )
            .file("src/io/mod.rs", "pub mod copy;\n")
            .file("src/io/copy.rs", "pub struct Error;\n")
            .file(
                "src/net.rs",
                "use std::io;\nuse std::task::{Context, Poll};\n\npub fn poll_ready(cx: &mut Context<'_>) -> Poll<io::Result<()>> {\n    let _ = cx;\n    Poll::Ready(Ok(()))\n}\n\npub fn fail() -> std::io::Error {\n    std::io::Error::other(\"x\")\n}\n",
            )
            .file(
                "src/runtime/driver.rs",
                "use crate::runtime::context::Context;\n\npub fn enter(ctx: &Context) -> u8 {\n    ctx.depth\n}\n",
            )
            .file("tests/support/panic.rs", "pub fn test_panic() {}\n")
            .file(
                "tests/io_panic.rs",
                "mod support {\n    pub mod panic;\n}\n\nuse support::panic::test_panic;\n\n#[test]\nfn panics() {\n    test_panic();\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let net = target_files(graph, "src/net.rs");
        for outside in [
            "src/runtime/context.rs",
            "src/runtime/task/mod.rs",
            "src/io/copy.rs",
        ] {
            assert!(!net.contains(&outside.to_string()), "{outside}\n{report}");
        }
        assert!(
            target_files(graph, "src/runtime/driver.rs")
                .contains(&"src/runtime/context.rs".to_string()),
            "{report}"
        );
        assert!(
            target_files(graph, "tests/io_panic.rs")
                .contains(&"tests/support/panic.rs".to_string()),
            "{report}"
        );
        // The audit's `try_current()` with no `use`: another file's function.
        assert!(
            !target_files(graph, "src/handle.rs").contains(&"src/runtime/context.rs".to_string()),
            "{report}"
        );
        assert!(
            !target_files(graph, "src/poll.rs").contains(&"src/runtime/context.rs".to_string()),
            "{report}"
        );
        assert!(
            !target_files(graph, "src/source.rs").contains(&"src/stream.rs".to_string()),
            "{report}"
        );
        assert!(
            target_files(graph, "src/sink.rs").contains(&"src/stream.rs".to_string()),
            "{report}"
        );
    }

    /// A Rust project of `files` under `src/`, with upstream's #2354 manifest.
    fn rust_project(files: &[(&str, &str)]) -> super::support::Indexed {
        let mut project = Project::new().file(
            "Cargo.toml",
            "[package]\nname = \"repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        for (file, text) in files {
            project = project.file(&format!("src/{file}"), text);
        }
        project.index()
    }

    /// The node of `kind` named `name` declared in `src/<file>`.
    fn declared<'g>(
        graph: &'g Graph,
        kind: &str,
        file: &str,
        name: &str,
    ) -> &'g super::support::NodeRow {
        let path = format!("src/{file}");
        graph
            .nodes
            .iter()
            .find(|n| n.kind == kind && n.file_path == path && n.name == name)
            .unwrap_or_else(|| panic!("{kind} {name} in {path}\n{}", graph.report()))
    }

    /// `file:name` of every symbol with a `references` edge to `id`, `src/`
    /// left out (upstream's `referencers`).
    fn referencers(graph: &Graph, id: &str) -> Vec<String> {
        let names: std::collections::BTreeSet<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references" && e.target == id)
            .filter_map(|e| graph.node(&e.source))
            .map(|n| {
                let file = n.file_path.strip_prefix("src/").unwrap_or(&n.file_path);
                format!("{file}:{}", n.name)
            })
            .collect();
        names.into_iter().collect()
    }

    /// `kind:qualifiedName` of every edge but `contains` out of the function
    /// named `name`.
    fn outgoing(graph: &Graph, name: &str) -> Vec<String> {
        let from = graph
            .nodes
            .iter()
            .find(|n| n.kind == "function" && n.name == name)
            .unwrap_or_else(|| panic!("function {name}\n{}", graph.report()));
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains" && e.source == from.id)
            .filter_map(|e| {
                graph
                    .node(&e.target)
                    .map(|n| format!("{}:{}", e.kind, n.qualified_name))
            })
            .collect();
        out.sort();
        out
    }

    const MODE: &str = "pub enum Mode {\n    A,\n    B,\n    C(u8),\n    D { x: u8 },\n}\n\npub fn takes(_m: Mode) {}\n\npub fn current() -> Mode { Mode::B }\n";

    /// Upstream #2354 (#2328): a variant path in an expression and in match
    /// arms, written through the module, references the enum.
    #[test]
    fn pr2354_a_variant_path_through_its_module_references_the_enum() {
        let project = rust_project(&[
            ("mode.rs", MODE),
            (
                "variant_use.rs",
                "use crate::mode;\n\npub fn code(flag: bool) -> u8 {\n    let m = if flag { mode::Mode::A } else { mode::Mode::B };\n    match m {\n        mode::Mode::A => 1,\n        mode::Mode::B => 2,\n        _ => 3,\n    }\n}\n",
            ),
            (
                "main.rs",
                "mod mode;\nmod variant_use;\n\nfn main() {\n    mode::takes(mode::Mode::A);\n    let _ = variant_use::code(true);\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            [
                "main.rs:main",
                "mode.rs:current",
                "mode.rs:takes",
                "variant_use.rs:code"
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: a bare `Mode::A` after `use crate::mode::Mode;`, at the
    /// receiver's column.
    #[test]
    fn pr2354_a_bare_variant_path_after_a_use_of_its_enum() {
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod bare;\n"),
            ("mode.rs", MODE),
            (
                "bare.rs",
                "use crate::mode::{self, Mode};\n\npub fn pick() -> u8 {\n    let _m = Mode::B;\n    0\n}\n\npub fn score() -> u8 {\n    match mode::current() {\n        Mode::A => 1,\n        _ => 0,\n    }\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            [
                "bare.rs:pick",
                "bare.rs:score",
                "mode.rs:current",
                "mode.rs:takes"
            ],
            "{}",
            graph.report()
        );
        let pick = declared(graph, "function", "bare.rs", "pick");
        let at: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references" && e.source == pick.id)
            .map(|e| format!("{}:{}", e.line.unwrap_or(0), e.col.unwrap_or(0)))
            .collect();
        assert_eq!(at, ["4:13"], "{}", graph.report());
    }

    /// Upstream #2354: tuple and struct variant patterns, and a variant read
    /// as a value.
    #[test]
    fn pr2354_tuple_and_struct_variant_patterns_and_a_variant_value() {
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod pat;\n"),
            ("mode.rs", MODE),
            (
                "pat.rs",
                "use crate::mode::{self, Mode};\n\npub fn tuple() -> u8 {\n    match mode::current() {\n        Mode::C(x) => x,\n        _ => 0,\n    }\n}\n\npub fn structure() -> u8 {\n    if let crate::mode::Mode::D { x } = mode::current() { x } else { 0 }\n}\n\npub fn ctor_value(xs: Vec<u8>) -> usize {\n    xs.into_iter().map(Mode::C).count()\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            [
                "mode.rs:current",
                "mode.rs:takes",
                "pat.rs:ctor_value",
                "pat.rs:structure",
                "pat.rs:tuple"
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: constructing a tuple or struct variant stays linked to
    /// the variant itself.
    #[test]
    fn pr2354_constructing_a_variant_stays_linked_to_the_variant() {
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod build;\n"),
            ("mode.rs", MODE),
            (
                "build.rs",
                "use crate::mode::Mode;\n\npub fn build() {\n    let _c = Mode::C(3);\n    let _d = Mode::D { x: 2 };\n}\n",
            ),
        ]);
        let graph = project.graph();
        assert_eq!(
            outgoing(graph, "build"),
            ["calls:Mode::C", "instantiates:Mode::D"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: `Self::A` inside `impl Mode`, in the enum's file and in
    /// another one.
    #[test]
    fn pr2354_self_variant_in_the_enums_impl() {
        let mode_impl = format!(
            "{MODE}\nimpl Mode {{\n    pub fn flip(&self) -> u8 {{\n        match self {{\n            Self::A => 1,\n            _ => 0,\n        }}\n    }}\n}}\n"
        );
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod ops;\n"),
            ("mode.rs", &mode_impl),
            (
                "ops.rs",
                "use crate::mode::Mode;\n\nimpl Mode {\n    pub fn is_b(&self) -> bool {\n        if let Self::B = self { true } else { false }\n    }\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            [
                "mode.rs:current",
                "mode.rs:flip",
                "mode.rs:takes",
                "ops.rs:is_b"
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: `Self::A` names the impl's type, never through a path
    /// written elsewhere on its line (`other::Mode::Z`, whose enum declares no
    /// `A`).
    #[test]
    fn pr2354_a_self_variant_ignores_another_path_on_its_line() {
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod other;\npub mod ops;\n"),
            ("mode.rs", MODE),
            ("other.rs", "pub enum Mode {\n    Y,\n    Z,\n}\n"),
            (
                "ops.rs",
                "use crate::mode::Mode;\nuse crate::other;\n\nimpl Mode {\n    pub fn to_other(&self) -> u8 {\n        match self {\n            Self::A => other::Mode::Z as u8,\n            _ => 0,\n        }\n    }\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            ["mode.rs:current", "mode.rs:takes", "ops.rs:to_other"],
            "{}",
            graph.report()
        );
        let other = declared(graph, "enum", "other.rs", "Mode");
        assert_eq!(
            referencers(graph, &other.id),
            ["ops.rs:to_other"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: a variant imported by name and used bare has no path to
    /// read the enum from, and never lands on another module's namesake.
    #[test]
    fn pr2354_a_variant_imported_by_name_has_no_path_to_read() {
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod other;\npub mod glob;\n"),
            ("mode.rs", MODE),
            ("other.rs", "pub enum Mode {\n    A,\n    Z,\n}\n"),
            (
                "glob.rs",
                "use crate::mode::Mode::*;\n\npub fn bare(m: u8) -> u8 {\n    match m {\n        0 => { let _ = A; 1 }\n        _ => 0,\n    }\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            ["mode.rs:current", "mode.rs:takes"],
            "{}",
            graph.report()
        );
        let other = declared(graph, "enum", "other.rs", "Mode");
        assert!(
            referencers(graph, &other.id).is_empty(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: the path picks the type that declares the variant,
    /// never a same-named type without it; another crate's enum is its own.
    #[test]
    fn pr2354_the_path_picks_the_type_declaring_the_variant() {
        let project = rust_project(&[
            (
                "lib.rs",
                "pub mod matching;\npub mod other;\npub mod user;\n",
            ),
            (
                "matching.rs",
                "pub enum Match {\n    None,\n    Ignore,\n}\n",
            ),
            ("other.rs", "pub struct Match;\n"),
            (
                "user.rs",
                "use crate::matching::Match;\n\npub enum ColorChoice {\n    Auto,\n    Ansi,\n}\n\npub fn first() -> u8 {\n    let _m = Match::None;\n    0\n}\n\npub fn color() -> u8 {\n    let _c = termcolor::ColorChoice::AlwaysAnsi;\n    0\n}\n",
            ),
        ]);
        let graph = project.graph();
        let matching = declared(graph, "enum", "matching.rs", "Match");
        assert_eq!(
            referencers(graph, &matching.id),
            ["user.rs:first"],
            "{}",
            graph.report()
        );
        let other = declared(graph, "struct", "other.rs", "Match");
        assert!(
            referencers(graph, &other.id).is_empty(),
            "{}",
            graph.report()
        );
        let color = declared(graph, "enum", "user.rs", "ColorChoice");
        assert!(
            referencers(graph, &color.id).is_empty(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: an associated const or function read through a type is
    /// not a variant path.
    #[test]
    fn pr2354_an_associated_const_or_function_is_no_variant_path() {
        let mode_impl = format!(
            "{MODE}\nimpl Mode {{\n    pub const ALL: [u8; 2] = [0, 1];\n    pub fn count() -> usize {{ Self::ALL.len() }}\n}}\n"
        );
        let project = rust_project(&[
            ("lib.rs", "pub mod mode;\npub mod limits;\npub mod user;\n"),
            ("mode.rs", &mode_impl),
            (
                "limits.rs",
                "pub struct Limits;\n\nimpl Limits {\n    pub const MAX: u8 = 3;\n    pub fn new() -> Self { Limits }\n}\n",
            ),
            (
                "user.rs",
                "use crate::limits::Limits;\nuse crate::mode::Mode;\n\npub fn cap() -> u8 {\n    Limits::MAX\n}\n\npub fn all() -> usize {\n    Mode::ALL.len()\n}\n\npub fn make() {\n    let _l = Limits::new();\n    let _f = Limits::new;\n}\n",
            ),
        ]);
        let graph = project.graph();
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            ["mode.rs:current", "mode.rs:takes"],
            "{}",
            graph.report()
        );
        let limits = declared(graph, "struct", "limits.rs", "Limits");
        assert!(
            referencers(graph, &limits.id).is_empty(),
            "{}",
            graph.report()
        );
        assert_eq!(
            outgoing(graph, "make"),
            ["calls:Limits::new"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2354: std and prelude enums never reach a same-named project
    /// enum, nor their variants a project variant.
    #[test]
    fn pr2354_std_and_prelude_enums_never_reach_a_project_enum() {
        let project = rust_project(&[
            ("lib.rs", "pub mod shapes;\npub mod user;\n"),
            (
                "shapes.rs",
                "pub enum Ordering {\n    Less,\n    Greater,\n}\n\npub enum Option {\n    Some,\n    None,\n}\n",
            ),
            (
                "user.rs",
                "use std::cmp::{self, Ordering};\n\npub fn sorted(a: u8, b: u8) -> bool {\n    a.cmp(&b) == Ordering::Less || b.cmp(&a) == cmp::Ordering::Greater || a.cmp(&b) != std::cmp::Ordering::Less\n}\n\npub fn first(o: Option<u8>) -> u8 {\n    match o {\n        Option::Some(x) => x,\n        Option::None => 0,\n    }\n}\n\npub fn bare(o: Option<u8>, r: Result<u8, ()>) -> u8 {\n    match (o, r) {\n        (Some(x), Ok(_)) => x,\n        (None, Err(())) => 0,\n        _ => 1,\n    }\n}\n",
            ),
        ]);
        let graph = project.graph();
        for name in ["Ordering", "Option"] {
            let shape = declared(graph, "enum", "shapes.rs", name);
            assert!(
                referencers(graph, &shape.id).is_empty(),
                "{name}\n{}",
                graph.report()
            );
        }
        for variant in graph.nodes.iter().filter(|n| n.kind == "enum_member") {
            assert!(
                !graph
                    .edges
                    .iter()
                    .any(|e| e.target == variant.id && e.kind != "contains"),
                "{}\n{}",
                variant.qualified_name,
                graph.report()
            );
        }
    }

    /// Upstream #2354: a module path, an associated function and a same-named
    /// enum elsewhere are not references to the enum.
    #[test]
    fn pr2354_a_module_path_or_associated_function_is_no_enum_reference() {
        let mode_impl =
            format!("{MODE}\nimpl Mode {{\n    pub fn new() -> Mode {{ Mode::A }}\n}}\n");
        let project = rust_project(&[
            (
                "lib.rs",
                "pub mod mode;\npub mod other;\npub mod util;\npub mod paths;\n",
            ),
            ("mode.rs", &mode_impl),
            ("other.rs", "pub enum Mode {\n    A,\n    Z,\n}\n"),
            ("util.rs", "pub fn take(x: u8) -> u8 { x }\n"),
            (
                "paths.rs",
                "use crate::mode;\nuse crate::other;\nuse crate::util;\n\npub fn via_module() -> u8 {\n    util::take(3)\n}\n\npub fn via_ctor() {\n    let _m = mode::Mode::new();\n}\n\npub fn via_other() -> u8 {\n    match other::Mode::A {\n        other::Mode::Z => 1,\n        _ => 0,\n    }\n}\n\npub fn via_mode() {\n    let _m = mode::Mode::A;\n}\n",
            ),
        ]);
        let graph = project.graph();
        let via_module = declared(graph, "function", "paths.rs", "via_module");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.kind == "references" && e.source == via_module.id),
            "{}",
            graph.report()
        );
        let other = declared(graph, "enum", "other.rs", "Mode");
        assert_eq!(
            referencers(graph, &other.id),
            ["paths.rs:via_other"],
            "{}",
            graph.report()
        );
        let mode = declared(graph, "enum", "mode.rs", "Mode");
        assert_eq!(
            referencers(graph, &mode.id),
            [
                "mode.rs:current",
                "mode.rs:new",
                "mode.rs:takes",
                "paths.rs:via_mode"
            ],
            "{}",
            graph.report()
        );
    }

    /// `Owner::member` of every call edge out of `file` (upstream's
    /// `callsFrom`), sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut calls: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        calls.sort();
        calls
    }

    /// Upstream #2177's Rust half: a CamelCase path receiver the project
    /// doesn't declare (`Vec::new`, `String::from`) is an outside type, a
    /// lowercase module path (`task::spawn`) never reaches a method, and a
    /// link further down a multi-line chain is named after the chain's head
    /// (`bat()\n.stdout(…)` is assert_cmd's, `Arg::new(…)\n.value_hint(…)`
    /// the project's). Upstream's fixture, with the audit's `String::from` to
    /// a `StringBuilder::from` and a project trait's associated function
    /// reached through the trait.
    #[test]
    fn pr2177_rust_calls_on_outside_types_and_module_paths_stay_outside() {
        let project = Project::new()
            .file(
                "src/lib.rs",
                "pub struct VecWithInitialized;\nimpl VecWithInitialized {\n    pub fn new() -> Self { VecWithInitialized }\n}\n\npub struct TaskTracker;\nimpl TaskTracker {\n    pub fn spawn(&self) {}\n}\n\npub struct OutputType;\nimpl OutputType {\n    pub fn stdout(&self) {}\n}\n\npub struct Arg;\nimpl Arg {\n    pub fn new(name: &str) -> Arg { Arg }\n    pub fn value_hint(self, hint: u8) -> Arg { self }\n}\n\npub struct StringBuilder;\nimpl StringBuilder {\n    pub fn from(text: &str) -> Self { StringBuilder }\n}\n\npub trait Plugin {\n    fn create() -> Self;\n}\n\npub struct Widget;\nimpl Widget {\n    pub fn fresh() -> Self { Widget }\n}\npub type WidgetRef = Widget;\n\npub fn run() {\n    let v: Vec<u8> = Vec::new();\n    task::spawn(async {});\n    bat()\n        .assert()\n        .stdout(\"x\");\n    let a = Arg::new(\"cmd\")\n        .value_hint(1);\n    let s = String::from(\"x\");\n    let r = WidgetRef::fresh();\n}\n\npub fn make<P: Plugin>() -> P {\n    <P as Plugin>::create()\n}\n\npub fn build() -> u8 {\n    let _b = Plugin::create;\n    0\n}\n",
            )
            .index();
        let graph = project.graph();
        let calls = calls_from(graph, "src/lib.rs");
        for outside in [
            "VecWithInitialized::new",
            "TaskTracker::spawn",
            "OutputType::stdout",
            "StringBuilder::from",
        ] {
            assert!(
                !calls.contains(&outside.to_string()),
                "{outside}: {calls:?}\n{}",
                graph.report()
            );
        }
        assert!(
            calls.contains(&"Arg::value_hint".to_string()),
            "{calls:?}\n{}",
            graph.report()
        );
        // A project type alias is a declared type: `WidgetRef::fresh()` keeps
        // its guess (`trait` and `type_alias` count, upstream #2177).
        assert!(
            calls.contains(&"Widget::fresh".to_string()),
            "{calls:?}\n{}",
            graph.report()
        );
    }

    /// Upstream #2231: a path through a generic type parameter (`T::Output`)
    /// names no project item, not even the alias another impl in its own file
    /// declares.
    #[test]
    fn pr2231_a_generic_parameter_path_names_no_alias_of_its_file() {
        let project = Project::new()
            .file("Cargo.toml", "[package]\nname = \"app\"\n")
            .file(
                "src/lib.rs",
                "pub struct Wrapper;\n\nimpl std::ops::Add for Wrapper {\n    type Output = Wrapper;\n    fn add(self, _other: Wrapper) -> Wrapper {\n        Wrapper\n    }\n}\n\npub fn sum<T: std::ops::Add>(a: T, b: T) -> T::Output {\n    a + b\n}\n",
            )
            .index();
        let graph = project.graph();
        let sum = graph
            .nodes
            .iter()
            .find(|n| n.kind == "function" && n.name == "sum")
            .expect("sum");
        let targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references" && e.source == sum.id)
            .filter_map(|e| graph.node(&e.target))
            .map(|t| format!("{} {}", t.kind, t.qualified_name))
            .collect();
        assert!(
            !targets.iter().any(|t| t.starts_with("type_alias")),
            "{targets:?}\n{}",
            graph.report()
        );
    }

    /// Upstream #2231: `Self::Error` in a trait is the trait's own associated
    /// type, never an impl's alias in another file nor a struct of that name,
    /// and in an impl the alias that impl declares.
    #[test]
    fn pr2231_a_self_path_reads_its_own_impl_or_trait() {
        let project = Project::new()
            .file("Cargo.toml", "[package]\nname = \"serde\"\n")
            .file("src/de/value.rs", "pub struct Error {\n    msg: String,\n}\n")
            .file(
                "src/de/mod.rs",
                "pub mod value;\npub mod unit;\n\npub trait Deserializer {\n    type Error;\n    fn deserialize_unit(self) -> Result<(), Self::Error>;\n}\n",
            )
            .file(
                "src/de/unit.rs",
                "use crate::de::value::Error;\n\npub struct Unit;\n\nimpl crate::de::Deserializer for Unit {\n    type Error = Error;\n    fn deserialize_unit(self) -> Result<(), Self::Error> {\n        Ok(())\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let targets_at = |file: &str, line: i64| -> Vec<String> {
            graph
                .edges
                .iter()
                .filter(|e| e.kind == "references" && e.line == Some(line))
                .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
                .filter_map(|e| graph.node(&e.target))
                .map(|t| format!("{} {} {}:{}", t.kind, t.name, t.file_path, t.start_line))
                .collect()
        };
        let in_trait = targets_at("src/de/mod.rs", 6);
        assert!(
            !in_trait.iter().any(|t| t.contains(" Error ")),
            "{in_trait:?}\n{}",
            graph.report()
        );
        let in_impl = targets_at("src/de/unit.rs", 7);
        assert!(
            in_impl.contains(&"type_alias Error src/de/unit.rs:6".to_string()),
            "{in_impl:?}\n{}",
            graph.report()
        );
        assert!(
            !in_impl.iter().any(|t| t.contains("src/de/value.rs")),
            "{in_impl:?}\n{}",
            graph.report()
        );
    }

    /// Upstream #2231: `Self::Error` in a signature is the enclosing impl's
    /// own associated type, and `V::Value` an associated type of a generic's
    /// bound — never a struct of that name. Upstream's fixture.
    #[test]
    fn pr2231_self_and_generic_parameter_paths_name_no_struct() {
        let project = Project::new()
            .file("Cargo.toml", "[package]\nname = \"serde\"\n")
            .file(
                "src/de/value.rs",
                "pub struct Error {\n    msg: String,\n}\n\npub struct Value;\n",
            )
            .file(
                "src/private/de.rs",
                "use crate::de::value::{Error, Value};\n\npub trait Deserializer {\n    type Error;\n}\n\npub struct ContentDeserializer<E> {\n    err: E,\n}\n\nimpl<E> Deserializer for ContentDeserializer<E> {\n    type Error = E;\n\n    fn deserialize_any<V: Visitor>(self, visitor: V) -> Result<V::Value, Self::Error> {\n        todo!()\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references" && e.line == Some(14))
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "src/private/de.rs")
            })
            .filter_map(|e| graph.node(&e.target))
            .map(|t| format!("{} {}:{}", t.kind, t.file_path, t.start_line))
            .collect();
        assert!(
            targets.contains(&"type_alias src/private/de.rs:12".to_string()),
            "{targets:?}\n{}",
            graph.report()
        );
        assert!(
            !targets.iter().any(|t| t.contains("src/de/value.rs")),
            "{targets:?}\n{}",
            graph.report()
        );
    }
}

mod php {
    use super::support::{Graph, Project};

    /// `Owner::member` of every call edge out of the method named `method` in
    /// `file`, sorted (upstream's `callees`).
    fn callees(graph: &Graph, file: &str, method: &str) -> Vec<String> {
        let from = graph
            .nodes
            .iter()
            .find(|n| n.kind == "method" && n.name == method && n.file_path == file)
            .unwrap_or_else(|| panic!("method {method} in {file}\n{}", graph.report()));
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls" && e.source == from.id)
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        out.sort();
        out
    }

    /// Upstream #2142: `$this->m()`, `self::m()`, `static::m()` and
    /// `parent::m()` name a method of the class the call is written in, a
    /// class it extends, or a trait any of them uses — never another class's
    /// method that shares the name; inside a trait `$this` is the using class,
    /// a base may call what a subclass defines, and past an ancestor outside
    /// the repository the repository's traits still count. Upstream's fixture.
    #[test]
    fn pr2142_php_this_self_and_parent_calls_stay_in_the_class_hierarchy() {
        let project = Project::new()
            .file(
                "core/lib/StringTranslationTrait.php",
                "<?php\nnamespace Drupal\\Core\\StringTranslation;\ntrait StringTranslationTrait {\n  protected function t($string) { return $string; }\n}\n",
            )
            .file(
                "core/lib/Views.php",
                "<?php\nnamespace Drupal\\views;\nclass Views {\n  public static function t($s) { return $s; }\n}\n",
            )
            .file(
                "core/lib/MarkupComparator.php",
                "<?php\nnamespace Drupal\\TestTools\\Comparator;\nclass MarkupInterfaceComparator {\n  public function assertEquals($expected, $actual) {}\n}\n",
            )
            .file(
                "core/lib/FormBase.php",
                "<?php\nnamespace Drupal\\Core\\Form;\nuse Drupal\\Core\\StringTranslation\\StringTranslationTrait;\nabstract class FormBase {\n  use StringTranslationTrait;\n  public function setUp() {}\n  protected function helper() {}\n}\n",
            )
            .file(
                "core/modules/my/MyForm.php",
                "<?php\nnamespace Drupal\\my\\Form;\nuse Drupal\\Core\\Form\\FormBase;\nclass MyForm extends FormBase {\n  public function setUp() {\n    parent::setUp();\n    $this->helper();\n    return $this->t('Hi');\n  }\n}\n",
            )
            .file(
                "core/tests/NodeTest.php",
                "<?php\nnamespace Drupal\\Tests\\node;\nuse PHPUnit\\Framework\\TestCase;\nclass NodeTest extends TestCase {\n  public function testTitle() {\n    $this->assertEquals(1, 1);\n  }\n}\n",
            )
            .file(
                "src/Testing/InteractsWithTime.php",
                "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\ntrait InteractsWithTime {\n  public function travelTo($date) {}\n}\n",
            )
            .file(
                "src/Validation/ValidatesAttributes.php",
                "<?php\nnamespace Illuminate\\Validation\\Concerns;\ntrait ValidatesAttributes {\n  public function validateSame($attribute) { return $this->getValue($attribute); }\n}\n",
            )
            .file(
                "src/Validation/Validator.php",
                "<?php\nnamespace Illuminate\\Validation;\nuse Illuminate\\Validation\\Concerns\\ValidatesAttributes;\nclass Validator {\n  use ValidatesAttributes;\n  public function getValue($attribute) { return null; }\n}\n",
            )
            .file(
                "app/Entity.php",
                "<?php\nnamespace BookStack\\Entities;\nabstract class Entity {\n  public function parentBook() { return $this->book(); }\n}\n",
            )
            .file(
                "app/Page.php",
                "<?php\nnamespace BookStack\\Entities;\nclass Page extends Entity {\n  public function book() { return null; }\n}\n",
            )
            .file(
                "tests/QueueTest.php",
                "<?php\nnamespace Illuminate\\Tests\\Integration;\nuse Orchestra\\Testbench\\TestCase;\nclass QueueTest extends TestCase {\n  public function testDelay() {\n    $this->travelTo('2000-01-02');\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            callees(graph, "core/modules/my/MyForm.php", "setUp"),
            [
                "Drupal\\Core\\Form::FormBase::helper",
                "Drupal\\Core\\Form::FormBase::setUp",
                "Drupal\\Core\\StringTranslation::StringTranslationTrait::t",
            ],
            "{report}"
        );
        assert!(
            callees(graph, "core/tests/NodeTest.php", "testTitle").is_empty(),
            "{report}"
        );
        assert_eq!(
            callees(
                graph,
                "src/Validation/ValidatesAttributes.php",
                "validateSame"
            ),
            ["Illuminate\\Validation::Validator::getValue"],
            "{report}"
        );
        assert_eq!(
            callees(graph, "app/Entity.php", "parentBook"),
            ["BookStack\\Entities::Page::book"],
            "{report}"
        );
        assert_eq!(
            callees(graph, "tests/QueueTest.php", "testDelay"),
            ["Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo"],
            "{report}"
        );
    }

    /// Upstream #2142's rule at the exact-name step: a same-namespace class's
    /// method of the name is the one candidate there, and it is no ancestor's
    /// of a test class extending PHPUnit's `TestCase`; a trait the test uses
    /// is.
    #[test]
    fn pr2142_a_this_call_in_a_vendor_subclass_skips_a_same_namespace_namesake() {
        let project = Project::new()
            .file(
                "tests/Fixtures.php",
                "<?php\nnamespace App\\Tests;\n\nclass Fixtures\n{\n    public function seed() {}\n}\n",
            )
            .file(
                "tests/CreatesUsers.php",
                "<?php\nnamespace App\\Tests;\n\ntrait CreatesUsers\n{\n    public function makeUser() {}\n}\n",
            )
            .file(
                "tests/UserTest.php",
                "<?php\nnamespace App\\Tests;\n\nuse PHPUnit\\Framework\\TestCase;\n\nclass UserTest extends TestCase\n{\n    use CreatesUsers;\n\n    public function testSeed()\n    {\n        $this->seed();\n        $this->makeUser();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "tests/UserTest.php", "testSeed"),
            ["App\\Tests::CreatesUsers::makeUser"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2152: a PHP call through a receiver nothing typed takes the
    /// one project method of that name only when the receiver is named after
    /// its class, or after a class that inherits it (`$page->save()` → Page
    /// extends Entity). Upstream's fixture.
    #[test]
    fn pr2152_a_php_untyped_receiver_keeps_a_unique_method_only_when_named_for_it() {
        let project = Project::new()
            .file(
                "src/Models/Entity.php",
                "<?php\nnamespace App\\Models;\n\nabstract class Entity\n{\n    public function save(): bool { return true; }\n}\n",
            )
            .file(
                "src/Models/Page.php",
                "<?php\nnamespace App\\Models;\n\nclass Page extends Entity\n{\n}\n",
            )
            .file(
                "src/Helper/ProgressBar.php",
                "<?php\nnamespace App\\Helper;\n\nclass ProgressBar\n{\n    public function getMessage(): string { return ''; }\n}\n",
            )
            .file(
                "src/Uploads/UserAvatars.php",
                "<?php\nnamespace App\\Uploads;\n\nclass UserAvatars\n{\n    public function assignToUser($user): void {}\n}\n",
            )
            .file(
                "src/Controller.php",
                "<?php\nnamespace App;\n\nfunction handle($page, $avatars, $user)\n{\n    try {\n        $page->save();\n        $avatars->assignToUser($user);\n    } catch (\\Exception $e) {\n        return $e->getMessage();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        let mut targets: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "src/Controller.php")
            })
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .collect();
        targets.sort();
        let report = graph.report();
        assert!(
            targets.contains(&"App\\Models::Entity::save".to_string()),
            "{targets:?}\n{report}"
        );
        assert!(
            targets.contains(&"App\\Uploads::UserAvatars::assignToUser".to_string()),
            "{targets:?}\n{report}"
        );
        assert!(
            !targets.contains(&"App\\Helper::ProgressBar::getMessage".to_string()),
            "{targets:?}\n{report}"
        );
    }

    /// `kind qualified_name` of every non-`contains`, non-`imports` edge
    /// target out of `file`'s nodes, as `source kind target`, sorted
    /// (upstream's `edgesFrom`).
    fn edges_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains" && e.kind != "imports")
            .filter_map(|e| {
                let source = graph.node(&e.source).filter(|n| n.file_path == file)?;
                let target = graph.node(&e.target)?;
                Some(format!(
                    "{} {} {}",
                    source.name, e.kind, target.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2103 (its PHP half): PHP has no implicit `$this`, so a call
    /// written without a receiver is a function call: BookStack's every
    /// `return redirect(…)` went to `ApiDocsController::redirect`, every
    /// `return view(…)` to a `$view` field. `$this->m()` and a project helper
    /// function still bind. Upstream's fixture.
    #[test]
    fn pr2103_a_bare_php_call_is_a_function_call() {
        let project = Project::new()
            .file(
                "app/Http/Controllers/BookController.php",
                "<?php\nclass BookController {\n    public function show(string $slug) {\n        $book = Book::findBySlug($slug);\n        if (is_null($book)) {\n            return redirect('/books');\n        }\n        return view('books.show', ['book' => $book]);\n    }\n    public function store() {\n        return redirect()->back()->withErrors(['name' => 'required']);\n    }\n}\n",
            )
            .file(
                "app/Http/Controllers/DocsController.php",
                "<?php\nclass DocsController { public function redirect() { return 1; } }\n",
            )
            .file("app/Views/Block.php", "<?php\nclass Block { public $view; }\n")
            .file(
                "app/Models/View.php",
                "<?php\nclass View { public static function incrementFor($x) {} }\n",
            )
            .file(
                "app/helpers.php",
                "<?php\nfunction format_title($t) { return ucfirst($t); }\n",
            )
            .file(
                "app/Http/Controllers/PageController.php",
                "<?php\nclass PageController {\n    public function show() {\n        $this->setTitle(format_title('x'));\n    }\n    protected function setTitle($t) {}\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let bound: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "app/Http/Controllers/BookController.php")
            })
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{} {}", n.kind, n.name))
            .collect();
        for wrong in ["method redirect", "field view", "class View"] {
            assert!(!bound.contains(&wrong.to_string()), "{bound:?}\n{report}");
        }
        assert_eq!(
            callees(graph, "app/Http/Controllers/PageController.php", "show"),
            vec![
                "PageController::setTitle".to_string(),
                "format_title".to_string()
            ],
            "{report}"
        );
    }

    /// Upstream #2103's form: `\\helper()` calls the global function `helper`,
    /// never a method of the name (KEEP-RUST: the extractor keeps the `\\` in
    /// the name).
    #[test]
    fn pr2103_a_php_function_called_from_the_global_namespace_is_that_function() {
        let project = Project::new()
            .file("app/helpers.php", "<?php\nfunction helper() { return 1; }\n")
            .file(
                "app/Decoy.php",
                "<?php\nclass Decoy\n{\n    public function helper() { return 2; }\n}\n",
            )
            .file(
                "app/Page.php",
                "<?php\nnamespace App;\n\nclass Page\n{\n    public function show()\n    {\n        return \\helper();\n    }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            callees(graph, "app/Page.php", "show"),
            vec!["helper".to_string()],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2141 (b): a call after `=>` in a PHP array is a bare call,
    /// not a member call (`'total' => count($items)` went to a same-named
    /// method: 166 wrong edges on BookStack). Upstream's fixture.
    #[test]
    fn pr2141_a_php_call_after_an_array_arrow_is_a_function_call() {
        let project = Project::new()
            .file(
                "app/CommentTree.php",
                "<?php\nclass CommentTree {\n  public function count(): int { return 0; }\n}\n",
            )
            .file("app/helpers.php", "<?php\nfunction user() { return null; }\n")
            .file(
                "app/Controller.php",
                "<?php\nfunction summary($items) {\n  return ['total' => count($items), 'by' => user()];\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Controller.php"),
            vec!["summary calls user".to_string()],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2141 (a): PHP names functions in any case, so a call written
    /// in another case than its function (`formatprice(10)` for
    /// `FormatPrice`) passes the name pre-filter and reaches it. Upstream's
    /// fixture.
    #[test]
    fn pr2141_a_php_call_in_another_case_reaches_its_function() {
        let project = Project::new()
            .file(
                "src/helpers.php",
                "<?php\nfunction FormatPrice($amount) { return $amount; }\n",
            )
            .file(
                "src/view.php",
                "<?php\nfunction render() { return formatprice(10); }\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "src/view.php"),
            vec!["render calls FormatPrice".to_string()],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2235: an unqualified PHP class name is the current
    /// namespace's class or the one a `use` imports; PHP never falls back to
    /// another namespace for classes. koel's `extends Request` all went to the
    /// first `Request` indexed. Upstream's fixture.
    #[test]
    fn pr2235_a_bare_php_class_name_is_its_namespaces_or_the_imported_one() {
        let project = Project::new()
            .file(
                "app/Http/Requests/API/Interaction/Request.php",
                "<?php\n\nnamespace App\\Http\\Requests\\API\\Interaction;\n\nabstract class Request {}\n",
            )
            .file(
                "app/Http/Requests/API/Request.php",
                "<?php\n\nnamespace App\\Http\\Requests\\API;\n\nabstract class Request {}\n",
            )
            .file(
                "app/Http/Requests/API/DeleteSongsRequest.php",
                "<?php\n\nnamespace App\\Http\\Requests\\API;\n\nclass DeleteSongsRequest extends Request {}\n",
            )
            .file(
                "app/Http/Requests/API/Playlist/PlaylistSongUpdateRequest.php",
                "<?php\n\nnamespace App\\Http\\Requests\\API\\Playlist;\n\nuse App\\Http\\Requests\\API\\Request;\n\nclass PlaylistSongUpdateRequest extends Request {}\n",
            )
            .file(
                "app/Http/Integrations/SearchRequest.php",
                "<?php\n\nnamespace App\\Http\\Integrations;\n\nuse Saloon\\Http\\Request;\n\nclass SearchRequest extends Request {}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            edges_from(graph, "app/Http/Requests/API/DeleteSongsRequest.php"),
            vec!["DeleteSongsRequest extends App\\Http\\Requests\\API::Request".to_string()],
            "{report}"
        );
        assert_eq!(
            edges_from(
                graph,
                "app/Http/Requests/API/Playlist/PlaylistSongUpdateRequest.php"
            ),
            vec!["PlaylistSongUpdateRequest extends App\\Http\\Requests\\API::Request".to_string()],
            "{report}"
        );
        assert_eq!(
            edges_from(graph, "app/Http/Integrations/SearchRequest.php"),
            Vec::<String>::new(),
            "{report}"
        );
    }

    /// Upstream #2286 for a class body's trait list: `use Shared, Sub\Logs;`
    /// names `App\Shared` and `App\Sub\Logs`, never another namespace's traits
    /// of those names (the extractor records each entry as written).
    #[test]
    fn pr2286_a_php_trait_list_names_each_trait_as_written() {
        let project = Project::new()
            .file(
                "app/Shared.php",
                "<?php\nnamespace App;\n\ntrait Shared\n{\n}\n",
            )
            .file(
                "app/Sub/Logs.php",
                "<?php\nnamespace App\\Sub;\n\ntrait Logs\n{\n}\n",
            )
            .file(
                "lib/Logs.php",
                "<?php\nnamespace Lib;\n\ntrait Logs\n{\n}\n",
            )
            .file(
                "app/Page.php",
                "<?php\nnamespace App;\n\nclass Page\n{\n    use Shared,\n        Sub\\Logs;\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Page.php"),
            vec![
                "Page implements App::Shared".to_string(),
                "Page implements App\\Sub::Logs".to_string(),
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2286 (#2256): a PHP class written with a namespace in its
    /// name resolves to that class: through a `use … as Alias;` namespace
    /// alias, relative to the current namespace, or fully qualified, for
    /// `new`, `extends`, `implements` and static calls, and never to a
    /// same-named class elsewhere when the named one is outside the project.
    /// Upstream's fixtures.
    #[test]
    fn pr2286_a_php_class_written_with_a_namespace_resolves_to_that_class() {
        let fields = "<?php\nnamespace App\\Fields;\n\ninterface Field {}\nclass Base {}\nclass FirstName extends Base implements Field\n{\n    public static function make(): self { return new self(); }\n}\n";
        let first_name = |ns: &str| {
            format!(
                "<?php\nnamespace {ns};\n\nclass FirstName\n{{\n    public function label(): string {{ return 'First name'; }}\n}}\n"
            )
        };
        let consumer = |use_line: &str, expr: &str| {
            format!(
                "<?php\nnamespace App\\Blocks\\Personal;\n\n{use_line}\n\nclass Personal\n{{\n    public function fields(): array\n    {{\n        return [{expr}];\n    }}\n}}\n"
            )
        };
        // Through an alias, past a same-named class elsewhere, and deeper.
        let project = Project::new()
            .file(
                "app/Blocks/Personal/Fields/FirstName.php",
                &first_name("App\\Blocks\\Personal\\Fields"),
            )
            .file("app/Blocks/Other/FirstName.php", &first_name("App\\Blocks\\Other"))
            .file(
                "app/Blocks/Personal/Fields/Text/FirstName.php",
                &first_name("App\\Blocks\\Personal\\Fields\\Text"),
            )
            .file(
                "app/Blocks/Personal/Personal.php",
                &consumer(
                    "use App\\Blocks\\Personal\\Fields as Field;",
                    "new Field\\FirstName(), new Field\\Text\\FirstName(), new Elsewhere\\FirstName()",
                ),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Blocks/Personal/Personal.php"),
            vec![
                "fields instantiates App\\Blocks\\Personal\\Fields::FirstName".to_string(),
                "fields instantiates App\\Blocks\\Personal\\Fields\\Text::FirstName".to_string(),
            ],
            "{}",
            graph.report()
        );
        // `extends`, `implements` and a static call through an alias.
        let project = Project::new()
            .file("app/Fields/FirstName.php", fields)
            .file(
                "app/Personal.php",
                "<?php\nnamespace App;\n\nuse App\\Fields as F;\n\nclass Personal extends F\\Base implements F\\Field\n{\n    public function make() { return F\\FirstName::make(); }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Personal.php"),
            vec![
                "Personal extends App\\Fields::Base".to_string(),
                "Personal implements App\\Fields::Field".to_string(),
                "make calls App\\Fields::FirstName::make".to_string(),
            ],
            "{}",
            graph.report()
        );
        // Relative to the current namespace.
        let project = Project::new()
            .file("app/Fields/FirstName.php", fields)
            .file(
                "app/Personal.php",
                "<?php\nnamespace App;\n\nclass Personal extends Fields\\Base\n{\n    public function a() { return new Fields\\FirstName(); }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Personal.php"),
            vec![
                "Personal extends App\\Fields::Base".to_string(),
                "a instantiates App\\Fields::FirstName".to_string(),
            ],
            "{}",
            graph.report()
        );
        // Fully qualified, beside a same-named class of the current namespace.
        let project = Project::new()
            .file("app/Fields/FirstName.php", fields)
            .file("app/Other/FirstName.php", &first_name("App\\Other"))
            .file(
                "app/Personal.php",
                "<?php\nnamespace App\\Other;\n\nclass Personal extends \\App\\Fields\\Base\n{\n    public function a() { return new \\App\\Fields\\FirstName(); }\n    public function b() { return \\App\\Fields\\FirstName::make(); }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Personal.php"),
            vec![
                "Personal extends App\\Fields::Base".to_string(),
                "a instantiates App\\Fields::FirstName".to_string(),
                "b calls App\\Fields::FirstName::make".to_string(),
            ],
            "{}",
            graph.report()
        );
        // Outside the project: no same-named project node of any kind, a type
        // hint included, nor past a line break after `new` (KEEP-RUST: the
        // extractor records `FirstName` for each of them). A comment right
        // after `new` the extractor records as the class name itself.
        let project = Project::new()
            .file("app/Fields/FirstName.php", fields)
            .file(
                "app/helpers.php",
                "<?php\nnamespace App;\n\nfunction FirstName() {}\n\nenum Kind\n{\n    case FirstName;\n}\n",
            )
            .file(
                "app/Personal.php",
                "<?php\nnamespace App;\n\nuse Vendor\\Lib as L;\n\nclass Personal extends L\\Base\n{\n    public function a() { return new \\Vendor\\Lib\\FirstName(); }\n    public function b() { return L\\FirstName::make(); }\n    public function c(L\\FirstName $name) {}\n    public function d() { return [new /* outside */ L\\FirstName(), new\n        \\Vendor\\Lib\\FirstName()]; }\n}\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            edges_from(graph, "app/Personal.php"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }
}

mod lua {
    use super::support::{Graph, Project};

    /// `file: qualified_name` of every call edge out of `file`'s nodes, sorted
    /// (upstream's `callsFrom`).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{}: {}", n.file_path, n.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2143: a Lua `local` belongs to its chunk, another file can
    /// never name it; a global function stays reachable. kong's spec helpers'
    /// `local it = it` took busted's `it(…)` in every other spec file.
    /// Upstream's fixture.
    #[test]
    fn pr2143_a_lua_local_is_invisible_outside_its_file() {
        let project = Project::new()
            .file(
                "spec/helpers.lua",
                "local it = it\nlocal ipairs = ipairs\nlocal function helper() return 1 end\nfunction global_helper() return 2 end\nreturn {}\n",
            )
            .file(
                "spec/plugin_spec.lua",
                "describe(\"plugin\", function()\n  it(\"works\", function()\n    for _, v in ipairs({ 1 }) do helper(v) end\n    global_helper()\n  end)\nend)\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "spec/plugin_spec.lua"),
            vec!["spec/helpers.lua: global_helper".to_string()],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2202: a bare Lua call reaches neither a table's method nor,
    /// for a Lua global, a project namesake; a function's own alias of a host
    /// table's member still reaches the method of the table named after its
    /// holder. Upstream's fixture.
    #[test]
    fn pr2202_a_bare_lua_call_is_luas_global_never_a_tables_method() {
        let project = Project::new()
            .file(
                "lua/app/linked_list.lua",
                "local LinkedList = {}\n\nfunction LinkedList:ipairs()\n  return function() end\nend\n\nreturn LinkedList\n",
            )
            .file(
                "lua/app/response.lua",
                "local _RESPONSE = {}\n\nfunction _RESPONSE.error(status)\n  return status\nend\n\nreturn _RESPONSE\n",
            )
            .file(
                "lua/app/request.lua",
                "local _REQUEST = {}\n\nfunction _REQUEST.get_query()\n  return {}\nend\n\nreturn _REQUEST\n",
            )
            .file(
                "lua/app/handler.lua",
                "local M = {}\n\nfunction M.run(items)\n  for _, v in ipairs(items) do\n    if not v then\n      error(\"missing\")\n    end\n  end\n  local get_query = kong.request.get_query\n  return get_query()\nend\n\nreturn M\n",
            )
            .index();
        let graph = project.graph();
        let calls = calls_from(graph, "lua/app/handler.lua");
        let report = graph.report();
        assert!(
            !calls.iter().any(|c| c.ends_with("LinkedList::ipairs")),
            "{calls:?}\n{report}"
        );
        assert!(
            !calls.iter().any(|c| c.ends_with("_RESPONSE::error")),
            "{calls:?}\n{report}"
        );
        assert!(
            calls.iter().any(|c| c.ends_with("_REQUEST::get_query")),
            "{calls:?}\n{report}"
        );
    }

    /// Upstream #2202, the method half on its own: a bare call named like no
    /// Lua global (`render(…)` on a parameter) is no exact match for a
    /// table's method of its name, here two of them, the nearer beside the
    /// caller. A lone one is still the last-resort guess (fuzzy, 0.5), as
    /// upstream's fuzzy matcher has no Lua rule.
    #[test]
    fn pr2202_a_bare_lua_call_never_reaches_a_tables_method_of_its_name() {
        let project = Project::new()
            .file(
                "lua/app/component.lua",
                "local Component = {}\n\nfunction Component:render()\n  return 1\nend\n\nreturn Component\n",
            )
            .file(
                "lib/other/pure.lua",
                "local PureComponent = {}\n\nfunction PureComponent:render()\n  return 2\nend\n\nreturn PureComponent\n",
            )
            .file(
                "lua/app/forward.lua",
                "local function forward(render)\n  return render()\nend\n\nreturn forward\n",
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "lua/app/forward.lua"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2188: a bare Lua call through a `local` alias reaches what
    /// the alias names: a module's member, also one the module hands out
    /// under another name or re-exports from a module of its own; the
    /// standard library and an outside module are no edge. Upstream's
    /// fixture.
    #[test]
    fn pr2188_a_bare_lua_call_follows_a_local_alias() {
        let project = Project::new()
            .file(
                "kong/tools/string.lua",
                "local _M = {}\n\nfunction _M.splitn(value)\n  return value\nend\n\nreturn _M\n",
            )
            .file(
                "kong/pdk/phases.lua",
                "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
            )
            .file(
                "kong/handler.lua",
                "local type = type\nlocal fmt = string.format\nlocal splitn = require(\"kong.tools.string\").splitn\nlocal phase_checker = require \"kong.pdk.phases\"\nlocal check_phase = phase_checker.check\nlocal cjson_encode = require(\"cjson\").encode\n\nlocal function run(v)\n  check_phase(\"access\")\n  if type(v) ~= \"string\" then\n    error(fmt(\"bad %s\", v))\n  end\n  return splitn(v), cjson_encode(v)\nend\n\nreturn { run = run }\n",
            )
            .file(
                "spec/internal/misc.lua",
                "local function unindent(s)\n  return s\nend\n\nreturn { unindent = unindent }\n",
            )
            .file(
                "spec/helpers.lua",
                "local misc = require(\"spec.internal.misc\")\n\nreturn {\n  unindent = misc.unindent,\n}\n",
            )
            .file(
                "spec/handler_spec.lua",
                "local helpers = require \"spec.helpers\"\nlocal unindent = helpers.unindent\n\ndescribe(\"handler\", function()\n  it(\"runs\", function()\n    unindent(\"a\")\n  end)\nend)\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "kong/handler.lua"),
            vec![
                "kong/pdk/phases.lua: check_phase".to_string(),
                "kong/tools/string.lua: _M::splitn".to_string(),
            ],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "spec/handler_spec.lua"),
            vec!["spec/internal/misc.lua: unindent".to_string()],
            "{report}"
        );
    }
}

mod r {
    use super::support::{Graph, Project};

    /// `name@file` of every call edge out of `file`'s nodes, sorted
    /// (upstream's fixture helper).
    fn targets(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| graph.node(&e.target))
            .map(|n| format!("{}@{}", n.name, n.file_path))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2144: testthat runs each test file in an environment of its
    /// own, so a variable one test assigns is invisible to the package and to
    /// other tests; a `helper-*.R` (or `setup-*.R`) file is sourced for every
    /// test and stays shared, and a package function a factory makes stays
    /// callable. Upstream's fixture, plus a second test file calling a name
    /// the first assigns at its top level.
    #[test]
    fn pr2144_an_r_test_files_top_level_variables_stay_in_that_file() {
        let project = Project::new()
            .file("DESCRIPTION", "Package: demo\n")
            .file(
                "R/geom-point.R",
                "geom_point <- make_constructor(GeomPoint)\ntable_of <- function() {\n  c(\"a\", \"b\")\n}\n",
            )
            .file(
                "tests/testthat/helper-s3.R",
                "quux <- structure(list(), class = \"quux\")\n",
            )
            .file(
                "tests/testthat/setup-data.R",
                "rows <- data.frame(x = 1)\n",
            )
            .file(
                "tests/testthat/test-scales.R",
                "c <- ggplot(df, aes(x, y))\nfixture <- data.frame(y = 2)\ntest_that(\"works\", {\n  geom_point()\n  quux()\n})\n",
            )
            .file(
                "tests/testthat/test-other.R",
                "test_that(\"other\", {\n  fixture()\n  rows()\n})\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            targets(graph, "R/geom-point.R"),
            Vec::<String>::new(),
            "{report}"
        );
        assert_eq!(
            targets(graph, "tests/testthat/test-scales.R"),
            vec![
                "geom_point@R/geom-point.R".to_string(),
                "quux@tests/testthat/helper-s3.R".to_string(),
            ],
            "{report}"
        );
        assert_eq!(
            targets(graph, "tests/testthat/test-other.R"),
            vec!["rows@tests/testthat/setup-data.R".to_string()],
            "{report}"
        );
    }

    /// Upstream #2239: R looks a call's name up among functions only, so a
    /// data binding that shadows a base function (`c <- data_frame(b = 3)`)
    /// is never what `c(1, 2)` calls; a project binding a factory makes
    /// (`geom_point <- make_constructor(…)`) stays. Upstream's fixture.
    #[test]
    fn pr2239_an_r_call_skips_a_data_binding_shadowing_a_base_function() {
        let project = Project::new()
            .file("DESCRIPTION", "Package: ggplot2\n")
            .file(
                "R/geom-point.R",
                "geom_point <- make_constructor(GeomPoint, position = \"identity\")\n",
            )
            .file(
                "tests/testthat/test-facet-wrap.R",
                "a <- data_frame(a = 1:3)\nc <- data_frame(b = 3)\n\ntest_that(\"facets\", {\n  p <- geom_point()\n  expect_equal(c(1, 2), a$a)\n})\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let names: Vec<String> = targets(graph, "tests/testthat/test-facet-wrap.R")
            .into_iter()
            .map(|t| t.split('@').next().unwrap_or_default().to_string())
            .collect();
        assert!(!names.contains(&"c".to_string()), "{names:?}\n{report}");
        assert!(
            names.contains(&"geom_point".to_string()),
            "{names:?}\n{report}"
        );
    }

    /// Upstream #2177, the R part: a plain call is a function call, never an
    /// object's method: base R's `range(x)` is no ggproto Coord's `range`.
    /// Upstream's fixture.
    #[test]
    fn pr2177_a_bare_r_call_is_a_function_not_an_objects_method() {
        let project = Project::new()
            .file(
                "R/coord.R",
                "Coord <- ggproto(\"Coord\",\n  range = function(panel_params) {\n    list(x = panel_params$x$dimension())\n  }\n)\n\nscale_limits <- function(x) {\n  range(x)\n}\n",
            )
            .index();
        let graph = project.graph();
        let calls: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "R/coord.R")
            })
            .filter_map(|e| graph.node(&e.target))
            .map(|n| n.qualified_name.clone())
            .filter(|q| q.ends_with("range"))
            .collect();
        assert_eq!(calls, Vec::<String>::new(), "{}", graph.report());
    }
}

mod dart_scope {
    use super::support::{Graph, Project};

    /// `from -> Owner::name` of every call edge out of `file`, sorted
    /// (upstream's fixture helper).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                let target = graph.node(&e.target)?;
                Some(format!("{} -> {}", source.name, target.qualified_name))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2145: a bare Dart call reaches a method only through the
    /// class it is written in (its members, what it extends, mixes in or
    /// implements, or an extension on one of those), the nearest one winning;
    /// outside any class, none. Upstream's fixture, plus an override in
    /// another file nearer than the method it overrides beside the call, and
    /// a chain whose links are written below its start.
    #[test]
    fn pr2145_a_bare_dart_call_stays_in_the_class_hierarchy() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/handler.dart",
                "class TestHandler {\n  void expect(Object value) {}\n  void test(String name) {}\n}\n\nabstract class HttpHeaders {\n  void add(String name, Object value);\n  void clear();\n}\n",
            )
            .file(
                "lib/bloc.dart",
                "abstract class Emittable<S> {\n  void emit(S state);\n}\n\nabstract class BlocBase<S> implements Emittable<S> {\n  @override\n  void emit(S state) {}\n}\n\nabstract class Cubit<S> extends BlocBase<S> {}\n\nclass CounterCubit extends Cubit<int> {\n  void increment() => emit(1);\n}\n",
            )
            .file(
                "lib/notifier.dart",
                "class Listenable {}\n\nmixin SingleMixin on Listenable {\n  void refresh() {}\n}\n\nclass ListNotifier extends Listenable {}\n\nclass ListNotifierSingle = ListNotifier with SingleMixin;\n\nclass RxList extends ListNotifierSingle {\n  void add(Object item) {\n    refresh();\n  }\n}\n",
            )
            .file(
                "lib/element.dart",
                "abstract class AnyNotifier<S> {}\n\nextension NotifierX<S> on AnyNotifier<S> {\n  void requireElement() {}\n}\n\nclass Notifier<\n  // the state\n  S\n> //\n    extends\n        AnyNotifier<\n          S\n        > {\n  void read() {\n    requireElement();\n  }\n}\n",
            )
            .file(
                "lib/list_ext.dart",
                "extension ListExtension<E> on List<E> {\n  void addIf(bool condition, E item) {\n    if (condition) add(item);\n  }\n}\n",
            )
            .file(
                "lib/login.dart",
                "class LoginState {\n  LoginState withEmail(String e) => this;\n  LoginState withPassword(String p) => this;\n}\n\nLoginState build() => LoginState().withEmail('a').withPassword('b');\n\nLoginState spread() => LoginState()\n    .withEmail('a')\n    .withPassword('b');\n",
            )
            .file(
                "test/handler_test.dart",
                "import 'package:test/test.dart';\n\nvoid main() {\n  test('works', () {\n    expect(1, 1);\n  });\n}\n",
            )
            .file(
                "lib/override.dart",
                "class Root {\n  void notify(int x) {}\n}\n\nclass Leaf extends Middle {\n  void run() {\n    notify(1);\n  }\n}\n",
            )
            .file(
                "lib/middle.dart",
                "class Middle extends Root {\n  @override\n  void notify(int x) {}\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "test/handler_test.dart"),
            Vec::<String>::new(),
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/bloc.dart"),
            vec!["increment -> BlocBase::emit".to_string()],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/notifier.dart"),
            vec!["add -> SingleMixin::refresh".to_string()],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/element.dart"),
            vec!["read -> NotifierX::requireElement".to_string()],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/list_ext.dart"),
            Vec::<String>::new(),
            "{report}"
        );
        assert!(
            calls_from(graph, "lib/login.dart")
                .contains(&"build -> LoginState::withPassword".to_string()),
            "{report}"
        );
        // A chain spread over lines keeps its links' methods too.
        let login = calls_from(graph, "lib/login.dart");
        for link in ["withEmail", "withPassword"] {
            assert!(
                login.contains(&format!("spread -> LoginState::{link}")),
                "{login:?}\n{report}"
            );
        }
        assert_eq!(
            calls_from(graph, "lib/override.dart"),
            vec!["run -> Middle::notify".to_string()],
            "{report}"
        );
    }

    /// A call whose receiver's name begins with the called name is no bare
    /// call: this extractor records `ticker.tick()` written in another call's
    /// arguments at the chain's start, `ticker`, so the receiver-less reader
    /// takes the name as a whole word (KEEP-RUST adaptation of #2145's
    /// reader, found by W4-DART-02's precision run on bloc's flutter_timer
    /// tests).
    #[test]
    fn pr2145_a_call_whose_receiver_begins_with_its_name_is_no_bare_call() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/ticker.dart", "class Ticker {\n  void tick() {}\n}\n")
            .file(
                "lib/main.dart",
                "import 'ticker.dart';\n\nvoid run(Ticker ticker) {\n  when(() => ticker.tick()).then();\n}\n\nvoid when(Object f) {}\n",
            )
            .index();
        let graph = project.graph();
        assert!(
            calls_from(graph, "lib/main.dart").contains(&"run -> Ticker::tick".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2207: a Dart core method name on an untyped receiver is the
    /// core type's unless the receiver names the owner, and an extension's
    /// owner is the type it is `on`: getx's `ext.endsWith(".avi")` is no
    /// `extension RxStringExt on Rx<String>`'s. Upstream's fixture.
    #[test]
    fn pr2207_a_dart_core_method_name_on_an_untyped_receiver_is_the_core_types() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/rx_string.dart",
                "class Rx<T> {\n  Rx(this.value);\n  T value;\n}\n\nextension RxStringExt on Rx<String> {\n  bool endsWith(String other) => value.endsWith(other);\n}\n\nextension RxnStringExt on Rx<String?> {\n  bool endsWith(String other) => false;\n}\n",
            )
            .file(
                "lib/get_utils.dart",
                "class GetUtils {\n  static bool isVideo(String filePath) {\n    var ext = filePath.toLowerCase();\n    return ext.endsWith(\".avi\");\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let targets: Vec<String> = calls_from(graph, "lib/get_utils.dart");
        assert!(
            !targets.iter().any(|t| t.ends_with("StringExt::endsWith")),
            "{targets:?}\n{}",
            graph.report()
        );
    }

    /// Upstream #2237: an unnamed `extension on X` is never `X` (here the
    /// extractor names it `<anonymous>`) and applies only in its own
    /// library: flutter_test's `find.text(…)` in another package is no
    /// `extension on TaskStatus { String get text }`. Upstream's fixture,
    /// its second check asked of the extension's file; plus a bare call of
    /// such an extension's member from a subclass in another file, which the
    /// class-hierarchy scope (#2145) alone would admit.
    #[test]
    fn pr2237_an_unnamed_dart_extension_applies_only_in_its_library() {
        let project = Project::new()
            .file("pubspec.yaml", "name: bloc_lint\n")
            .file(
                "lib/src/rules/avoid_build_context_extensions.dart",
                "import 'package:analyzer/dart/ast/token.dart';\n\nextension on Token {\n  bool get isBlocType => lexeme.endsWith('Bloc');\n}\n",
            )
            .file(
                "lib/src/linter.dart",
                "import 'package:analyzer/dart/ast/token.dart';\n\nclass Linter {\n  void visit(Token beginToken) {}\n}\n",
            )
            .file(
                "examples/visualizer/lib/timeline_page.dart",
                "enum TaskStatus { queued, done }\n\nextension on TaskStatus {\n  String get text => 'x';\n}\n\nString label(TaskStatus status) => status.text;\n",
            )
            .file(
                "examples/counter/test/counter_page_test.dart",
                "import 'package:flutter_test/flutter_test.dart';\n\nvoid main() {\n  testWidgets('shows', (tester) async {\n    expect(find.text('0'), findsOneWidget);\n  });\n}\n",
            )
            .file(
                "lib/src/foo.dart",
                "class Foo {}\n\nextension on Foo {\n  void helper() {}\n}\n\nclass Near extends Foo {\n  void run() {\n    helper();\n  }\n}\n",
            )
            .file(
                "lib/src/bar.dart",
                "import 'foo.dart';\n\nclass Bar extends Foo {\n  void run() {\n    helper();\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let targets_from = |file: &str| -> Vec<String> {
            graph
                .edges
                .iter()
                .filter(|e| e.kind != "contains")
                .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
                .filter_map(|e| graph.node(&e.target))
                .map(|t| format!("{}:{}", t.file_path, t.qualified_name))
                .collect()
        };
        let report = graph.report();
        assert!(
            !targets_from("lib/src/linter.dart")
                .iter()
                .any(|t| t.starts_with("lib/src/rules/avoid_build_context_extensions.dart:")),
            "{report}"
        );
        assert!(
            !targets_from("examples/counter/test/counter_page_test.dart")
                .iter()
                .any(|t| t.starts_with("examples/visualizer/lib/timeline_page.dart:")),
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/src/foo.dart"),
            vec!["run -> <anonymous>::helper".to_string()],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/src/bar.dart"),
            Vec::<String>::new(),
            "{report}"
        );
    }
}

/// Upstream #2386, #2388 and #2391: a Dart library is a file and its parts;
/// a name written without a receiver reaches only what that library declares
/// or what a library it imports exports, and an `import`, `export` or `part`
/// links the file its URI names.
mod dart_libraries {
    use super::support::{self, Graph, Project};

    /// `<kind> <line> -> <target file>:<target qualified name>` for every
    /// edge out of `file` but `contains`, sorted (upstream's `edgesFrom`,
    /// without the reference name, which an edge does not keep here).
    fn edges_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind != "contains")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| {
                let target = graph.node(&e.target)?;
                Some(format!(
                    "{} {} -> {}:{}",
                    e.kind,
                    e.line.unwrap_or_default(),
                    target.file_path,
                    target.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// `<line> -> <target>` for every `imports` edge out of `file`, a file by
    /// its path and anything else by kind, file and name (upstream's
    /// `importsFrom`), sorted.
    fn imports_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "imports")
            .filter(|e| graph.node(&e.source).is_some_and(|n| n.file_path == file))
            .filter_map(|e| {
                let target = graph.node(&e.target)?;
                let named = if target.kind == "file" {
                    target.file_path.clone()
                } else {
                    format!("{} {}#{}", target.kind, target.file_path, target.name)
                };
                Some(format!("{} -> {named}", e.line.unwrap_or_default()))
            })
            .collect();
        out.sort();
        out
    }

    /// The URIs of `file`'s imports left unresolved, sorted.
    fn unresolved_imports_of(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .unresolved
            .iter()
            .filter(|r| r.file_path == file && r.reference_kind == "imports")
            .map(|r| r.reference_name.clone())
            .collect();
        out.sort();
        out
    }

    /// `<name> | <signature>` of each import node in `file`, sorted.
    fn import_nodes_in(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == file && n.kind == "import")
            .map(|n| format!("{} | {}", n.name, n.signature.clone().unwrap_or_default()))
            .collect();
        out.sort();
        out
    }

    /// Upstream #2386: a receiver-less Dart call, constructor call or type
    /// name links only to a top-level declaration its library can see — its
    /// own (the file and its parts) or one a library it imports without a
    /// prefix exports, through `export` chains and `show` / `hide` — and a
    /// type written through an import prefix (`p.Report`) is what that import
    /// exports. A library's own declaration shadows an imported one, and a
    /// bare call never reaches another type's enum constant or static
    /// constant. Upstream's fixture. Its field-type case (`final Box<int>
    /// box;`, #2360) and its typed receiver's call into an unnamed extension
    /// in another part (`s.area()`) came with W4-DART-03; the unnamed
    /// extension's reach across its library is checked through a bare call
    /// too.
    #[test]
    fn pr2386_a_bare_dart_name_reaches_only_what_its_library_can_see() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "test/integration/annotated.dart",
                "part 'annotated.g.dart';\n\nString family(Object ref, int id) => 'family $id';\n",
            )
            .file(
                "test/integration/annotated.g.dart",
                "part of 'annotated.dart';\n\nString annotatedGenerated(Object ref) => family(ref, 1);\n",
            )
            .file(
                "test/integration/async.dart",
                "import 'dart:async';\n\npart 'async.g.dart';\n\nFuture<String> family(Object ref, int first) async => '$first';\n\nclass GenericClass {}\n",
            )
            .file(
                "test/integration/async.g.dart",
                "part of 'async.dart';\n\nFuture<String> asyncGenerated(Object ref) {\n  return family(ref, 2);\n}\n\nfinal GenericClass generic = GenericClass();\n",
            )
            .file("test/integration/annotated_types.dart", "class GenericClass {}\n")
            .file(
                "test/third_party/fake_async.dart",
                "T fakeAsync<T>(T Function(Object async) callback) => callback(Object());\n",
            )
            .file(
                "test/src/core/devtool_test.dart",
                "import 'package:fake_async/fake_async.dart';\n\nvoid main() {\n  fakeAsync((async) {});\n}\n",
            )
            .file("test/old/utils.dart", "List<Object> errorsOf(void Function() cb) => [];\n")
            .file("test/src/utils.dart", "List<Object> errorsOf(void Function() cb) => [];\n")
            .file(
                "test/feature/uni_directional_test.dart",
                "import '../src/utils.dart';\n\nvoid main() {\n  errorsOf(() {});\n}\n",
            )
            .file("packages/kit/pubspec.yaml", "name: kit\n")
            .file(
                "packages/kit/lib/kit.dart",
                "export 'src/impl.dart'\n    show\n        // the public API\n        shown,\n        Widget;\nexport 'src/cond_stub.dart' if (dart.library.io) 'src/cond_io.dart';\n",
            )
            .file(
                "packages/kit/lib/src/impl.dart",
                "int shown() => 1;\nint notShown() => 2;\nclass Widget {}\n",
            )
            .file("packages/kit/lib/src/cond_stub.dart", "int platformName() => 0;\n")
            .file("packages/kit/lib/src/cond_io.dart", "int platformName() => 1;\n")
            .file(
                "packages/kit/lib/src/prefixed.dart",
                "int viaPrefix() => 3;\nclass Report {}\n",
            )
            .file(
                "packages/kit/lib/src/hidden.dart",
                "int hiddenOne() => 4;\nint keptOne() => 5;\n",
            )
            .file(
                "lib/main.dart",
                "import 'package:kit/kit.dart';\nimport 'package:kit/src/prefixed.dart' as p;\nimport 'package:kit/src/hidden.dart' hide hiddenOne;\n\nvoid run(p.Report report) {\n  shown();\n  notShown();\n  platformName();\n  viaPrefix();\n  hiddenOne();\n  keptOne();\n  Widget();\n}\n",
            )
            .file("lib/a_report.dart", "class Report {}\n")
            .file(
                "lib/named.dart",
                "library app.named;\n\npart 'named_part.dart';\n\nint namedHelper() => 6;\n",
            )
            .file(
                "lib/named_part.dart",
                "part of app.named;\n\nint usesNamed() => namedHelper();\n",
            )
            .file("lib/other_named.dart", "int namedHelper() => 7;\n")
            .file("lib/a_util.dart", "int helper() => 1;\n")
            .file(
                "lib/feature.dart",
                "import 'a_util.dart';\n\npart 'feature_part.dart';\n\nint runFeature() => helper();\n",
            )
            .file("lib/feature_part.dart", "part of 'feature.dart';\n\nint helper() => 2;\n")
            .file(
                "lib/kinds.dart",
                "enum Level { info, warning }\n\nclass TypeChecker {\n  static const allOf = 'any';\n}\n",
            )
            .file(
                "lib/check.dart",
                "import 'package:mason_logger/mason_logger.dart';\nimport 'package:matcher/matcher.dart';\n\nvoid check(Object value) {\n  info('checking');\n  expect(value, allOf([isNotNull]));\n}\n",
            )
            .file(
                "lib/colors.dart",
                "import 'package:flutter/material.dart';\n\nextension on Color {\n  Color brighten() => this;\n}\n",
            )
            .file("lib/models/box.dart", "class Box<T> {\n  T? value;\n}\n")
            .file(
                "lib/widgets/box_ext.dart",
                "import '../models/box.dart';\n\nextension<T> on Box<T> {\n  bool get isEmpty => value == null;\n}\n",
            )
            .file(
                "lib/widgets/panel.dart",
                "import '../models/box.dart';\nimport 'box_ext.dart';\n\nclass Panel {\n  final Box<int> box;\n  Panel(this.box);\n}\n",
            )
            .file(
                "lib/shapes/shapes.dart",
                "part 'area_ext.dart';\npart 'use.dart';\n\nclass Shape {}\n",
            )
            .file(
                "lib/shapes/area_ext.dart",
                "part of 'shapes.dart';\n\nextension on Shape {\n  int area() => 1;\n}\n",
            )
            .file(
                "lib/shapes/use.dart",
                "part of 'shapes.dart';\n\nint total(Shape s) => s.area();\n\nclass Square extends Shape {\n  int twice() => area() * 2;\n}\n",
            )
            .file(
                "lib/circle.dart",
                "import 'shapes/shapes.dart';\n\nclass Circle extends Shape {\n  int twice() => area() * 2;\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();

        // A generated part calls and names its own library's declarations.
        let generated = edges_from(graph, "test/integration/async.g.dart");
        assert!(
            generated.contains(&"calls 4 -> test/integration/async.dart:family".to_string()),
            "{generated:?}\n{report}"
        );
        assert!(
            !generated.iter().any(|e| e.contains("annotated")),
            "{generated:?}\n{report}"
        );
        let generic: Vec<&String> = generated
            .iter()
            .filter(|e| e.contains("GenericClass"))
            .collect();
        assert!(!generic.is_empty(), "{generated:?}\n{report}");
        assert!(
            generic
                .iter()
                .all(|e| e.ends_with("test/integration/async.dart:GenericClass")),
            "{generated:?}\n{report}"
        );
        // A package function no imported library declares links nothing.
        assert!(
            !edges_from(graph, "test/src/core/devtool_test.dart")
                .iter()
                .any(|e| e.contains("fake_async")),
            "{report}"
        );
        // The imported library, not a namesake no import names.
        assert!(
            edges_from(graph, "test/feature/uni_directional_test.dart")
                .contains(&"calls 4 -> test/src/utils.dart:errorsOf".to_string()),
            "{report}"
        );
        // Export chains, `show`, `hide` and import prefixes.
        let main = edges_from(graph, "lib/main.dart");
        for wanted in [
            "calls 6 -> packages/kit/lib/src/impl.dart:shown",
            "calls 11 -> packages/kit/lib/src/hidden.dart:keptOne",
            "instantiates 12 -> packages/kit/lib/src/impl.dart:Widget",
        ] {
            assert!(
                main.contains(&wanted.to_string()),
                "{wanted}\n{main:?}\n{report}"
            );
        }
        assert!(
            main.iter()
                .any(|e| e.starts_with("calls 8 -> packages/kit/lib/src/cond_")
                    && e.ends_with(".dart:platformName")),
            "{main:?}\n{report}"
        );
        assert!(
            !main.iter().any(|e| e.ends_with(":notShown")
                || e.ends_with(":viaPrefix")
                || e.ends_with(":hiddenOne")),
            "{main:?}\n{report}"
        );
        assert_eq!(
            main.iter()
                .filter(|e| e.ends_with(":Report"))
                .cloned()
                .collect::<Vec<_>>(),
            vec!["references 5 -> packages/kit/lib/src/prefixed.dart:Report".to_string()],
            "{report}"
        );
        // A part names its library by the library's name.
        assert!(
            edges_from(graph, "lib/named_part.dart")
                .contains(&"calls 3 -> lib/named.dart:namedHelper".to_string()),
            "{report}"
        );
        // The library's own declaration shadows the imported one.
        assert_eq!(
            edges_from(graph, "lib/feature.dart")
                .into_iter()
                .filter(|e| e.ends_with(":helper"))
                .collect::<Vec<_>>(),
            vec!["calls 5 -> lib/feature_part.dart:helper".to_string()],
            "{report}"
        );
        // No bare call reaches another type's enum constant or static constant.
        assert_eq!(
            edges_from(graph, "lib/check.dart")
                .into_iter()
                .filter(|e| !e.starts_with("imports "))
                .collect::<Vec<_>>(),
            Vec::<String>::new(),
            "{report}"
        );
        // An unnamed extension on Color is no Color, nor a generic one on
        // `Box<T>` a `Box` (a field's type, #2360).
        assert!(
            !edges_from(graph, "lib/colors.dart")
                .iter()
                .any(|e| e.contains("Color")),
            "{report}"
        );
        assert_eq!(
            edges_from(graph, "lib/widgets/panel.dart")
                .into_iter()
                .filter(|e| e.ends_with(":Box"))
                .collect::<Vec<_>>(),
            vec!["references 5 -> lib/models/box.dart:Box".to_string()],
            "{report}"
        );
        // An unnamed extension applies in every file of its library, and in
        // no other library.
        for edge in [
            "calls 3 -> lib/shapes/area_ext.dart:<anonymous>::area",
            "calls 6 -> lib/shapes/area_ext.dart:<anonymous>::area",
        ] {
            assert!(
                edges_from(graph, "lib/shapes/use.dart").contains(&edge.to_string()),
                "{edge}\n{report}"
            );
        }
        assert!(
            !edges_from(graph, "lib/circle.dart")
                .iter()
                .any(|e| e.ends_with("::area")),
            "{report}"
        );
    }

    /// Upstream #2386: a type written through an import prefix is what that
    /// import exports, though a library imported without a prefix exports a
    /// type of that name too; the type written without the prefix is that
    /// library's.
    #[test]
    fn pr2386_a_prefixed_dart_type_is_what_its_prefix_imports() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/reports.dart", "class Report {}\n")
            .file("lib/other_reports.dart", "class Report {}\n")
            .file(
                "lib/use.dart",
                "import 'reports.dart';\nimport 'other_reports.dart' as p;\n\nvoid run(p.Report a, Report b) {}\n",
            )
            .index();
        let graph = project.graph();
        let mut columns: Vec<(i64, String)> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "references")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "lib/use.dart")
            })
            .filter_map(|e| {
                let target = graph.node(&e.target)?;
                Some((e.col.unwrap_or_default(), target.file_path.clone()))
            })
            .collect();
        columns.sort();
        assert_eq!(
            columns,
            vec![
                (11, "lib/other_reports.dart".to_string()),
                (21, "lib/reports.dart".to_string()),
            ],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2386: a Dart file in no package — a mason brick's template,
    /// whose `package:{{name}}/…` imports no pubspec.yaml explains — is
    /// resolved as before, by name.
    #[test]
    fn pr2386_dart_files_outside_any_package_resolve_by_name() {
        let project = Project::new()
            .file(
                "bricks/feature/__brick__/helper.dart",
                "int helper() => 1;\n",
            )
            .file(
                "bricks/feature/__brick__/page.dart",
                "import 'package:{{name}}/helper.dart';\n\nint page() => helper();\n",
            )
            .index();
        let graph = project.graph();
        assert!(
            edges_from(graph, "bricks/feature/__brick__/page.dart")
                .contains(&"calls 3 -> bricks/feature/__brick__/helper.dart:helper".to_string()),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2388: an `import` or `export` links the one project file its
    /// URI names — `package:<name>/<path>` is `<root>/lib/<path>` of the
    /// package whose pubspec.yaml says `name: <name>`, anything else a path
    /// from the importing file — or nothing: a `dart:` library, a package
    /// outside the project, a file the project does not have. Same-named
    /// packages are told apart by the importer's own package, then by its
    /// `path:` dependency. Upstream's fixture.
    #[test]
    fn pr2388_a_dart_import_links_the_file_its_uri_names() {
        let project = Project::new()
            .file("packages/riverpod/pubspec.yaml", "name: riverpod\n")
            .file(
                "packages/riverpod/lib/riverpod.dart",
                "export 'src/core/foundation.dart';\nexport 'package:meta/meta.dart' show internal;\n",
            )
            .file("packages/riverpod/lib/src/core/foundation.dart", "class ProviderBase {}\n")
            .file(
                "packages/riverpod/lib/src/core/container.dart",
                "import 'dart:async';\n\nimport 'package:meta/meta.dart';\n\nimport 'foundation.dart';\n\nclass Container extends ProviderBase {}\n",
            )
            .file("examples/pub/pubspec.yaml", "name: pub\n")
            .file(
                "examples/pub/lib/pub_repository.dart",
                "import 'package:flutter/foundation.dart';\nimport 'package:riverpod/riverpod.dart';\nimport 'package:riverpod/src/core/foundation.dart' as core;\n\nclass PubRepository {}\n",
            )
            .file("packages/bloc_tools/pubspec.yaml", "name: bloc_tools\n")
            .file(
                "packages/bloc_tools/lib/src/command_runner.dart",
                "import 'package:args/command_runner.dart';\n\nclass BlocToolsCommandRunner {}\n",
            )
            .file("examples/flutter_counter/pubspec.yaml", "name: flutter_counter\n")
            .file(
                "examples/flutter_counter/integration_test/app_test.dart",
                "import 'package:integration_test/integration_test.dart';\n\nvoid main() {}\n",
            )
            .file(
                "examples/flutter_counter/test_driver/integration_test.dart",
                "import 'package:integration_test/integration_test_driver.dart';\n\nFuture<void> main() async {}\n",
            )
            .file("pubspec.yaml", "name: app\n")
            .file("test/src/utils.dart", "int errorsOf() => 0;\n")
            .file("test/old/utils.dart", "int errorsOf() => 0;\n")
            .file(
                "test/feature/uni_directional_test.dart",
                "import '../src/utils.dart';\n\nvoid main() {}\n",
            )
            .file("website/docs/cancel/detail_screen/codegen.dart", "class DetailScreen {}\n")
            .file(
                "website/i18n/it/cancel/home_screen.dart",
                "import 'detail_screen/codegen.dart';\n\nclass HomeScreen {}\n",
            )
            .file(
                "examples/login_a/pubspec.yaml",
                "name: login_a\ndependencies:\n  auth_repo:\n    # the app's own copy\n    path: packages/auth_repo\n  bloc: ^9.0.0\n",
            )
            .file("examples/login_a/packages/auth_repo/pubspec.yaml", "name: auth_repo\n")
            .file(
                "examples/login_a/packages/auth_repo/lib/auth_repo.dart",
                "class AuthRepo {}\n",
            )
            .file(
                "examples/login_a/lib/app.dart",
                "import 'package:auth_repo/auth_repo.dart';\n\nclass AppA {}\n",
            )
            .file(
                "examples/login_b/pubspec.yaml",
                "name: login_b\ndependencies:\n  auth_repo: { path: \"packages/auth_repo\" }\n",
            )
            .file("examples/login_b/packages/auth_repo/pubspec.yaml", "name: auth_repo\n")
            .file(
                "examples/login_b/packages/auth_repo/lib/auth_repo.dart",
                "class AuthRepo {}\n",
            )
            .file(
                "examples/login_b/lib/app.dart",
                "import 'package:auth_repo/auth_repo.dart';\n\nclass AppB {}\n",
            )
            .file("examples/login_c/pubspec.yaml", "name: login_c\n")
            .file(
                "examples/login_c/lib/app.dart",
                "import 'package:auth_repo/auth_repo.dart';\n\nclass AppC {}\n",
            )
            .file("packages/a/example/pubspec.yaml", "name: example\n")
            .file(
                "packages/a/example/lib/main.dart",
                "import 'package:example/src/screen.dart';\n\nvoid main() {}\n",
            )
            .file("packages/a/example/lib/src/screen.dart", "class ScreenA {}\n")
            .file("packages/b/example/pubspec.yaml", "name: example\n")
            .file("packages/b/example/lib/src/screen.dart", "class ScreenB {}\n")
            .file(
                "packages/riverpod/lib/src/missing_user.dart",
                "import 'package:riverpod/src/core/foundation.g.dart';\nimport 'package:riverpod/src/missing_user.dart';\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let none = Vec::<String>::new();
        assert_eq!(
            imports_from(graph, "examples/pub/lib/pub_repository.dart"),
            vec![
                "2 -> packages/riverpod/lib/riverpod.dart".to_string(),
                "3 -> packages/riverpod/lib/src/core/foundation.dart".to_string(),
            ],
            "{report}"
        );
        assert_eq!(
            unresolved_imports_of(graph, "examples/pub/lib/pub_repository.dart"),
            vec!["package:flutter/foundation.dart".to_string()],
            "{report}"
        );
        for file in [
            "packages/bloc_tools/lib/src/command_runner.dart",
            "examples/flutter_counter/integration_test/app_test.dart",
            "examples/flutter_counter/test_driver/integration_test.dart",
            "website/i18n/it/cancel/home_screen.dart",
            "packages/riverpod/lib/src/missing_user.dart",
            "examples/login_c/lib/app.dart",
        ] {
            assert_eq!(imports_from(graph, file), none, "{file}\n{report}");
        }
        assert_eq!(
            imports_from(graph, "packages/riverpod/lib/src/core/container.dart"),
            vec!["5 -> packages/riverpod/lib/src/core/foundation.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            unresolved_imports_of(graph, "packages/riverpod/lib/src/core/container.dart"),
            vec![
                "dart:async".to_string(),
                "package:meta/meta.dart".to_string()
            ],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "packages/riverpod/lib/riverpod.dart"),
            vec!["1 -> packages/riverpod/lib/src/core/foundation.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "test/feature/uni_directional_test.dart"),
            vec!["1 -> test/src/utils.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "examples/login_a/lib/app.dart"),
            vec!["1 -> examples/login_a/packages/auth_repo/lib/auth_repo.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "examples/login_b/lib/app.dart"),
            vec!["1 -> examples/login_b/packages/auth_repo/lib/auth_repo.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "packages/a/example/lib/main.dart"),
            vec!["1 -> packages/a/example/lib/src/screen.dart".to_string()],
            "{report}"
        );
    }

    /// Upstream #2391: `part '<uri>';` mints an import node and links the
    /// library's file to the part's by the URI rules an import follows, so a
    /// change to a part reaches what depends on its library; `part of`
    /// records nothing. Upstream's fixture (its viewer rails are W7's).
    #[test]
    fn pr2391_a_dart_library_links_the_files_its_parts_name() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/src/framework.dart",
                "import 'dart:async';\n\nimport 'package:meta/meta.dart';\n\npart 'core/element.dart';\npart \"core/container.dart\";\npart 'framework.g.dart';\n",
            )
            .file(
                "lib/src/core/element.dart",
                "part of '../framework.dart';\n\nclass Element {}\n",
            )
            .file(
                "lib/src/core/container.dart",
                "part of '../framework.dart';\n\nclass Container {}\n",
            )
            .file(
                "lib/src/framework.g.dart",
                "// GENERATED CODE - DO NOT MODIFY BY HAND\n\npart of 'framework.dart';\n\nString _$frameworkHash() => 'abc';\n",
            )
            .file("lib/app.dart", "export 'src/framework.dart';\n")
            .file(
                "test/framework_test.dart",
                "import 'package:app/app.dart';\n\nvoid main() {}\n",
            )
            .file("packages/kit/pubspec.yaml", "name: kit\n")
            .file("packages/kit/lib/kit.dart", "part 'package:kit/src/widgets.dart';\n")
            .file(
                "packages/kit/lib/src/widgets.dart",
                "part of 'package:kit/kit.dart';\n\nclass Widget {}\n",
            )
            .file("lib/legacy/models.dart", "library app.models;\n\npart 'user.dart';\n")
            .file("lib/legacy/user.dart", "part of app.models;\n\nclass User {}\n")
            .file("examples/a/pubspec.yaml", "name: a\n")
            .file("examples/a/lib/main.dart", "part 'main.g.dart';\n\nvoid main() {}\n")
            .file("examples/b/pubspec.yaml", "name: b\n")
            .file("examples/b/lib/main.dart", "part 'main.g.dart';\n\nvoid main() {}\n")
            .file("examples/b/lib/main.g.dart", "part of 'main.dart';\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let none = Vec::<String>::new();
        assert_eq!(
            imports_from(graph, "lib/src/framework.dart"),
            vec![
                "5 -> lib/src/core/element.dart".to_string(),
                "6 -> lib/src/core/container.dart".to_string(),
                "7 -> lib/src/framework.g.dart".to_string(),
            ],
            "{report}"
        );
        assert_eq!(
            unresolved_imports_of(graph, "lib/src/framework.dart"),
            vec![
                "dart:async".to_string(),
                "package:meta/meta.dart".to_string()
            ],
            "{report}"
        );
        assert_eq!(
            import_nodes_in(graph, "lib/src/framework.dart"),
            vec![
                "core/container.dart | part \"core/container.dart\";".to_string(),
                "core/element.dart | part 'core/element.dart';".to_string(),
                "dart:async | import 'dart:async';".to_string(),
                "framework.g.dart | part 'framework.g.dart';".to_string(),
                "package:meta/meta.dart | import 'package:meta/meta.dart';".to_string(),
            ],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "packages/kit/lib/kit.dart"),
            vec!["1 -> packages/kit/lib/src/widgets.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "lib/legacy/models.dart"),
            vec!["3 -> lib/legacy/user.dart".to_string()],
            "{report}"
        );
        for part in [
            "lib/src/core/element.dart",
            "lib/src/framework.g.dart",
            "packages/kit/lib/src/widgets.dart",
            "lib/legacy/user.dart",
        ] {
            assert_eq!(imports_from(graph, part), none, "{part}\n{report}");
            assert_eq!(unresolved_imports_of(graph, part), none, "{part}\n{report}");
            assert_eq!(import_nodes_in(graph, part), none, "{part}\n{report}");
        }
        assert_eq!(
            imports_from(graph, "examples/a/lib/main.dart"),
            none,
            "{report}"
        );
        assert_eq!(
            unresolved_imports_of(graph, "examples/a/lib/main.dart"),
            vec!["main.g.dart".to_string()],
            "{report}"
        );
        assert_eq!(
            imports_from(graph, "examples/b/lib/main.dart"),
            vec!["1 -> examples/b/lib/main.g.dart".to_string()],
            "{report}"
        );
        // A change to a part reaches its library and what imports it.
        let output = support::codegraph(
            project.root(),
            ["affected", "lib/src/core/element.dart", "-p", "."],
        );
        let affected: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("affected output");
        let mut files: Vec<String> = affected["affectedFiles"]
            .as_array()
            .expect("affectedFiles")
            .iter()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                "lib/app.dart".to_string(),
                "lib/src/framework.dart".to_string(),
                "test/framework_test.dart".to_string(),
            ],
            "{affected}\n{report}"
        );
    }
}

/// W4-DART-03: Dart getter reads, the types a declaration or body names, and
/// initializers (upstream #2360, #2367).
mod dart_reads {
    use super::support::{Graph, NodeRow, Project};

    /// The node with qualified name `qualified`, in `file` when one is given.
    #[track_caller]
    fn node<'g>(graph: &'g Graph, file: Option<&str>, qualified: &str) -> &'g NodeRow {
        let found: Vec<&NodeRow> = graph
            .nodes
            .iter()
            .filter(|n| n.qualified_name == qualified && file.is_none_or(|f| n.file_path == f))
            .collect();
        assert_eq!(found.len(), 1, "one node {qualified}\n{}", graph.report());
        found[0]
    }

    /// `<edge kind> <source qualified name>` of each `calls`, `references` or
    /// `instantiates` edge into the node `qualified`, sorted (upstream's
    /// `callersOf`).
    fn callers_of(graph: &Graph, qualified: &str) -> Vec<String> {
        let target = node(graph, None, qualified).id.clone();
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| {
                e.target == target
                    && matches!(e.kind.as_str(), "calls" | "references" | "instantiates")
            })
            .filter_map(|e| {
                Some(format!(
                    "{} {}",
                    e.kind,
                    graph.node(&e.source)?.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// `<source kind> <source qualified name>` of each `kind` edge into the
    /// node `qualified`, sorted and de-duplicated (upstream's `referrers` and
    /// `sources`).
    fn sources(graph: &Graph, file: Option<&str>, qualified: &str, kind: &str) -> Vec<String> {
        let target = node(graph, file, qualified).id.clone();
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.target == target && e.kind == kind)
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                Some(format!("{} {}", source.kind, source.qualified_name))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The qualified names of the targets of `from`'s `kind` edges, sorted
    /// and de-duplicated (upstream's `targets`).
    fn targets(graph: &Graph, file: &str, qualified: &str, kind: &str) -> Vec<String> {
        let source = node(graph, Some(file), qualified).id.clone();
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.source == source && e.kind == kind)
            .filter_map(|e| Some(graph.node(&e.target)?.qualified_name.clone()))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Upstream #2338 (`dart-getter-reads.test.ts`, its fixture): a getter
    /// read runs the getter, so it links, as a call, the getter the
    /// receiver's type reaches — declared on it, inherited, or added by an
    /// extension, on an enum too — through a parameter, a local, a field or a
    /// type named directly; a field read, an outside type's getter, an
    /// untyped receiver and another library's unnamed extension link
    /// nothing. A member an extension adds to an enum is reached by a call.
    #[test]
    fn pr2360_a_dart_getter_read_links_the_getter_its_receiver_reaches() {
        let project = Project::new()
            .file("pubspec.yaml", "name: repro\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file(
                "lib/shape.dart",
                "enum Shape { circle, square }\n\nextension ShapeInfo on Shape {\n  String get label => name.toUpperCase();\n  String shout() => name.toUpperCase();\n}\n\nclass Box {\n  const Box(this.size);\n  final int size;\n  int get area => size * size;\n  int grow() => size + 1;\n}\n\nextension BoxActions on Box {\n  int twice() => size * 2;\n}\n",
            )
            .file(
                "lib/use.dart",
                "import 'shape.dart';\n\nString a(Shape s) => s.label;   // getter, extension on enum\nString b(Shape s) => s.shout(); // method, extension on enum\nint c(Box x) => x.area;         // getter, plain class\nint d(Box x) => x.grow();       // method, plain class\nint e(Box x) => x.twice();      // method, extension on class\n",
            )
            .file(
                "lib/render.dart",
                "import 'shape.dart';\n\nclass Square extends Box {\n  const Square(int size) : super(size);\n  static Square get unit => const Square(1);\n}\n\nclass Renderer {\n  final Box box;\n  final Shape shape;\n  Renderer(this.box, this.shape);\n\n  int paint(Box other) {\n    final local = Box(2);\n    return other.area + local.area;\n  }\n\n  int viaField() {\n    return box.area;\n  }\n\n  String tag() {\n    final text = shape.label;\n    return text + shape.shout();\n  }\n}\n\nint inherited(Square s) {\n  return s.area;\n}\n\nSquare staticGetter() => Square.unit;\n",
            )
            .file(
                "lib/other.dart",
                "class Panel {\n  int get size => 0;\n  int get length => 0;\n  int get grow => 0;\n}\n\nextension on Box {\n  int get hidden => 0;\n}\n",
            )
            .file(
                "lib/negatives.dart",
                "import 'shape.dart';\n\nint fieldRead(Box x) => x.size;           // a field, not Panel's getter\nint outside(List<int> xs) => xs.length;   // a getter of a type outside the project\nint untyped(dynamic d) => d.area;         // nothing says what d is\nint unnamed(Box x) => x.hidden;           // an unnamed extension in another library\nint Function() tearOff(Box x) => x.grow;  // a method read as a value, not run\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();

        // Reads on a plain class, through a parameter, a local and a field,
        // and an inherited getter.
        assert_eq!(
            callers_of(graph, "Box::area"),
            [
                "calls Renderer::paint",
                "calls Renderer::paint",
                "calls Renderer::viaField",
                "calls c",
                "calls inherited",
            ],
            "{report}"
        );
        // A getter an extension on an enum adds.
        assert_eq!(
            callers_of(graph, "ShapeInfo::label"),
            ["calls Renderer::tag", "calls a"],
            "{report}"
        );
        // A static getter read through its class.
        assert_eq!(
            callers_of(graph, "Square::unit"),
            ["calls staticGetter"],
            "{report}"
        );
        // A field, an outside type, an untyped receiver, an unnamed extension
        // of another library: nothing.
        let negatives: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.file_path == "lib/negatives.dart")
            })
            .map(|e| graph.describe_edge(e))
            .collect();
        assert_eq!(negatives, Vec::<String>::new(), "{report}");
        assert_eq!(
            callers_of(graph, "Panel::size"),
            Vec::<String>::new(),
            "{report}"
        );
        assert_eq!(
            callers_of(graph, "Panel::length"),
            Vec::<String>::new(),
            "{report}"
        );
        // A method an extension on an enum adds, and the class's own methods
        // and extensions.
        assert_eq!(
            callers_of(graph, "ShapeInfo::shout"),
            ["calls Renderer::tag", "calls b"],
            "{report}"
        );
        assert_eq!(callers_of(graph, "Box::grow"), ["calls d"], "{report}");
        assert_eq!(
            callers_of(graph, "BoxActions::twice"),
            ["calls e"],
            "{report}"
        );
    }

    /// Upstream #2327 (`dart-type-positions.test.ts`, its fixture): the type an
    /// extension is `on`, a field's type (an enum's too), a top-level
    /// variable's (the file's), a constant's initializer and every type a
    /// body names (locals, generic arguments, casts, type tests, catch
    /// clauses) are references; a constructor call instantiates without one,
    /// and an import prefix names nothing. As amended by #2367: a `static
    /// const` field's initializer constructs from its constant.
    #[test]
    fn pr2360_the_types_a_dart_declaration_or_body_names_are_referenced() {
        let project = Project::new()
            .file("pubspec.yaml", "name: repro\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("lib/report.dart", "class Report {\n  final int score;\n  const Report(this.score);\n}\n")
            .file("lib/a_parameter.dart", "import 'report.dart';\n\nint score(Report r) => r.score;\n")
            .file("lib/b_return_generic.dart", "import 'report.dart';\n\nFuture<Report?> load() async => null;\n")
            .file("lib/c_extension_on.dart", "import 'report.dart';\n\nextension ReportX on Report {\n  bool get high => score > 5;\n}\n")
            .file("lib/d_field_type.dart", "import 'report.dart';\n\nclass Holder {\n  final Report report;\n  const Holder(this.report);\n}\n")
            .file("lib/e_toplevel_initializer_generic.dart", "import 'report.dart';\n\nclass Family<T, A> {\n  const Family();\n}\n\nfinal reportProvider = Family<Report?, String>();\n")
            .file("lib/f_expression_generic.dart", "import 'report.dart';\n\nvoid run() {\n  Future<Report?>.value(null);\n}\n")
            .file("lib/model.dart", "class Score {\n  const Score(this.value);\n  final int value;\n}\n\nclass ScoreError implements Exception {}\n")
            .file("lib/node.dart", "class Node {\n  Node? next;\n}\n")
            .file(
                "lib/more.dart",
                "import 'model.dart' as m;\nimport 'model.dart' as M;\nimport 'model.dart';\n\nextension ScoresX on List<Score> {\n  int get total => length;\n}\n\nenum Grade {\n  a(null);\n  const Grade(this.best);\n  final Score? best;\n}\n\nclass Board {\n  Score? top;\n  final List<Score> all = [];\n  static const Score zero = Score(0);\n  var cache = <String, Score>{};\n  late final m.Score prefixed;\n  late final M.Score upper;\n}\n\nScore? current;\nfinal scores = <Score>[];\n\nint body(Object o) {\n  final Score s = o as Score;\n  if (o is Score) {}\n  final made = Score(1);\n  final built = const Score(2);\n  try {} on ScoreError catch (_) {}\n  return s.value + made.value + built.value;\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();

        assert_eq!(
            sources(graph, None, "Report", "references"),
            [
                "class Holder",
                "class ReportX",
                "constant reportProvider",
                "function load",
                "function run",
                "function score",
            ],
            "{report}"
        );
        assert_eq!(
            sources(graph, None, "Score", "references"),
            [
                "class Board",
                "class ScoresX",
                "constant scores",
                "enum Grade",
                "file lib/more.dart",
                "function body",
            ],
            "{report}"
        );
        assert_eq!(
            sources(graph, None, "ScoreError", "references"),
            ["function body"],
            "{report}"
        );
        // `Score(1)` and `const Score(2)` instantiate, and reference nothing of
        // their own; nor does the import prefix `m`.
        let body = node(graph, None, "body").id.clone();
        let score = node(graph, None, "Score").id.clone();
        let mut lines: Vec<i64> = graph
            .edges
            .iter()
            .filter(|e| e.source == body && e.target == score && e.kind == "references")
            .filter_map(|e| e.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        // Upstream's lines 26 and 27: this fixture adds an uppercase prefix's
        // import and field above them.
        assert_eq!(lines, [28, 29], "{report}");
        let mut built: Vec<i64> = graph
            .edges
            .iter()
            .filter(|e| e.source == body && e.target == score && e.kind == "instantiates")
            .filter_map(|e| e.line)
            .collect();
        built.sort_unstable();
        assert_eq!(built, [30, 31], "{report}");
        assert_eq!(
            sources(graph, None, "Score", "instantiates"),
            ["constant Board::zero", "function body"],
            "{report}"
        );
        assert!(
            !graph
                .unresolved
                .iter()
                .any(|r| r.file_path == "lib/more.dart"
                    && matches!(r.reference_name.as_str(), "m" | "M")),
            "{report}"
        );
        // A class whose field holds its own type names itself: no self-edge.
        assert_eq!(
            sources(graph, None, "Node", "references"),
            Vec::<String>::new(),
            "{report}"
        );
    }

    /// Upstream #2367 (`dart-initializer-calls.test.ts`, its fixture): a
    /// `final` / `const` initializer is code its constant runs, a field's or a
    /// `var`'s is its class's or the file's, a local declaration's second
    /// variable stays its function's, and a function passed as a value is
    /// captured once. A call through a type's static constant calls that
    /// constant. KEEP-RUST: this grammar ends an arrow function in front of
    /// the call its body makes (`(ref) => createDio()`), which reads as the
    /// call its body ends with.
    #[test]
    fn pr2367_a_dart_initializer_belongs_to_its_constant_class_or_file() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file(
                "lib/riverpod.dart",
                "class Ref {\n  T watch<T>(Object provider) => throw 0;\n  T read<T>(Object provider) => throw 0;\n}\n\nclass Provider<T> {\n  Provider(T Function(Ref ref) create);\n\n  static const autoDispose = AutoDisposeProviderBuilder();\n}\n\nclass AutoDisposeProviderBuilder {\n  const AutoDisposeProviderBuilder();\n\n  Provider<T> call<T>(T Function(Ref ref) create) => Provider(create);\n}\n\nclass ProviderFamilyBuilder {\n  Object autoDispose() => this;\n}\n\nclass StateNotifierProvider<N, S> {\n  StateNotifierProvider(N Function(Ref ref) create);\n}\n\nclass FutureProvider<T> {\n  FutureProvider(Future<T> Function(Ref ref) create);\n\n  static const family = FutureProviderFamilyBuilder();\n}\n\nclass FutureProviderFamilyBuilder {\n  const FutureProviderFamilyBuilder();\n\n  FutureProvider<T> call<T, A>(Future<T> Function(Ref ref, A arg) create) => throw 0;\n}\n",
            )
            .file(
                "lib/builders.dart",
                "class AutoDisposeFutureProviderBuilder {\n  Object family() => this;\n}\n",
            )
            .file(
                "lib/data.dart",
                "class Dio {}\n\nclass Api {\n  Api(Dio dio);\n}\n\nclass Repository {\n  Repository(Dio dio);\n}\n\nclass Counter {\n  Counter(Api api);\n}\n\nDio createDio() => Dio();\nvoid register(void Function() cb) {}\nint start() => 0;\n",
            )
            .file(
                "lib/providers.dart",
                "import 'riverpod.dart';\nimport 'data.dart';\n\nfinal dioProvider = Provider((ref) => createDio());\nfinal apiProvider = Provider((ref) => Api(ref.watch(dioProvider)));\nfinal repoProvider = Provider((ref) => Repository(ref.watch(dioProvider)));\nfinal counterProvider = StateNotifierProvider<Counter, int>((ref) => Counter(ref.read(apiProvider)));\n",
            )
            .file(
                "lib/more.dart",
                "import 'riverpod.dart';\nimport 'data.dart';\nimport 'providers.dart';\n\nfinal typedProvider = Provider((Ref ref) => ref.read(dioProvider));\nfinal blockProvider = Provider((ref) {\n  final dio = createDio();\n  return Repository(dio);\n});\nvoid onTap() {}\nfinal wired = register(onTap);\nfinal handler = onTap;\nvar cache = createDio();\nvar registry = <String, Dio>{};\nlate final lazyRepo = Repository(createDio());\nfinal autoProvider = Provider.autoDispose((ref) => createDio());\nfinal userProvider = FutureProvider.family((ref, int id) async => Api(createDio()));\n\nProvider<Dio> makeAuto() => Provider.autoDispose((ref) => Dio());\n\nclass Service {\n  static final instance = Service._(createDio());\n  static var shared = Api(createDio());\n  final repo = Repository(Dio());\n  final Dio dio;\n  Service._(this.dio);\n\n  int run() {\n    var total = 0;\n    for (var i = 0, j = start(); i < j; i++) {\n      total += i;\n    }\n    return total;\n  }\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let providers = "lib/providers.dart";
        let more = "lib/more.dart";

        // The providers construct what they build and call what they build it with.
        assert_eq!(
            targets(graph, providers, "dioProvider", "calls"),
            ["createDio"],
            "{report}"
        );
        // The call is written where the arrow's tail starts (KEEP-RUST: this
        // grammar ends the arrow in front of the call its body makes).
        let dio = node(graph, Some(providers), "dioProvider").id.clone();
        let at: Vec<(Option<i64>, Option<i64>)> = graph
            .edges
            .iter()
            .filter(|e| e.source == dio && e.kind == "calls")
            .map(|e| (e.line, e.col))
            .collect();
        assert_eq!(at, [(Some(4), Some(38))], "{report}");
        // A `var`'s initializer is walked as a body, for the file: the types
        // it names too.
        assert!(
            sources(graph, None, "Dio", "references").contains(&"file lib/more.dart".to_string()),
            "{report}"
        );
        assert_eq!(
            targets(graph, providers, "apiProvider", "instantiates"),
            ["Api", "Provider"],
            "{report}"
        );
        assert_eq!(
            targets(graph, providers, "repoProvider", "instantiates"),
            ["Provider", "Repository"],
            "{report}"
        );
        assert_eq!(
            targets(graph, providers, "counterProvider", "instantiates"),
            ["Counter", "StateNotifierProvider"],
            "{report}"
        );
        // A typed closure parameter types its receiver; a block-bodied closure is walked whole.
        assert_eq!(
            targets(graph, more, "typedProvider", "calls"),
            ["Ref::read"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "blockProvider", "calls"),
            ["createDio"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "blockProvider", "instantiates"),
            ["Provider", "Repository"],
            "{report}"
        );
        // A function passed as a value, or held by a constant, is the
        // constant's reference, captured once.
        assert_eq!(
            targets(graph, more, "wired", "calls"),
            ["register"],
            "{report}"
        );
        assert_eq!(
            sources(graph, Some(more), "onTap", "references"),
            ["constant handler", "constant wired"],
            "{report}"
        );
        let on_tap = node(graph, Some(more), "onTap").id.clone();
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.target == on_tap && e.kind == "references")
                .count(),
            2,
            "{report}"
        );
        // `var` and `late final` mint no node: the file runs them.
        assert_eq!(
            targets(graph, more, more, "calls"),
            ["createDio"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, more, "instantiates"),
            ["Repository"],
            "{report}"
        );
        // A `static final` is the class's constant; a `static var` and an
        // instance field mint no node, so they are the class's.
        assert_eq!(
            targets(graph, more, "Service::instance", "calls"),
            ["Service::_", "createDio"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "Service", "calls"),
            ["createDio"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "Service", "instantiates"),
            ["Api", "Dio", "Repository"],
            "{report}"
        );
        // `Provider.autoDispose(…)` calls the static constant, whose own
        // initializer constructs the builder, and not a same-named method of
        // another type, in its file or not.
        let auto_dispose = node(graph, Some("lib/riverpod.dart"), "Provider::autoDispose");
        assert_eq!(auto_dispose.kind, "constant", "{report}");
        assert_eq!(
            targets(graph, more, "autoProvider", "calls"),
            ["Provider::autoDispose", "createDio"],
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "makeAuto", "calls"),
            ["Provider::autoDispose"],
            "{report}"
        );
        assert_eq!(
            targets(
                graph,
                "lib/riverpod.dart",
                "Provider::autoDispose",
                "instantiates"
            ),
            ["AutoDisposeProviderBuilder"],
            "{report}"
        );
        assert_eq!(
            sources(graph, None, "ProviderFamilyBuilder::autoDispose", "calls"),
            Vec::<String>::new(),
            "{report}"
        );
        assert_eq!(
            targets(graph, more, "userProvider", "calls"),
            ["FutureProvider::family", "createDio"],
            "{report}"
        );
        assert_eq!(
            sources(
                graph,
                None,
                "AutoDisposeFutureProviderBuilder::family",
                "calls"
            ),
            Vec::<String>::new(),
            "{report}"
        );
        // Each provider and declaration above is a caller of what it builds.
        assert_eq!(
            sources(graph, None, "createDio", "calls"),
            [
                "class Service",
                "constant Service::instance",
                "constant autoProvider",
                "constant blockProvider",
                "constant dioProvider",
                "constant userProvider",
                "file lib/more.dart",
            ],
            "{report}"
        );
        assert_eq!(
            sources(graph, None, "Repository", "instantiates"),
            [
                "class Service",
                "constant blockProvider",
                "constant repoProvider",
                "file lib/more.dart",
            ],
            "{report}"
        );
        // A local declaration's second variable is its function's, once.
        assert_eq!(
            targets(graph, more, "Service::run", "calls"),
            ["start"],
            "{report}"
        );
        let run = node(graph, Some(more), "Service::run").id.clone();
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.source == run && e.kind == "calls")
                .count(),
            1,
            "{report}"
        );
        assert_eq!(
            sources(graph, None, "start", "calls"),
            ["method Service::run"],
            "{report}"
        );
    }

    /// KEEP-RUST (W4-DART-03): what this grammar writes differently from
    /// upstream's. A `new` or `const` call writes its class, and a named
    /// constructor, in its `type` (`const Foo.named()`, a prefixed
    /// `const p.Foo()`), and calls them; an arrow function ended in front of
    /// the call or member access its body makes reads as the tail's: its
    /// receiver typed (`(Ref ref) => ref.watch(x)` is `Ref`'s `watch`, and
    /// `ref.read(x)` no namesake's `read` when `Ref` declares none), its
    /// static read the type's, its member read the getter's.
    #[test]
    fn keep_rust_dart_constructor_calls_and_arrow_tails_read_as_written() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/kit.dart",
                "class Foo {\n  const Foo();\n}\n\nclass Ref {\n  int watch(Object p) => 0;\n}\n\nclass Box {\n  int read() => 1;\n  int get area => 1;\n}\n\nenum Mode { dark }\n",
            )
            .file(
                "lib/use.dart",
                "import 'kit.dart';\nimport 'kit.dart' as p;\n\nvoid build() {\n  final a = new Foo();\n  final b = const p.Foo();\n  final c = new Foo.named();\n  final d = (Ref ref) => ref.watch(1);\n  final g = (Ref ref) => ref.read(1);\n  final e = (Object x) => Mode.dark;\n  final f = (Box b) => b.area;\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let mut built: Vec<(Option<i64>, String)> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "instantiates")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.qualified_name == "build")
            })
            .filter_map(|e| Some((e.line, graph.node(&e.target)?.qualified_name.clone())))
            .collect();
        built.sort();
        assert_eq!(
            built,
            [(Some(5), "Foo".to_string()), (Some(6), "Foo".to_string())],
            "{report}"
        );
        assert!(
            graph
                .unresolved
                .iter()
                .any(|r| r.reference_name == "Foo.named" && r.reference_kind == "calls"),
            "{report}"
        );
        assert_eq!(
            targets(graph, "lib/use.dart", "build", "calls"),
            ["Box::area", "Ref::watch"],
            "{report}"
        );
        assert!(
            targets(graph, "lib/use.dart", "build", "references").contains(&"Mode".to_string()),
            "{report}"
        );
    }

    /// Upstream #2338's `dartMemberOf`: an unnamed extension applies only in
    /// its own library, so for a read in another one it is no candidate at
    /// all, and a named extension's getter of the same rank answers, though
    /// the unnamed one sits nearer the read (the tie would go to it).
    #[test]
    fn pr2360_an_unnamed_extension_of_another_library_is_passed_over_for_a_named_one() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/box.dart", "class Box {}\n")
            .file(
                "lib/a/other.dart",
                "import '../box.dart';\n\nextension on Box {\n  int get hidden => 0;\n}\n",
            )
            .file(
                "lib/b/named.dart",
                "import '../box.dart';\n\nextension BoxHidden on Box {\n  int get hidden => 1;\n}\n",
            )
            .file("lib/a/use.dart", "import '../box.dart';\n\nint read(Box x) => x.hidden;\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            targets(graph, "lib/a/use.dart", "read", "calls"),
            ["BoxHidden::hidden"],
            "{report}"
        );
    }
}

/// W4-DART-03b: a later link of a Dart call chain calls a member of what the
/// chain before it evaluates to (upstream #2376, #2383).
mod dart_chains {
    use super::support::{Graph, Project};

    /// `<caller name> -> <target qualified name>` for every `calls` edge out
    /// of a file's nodes, sorted (upstream's `callsFrom`).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                (source.file_path == file).then_some(())?;
                Some(format!(
                    "{} -> {}",
                    source.name,
                    graph.node(&e.target)?.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2376 (`dart-chain-links.test.ts`, its fixture): a later link
    /// of a chain calls a member of what the chain before it evaluates to,
    /// typed from the head and each link's declared type, or nothing: a
    /// Stream's `transform` is no project method of that name,
    /// `X.autoDispose.family(…)` is the `family` of the builder X's constant
    /// holds, a fluent chain stays on the type each link returns, `super.m()`
    /// is the superclass's, `context.read<T>()` hands back a T, a
    /// parenthesized receiver is what its last operand is, and a getter an
    /// annotated extension adds is in the chain.
    #[test]
    fn pr2376_a_dart_chain_link_calls_a_member_of_what_the_chain_before_it_is() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/pipe.dart", "class BlocPipe {\n  Object? transform(Object? value) => value;\n}\n")
            .file("lib/bloc.dart", "import 'dart:async';\n\nclass Bloc<E> {\n  Stream<dynamic> transformEvents(Stream<E> events, Stream<dynamic> Function(E) mapper) {\n    return events\n        .map(mapper)\n        .transform<dynamic>(const _FlatMap<dynamic>());\n  }\n}\n\nclass _FlatMap<T> {\n  const _FlatMap();\n}\n")
            .file("lib/builders.dart", "class FutureProviderBuilder {\n  const FutureProviderBuilder();\n\n  FutureProviderFamilyBuilder get family => const FutureProviderFamilyBuilder();\n}\n\nclass FutureProviderFamilyBuilder {\n  const FutureProviderFamilyBuilder();\n\n  Object call(Object create) => create;\n}\n\nclass StateProviderBuilder {\n  const StateProviderBuilder();\n\n  StateProviderFamilyBuilder get family => const StateProviderFamilyBuilder();\n}\n\nclass StateProviderFamilyBuilder {\n  const StateProviderFamilyBuilder();\n\n  Object call(Object create) => create;\n}\n")
            .file("lib/providers.dart", "import 'builders.dart';\n\nclass FutureProvider {\n  static const autoDispose = FutureProviderBuilder();\n}\n\nclass StateProvider {\n  static const autoDispose = StateProviderBuilder();\n}\n")
            .file("lib/app.dart", "import 'providers.dart';\n\nfinal users = FutureProvider.autoDispose.family<int, String>((ref, id) => 0);\nfinal counts = StateProvider.autoDispose\n    .family<int, String>((ref, id) => 0);\n")
            .file("lib/sign_up/sign_up_state.dart", "class SignUpState {\n  const SignUpState();\n\n  SignUpState withEmail(String email) => this;\n  SignUpState withPassword(String password) => this;\n}\n")
            .file("test/sign_up/login_state.dart", "class LoginState {\n  const LoginState();\n\n  LoginState withEmail(String email) => this;\n  LoginState withPassword(String password) => this;\n}\n")
            .file("test/sign_up/sign_up_state_test.dart", "import 'package:app/sign_up/sign_up_state.dart';\n\nvoid main() {\n  final constructed = SignUpState().withEmail('a').withPassword('b');\n  final constant = const SignUpState().withEmail('a');\n}\n")
            .file("lib/counter.dart", "abstract class BlocEventSink<E> {\n  void add(E event);\n}\n\nabstract class BlocBase<S> {\n  Future<void> close() async {}\n}\n\nabstract class Bloc<E, S> extends BlocBase<S> implements BlocEventSink<E> {\n  @override\n  void add(E event) {}\n\n  @override\n  Future<void> close() async {\n    await super.close();\n  }\n}\n\nclass CounterBloc extends Bloc<int, int> {\n  void increment() {}\n}\n")
            .file("lib/counter_view.dart", "import 'package:flutter/widgets.dart';\nimport 'counter.dart';\n\nclass CounterView {\n  void onTap(BuildContext context) {\n    context.read<CounterBloc>().add(1);\n  }\n}\n")
            .file("lib/todo.dart", "class Todo {\n  const Todo();\n\n  Todo copyWith({String? title}) => this;\n}\n")
            .file("lib/edit_todo.dart", "import 'todo.dart';\n\nclass EditTodoState {\n  const EditTodoState({this.initialTodo});\n\n  final Todo? initialTodo;\n\n  EditTodoState copyWith({Todo? initialTodo}) => this;\n}\n\nclass EditTodoBloc {\n  EditTodoState state = const EditTodoState();\n\n  void submit() {\n    final todo = (state.initialTodo ?? const Todo()).copyWith(title: 'x');\n  }\n}\n")
            .file("lib/container.dart", "import 'package:meta/meta.dart';\n\nclass ProviderPointerManager {\n  void readPointer(Object provider) {}\n}\n\nclass ProviderContainer {\n  final ProviderPointerManager _pointerManager = ProviderPointerManager();\n}\n\n@internal\nextension ProviderContainerTest on ProviderContainer {\n  ProviderPointerManager get pointerManager => _pointerManager;\n}\n\nvoid probe(ProviderContainer container) {\n  container.pointerManager.readPointer(1);\n}\n")
            .file("lib/other_manager.dart", "class OtherManager {\n  void readPointer(Object provider) {}\n}\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "lib/bloc.dart"),
            Vec::<String>::new(),
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/app.dart"),
            [
                "counts -> StateProviderBuilder::family",
                "users -> FutureProviderBuilder::family",
            ],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "test/sign_up/sign_up_state_test.dart"),
            [
                "main -> SignUpState::withEmail",
                "main -> SignUpState::withEmail",
                "main -> SignUpState::withPassword",
            ],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/counter.dart"),
            ["close -> BlocBase::close"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/counter_view.dart"),
            ["onTap -> Bloc::add"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/edit_todo.dart"),
            ["submit -> Todo::copyWith"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/container.dart"),
            [
                "probe -> ProviderContainerTest::pointerManager",
                "probe -> ProviderPointerManager::readPointer",
            ],
            "{report}"
        );
    }

    /// Upstream #2383 (`dart-generic-factory-chain.test.ts`, its fixtures): a
    /// call chained on a generic static factory calls a member of the type
    /// the factory's type argument names, whether the factory is the
    /// project's (`static T of<T>(…)`, its own type parameter given by the
    /// call) or a package's (an outside `of<T>(…)` hands back its T), wrapped,
    /// through an import prefix, through `?.`, or inherited; never a member
    /// found by its name, and nothing without a type argument. KEEP-RUST: the
    /// extractor records every such link by its bare name, so upstream's
    /// fallback in `matchDottedCallChain` has no counterpart: the chain is
    /// typed at the call site either way.
    #[test]
    fn pr2383_a_chain_on_a_generic_lookup_calls_a_member_of_its_type_argument() {
        let library = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("lib/bloc_provider.dart", "class BlocProvider<T> {\n  const BlocProvider();\n\n  static T of<T>(Object context, {bool listen = false}) => throw 0;\n\n  static T? maybeOf<T>(Object context, {bool listen = false}) => null;\n}\n")
            .file("lib/bloc.dart", "abstract class Bloc<E, S> {\n  void add(E event) {}\n}\n")
            .file("lib/counter/counter_cubit.dart", "class CounterCubit {\n  void increment() {}\n}\n")
            .file("lib/counter/counter_bloc.dart", "import '../bloc.dart';\n\nclass CounterBloc extends Bloc<int, int> {}\n")
            .file("lib/counter/step_counter.dart", "class StepCounter {\n  void increment() {}\n  void decrement() {}\n  void saveTodo(Object todo) {}\n}\n")
            .file("lib/counter/counter_page.dart", "import '../bloc_provider.dart';\nimport 'counter_cubit.dart';\nimport 'counter_cubit.dart' as cubits;\nimport 'counter_bloc.dart';\n\nclass Button {\n  const Button({Object? onPressed});\n}\n\nclass CounterPage {\n  void tap(Object context) {\n    BlocProvider.of<CounterCubit>(context, listen: false).increment();\n  }\n\n  void tapWrapped(Object context) {\n    BlocProvider.of<CounterCubit>(context, listen: false)\n        .increment();\n  }\n\n  void tapPrefixed(Object context) {\n    BlocProvider.of<cubits.CounterCubit>(context, listen: false).increment();\n  }\n\n  void tapMaybe(Object context) {\n    BlocProvider.maybeOf<CounterCubit>(context, listen: false)?.increment();\n  }\n\n  void tapBloc(Object context) {\n    BlocProvider.of<CounterBloc>(context, listen: false).add(1);\n  }\n\n  void tapMissing(Object context) {\n    BlocProvider.of<CounterCubit>(context, listen: false).decrement();\n  }\n\n  void tapInferred(Object context) {\n    BlocProvider.of(context, listen: false).increment();\n  }\n\n  // Parsed as two comparisons, so increment arrives by its bare name.\n  void tapRecovered(Object context) {\n    BlocProvider.of<CounterCubit>(context).increment();\n  }\n\n  Object build(Object context) => Button(\n        onPressed: () => BlocProvider.of<CounterCubit>(context, listen: false).increment(),\n      );\n}\n")
            .index();
        let graph = library.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "lib/counter/counter_page.dart"),
            [
                "build -> BlocProvider::of",
                "build -> CounterCubit::increment",
                "tap -> BlocProvider::of",
                "tap -> CounterCubit::increment",
                "tapBloc -> Bloc::add",
                "tapBloc -> BlocProvider::of",
                "tapInferred -> BlocProvider::of",
                "tapMaybe -> BlocProvider::maybeOf",
                "tapMaybe -> CounterCubit::increment",
                "tapMissing -> BlocProvider::of",
                "tapPrefixed -> BlocProvider::of",
                "tapPrefixed -> CounterCubit::increment",
                "tapRecovered -> BlocProvider::of",
                "tapRecovered -> CounterCubit::increment",
                "tapWrapped -> BlocProvider::of",
                "tapWrapped -> CounterCubit::increment",
            ],
            "{report}"
        );
        let app = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("lib/counter/counter_cubit.dart", "class CounterCubit {\n  void increment() {}\n}\n")
            .file("lib/counter/step_counter.dart", "class StepCounter {\n  void increment() {}\n  void decrement() {}\n  void saveTodo(Object todo) {}\n}\n")
            .file("lib/todos/todos_repository.dart", "class TodosRepository {\n  void saveTodo(Object todo) {}\n}\n")
            .file("lib/counter/counter_page.dart", "import 'package:flutter_bloc/flutter_bloc.dart';\nimport '../todos/todos_repository.dart';\nimport 'counter_cubit.dart';\n\nclass CounterPage {\n  void tap(Object context) {\n    BlocProvider.of<CounterCubit>(context, listen: false).increment();\n  }\n\n  void save(Object context, Object todo) {\n    RepositoryProvider.of<TodosRepository>(context, listen: false).saveTodo(todo);\n  }\n\n  void tapInferred(Object context) {\n    BlocProvider.of(context, listen: false).increment();\n  }\n\n  // Parsed as two comparisons, so increment arrives by its bare name.\n  void tapRecovered(Object context) {\n    BlocProvider.of<CounterCubit>(context).increment();\n  }\n}\n")
            .index();
        let graph = app.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "lib/counter/counter_page.dart"),
            [
                "save -> TodosRepository::saveTodo",
                "tap -> CounterCubit::increment",
                "tapRecovered -> CounterCubit::increment",
            ],
            "{report}"
        );
    }

    /// KEEP-RUST: each piece of evidence a chain's typing reads, as upstream
    /// #2376 reads it: an import prefix before the head names a library; a
    /// `const` / `new` head's named constructor is its own link, and one the
    /// index holds no node for constructs its type; a call through a type
    /// reaches only its static members (a factory, not an instance getter of
    /// its name); a called value runs its type's `call`; a lookup its type
    /// declares answers for itself, its type argument does not; a return type
    /// that is a type parameter of the declaring class says nothing; an
    /// initializer's type arguments end nothing; a constant's declared type
    /// wins; an uppercase constant is a value; an abstract getter's type and a
    /// member-level initializer type a field, a method body's local of its
    /// name does not; no call runs a setter; an enum value is its enum; a
    /// `toString()` is a String; `(o as T)` is a T; a method named without a
    /// call is no value; a `catch` variable says no type; a bare call in a
    /// class is a member of it first; a closure's untyped parameter hides a
    /// field of its name. And the reading of a reference placed where its
    /// chain starts: a receiver-less call of the name inside the chain's
    /// arguments is that call's own; an arrow this grammar ended in front of
    /// a call (`() => a ?? b.load()`, recorded by the bare name where the
    /// arrow's tail starts) is a chain link written after `b.`, which links
    /// nothing rather than a namesake.
    #[test]
    fn keep_rust_dart_chain_typing_reads_the_evidence_upstream_reads() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/types.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n}\n\nclass Repo {\n  Item load() => Item();\n  static Repo create() => Repo();\n}\n\nclass Konst {\n  const Konst();\n  const Konst.named();\n  factory Konst.build() => const Konst();\n  Item make() => Item();\n}\n\nclass Res {\n  Res();\n  Item get error => Item();\n  Other other() => Other();\n\n  factory Res.error() => Res();\n}\n\nclass Family {\n  Item call() => Item();\n}\n\nclass Builder {\n  Family get family => Family();\n}\n\nclass Store {\n  read<T>() => null;\n}\n\nclass Holder<Item> {\n  Item get held => throw 0;\n}\n\nclass Pair<A, B> {\n  static Pair<A, B> of<A, B>() => throw 0;\n  Item first() => Item();\n}\n\nabstract class Src {\n  Item get item;\n}\n\nclass Late {\n  void m() {\n    final cache = Other();\n  }\n\n  final cache = Cache();\n}\n\nclass Cache {\n  Item get() => Item();\n}\n\nclass Slot {\n  set value(int v) {}\n}\n\nclass Box {\n  Slot wrap() => Slot();\n}\n\nclass A {\n  B next() => B();\n}\n\nclass B {\n  void m() {}\n}\n\nclass Field {\n  final A a = A();\n\n  void viaClosure() {\n    [1].forEach((a) => a.next().m());\n  }\n\n  Item make() => Item();\n\n  void viaMember() {\n    make().touch();\n  }\n}\n\nenum Mode { dark }\n\nextension ModeX on Mode {\n  Item label() => Item();\n}\n\nextension StringX on String {\n  Item asItem() => Item();\n}\n\nRepo make() => Repo();\n\nObject makeAny() => Object();\n\nfinal Repo declared = makeAny();\n\nfinal pair = Pair.of<int, String>();\n\nfinal Cfg = Repo();\n\nItem wrapIt(Object o) => Item();\n\nvoid touch() {}\n")
            .file(
                "lib/lib.dart",
                "import 'types.dart';\nimport 'other.dart';\n\npart 'part.dart';\n\nItem mk2() => Item();\n",
            )
            .file("lib/other.dart", "import 'types.dart';\n\nOther mk2() => Other();\n")
            .file("lib/part.dart", "part of 'lib.dart';\n\nvoid inPart() {\n  mk2().save();\n}\n")
            .file("lib/use.dart", "import 'types.dart';\nimport 'types.dart' as t;\n\nvoid prefixedChain() {\n  t.Repo.create().load().save();\n}\n\nvoid prefixedFactory() {\n  t.Konst.build();\n}\n\nvoid namedConst() {\n  const Konst.named().make().save();\n  Konst.named().make().save();\n  Konst.build().make().save();\n}\n\nvoid staticAgainstInstance() {\n  Res.error().other().save();\n}\n\nvoid calledValues(Family fam, Builder b) {\n  fam().save();\n  b.family().save();\n}\n\nvoid declaredLookup(Store store) {\n  store.read<Repo>().load();\n}\n\nvoid ownersParameter(Holder holder) {\n  holder.held.save();\n}\n\nvoid typeArgumentsInInitializer() {\n  pair.first().save();\n}\n\nvoid declaredConstant() {\n  declared.load().save();\n}\n\nvoid uppercaseConstant() {\n  Cfg.load().save();\n}\n\nvoid abstractGetter(Src s) {\n  s.item.save();\n}\n\nvoid memberLevelField(Late late) {\n  late.cache.get().save();\n}\n\nvoid noSetterCall(Box box) {\n  box.wrap().value(1);\n}\n\nvoid enumValue() {\n  Mode.dark.label().save();\n}\n\nvoid toStringIsAString() {\n  make().toString().asItem().save();\n}\n\nvoid cast(Object o) {\n  (o as Item?)!.touch();\n}\n\nvoid tearOff(Item item) {\n  item.touch.save();\n}\n\nvoid caught(Repo r) {\n  try {} catch (r) {\n    r.load().save();\n  }\n}\n\nvoid innerFirst() {\n  wrapIt(touch()).touch();\n}\n\nvoid misparsedArrow(Repo? a, Repo b) {\n  final f = () => a ?? b.load();\n}\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let from = |file: &str, caller: &str| -> Vec<String> {
            calls_from(graph, file)
                .into_iter()
                .filter(|edge| edge.starts_with(&format!("{caller} -> ")))
                .collect()
        };
        let use_from = |caller: &str| from("lib/use.dart", caller);
        assert_eq!(
            use_from("prefixedChain"),
            [
                "prefixedChain -> Item::save",
                "prefixedChain -> Repo::create",
                "prefixedChain -> Repo::load"
            ],
            "{report}"
        );
        // `const Konst.named();` is indexed since upstream #2380, so both
        // calls of it link the constructor, the chains typed as before.
        assert_eq!(
            use_from("namedConst"),
            [
                "namedConst -> Item::save",
                "namedConst -> Item::save",
                "namedConst -> Item::save",
                "namedConst -> Konst::build",
                "namedConst -> Konst::make",
                "namedConst -> Konst::make",
                "namedConst -> Konst::make",
                "namedConst -> Konst::named",
                "namedConst -> Konst::named"
            ],
            "{report}"
        );
        assert_eq!(
            use_from("staticAgainstInstance"),
            [
                "staticAgainstInstance -> Other::save",
                "staticAgainstInstance -> Res::error",
                "staticAgainstInstance -> Res::other"
            ],
            "{report}"
        );
        assert_eq!(
            use_from("calledValues"),
            [
                "calledValues -> Builder::family",
                "calledValues -> Item::save",
                "calledValues -> Item::save"
            ],
            "{report}"
        );
        assert_eq!(
            use_from("declaredLookup"),
            ["declaredLookup -> Store::read"],
            "{report}"
        );
        assert_eq!(
            use_from("ownersParameter"),
            ["ownersParameter -> Holder::held"],
            "{report}"
        );
        assert_eq!(
            use_from("typeArgumentsInInitializer"),
            [
                "typeArgumentsInInitializer -> Item::save",
                "typeArgumentsInInitializer -> Pair::first"
            ],
            "{report}"
        );
        // The call through the constant itself is the plain path's, which
        // reads no other file's constant; the link after it is typed by it.
        assert_eq!(
            use_from("declaredConstant"),
            ["declaredConstant -> Item::save"],
            "{report}"
        );
        // The call through the constant itself is the plain path's, which
        // reads no other file's constant; the link after it is typed by it.
        assert_eq!(
            use_from("uppercaseConstant"),
            ["uppercaseConstant -> Item::save"],
            "{report}"
        );
        assert_eq!(
            use_from("abstractGetter"),
            ["abstractGetter -> Item::save"],
            "{report}"
        );
        assert_eq!(
            use_from("memberLevelField"),
            [
                "memberLevelField -> Cache::get",
                "memberLevelField -> Item::save"
            ],
            "{report}"
        );
        assert_eq!(
            use_from("noSetterCall"),
            ["noSetterCall -> Box::wrap"],
            "{report}"
        );
        assert_eq!(
            use_from("enumValue"),
            ["enumValue -> Item::save", "enumValue -> ModeX::label"],
            "{report}"
        );
        assert_eq!(
            use_from("toStringIsAString"),
            [
                "toStringIsAString -> Item::save",
                "toStringIsAString -> StringX::asItem",
                "toStringIsAString -> make"
            ],
            "{report}"
        );
        // A named factory reached through a prefixed type is its static
        // member, by the `Konst.build(` its head writes.
        assert_eq!(
            use_from("prefixedFactory"),
            ["prefixedFactory -> Konst::build"],
            "{report}"
        );
        // A part's library holds the function its call names.
        assert_eq!(
            from("lib/part.dart", "inPart"),
            ["inPart -> Item::save", "inPart -> mk2"],
            "{report}"
        );
        assert_eq!(use_from("cast"), ["cast -> Item::touch"], "{report}");
        assert_eq!(use_from("tearOff"), Vec::<String>::new(), "{report}");
        // `r.load()` is the plain path's (it reads the parameter); the link
        // after it reads the `catch` variable, which says no type.
        assert_eq!(use_from("caught"), ["caught -> Repo::load"], "{report}");
        assert_eq!(
            use_from("innerFirst"),
            [
                "innerFirst -> Item::touch",
                "innerFirst -> touch",
                "innerFirst -> wrapIt"
            ],
            "{report}"
        );
        assert_eq!(use_from("misparsedArrow"), Vec::<String>::new(), "{report}");
        assert_eq!(
            from("lib/types.dart", "viaClosure"),
            ["viaClosure -> A::next"],
            "{report}"
        );
        assert_eq!(
            from("lib/types.dart", "viaMember"),
            ["viaMember -> Field::make", "viaMember -> Item::touch"],
            "{report}"
        );
    }

    /// KEEP-RUST: a later link is read back after the place the extractor
    /// put it, in the encoding the extractor wrote. A link after a receiver
    /// no chain reads (`fs[0]().m()`) links nothing, not a namesake of the
    /// type around it; a bare link and a link past a segment at one place
    /// (`x.s.save().save()`) are each their own; an uppercase constant heads
    /// a chain as the value it holds, not as a type.
    #[test]
    fn keep_rust_dart_chain_links_are_read_back_as_the_extractor_recorded_them() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/types.dart",
                "class Item {\n  void save() {}\n}\n\nclass Other {\n  Item save() => Item();\n}\n\nclass Holder {\n  Other get s => Other();\n}\n\nclass Repo {\n  Item load() => Item();\n}\n\nfinal Instance = Repo();\n",
            )
            .file(
                "lib/use.dart",
                "import 'types.dart';\n\nclass Box {\n  void m() {}\n\n  void unreadable(List<Function> fs) {\n    fs[0]().m();\n  }\n}\n\nvoid encodings(Holder x) {\n  x.s.save().save();\n}\n\nvoid uppercase() {\n  Instance.load().save();\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let calls = calls_from(graph, "lib/use.dart");
        assert!(
            !calls.contains(&"unreadable -> Box::m".to_string()),
            "{report}"
        );
        assert!(
            calls.contains(&"encodings -> Other::save".to_string())
                && calls.contains(&"encodings -> Item::save".to_string()),
            "{report}"
        );
        assert!(
            calls.contains(&"uppercase -> Item::save".to_string()),
            "{report}"
        );
    }

    /// KEEP-RUST: the extractor keeps a member chain's last segment
    /// (`x.a.m()` is `a.m`, #1496), records a call through `?.` as it
    /// records one through `.` (`y?.m()` is `y.m`, `make()?.load()` is
    /// `load`), and places every reference of a chain where the chain starts:
    /// the link is the call written after that position as the reference
    /// records it, on its line or below it, so a receiver-less call of the
    /// name before it (`touch(); r.load().touch();`) is not it. One chain's
    /// links of a name carry one reference, which links only what all of them
    /// reach (`b.add().add()` does; `p.flip().flip().flip()`, a Q's then a
    /// P's, links nothing). A field's initializer types it (`this._cache`),
    /// a static method's and a top-level constant's declared types type the
    /// links after them, and an extension on a type parameter of its own
    /// applies to a project type. The extractor now reads a method's return
    /// type and `static` through the signature this grammar wraps them in.
    #[test]
    fn keep_rust_dart_chain_links_recorded_past_a_member_or_through_null_aware_calls() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/types.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Repo {\n  Item load() => Item();\n  static Repo create() => Repo();\n}\n\nclass A {\n  void m() {}\n  B next() => B();\n}\n\nclass B {\n  void m() {}\n  A next() => A();\n}\n\nclass P {\n  Q flip() => Q();\n}\n\nclass Q {\n  P flip() => P();\n}\n\nclass Builder {\n  Builder add() => this;\n}\n\nclass Cache {\n  Item get() => Item();\n}\n\nclass X {\n  final A a;\n  final _cache = Cache();\n  X(this.a);\n\n  void viaThis() {\n    this.a.m();\n    this._cache.get().save();\n  }\n}\n\nclass Y {\n  void m() {}\n  X go(Object o) => X(A());\n}\n\nclass W {\n  final B a;\n  W(this.a);\n}\n\nclass Base {\n  void dispose() {}\n}\n\nclass Child extends Base {\n  @override\n  void dispose() {\n    super.dispose();\n  }\n}\n\nextension ObjectX<T> on T {\n  T also() => this;\n}\n\nfinal repo = Repo();\n\nRepo make() => Repo();\n\nvoid go() {}\n\nvoid touch() {}\n\nclass Konst {\n  const Konst();\n}\n")
            .file(
                "lib/prefixed.dart",
                "import 'types.dart' as t;\n\nvoid prefixed() {\n  final k = const t.Konst();\n  final n = new t.Konst();\n}\n",
            )
            .file("lib/use.dart", "import 'types.dart';\n\nvoid viaMember(X x, X? n) {\n  x.a.m();\n  x.a?.m();\n  n?.a.m();\n}\n\nvoid twoChains(Y y, W w) {\n  y.go(1).a.m();\n  w.a.m();\n}\n\nvoid nullAware(Y? y, Object? u) {\n  y?.m();\n  make()?.load().save();\n  u?.unknown();\n}\n\nvoid multiLine(Repo r) {\n  r\n      .load()\n      .save();\n}\n\nvoid nested(Y y) {\n  y.go(go()).a.m();\n}\n\nvoid sameLine(Repo r) {\n  touch(); r.load().touch();\n}\n\nvoid repeated(A a, P p, Builder b) {\n  a.next().next().m();\n  p.flip().flip().flip();\n  b.add().add().add();\n}\n\nvoid statics() {\n  Repo.create().load().save();\n}\n\nvoid topLevel() {\n  repo.load().touch().save();\n}\n\nvoid universal(Item item) {\n  item.touch().also().save();\n}\n\nvoid selfNamed() {\n  final self = Item();\n  self.touch().save();\n}\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let from = |caller: &str| -> Vec<String> {
            calls_from(graph, "lib/use.dart")
                .into_iter()
                .filter(|edge| edge.starts_with(&format!("{caller} -> ")))
                .collect()
        };
        assert_eq!(
            from("viaMember"),
            [
                "viaMember -> A::m",
                "viaMember -> A::m",
                "viaMember -> A::m"
            ],
            "{report}"
        );
        // Each chain's link is its own: `w.a.m()` below is another chain's,
        // through a W's B.
        assert_eq!(
            from("twoChains"),
            [
                "twoChains -> A::m",
                "twoChains -> B::m",
                "twoChains -> Y::go"
            ],
            "{report}"
        );
        assert_eq!(
            from("nullAware"),
            [
                "nullAware -> Item::save",
                "nullAware -> Repo::load",
                "nullAware -> Y::m",
                "nullAware -> make"
            ],
            "{report}"
        );
        assert!(
            graph
                .unresolved
                .iter()
                .any(|r| r.reference_name == "u.unknown"),
            "{report}"
        );
        assert_eq!(
            from("multiLine"),
            ["multiLine -> Item::save", "multiLine -> Repo::load"],
            "{report}"
        );
        assert_eq!(
            from("nested"),
            ["nested -> A::m", "nested -> Y::go", "nested -> go"],
            "{report}"
        );
        assert_eq!(
            from("sameLine"),
            [
                "sameLine -> Item::touch",
                "sameLine -> Repo::load",
                "sameLine -> touch"
            ],
            "{report}"
        );
        assert_eq!(
            from("repeated"),
            [
                "repeated -> A::m",
                "repeated -> A::next",
                "repeated -> B::next",
                // `b.add` and both links: one site, one edge.
                "repeated -> Builder::add",
                "repeated -> P::flip",
            ],
            "{report}"
        );
        assert_eq!(
            from("statics"),
            [
                "statics -> Item::save",
                "statics -> Repo::create",
                "statics -> Repo::load"
            ],
            "{report}"
        );
        assert_eq!(
            from("topLevel"),
            [
                "topLevel -> Item::save",
                "topLevel -> Item::touch",
                "topLevel -> Repo::load"
            ],
            "{report}"
        );
        // `also` returns its extension's own `T`: what it is called on says
        // nothing more, so `save` links nothing.
        assert_eq!(
            from("universal"),
            ["universal -> Item::touch", "universal -> ObjectX::also"],
            "{report}"
        );
        // A Dart `self` is a variable like any other, not the object as in
        // Python or Rust: `self.touch()` is a call through it, and the link
        // after it is typed by its declaration.
        assert_eq!(
            from("selfNamed"),
            ["selfNamed -> Item::save", "selfNamed -> Item::touch"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/types.dart"),
            [
                "dispose -> Base::dispose",
                "viaThis -> A::m",
                "viaThis -> Cache::get",
                "viaThis -> Item::save",
            ],
            "{report}"
        );
        // A constructor called through an import prefix is no chain link: it
        // instantiates what that import exports.
        let mut built: Vec<(Option<i64>, String)> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "instantiates")
            .filter(|e| {
                graph
                    .node(&e.source)
                    .is_some_and(|n| n.qualified_name == "prefixed")
            })
            .filter_map(|e| Some((e.line, graph.node(&e.target)?.qualified_name.clone())))
            .collect();
        built.sort();
        assert_eq!(
            built,
            [
                (Some(4), "Konst".to_string()),
                (Some(5), "Konst".to_string())
            ],
            "{report}"
        );
        let create = graph
            .nodes
            .iter()
            .find(|n| n.qualified_name == "Repo::create")
            .expect("Repo::create");
        assert!(create.is_static, "{report}");
        assert_eq!(create.return_type.as_deref(), Some("Repo"), "{report}");
        let touch = graph
            .nodes
            .iter()
            .find(|n| n.qualified_name == "Item::touch")
            .expect("Item::touch");
        assert!(!touch.is_static, "{report}");
        assert_eq!(touch.return_type.as_deref(), Some("Item"), "{report}");
    }
}

mod dart_local_scope {
    use super::support::{Graph, Project};

    /// `<target qualified name>:<line>` of each call or construction out of
    /// the node `qualified` of `file`, sorted (upstream's `calls`).
    fn calls(graph: &Graph, file: &str, qualified: &str) -> Vec<String> {
        let node = graph
            .nodes
            .iter()
            .find(|n| n.file_path == file && n.qualified_name == qualified)
            .unwrap_or_else(|| panic!("{file} {qualified}\n{}", graph.report()));
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.source == node.id && (e.kind == "calls" || e.kind == "instantiates"))
            .filter_map(|e| {
                Some(format!(
                    "{}:{}",
                    graph.node(&e.target)?.qualified_name,
                    e.line?
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// KEEP-RUST: the extractor records a chain's later link where the chain
    /// starts, so whether a call has a receiver is read as the bare-call
    /// rules read it: `repo.build().create()` calls `Builder.create`, though
    /// the local it initializes, written nearer the chain's start, is named
    /// `create` too. A local function is no member: inside a method it is
    /// nearer than the method the class inherits, and a spread's `...items()`
    /// calls its parameter (W4-DART-03c).
    #[test]
    fn keep_rust_a_chain_link_and_a_local_function_are_read_as_written() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/repo.dart",
                "class Item {}\n\nclass Builder {\n  Item create() => Item();\n}\n\nclass Repo {\n  Builder build() => Builder();\n}\n\nvoid make(Repo repo) {\n  final create = repo.build().create();\n  print(create);\n}\n\nclass Base {\n  void local() {}\n}\n\nclass Sub extends Base {\n  void m() {\n    void local() {}\n    local();\n  }\n}\n\nList<int> items() => [];\n\nList<int> spread(List<int> Function() items) => [...items()];\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls(graph, "lib/repo.dart", "make"),
            ["Builder::create:12", "Repo::build:12"],
            "{report}"
        );
        assert_eq!(
            calls(graph, "lib/repo.dart", "Sub::m"),
            ["Sub::m::local:23"],
            "{report}"
        );
        // A spread's call has no receiver: the parameter it calls.
        assert_eq!(
            calls(graph, "lib/repo.dart", "spread"),
            Vec::<String>::new(),
            "{report}"
        );
    }

    /// Upstream #2379 (`dart-local-call-scope.test.ts`, its fixture): a bare
    /// Dart call to a name a parameter or local around it binds calls that
    /// parameter or local, never a namesake elsewhere — riverpod's
    /// `create(ref, arg)`, a closure's parameter, a loop variable, a test's
    /// mock, a bloc handler's `emit` parameter — while calls the binding does
    /// not cover, `this.` calls and calls to a local function link as before.
    /// KEEP-RUST: the extractor qualifies a local function by the function
    /// it is declared in (`outer::helper`, `outer::make`; upstream's are
    /// `helper`, `make`), and attributes a call in a local function's body to
    /// the function around it too.
    #[test]
    fn pr2379_a_dart_call_to_a_parameter_or_local_calls_that() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("doc/mutation.dart", "class Mutation<T> {\n  Object call(String key) => key;\n}\n\nfinal create = Mutation<Object>();\nfinal createTodo = create('create_todo');\n")
            .file("lib/family.dart", "class Ref {}\n\nclass Override {\n  Override(Object Function(Ref ref) build);\n}\n\nmixin FunctionalFamilyOverride<CreatedT, ArgT> {\n  ArgT get argument => throw 0;\n\n  Override overrideWith(CreatedT Function(Ref ref, ArgT arg) create) {\n    return Override((ref) => create(ref, argument) as Object);\n  }\n}\n\nclass Cache<T> {\n  final _values = <Object, T>{};\n\n  T upsert(Object key, T Function() create) {\n    final existing = _values[key];\n    if (existing != null) return existing;\n    final created = create();\n    _values[key] = created;\n    return created;\n  }\n}\n\nclass Holder {\n  void create() {}\n\n  void run(void Function() create) {\n    this.create();\n    create();\n  }\n}\n\nenum Kind { set, list }\n\nR pick<R>({required R Function() set, required R Function() list}) => set();\n\nclass Box {}\n\nBox? find() => null;\n\nvoid check() {\n  final Box? box = find();\n  print(box);\n  Box();\n}\n")
            .file("test/matrix.dart", "typedef Factory = Object Function(Object Function(Object ref, Object? arg) create, {String? name});\n\nfinal factories = <Factory>[\n  (create, {name}) => create(Object(), name),\n];\n\nfinal generic = <T>(T Function() create) => create();\n")
            .file("test/other_test.dart", "void listener() {}\n\nvoid fn() {}\n")
            .file("test/listener_test.dart", "class Listener<T> {\n  void call(T? previous, T next) {}\n}\n\nvoid main() {\n  final listener = Listener<int>();\n  listener(0, 1);\n\n  void Function() build = () {};\n  build();\n\n  for (final fn in <void Function()>[]) {\n    fn();\n  }\n\n  final (create, _) = (() => 0, 1);\n  create();\n}\n")
            .file("lib/bloc.dart", "abstract class Emitter<S> {\n  void call(S state);\n}\n\nclass Bloc<S> {\n  void emit(S state) {}\n\n  void on<E>(void Function(E event, Emitter<S> emit) handler) {}\n}\n\nclass CounterBloc extends Bloc<int> {\n  CounterBloc() {\n    on<String>((event, emit) => emit(1));\n  }\n\n  void onReset(String event, Emitter<int> emit) {\n    emit(0);\n  }\n\n  void bump() => emit(2);\n}\n")
            .file("lib/util.dart", "void notify(int value) {}\n")
            .file("lib/wire.dart", "import 'util.dart';\n\nvoid wire(List<int> values) {\n  values.forEach((notify) => print(notify));\n  notify(1);\n  for (final notify in values) {\n    print(notify);\n  }\n  notify(2);\n}\n\nint outer(int Function() make) {\n  int helper() {\n    int make() => 2;\n    return make();\n  }\n\n  return helper() + make();\n}\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let none = Vec::<String>::new();

        // Only the docs example's own file calls into it.
        let docs: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == "doc/mutation.dart")
            .map(|n| n.id.as_str())
            .collect();
        let mut callers: Vec<&str> = graph
            .edges
            .iter()
            .filter(|e| {
                (e.kind == "calls" || e.kind == "instantiates") && docs.contains(&e.target.as_str())
            })
            .filter_map(|e| Some(graph.node(&e.source)?.file_path.as_str()))
            .collect();
        callers.dedup();
        assert_eq!(callers, ["doc/mutation.dart"], "{report}");

        // A parameter: a method's, a function-typed one inside a closure,
        // closures' in initializers; one may be named `set`.
        assert_eq!(
            calls(
                graph,
                "lib/family.dart",
                "FunctionalFamilyOverride::overrideWith"
            ),
            ["Override:11"],
            "{report}"
        );
        assert_eq!(
            calls(graph, "lib/family.dart", "Cache::upsert"),
            none,
            "{report}"
        );
        assert_eq!(calls(graph, "lib/family.dart", "pick"), none, "{report}");
        assert_eq!(
            calls(graph, "test/matrix.dart", "factories"),
            none,
            "{report}"
        );
        assert_eq!(
            calls(graph, "test/matrix.dart", "generic"),
            none,
            "{report}"
        );

        // A parameter shadows a method of the class, which `this.` still
        // reaches.
        assert_eq!(
            calls(graph, "lib/family.dart", "Holder::run"),
            ["Holder::create:31"],
            "{report}"
        );

        // A local: a mock, a function-typed variable, a loop variable, a
        // destructured one.
        assert_eq!(
            calls(graph, "test/listener_test.dart", "main"),
            ["Listener:6"],
            "{report}"
        );
        let other: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.file_path == "test/other_test.dart")
            .map(|n| n.id.as_str())
            .collect();
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && other.contains(&e.target.as_str())),
            "{report}"
        );

        // A handler's `Emitter` parameter is not `Bloc.emit`; a bare call of
        // the inherited method is.
        assert_eq!(
            calls(graph, "lib/bloc.dart", "CounterBloc"),
            ["Bloc::on:13"],
            "{report}"
        );
        assert_eq!(
            calls(graph, "lib/bloc.dart", "CounterBloc::onReset"),
            none,
            "{report}"
        );
        assert_eq!(
            calls(graph, "lib/bloc.dart", "CounterBloc::bump"),
            ["Bloc::emit:20"],
            "{report}"
        );

        // A sibling closure's parameter and a finished loop's variable cover
        // nothing after them.
        assert_eq!(
            calls(graph, "lib/wire.dart", "wire"),
            ["notify:5", "notify:9"],
            "{report}"
        );

        // A local function is nearer than the parameter it shadows, inside
        // its block only.
        assert_eq!(
            calls(graph, "lib/wire.dart", "outer::helper"),
            ["outer::make:15"],
            "{report}"
        );
        let outer = calls(graph, "lib/wire.dart", "outer");
        assert!(outer.contains(&"outer::helper:18".to_string()), "{report}");
        assert!(!outer.contains(&"outer::make:18".to_string()), "{report}");

        // A nullable typed local's type is not a binding: `Box()` still
        // constructs `Box`.
        assert_eq!(
            calls(graph, "lib/family.dart", "check"),
            ["Box:47", "find:45"],
            "{report}"
        );
    }
}

mod dart_constructors {
    use super::support::{Graph, NodeRow, Project};

    fn find<'g>(graph: &'g Graph, file: &str, qualified: &str) -> Option<&'g NodeRow> {
        graph
            .nodes
            .iter()
            .find(|n| n.file_path == file && n.qualified_name == qualified)
    }

    fn node<'g>(graph: &'g Graph, file: &str, qualified: &str) -> &'g NodeRow {
        find(graph, file, qualified)
            .unwrap_or_else(|| panic!("{file} {qualified}\n{}", graph.report()))
    }

    /// The qualified names `from`'s `kind` edges reach, deduplicated and
    /// sorted (upstream's `targets`).
    fn targets(graph: &Graph, from: &NodeRow, kind: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.source == from.id && e.kind == kind)
            .filter_map(|e| Some(graph.node(&e.target)?.qualified_name.clone()))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// `<kind> <source qualified name>` of every edge but `contains` into
    /// `to`, deduplicated and sorted (upstream's `sources`).
    fn sources(graph: &Graph, to: &NodeRow) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.target == to.id && e.kind != "contains")
            .filter_map(|e| {
                Some(format!(
                    "{} {}",
                    e.kind,
                    graph.node(&e.source)?.qualified_name
                ))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// KEEP-RUST (W4-DART-03c): this grammar puts the annotations of a
    /// member with a body inside the declaration its node is made of, so a
    /// member's node starts at its annotations where upstream's starts at its
    /// signature. Whether a member is a constructor, a getter or a setter is
    /// read past them: riverpod's `@visibleForTesting factory
    /// ProviderContainer.test(…)` is a constructor its 1269 calls reach, an
    /// annotated getter is read, and an annotated setter is no call target.
    #[test]
    fn keep_rust_a_dart_members_head_is_read_past_its_annotations() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/container.dart",
                "class Item {\n  void save() {}\n}\n\nclass ProviderContainer {\n  ProviderContainer();\n\n  /// For tests.\n  @visibleForTesting\n  // ignore: invalid_use\n  factory ProviderContainer.test({int retry = 0}) {\n    return ProviderContainer();\n  }\n\n  @override\n  @protected\n  Item get item => Item();\n}\n\nclass Slot {\n  @override\n  set value(int v) {}\n}\n\nclass Box {\n  Slot wrap() => Slot();\n}\n",
            )
            .file(
                "lib/use.dart",
                "import 'container.dart';\n\nvoid made() {\n  ProviderContainer.test();\n}\n\nvoid read(ProviderContainer container) {\n  container.item.save();\n}\n\nvoid write(Box box) {\n  box.wrap().value(1);\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let use_calls = |name: &str| targets(graph, node(graph, "lib/use.dart", name), "calls");
        assert_eq!(use_calls("made"), ["ProviderContainer::test"], "{report}");
        assert_eq!(
            use_calls("read"),
            ["Item::save", "ProviderContainer::item"],
            "{report}"
        );
        assert_eq!(use_calls("write"), ["Box::wrap"], "{report}");
    }

    /// A constructor is torn off through its class, never by its name alone
    /// (upstream #2380): a function value `internal` passed on, which no
    /// local binds, is not `ChangeNotifierProvider.internal`.
    #[test]
    fn pr2380_a_dart_constructor_is_no_bare_function_value() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/provider.dart",
                "class ChangeNotifierProvider {\n  const ChangeNotifierProvider.internal();\n\n  void wire() => register(internal);\n}\n\nvoid register(Object Function() make) {}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let internal = node(
            graph,
            "lib/provider.dart",
            "ChangeNotifierProvider::internal",
        );
        assert_eq!(sources(graph, internal), Vec::<String>::new(), "{report}");
        // The function value is recorded, and left unresolved.
        assert!(
            graph
                .unresolved
                .iter()
                .any(|r| r.reference_name == "internal" && r.line == 4),
            "{report}"
        );
        assert_eq!(
            targets(
                graph,
                node(graph, "lib/provider.dart", "ChangeNotifierProvider::wire"),
                "calls"
            ),
            ["register"],
            "{report}"
        );
    }

    /// Upstream #2380 (`dart-const-constructors.test.ts`, its fixture): named
    /// `const` constructors and redirecting factories are methods named by the
    /// constructor, returning their class, with a parameters-only signature
    /// (the unnamed ones stay unindexed, and a redirect's target names
    /// nothing); a redirecting factory refers to the class it constructs; and
    /// a call through a type's name reaches that type's own constructor or
    /// static member, type arguments or not, or nothing: never another type's
    /// member of that name, a supertype's, a same-file look-alike's, an
    /// instance getter sharing a factory's name, or a project method sharing
    /// an SDK call's name. A constructor is never reached by a bare name, the
    /// last link of a chain or an annotation. KEEP-RUST: the extractor keeps
    /// `BlocProvider<Object>.value(…)` by its member at the chain's start, and
    /// the chain reader types `BlocProvider<Object>` as the type (W4-DART-03b)
    /// where upstream records `BlocProvider.value`; the edges are the same.
    #[test]
    fn pr2380_a_dart_call_through_a_type_reaches_that_types_own_constructor() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("lib/bloc_provider.dart", "class BlocProvider<T> {\n  const BlocProvider({required T Function() create}) : _create = create, _value = null;\n\n  const BlocProvider.value({required T value}) : this._(value: value);\n\n  const BlocProvider._({T? value}) : _create = null, _value = value;\n\n  final T Function()? _create;\n  final T? _value;\n\n  void handle(Object value) => accept(value);\n}\n\nvoid accept(Object o) {}\n")
            .file("lib/repository_provider.dart", "class RepositoryProvider<T> {\n  RepositoryProvider({required T Function() create});\n\n  RepositoryProvider.value({required T value});\n\n  static T of<T>(Object context) => throw 0;\n}\n")
            .file("lib/new_car_state.dart", "class NewCarState {\n  const NewCarState._({this.brands = const []});\n\n  const NewCarState.initial() : this._();\n\n  const factory NewCarState.loading() = _Loading;\n\n  const factory NewCarState() = _Loading.named;\n\n  const factory NewCarState.fromJson(Map<String, Object?> json) = _Loading.fromJson;\n\n  final List<String> brands;\n}\n\nclass _Loading extends NewCarState {\n  const _Loading() : super._();\n\n  const _Loading.named() : super._();\n\n  const _Loading.fromJson(Map<String, Object?> json) : super._();\n}\n")
            .file("lib/profile_wizard_state.dart", "class ProfileWizardState {\n  ProfileWizardState.initial() : step = 0;\n\n  final int step;\n\n  Object fromJson(Object json) => json;\n}\n")
            .file("lib/family.dart", "class EmptyFamily extends Family {\n  EmptyFamily._() : super._();\n}\n\nclass Family {\n  const Family._();\n\n  static Family make() => Family._();\n}\n")
            .file("lib/sub.dart", "abstract class Base {\n  const Base();\n\n  factory Base.named() => const _BaseImpl();\n}\n\nclass _BaseImpl extends Base {\n  const _BaseImpl();\n}\n\nclass Sub extends Base {\n  const Sub();\n\n  const factory Sub.named() = _SubImpl;\n}\n\nclass _SubImpl extends Sub {\n  const _SubImpl();\n}\n")
            .file("lib/analysis_options.dart", "class AnalysisOptions {\n  const AnalysisOptions(this.path);\n\n  factory AnalysisOptions.parse(String path) => AnalysisOptions(path);\n\n  final String path;\n}\n")
            .file("lib/linter.dart", "extension on Uri {\n  String get canonicalizedPath => path;\n}\n")
            .file("lib/async_value.dart", "sealed class AsyncValue<T> {\n  const AsyncValue._();\n\n  Object? get error => null;\n\n  const factory AsyncValue.error(Object error) = AsyncError<T>;\n}\n\nclass AsyncError<T> extends AsyncValue<T> {\n  const AsyncError(this.error) : super._();\n\n  @override\n  final Object error;\n}\n\nObject fail(Object e) => AsyncValue.error(e);\n")
            .file("lib/change_notifier_provider.dart", "class ChangeNotifierProvider {\n  const ChangeNotifierProvider.internal();\n}\n")
            .file("lib/scope.dart", "import 'package:meta/meta.dart';\n\n@internal\nObject guarded() => 0;\n")
            .file("lib/app.dart", "import 'bloc_provider.dart';\nimport 'repository_provider.dart';\nimport 'new_car_state.dart';\nimport 'sub.dart';\n\nObject build(Object counter) => BlocProvider.value(value: counter);\nObject typed(Object counter) => BlocProvider<Object>.value(value: counter);\nObject typedRepository(Object repository) => RepositoryProvider<Object>.value(value: repository);\nNewCarState start() => NewCarState.initial();\nNewCarState load() => NewCarState.loading();\nSub sub() => Sub.named();\nObject find(Object context) => BlocProvider.of(context);\nUri link() => Uri.parse('https://example.com');\nObject tail(dynamic box) => box.inner.value(1);\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let none = Vec::<String>::new();

        let value = node(graph, "lib/bloc_provider.dart", "BlocProvider::value");
        assert_eq!(value.kind, "method", "{report}");
        assert_eq!(
            value.return_type.as_deref(),
            Some("BlocProvider"),
            "{report}"
        );
        assert_eq!(
            value.signature.as_deref(),
            Some("({required T value})"),
            "{report}"
        );
        node(graph, "lib/bloc_provider.dart", "BlocProvider::_");
        assert!(
            find(
                graph,
                "lib/bloc_provider.dart",
                "BlocProvider::BlocProvider"
            )
            .is_none(),
            "{report}"
        );
        node(graph, "lib/new_car_state.dart", "NewCarState::initial");
        let loading = node(graph, "lib/new_car_state.dart", "NewCarState::loading");
        assert_eq!(loading.signature.as_deref(), Some("()"), "{report}");
        assert!(
            find(graph, "lib/new_car_state.dart", "NewCarState::NewCarState").is_none(),
            "{report}"
        );
        assert!(
            find(graph, "lib/new_car_state.dart", "NewCarState::named").is_none(),
            "{report}"
        );
        node(graph, "lib/new_car_state.dart", "_Loading::named");
        node(graph, "lib/sub.dart", "Sub::named");
        // A redirecting factory refers to the class it constructs, and not to
        // that class's constructor as if it were a type.
        assert_eq!(
            targets(graph, loading, "references"),
            ["_Loading"],
            "{report}"
        );
        assert_eq!(
            targets(
                graph,
                node(graph, "lib/new_car_state.dart", "NewCarState::fromJson"),
                "references"
            ),
            ["_Loading"],
            "{report}"
        );
        assert_eq!(
            sources(
                graph,
                node(
                    graph,
                    "lib/profile_wizard_state.dart",
                    "ProfileWizardState::fromJson"
                )
            ),
            none,
            "{report}"
        );

        // Each call reaches the constructor of the type it names, type
        // arguments or not...
        let app = |name: &str| node(graph, "lib/app.dart", name);
        assert_eq!(
            targets(graph, app("build"), "calls"),
            ["BlocProvider::value"],
            "{report}"
        );
        assert_eq!(
            targets(graph, app("typed"), "calls"),
            ["BlocProvider::value"],
            "{report}"
        );
        assert_eq!(
            targets(graph, app("typedRepository"), "calls"),
            ["RepositoryProvider::value"],
            "{report}"
        );
        assert_eq!(
            targets(graph, app("start"), "calls"),
            ["NewCarState::initial"],
            "{report}"
        );
        assert_eq!(
            targets(graph, app("load"), "calls"),
            ["NewCarState::loading"],
            "{report}"
        );
        assert_eq!(
            targets(graph, app("sub"), "calls"),
            ["Sub::named"],
            "{report}"
        );
        assert_eq!(
            targets(
                graph,
                node(graph, "lib/family.dart", "Family::make"),
                "calls"
            ),
            ["Family::_"],
            "{report}"
        );
        // A constructor, not the instance getter that shares its name.
        let factory = graph
            .nodes
            .iter()
            .find(|n| {
                n.file_path == "lib/async_value.dart"
                    && n.qualified_name == "AsyncValue::error"
                    && n.signature.as_deref() == Some("(Object error)")
            })
            .unwrap_or_else(|| panic!("the error factory\n{report}"));
        let fail = node(graph, "lib/async_value.dart", "fail");
        let called: Vec<&str> = graph
            .edges
            .iter()
            .filter(|e| e.source == fail.id && e.kind == "calls")
            .map(|e| e.target.as_str())
            .collect();
        assert_eq!(called, [factory.id.as_str()], "{report}");
        // ...and never another type's member of that name.
        assert_eq!(
            sources(
                graph,
                node(
                    graph,
                    "lib/repository_provider.dart",
                    "RepositoryProvider::value"
                )
            ),
            ["calls typedRepository"],
            "{report}"
        );
        assert_eq!(
            sources(
                graph,
                node(
                    graph,
                    "lib/profile_wizard_state.dart",
                    "ProfileWizardState::initial"
                )
            ),
            none,
            "{report}"
        );
        assert_eq!(
            sources(graph, node(graph, "lib/family.dart", "EmptyFamily::_")),
            none,
            "{report}"
        );
        assert_eq!(
            sources(graph, node(graph, "lib/sub.dart", "Base::named")),
            none,
            "{report}"
        );
        // A member the named type does not declare is outside the project.
        assert_eq!(targets(graph, app("find"), "calls"), none, "{report}");
        assert_eq!(
            sources(
                graph,
                node(
                    graph,
                    "lib/repository_provider.dart",
                    "RepositoryProvider::of"
                )
            ),
            none,
            "{report}"
        );
        assert_eq!(targets(graph, app("link"), "calls"), none, "{report}");
        assert_eq!(
            sources(
                graph,
                node(graph, "lib/analysis_options.dart", "AnalysisOptions::parse")
            ),
            none,
            "{report}"
        );

        // A constructor is only ever reached through its class: a parameter
        // that shares its name, the last link of a chain, or an annotation is
        // not it.
        assert_eq!(targets(graph, app("tail"), "calls"), none, "{report}");
        assert_eq!(
            sources(graph, value),
            ["calls build", "calls typed"],
            "{report}"
        );
        assert_eq!(
            sources(
                graph,
                node(
                    graph,
                    "lib/change_notifier_provider.dart",
                    "ChangeNotifierProvider::internal"
                )
            ),
            none,
            "{report}"
        );
    }

    /// KEEP-RUST (W4d end review): a named constructor is reached through
    /// its class, never through a value of it. Upstream's member lookup reads
    /// one as an instance member, so `x.value()` on a parameter typed `Foo`,
    /// a local initialized from a call, or the last link of a chain typed
    /// `Foo` called `Foo.value`'s constructor over the `value()` an extension
    /// on `Foo` brings; with no such member it links nothing, also through the
    /// matcher's fallbacks that read a receiver `bar` as the class `Bar` or
    /// score a receiver's words (`barValue`).
    #[test]
    fn keep_rust_a_dart_value_never_calls_a_named_constructor_of_its_type() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\nenvironment:\n  sdk: \">=3.0.0 <4.0.0\"\n")
            .file("lib/foo.dart", "class Foo {\n  const Foo.value(this.v);\n\n  final int v;\n}\n\nFoo build() => const Foo.value(1);\n\nextension FooX on Foo {\n  int value() => v;\n}\n")
            .file("lib/bar.dart", "class Bar {\n  const Bar.empty();\n}\n\nBar make() => const Bar.empty();\n")
            .file("lib/use.dart", "import 'foo.dart';\nimport 'bar.dart';\n\nint viaParameter(Foo x) => x.value();\n\nint viaLocal() {\n  final Foo y = build();\n  return y.value();\n}\n\nint viaChain() => build().value();\n\nFoo viaType() => Foo.value(2);\n\nvoid emptyParameter(Bar b) => b.empty();\n\nvoid emptyChain() => make().empty();\n\nvoid emptyNamed(Bar bar) => bar.empty();\n\nvoid emptyWord(Bar barValue) => barValue.empty();\n\nvoid emptyUntyped(dynamic bar) => bar.empty();\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let none = Vec::<String>::new();
        let usage = |name: &str| node(graph, "lib/use.dart", name);
        for caller in ["viaParameter", "viaLocal", "viaChain"] {
            assert!(
                targets(graph, usage(caller), "calls").contains(&"FooX::value".to_string()),
                "{caller}\n{report}"
            );
        }
        // Through its class, the constructor is still what the call means.
        assert_eq!(
            targets(graph, usage("viaType"), "calls"),
            ["Foo::value"],
            "{report}"
        );
        assert_eq!(
            sources(graph, node(graph, "lib/foo.dart", "Foo::value")),
            ["calls build", "calls viaType"],
            "{report}"
        );
        // No member of that name: nothing to call, whatever the receiver is
        // named (`bar` reads as the class `Bar`, `barValue` shares its word)
        // or however little is known of it.
        for caller in ["emptyParameter", "emptyNamed", "emptyWord", "emptyUntyped"] {
            assert_eq!(
                targets(graph, usage(caller), "calls"),
                none,
                "{caller}\n{report}"
            );
        }
        assert_eq!(
            sources(graph, node(graph, "lib/bar.dart", "Bar::empty")),
            ["calls make"],
            "{report}"
        );
        assert!(
            !targets(graph, usage("emptyChain"), "calls").contains(&"Bar::empty".to_string()),
            "{report}"
        );
    }
}

mod dart_prefixes {
    use super::support::{Graph, Project};

    /// `<kind> <line> -> <target file>:<target qualified name>` for every call
    /// or construction out of `file`'s nodes, sorted (upstream's `callsFrom`,
    /// without the reference name: KEEP-RUST, a chain's link is recorded by
    /// its member where the chain starts).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls" || e.kind == "instantiates")
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                (source.file_path == file).then_some(())?;
                let target = graph.node(&e.target)?;
                Some(format!(
                    "{} {} -> {}:{}",
                    e.kind, e.line?, target.file_path, target.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2389 (`dart-import-prefix-calls.test.ts`, its fixture): a
    /// Dart call through an import prefix calls a top-level declaration the
    /// libraries imported with that prefix export, `export` chains, `show`
    /// and `hide` followed and a part taking its library's prefixes, or
    /// nothing in the project (a package outside the repository, a name not
    /// shown or hidden, a deferred library's `loadLibrary`); a constructor
    /// call constructs the prefixed library's class, and a chain headed by
    /// the prefix (`p.Box.create()`, `p.Box<int>.named()`) is on the type the
    /// prefix brings in. Never another library's namesake or a method by the
    /// receiver's name; a parameter named like the prefix is the parameter.
    /// KEEP-RUST: `const kit.Box.named()` is read as the named constructor it
    /// calls (W4-DART-03a), so it links `Box.named` of the type kit brings
    /// in, where upstream records `kit.named` and links nothing.
    #[test]
    fn pr2389_a_dart_call_through_an_import_prefix_reaches_what_that_import_exports() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file("lib/legacy.dart", "class Http {\n  Future<String> get(Uri url) async => '';\n}\n\nclass Client {\n  Client();\n  static Client fromEnv() => Client();\n}\n\nclass Formatter {\n  String describe(Object value) => '';\n}\n\nclass Loader {\n  int load() => 0;\n}\n\nclass Report {\n  Report.empty();\n  static Report blank() => Report.empty();\n}\n\nclass Box<T> {\n  const Box.named();\n  static Box<int> create() => const Box.named();\n}\n\nString describe(Object value) => 'legacy';\nint shown() => 0;\n")
            .file("lib/src/format.dart", "String describe(Object value) => '$value';\n\nclass Report {\n  Report();\n  Report.empty();\n  static Report blank() => Report();\n}\n")
            .file("lib/src/tools.dart", "int secret() => 1;\nint open() => 2;\n")
            .file("lib/lazy.dart", "int load() => 1;\n")
            .file("packages/kit/pubspec.yaml", "name: kit\n")
            .file("packages/kit/lib/kit.dart", "export 'src/impl.dart' show shown, Widget, Box, named;\n")
            .file("packages/kit/lib/src/impl.dart", "int shown() => 1;\nint notShown() => 2;\nint named() => 3;\n\nclass Widget {\n  Widget();\n}\n\nclass Box<T> {\n  const Box.named();\n  static Box<int> create() => const Box.named();\n}\n")
            .file("lib/net.dart", "import 'package:http/http.dart' as http;\n\nFuture<void> fetch() async {\n  await http.get(Uri.parse('https://example.com'));\n  http.Client();\n  http.Client.fromEnv();\n}\n")
            .file("lib/app.dart", "import 'package:kit/kit.dart' as kit;\nimport 'src/format.dart' as fmt;\nimport 'src/tools.dart' as tools hide secret;\nimport 'lazy.dart' deferred as lazy;\n\npart 'app_part.dart';\n\nString run(Object value) {\n  fmt.Report();\n  fmt.Report.empty();\n  fmt.Report.blank();\n  kit.shown();\n  kit.notShown();\n  kit.Widget();\n  tools.open();\n  tools.secret();\n  final box = kit.Box<int>.named();\n  kit.Box.create();\n  const kit.Box.named();\n  return fmt.describe(value);\n}\n\nFuture<int> boot() async {\n  await lazy.loadLibrary();\n  return lazy.load();\n}\n")
            .file("lib/app_part.dart", "part of 'app.dart';\n\nString again(Object value) => fmt.describe(value);\n")
            .file("lib/shadow.dart", "import 'package:http/http.dart' as http;\nimport 'legacy.dart';\n\nFuture<String> viaParameter(Http http) => http.get(Uri());\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "lib/net.dart"),
            Vec::<String>::new(),
            "{report}"
        );
        let app = calls_from(graph, "lib/app.dart");
        for edge in [
            "calls 20 -> lib/src/format.dart:describe",
            "calls 25 -> lib/lazy.dart:load",
            "calls 12 -> packages/kit/lib/src/impl.dart:shown",
            "instantiates 14 -> packages/kit/lib/src/impl.dart:Widget",
            "calls 15 -> lib/src/tools.dart:open",
            "instantiates 9 -> lib/src/format.dart:Report",
            "calls 10 -> lib/src/format.dart:Report::empty",
            "calls 11 -> lib/src/format.dart:Report::blank",
            "calls 17 -> packages/kit/lib/src/impl.dart:Box::named",
            "calls 18 -> packages/kit/lib/src/impl.dart:Box::create",
            "calls 19 -> packages/kit/lib/src/impl.dart:Box::named",
        ] {
            assert!(app.contains(&edge.to_string()), "{edge}\n{report}");
        }
        // Not shown by kit.dart, hidden from `tools`, the deferred library's
        // own `loadLibrary`: lines 13, 16 and 24.
        assert!(
            !app.iter().any(|edge| [" 13 ", " 16 ", " 24 "]
                .iter()
                .any(|line| edge.contains(line))),
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/app_part.dart"),
            ["calls 3 -> lib/src/format.dart:describe"],
            "{report}"
        );
        assert!(
            !app.iter().any(|edge| edge.contains("lib/legacy.dart")),
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "lib/shadow.dart"),
            ["calls 4 -> lib/legacy.dart:Http::get"],
            "{report}"
        );
    }

    /// A chain headed by an import prefix (upstream #2389's
    /// `typeDartPrefixedHead`) is typed from what the prefix brings in: a
    /// function's declared return type, a constant's type; a member or a
    /// parameter named like the prefix hides it, there, as it hides a plain
    /// prefixed call. A null-aware `fmt?.describe(…)` is written on no
    /// prefix, whatever `fmt` names.
    #[test]
    fn pr2389_a_prefixed_chain_is_typed_from_what_its_prefix_brings_in() {
        let project = Project::new()
            .file("pubspec.yaml", "name: app\n")
            .file(
                "lib/src/format.dart",
                "class Item {\n  void save() {}\n}\n\nItem make() => Item();\n\nconst config = Item();\n\nString describe(Object value) => '';\n",
            )
            .file(
                "lib/legacy.dart",
                "class Other {\n  void save() {}\n}\n\nOther make() => Other();\n\nclass Fmt {\n  String describe(Object value) => '';\n\n  Other make() => Other();\n}\n",
            )
            .file(
                "lib/app.dart",
                "import 'src/format.dart' as fmt;\nimport 'legacy.dart';\n\nvoid viaFunction() {\n  fmt.make().save();\n}\n\nvoid viaConstant() {\n  fmt.config.save();\n}\n\nvoid viaParameter(Fmt fmt) {\n  fmt.describe(1);\n}\n\nclass Holder {\n  final Fmt fmt = Fmt();\n\n  void viaMember() {\n    fmt.describe(2);\n  }\n}\n\nvoid viaParameterChain(Fmt fmt) {\n  fmt.make().save();\n}\n\nvoid viaNullableParameter(Fmt? fmt) {\n  fmt?.describe(3);\n}\n",
            )
            .index();
        let graph = project.graph();
        let report = graph.report();
        let calls = calls_from(graph, "lib/app.dart");
        for edge in [
            "calls 5 -> lib/src/format.dart:make",
            "calls 5 -> lib/src/format.dart:Item::save",
            "calls 9 -> lib/src/format.dart:Item::save",
            "calls 13 -> lib/legacy.dart:Fmt::describe",
            "calls 20 -> lib/legacy.dart:Fmt::describe",
            "calls 25 -> lib/legacy.dart:Fmt::make",
            "calls 25 -> lib/legacy.dart:Other::save",
        ] {
            assert!(calls.contains(&edge.to_string()), "{edge}\n{report}");
        }
        assert!(
            !calls
                .iter()
                .any(|edge| edge.contains("Other") && !edge.starts_with("calls 25 ")),
            "{report}"
        );
        assert!(
            !calls
                .iter()
                .any(|edge| edge == "calls 25 -> lib/src/format.dart:Item::save"),
            "{report}"
        );
        assert!(
            !calls
                .iter()
                .any(|edge| edge.contains("format.dart:describe")),
            "{report}"
        );
    }

    /// `<decorated> -> <target kind> <target file>:<target qualified name>`
    /// for each `decorates` edge, sorted (upstream's `decorations`).
    fn decorations(graph: &Graph) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "decorates")
            .filter_map(|e| {
                let from = graph.node(&e.source)?;
                let to = graph.node(&e.target)?;
                Some(format!(
                    "{} -> {} {}:{}",
                    from.qualified_name, to.kind, to.file_path, to.qualified_name
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2390 (`dart-annotation-targets.test.ts`, its fixture): a
    /// Dart annotation is a constant expression, so it links what it is
    /// written as: a constant or (called) a class its library sees, through
    /// its import prefix; a type's static constant, enum value or (called)
    /// named constructor; on a member, a static constant of the type around
    /// it. Never a method, getter or function: `@riverpod` is
    /// riverpod_annotation's constant, not the analyzer's extension getter,
    /// and `@meta.protected` / `@meta.immutable` are package:meta's. KEEP-RUST:
    /// this grammar writes a dotted annotation's name as a `qualified` node,
    /// recorded by its last name as upstream records it.
    #[test]
    fn pr2390_a_dart_annotation_links_what_it_is_written_as() {
        let project = Project::new()
            .file("packages/riverpod_annotation/pubspec.yaml", "name: riverpod_annotation\n")
            .file("packages/riverpod_annotation/lib/riverpod_annotation.dart", "export 'src/riverpod_annotation.dart';\n")
            .file("packages/riverpod_annotation/lib/src/riverpod_annotation.dart", "final class Riverpod {\n  const Riverpod({this.keepAlive = false});\n\n  const Riverpod.forever() : keepAlive = true;\n\n  static const scoped = Riverpod();\n\n  final bool keepAlive;\n}\n\nconst riverpod = Riverpod();\n\nenum Retry { never, always }\n")
            .file("packages/riverpod_analyzer_utils/pubspec.yaml", "name: riverpod_analyzer_utils\n")
            .file("packages/riverpod_analyzer_utils/lib/src/nodes.dart", "part 'nodes/annotation.dart';\n\nconst _ast = Object();\n\nclass AnnotatedNode {}\n\nbool protected(Object node) => true;\n")
            .file("packages/riverpod_analyzer_utils/lib/src/nodes/annotation.dart", "part of '../nodes.dart';\n\n@_ast\nextension RiverpodAnnotatedAnnotatedNodeOfX on AnnotatedNode {\n  Object? get riverpod => null;\n}\n")
            .file("examples/counter/pubspec.yaml", "name: counter\n")
            .file("examples/counter/lib/counter.dart", "import 'package:meta/meta.dart' as meta;\nimport 'package:riverpod_annotation/riverpod_annotation.dart';\n\n@riverpod\nint counter(Object ref) => 0;\n\n@riverpod\n// expect_lint: functional_ref\nint linted(Object ref) => 0;\n\n@Riverpod(keepAlive: true)\nclass Todos {\n  static const marker = Riverpod();\n\n  @marker\n  void add() {}\n\n  @meta.protected\n  void reset() {}\n}\n\n@Riverpod.forever()\nint kept(Object ref) => 0;\n\n@Riverpod.scoped\nint scopedCounter(Object ref) => 0;\n\n@Retry.always\nint retried(Object ref) => 0;\n\n@meta.immutable\nclass Frozen {}\n")
            .file("examples/counter/lib/immutable.dart", "const immutable = Object();\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        let annotation = "packages/riverpod_annotation/lib/src/riverpod_annotation.dart";
        let app = "examples/counter/lib/counter.dart";
        let expected: Vec<String> = [
            "RiverpodAnnotatedAnnotatedNodeOfX -> constant packages/riverpod_analyzer_utils/lib/src/nodes.dart:_ast".to_string(),
            format!("Todos -> class {annotation}:Riverpod"),
            format!("Todos::add -> constant {app}:Todos::marker"),
            format!("counter -> constant {annotation}:riverpod"),
            format!("kept -> method {annotation}:Riverpod::forever"),
            format!("linted -> constant {annotation}:riverpod"),
            format!("retried -> enum_member {annotation}:Retry::always"),
            format!("scopedCounter -> constant {annotation}:Riverpod::scoped"),
        ]
        .into_iter()
        .collect();
        let mut expected = expected;
        expected.sort();
        assert_eq!(decorations(graph), expected, "{report}");
        // The dotted ones are recorded, and left to package:meta.
        for name in ["protected", "immutable"] {
            assert!(
                graph
                    .unresolved
                    .iter()
                    .any(|r| r.reference_kind == "decorates" && r.reference_name == name),
                "{name}\n{report}"
            );
        }
    }
}

mod solidity_scope {
    use super::support::{Graph, Project};

    /// The qualified names of every call edge out of `file`'s nodes, sorted
    /// (upstream's `callsFrom`).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                (source.file_path == file).then_some(())?;
                Some(graph.node(&e.target)?.qualified_name.clone())
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2178 (a) (`solidity-erlang-call-scope.test.ts`, its
    /// Solidity fixture): a Solidity call written without a receiver reaches
    /// a function of the contract around it or of one it inherits through
    /// its `is` clause, never a sibling's override: OpenZeppelin's
    /// `_msgSender()` in a Context-derived Governor went to ERC2771Context's.
    /// The files sit in OpenZeppelin's directories: side by side, as
    /// upstream's fixture has them, the ranking already picks Context's.
    #[test]
    fn pr2178_a_bare_solidity_call_reaches_the_inherited_function_not_a_sibling_override() {
        let project = Project::new()
            .file("contracts/utils/Context.sol", "pragma solidity ^0.8.20;\n\nabstract contract Context {\n    function _msgSender() internal view virtual returns (address) {\n        return msg.sender;\n    }\n}\n")
            .file("contracts/metatx/ERC2771Context.sol", "pragma solidity ^0.8.20;\n\nimport {Context} from \"../utils/Context.sol\";\n\nabstract contract ERC2771Context is Context {\n    function _msgSender() internal view virtual override returns (address) {\n        return msg.sender;\n    }\n}\n")
            .file("contracts/governance/Governor.sol", "pragma solidity ^0.8.20;\n\nimport {Context} from \"../utils/Context.sol\";\n\nabstract contract Governor is Context {\n    function castVote() public returns (address) {\n        return _msgSender();\n    }\n}\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "contracts/governance/Governor.sol"),
            ["Context::_msgSender"],
            "{}",
            graph.report()
        );
    }

    /// The `is` clause is read as written: constructor arguments dropped, a
    /// base named through an import alias by its last name, a head spread
    /// over lines with comments in it, the chain followed through the bases'
    /// own heads; a contract it does not inherit is passed over even in the
    /// same file. A call with a receiver, a library function called through
    /// its library (`MathLib.add(…)`) and a free function are not judged;
    /// outside any contract a bare call reaches no member.
    /// KEEP-RUST: a `//` comment in the head drops only the rest of its own
    /// line (upstream joins the head's lines before dropping it, so the
    /// bases written below it are lost).
    #[test]
    fn keep_rust_a_solidity_is_clause_is_read_as_written() {
        let project = Project::new()
            .file("contracts/Base.sol", "pragma solidity ^0.8.20;\n\nabstract contract Ownable {\n    function _checkOwner() internal view virtual {}\n}\n\nabstract contract Pausable {\n    function _requireNotPaused() internal view virtual {}\n}\n\nabstract contract Nonces {\n    function _useNonce(address owner) internal virtual returns (uint256) {\n        return 0;\n    }\n}\n\nabstract contract Middle is Nonces {}\n\nlibrary MathLib {\n    function add(uint256 a, uint256 b) internal pure returns (uint256) {\n        return a + b;\n    }\n}\n\nfunction freeHelper() pure returns (uint256) {\n    return 3;\n}\n")
            .file("contracts/Vault.sol", "pragma solidity ^0.8.20;\n\nimport \"./Base.sol\" as B;\n\ncontract Sibling {\n    function _checkOwner() internal view {}\n    function _requireNotPaused() internal view {}\n    function _useNonce(address owner) internal returns (uint256) {\n        return 1;\n    }\n    function helper() public pure returns (uint256) {\n        return 2;\n    }\n}\n\ncontract Vault is\n    Ownable(msg.sender), // the owner\n    B.Pausable,\n    /* nonces */ Middle\n{\n    Sibling sibling;\n\n    function run() public returns (uint256) {\n        _checkOwner();\n        _requireNotPaused();\n        _useNonce(msg.sender);\n        sibling.helper();\n        MathLib.add(1, 2);\n        return freeHelper();\n    }\n}\n\nfunction loose() pure returns (uint256) {\n    return helper();\n}\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "contracts/Vault.sol"),
            [
                "MathLib::add",
                "Nonces::_useNonce",
                "Ownable::_checkOwner",
                "Pausable::_requireNotPaused",
                "Sibling::helper",
                "freeHelper",
            ],
            "{}",
            graph.report()
        );
    }
}

mod erlang_imports {
    use super::support::{Graph, Project};

    /// The qualified names of every call edge out of `file`'s nodes, sorted
    /// (upstream's `callsFrom`).
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .filter_map(|e| {
                let source = graph.node(&e.source)?;
                (source.file_path == file).then_some(())?;
                Some(graph.node(&e.target)?.qualified_name.clone())
            })
            .collect();
        out.sort();
        out
    }

    /// Upstream #2178 (b) (`solidity-erlang-call-scope.test.ts`, its Erlang
    /// fixture): another module's function is called bare only through
    /// `-import(Mod, [f/N])`: cowboy's `-import(req_SUITE, [do_get/3])` went
    /// to compress_SUITE's `do_get/3`.
    #[test]
    fn pr2178_a_bare_erlang_call_reaches_the_module_it_is_imported_from() {
        let project = Project::new()
            .file("test/req_SUITE.erl", "-module(req_SUITE).\n-export([do_get/3]).\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n")
            .file("test/compress_SUITE.erl", "-module(compress_SUITE).\n-export([do_get/3]).\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n")
            .file("test/static_handler_SUITE.erl", "-module(static_handler_SUITE).\n-export([etag/1]).\n-import(req_SUITE, [do_get/3]).\n\netag(Config) -> do_get(\"/etag\", [], Config).\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "test/static_handler_SUITE.erl"),
            ["req_SUITE::do_get/3"],
            "{}",
            graph.report()
        );
    }

    /// An import list is read as Erlang writes it: a quoted atom is one name,
    /// whatever comma or slash it holds, and a `%` comment between entries is
    /// no entry (plan gate round 1).
    #[test]
    fn keep_rust_an_erlang_import_list_with_quoted_atoms_and_comments() {
        let project = Project::new()
            .file("test/req_SUITE.erl", "-module(req_SUITE).\n-export(['foo,bar'/1, 'a/b'/2, do_get/3]).\n\n'foo,bar'(X) -> X.\n'a/b'(X, Y) -> {X, Y}.\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n")
            .file("test/compress_SUITE.erl", "-module(compress_SUITE).\n-export(['foo,bar'/1, 'a/b'/2, do_get/3]).\n\n'foo,bar'(X) -> X.\n'a/b'(X, Y) -> {X, Y}.\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n")
            .file("test/quoted_SUITE.erl", "-module(quoted_SUITE).\n-export([run/1]).\n-import(req_SUITE, ['foo,bar'/1, % the odd one, not/9\n                    'a/b'/2,\n                    do_get/3]).\n\nrun(C) -> 'foo,bar'(C), 'a/b'(C, C), do_get(\"/\", [], C).\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            calls_from(graph, "test/quoted_SUITE.erl"),
            [
                "req_SUITE::a/b/2",
                "req_SUITE::do_get/3",
                "req_SUITE::foo,bar/1"
            ],
            "{}",
            graph.report()
        );
    }

    /// With no `-import`, another module's function is never guessed; a
    /// function a `.hrl` declares is (a module includes it); an import list
    /// spread over lines, with quoted atoms, names its functions; a module
    /// the project lacks links nothing; a call inside a `.hrl` reaches what
    /// it reached before.
    #[test]
    fn keep_rust_an_erlang_import_list_and_headers_are_read_as_written() {
        let project = Project::new()
            .file("test/req_SUITE.erl", "-module(req_SUITE).\n-export([do_get/3, do_put/2]).\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\ndo_put(Path, Config) -> {Path, Config}.\n")
            .file("test/compress_SUITE.erl", "-module(compress_SUITE).\n-export([do_get/3, do_put/2, map/2]).\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\ndo_put(Path, Config) -> {Path, Config}.\nmap(F, L) -> {F, L}.\n")
            .file("src/fmt_util.erl", "-module(fmt_util).\n-export([describe/1]).\n\ndescribe(Value) -> Value.\n")
            .file("include/helpers.hrl", "helper(Config) -> describe(Config).\n")
            .file("test/plain_SUITE.erl", "-module(plain_SUITE).\n-export([etag/1]).\n-include(\"../include/helpers.hrl\").\n\netag(Config) -> do_get(\"/etag\", [], Config), helper(Config).\n")
            .file("test/multi_SUITE.erl", "-module(multi_SUITE).\n-export([put/1, mapped/1]).\n-import(req_SUITE,\n        [do_get/3,\n         'do_put'/2]).\n-import(lists, [map/2]).\n\nput(Config) -> do_put(\"/put\", Config).\nmapped(L) -> map(fun(X) -> X end, L).\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(
            calls_from(graph, "test/plain_SUITE.erl"),
            ["helper/1"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "test/multi_SUITE.erl"),
            ["req_SUITE::do_put/2"],
            "{report}"
        );
        assert_eq!(
            calls_from(graph, "include/helpers.hrl"),
            ["fmt_util::describe/1"],
            "{report}"
        );
    }
}

mod shopify_themes {
    use super::support::{Graph, Project};

    /// A JSON template naming one section `type` (upstream's `jsonTemplate`).
    fn json_template(section: &str) -> String {
        format!("{{\"sections\":{{\"main\":{{\"type\":\"{section}\"}}}},\"order\":[\"main\"]}}")
    }

    /// The indexed JSON files, sorted (upstream's `indexedJson`).
    fn indexed_json(graph: &Graph) -> Vec<String> {
        let mut out: Vec<String> = graph
            .files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| path.ends_with(".json"))
            .collect();
        out.sort();
        out
    }

    /// The other files `file`'s nodes link to, sorted and deduplicated
    /// (upstream's `getFileDependencies`).
    fn links_of(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains")
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                (source.file_path == file && target.file_path != file)
                    .then(|| target.file_path.clone())
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The other files whose nodes link to `file`, sorted and deduplicated
    /// (upstream's `getFileDependents`).
    fn dependents_of(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind != "contains")
            .filter_map(|edge| {
                let source = graph.node(&edge.source)?;
                let target = graph.node(&edge.target)?;
                (target.file_path == file && source.file_path != file)
                    .then(|| source.file_path.clone())
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Upstream #2393: JSON in `templates/` or `sections/` folders that no
    /// Shopify theme holds (jasontaylordev/CleanArchitecture's .NET project
    /// template, a CMS's content) is not Liquid, even when a Liquid file is
    /// named like one of its "types".
    #[test]
    fn pr2393_json_under_templates_or_sections_outside_a_theme_is_left_out() {
        let project = Project::new()
            .file(
                "templates/ca-use-case/.template.config/dotnetcli.host.json",
                "{\"symbolInfo\":{\"UseCase\":{\"longName\":\"name\"}}}",
            )
            .file(
                "templates/ca-use-case/.template.config/template.json",
                "{\"identity\":\"CleanArchitecture.UseCase\",\"sections\":{\"main\":{\"type\":\"hero\"}}}",
            )
            .file("sections/home.json", &json_template("hero"))
            .file("sections/hero.liquid", "<h1>{{ page.title }}</h1>\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            indexed_json(graph),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert!(
            graph
                .files
                .iter()
                .any(|file| file.path == "sections/hero.liquid"),
            "the Liquid file itself is still indexed:\n{}",
            graph.report()
        );
        assert_eq!(
            dependents_of(graph, "sections/hero.liquid"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2393: a theme in a subdirectory with no `layout/theme.liquid`
    /// committed is marked by its `config/settings_schema.json`; the same
    /// JSON under `tools/templates/` is in no theme.
    #[test]
    fn pr2393_a_settings_schema_marks_a_theme_in_a_subdirectory() {
        let project = Project::new()
            .file(
                "theme/config/settings_schema.json",
                "[{\"name\":\"theme_info\"}]",
            )
            .file(
                "theme/sections/main-product.liquid",
                "<div>{{ product.title }}</div>\n",
            )
            .file(
                "theme/templates/product.json",
                &json_template("main-product"),
            )
            .file(
                "tools/templates/product.json",
                &json_template("main-product"),
            )
            .index();
        let graph = project.graph();
        assert_eq!(
            indexed_json(graph),
            ["theme/templates/product.json"],
            "{}",
            graph.report()
        );
        assert_eq!(
            dependents_of(graph, "theme/sections/main-product.liquid"),
            ["theme/templates/product.json"],
            "{}",
            graph.report()
        );
    }

    /// Upstream #2393: JSON stored as Liquid leaves once its folder is no
    /// longer a theme, and comes back when it is one again, without a
    /// re-index (a full sync here; the scoped sync is the sync matrix's).
    #[test]
    fn pr2393_json_templates_leave_and_return_with_the_theme_marker() {
        let mut project = Project::new()
            .file("layout/theme.liquid", "{{ content_for_layout }}\n")
            .file(
                "sections/main-product.liquid",
                "<div>{{ product.title }}</div>\n",
            )
            .file("templates/page.json", &json_template("main-product"))
            .file("templates/product.json", &json_template("main-product"))
            .index();
        assert_eq!(
            indexed_json(project.graph()),
            ["templates/page.json", "templates/product.json"],
            "{}",
            project.graph().report()
        );

        project.remove("layout/theme.liquid");
        let graph = project.sync();
        assert_eq!(
            indexed_json(graph),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
        assert_eq!(
            dependents_of(graph, "sections/main-product.liquid"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );

        project.write("config/settings_schema.json", "[]");
        let graph = project.sync();
        assert_eq!(
            indexed_json(graph),
            ["templates/page.json", "templates/product.json"],
            "{}",
            graph.report()
        );
        assert_eq!(
            dependents_of(graph, "sections/main-product.liquid"),
            ["templates/page.json", "templates/product.json"],
            "{}",
            graph.report()
        );
    }

    /// A theme at the project root, a theme in a folder marked only by its
    /// settings schema, and one in a folder with no 404 section and no price
    /// snippet of its own: the shape of panoply/syncify's example themes
    /// (upstream's `severalThemes`).
    fn several_themes() -> Project {
        Project::new()
            .file(
                "layout/theme.liquid",
                "{% section 'header' %}{{ content_for_layout }}\n",
            )
            .file("sections/header.liquid", "{% render 'price' %}\n")
            .file("sections/main.liquid", "<main></main>\n")
            .file("snippets/price.liquid", "{{ product.price | money }}\n")
            .file("straps/dusk/config/settings_schema.json", "[]")
            .file(
                "straps/dusk/sections/404.liquid",
                "<h1>{{ 'templates.404.title' | t }}</h1>\n",
            )
            .file("straps/dusk/templates/404.json", &json_template("404"))
            .file(
                "examples/tailwind/layout/theme.liquid",
                "{% section 'header' %}{{ content_for_layout }}\n",
            )
            .file(
                "examples/tailwind/sections/header.liquid",
                "{% liquid\n  render 'price'\n  render 'icon'\n%}\n",
            )
            .file(
                "examples/tailwind/sections/main.liquid",
                "<main class=\"p-4\"></main>\n",
            )
            .file("examples/tailwind/snippets/icon.liquid", "<svg></svg>\n")
            .file(
                "examples/tailwind/templates/index.json",
                &json_template("main"),
            )
            .file(
                "examples/tailwind/templates/404.json",
                &json_template("404"),
            )
            // Liquid in no theme: an app's theme extension.
            .file(
                "extensions/reviews/blocks/stars.liquid",
                "{% render 'star' %}\n",
            )
            .file("extensions/reviews/snippets/star.liquid", "<svg></svg>\n")
    }

    /// Upstream #2396: a `{% render %}`, a `{% section %}` or a JSON
    /// template's section `type` is looked up in the theme the file belongs
    /// to, never in another one. What the theme lacks stays unlinked;
    /// outside any theme, a path resolves as before.
    #[test]
    fn pr2396_a_themes_sections_and_snippets_link_only_inside_that_theme() {
        let project = several_themes().index();
        let graph = project.graph();
        let expected: [(&str, &[&str]); 8] = [
            ("layout/theme.liquid", &["sections/header.liquid"]),
            ("sections/header.liquid", &["snippets/price.liquid"]),
            (
                "straps/dusk/templates/404.json",
                &["straps/dusk/sections/404.liquid"],
            ),
            (
                "examples/tailwind/layout/theme.liquid",
                &["examples/tailwind/sections/header.liquid"],
            ),
            (
                "examples/tailwind/templates/index.json",
                &["examples/tailwind/sections/main.liquid"],
            ),
            ("examples/tailwind/templates/404.json", &[]),
            (
                "examples/tailwind/sections/header.liquid",
                &["examples/tailwind/snippets/icon.liquid"],
            ),
            (
                "extensions/reviews/blocks/stars.liquid",
                &["extensions/reviews/snippets/star.liquid"],
            ),
        ];
        for (file, links) in expected {
            assert_eq!(links_of(graph, file), links, "{file}\n{}", graph.report());
        }
    }

    /// Upstream #2396: sync resolves again what named a deleted file, and
    /// another theme's 404 section is no stand-in for the theme's own.
    #[test]
    fn pr2396_a_themes_references_stay_inside_it_when_its_section_is_deleted() {
        let mut project = several_themes()
            .file("examples/tailwind/sections/404.liquid", "<h1>404</h1>\n")
            .index();
        assert_eq!(
            links_of(project.graph(), "examples/tailwind/templates/404.json"),
            ["examples/tailwind/sections/404.liquid"],
            "{}",
            project.graph().report()
        );

        project.remove("examples/tailwind/sections/404.liquid");
        let graph = project.sync();
        assert_eq!(
            links_of(graph, "examples/tailwind/templates/404.json"),
            Vec::<String>::new(),
            "{}",
            graph.report()
        );
    }
}

mod pascal_includes {
    use super::support::{Graph, Project};

    const DIRECTIVES: &str = "{ Compiler version switches }\n{$IFDEF VER150}\n  {$DEFINE DELPHI7}\n{$ENDIF}\n{$IFNDEF FPC}\n  {$WARN SYMBOL_PLATFORM OFF}\n{$ELSE}\n  {$MODE DELPHI}{$H+}\n{$ENDIF}\n";
    const ROUTINES: &str = "procedure Foo;\nbegin\n  WriteLn('foo');\nend;\n\nfunction Bar(A: Integer): Integer;\nbegin\n  Result := A + 1;\nend;\n";

    fn language_of<'g>(graph: &'g Graph, path: &str) -> Option<&'g str> {
        graph
            .files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.language.as_str())
    }

    /// `kind:name:language` of every node in `path` but its file node, sorted
    /// (upstream's `symbolsIn`).
    fn symbols_in(graph: &Graph, path: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
            .nodes
            .iter()
            .filter(|node| node.file_path == path && node.kind != "file")
            .map(|node| format!("{}:{}:{}", node.kind, node.name, node.language))
            .collect();
        out.sort();
        out
    }

    fn has_no_errors(graph: &Graph, path: &str) -> bool {
        graph
            .files
            .iter()
            .find(|file| file.path == path)
            .is_some_and(|file| file.errors.is_null() || file.errors == serde_json::json!([]))
    }

    /// Upstream #2289 (`pascal-inc-detection.test.ts`): a tree of Pascal
    /// includes with no `.pas` file indexes them as Pascal.
    #[test]
    fn pr2289_a_tree_of_pascal_includes_indexes_as_pascal() {
        let project = Project::new()
            .file("inc/defs.inc", DIRECTIVES)
            .file("inc/routines.inc", ROUTINES)
            .index();
        let graph = project.graph();
        assert_eq!(
            language_of(graph, "inc/defs.inc"),
            Some("pascal"),
            "{}",
            graph.report()
        );
        assert_eq!(
            language_of(graph, "inc/routines.inc"),
            Some("pascal"),
            "{}",
            graph.report()
        );
        assert_eq!(
            symbols_in(graph, "inc/routines.inc"),
            ["function:Bar:pascal", "function:Foo:pascal"],
            "{}",
            graph.report()
        );
        assert!(
            has_no_errors(graph, "inc/routines.inc"),
            "{}",
            graph.report()
        );
    }

    /// Upstream #2289: each include is classified by its content, and a sync
    /// of edited includes keeps the language.
    #[test]
    fn pr2289_each_include_is_classified_by_content_and_sync_keeps_it() {
        let mut project = Project::new()
            .file(
                "U.pas",
                "unit U;\n\ninterface\n\n{$I defs.inc}\n\nprocedure Foo;\n\nimplementation\n\n{$I routines.inc}\n\nend.\n",
            )
            .file("defs.inc", DIRECTIVES)
            .file("routines.inc", ROUTINES)
            .file(
                "drupal/mymodule.inc",
                "<?php\n\nfunction mymodule_helper() {\n  return 1;\n}\n",
            )
            .file("native/table.inc", "#define X 1\n#define Y 2\n")
            .index();
        let graph = project.graph();
        let report = graph.report();
        assert_eq!(language_of(graph, "defs.inc"), Some("pascal"), "{report}");
        assert_eq!(
            language_of(graph, "routines.inc"),
            Some("pascal"),
            "{report}"
        );
        assert_eq!(
            language_of(graph, "drupal/mymodule.inc"),
            Some("php"),
            "{report}"
        );
        assert_eq!(
            language_of(graph, "native/table.inc"),
            Some("php"),
            "{report}"
        );
        // A directive-only fragment: a file record, no symbols, no errors.
        assert_eq!(
            symbols_in(graph, "defs.inc"),
            Vec::<String>::new(),
            "{report}"
        );
        assert!(has_no_errors(graph, "defs.inc"), "{report}");
        assert_eq!(
            symbols_in(graph, "routines.inc"),
            ["function:Bar:pascal", "function:Foo:pascal"],
            "{report}"
        );
        assert_eq!(
            symbols_in(graph, "drupal/mymodule.inc"),
            ["function:mymodule_helper:php"],
            "{report}"
        );

        project.write(
            "defs.inc",
            &format!("{DIRECTIVES}{{$DEFINE USE_FAST_MM}}\n"),
        );
        project.write(
            "routines.inc",
            &format!("{ROUTINES}procedure Baz;\nbegin\nend;\n"),
        );
        let graph = project.sync();
        let report = graph.report();
        assert_eq!(language_of(graph, "defs.inc"), Some("pascal"), "{report}");
        assert_eq!(
            language_of(graph, "routines.inc"),
            Some("pascal"),
            "{report}"
        );
        assert_eq!(
            symbols_in(graph, "routines.inc"),
            [
                "function:Bar:pascal",
                "function:Baz:pascal",
                "function:Foo:pascal"
            ],
            "{report}"
        );
        assert_eq!(
            language_of(graph, "drupal/mymodule.inc"),
            Some("php"),
            "{report}"
        );
    }

    /// Upstream #2289: a `codegraph.json` mapping for `.inc` is the project's
    /// answer, kept through index and sync.
    #[test]
    fn pr2289_a_codegraph_json_inc_mapping_is_not_second_guessed() {
        let mut project = Project::new()
            .file(
                ".codegraph/codegraph.json",
                "{ \"extensions\": { \".inc\": \"php\" } }\n",
            )
            .file("defs.inc", DIRECTIVES)
            .index();
        assert_eq!(
            language_of(project.graph(), "defs.inc"),
            Some("php"),
            "{}",
            project.graph().report()
        );
        project.write("defs.inc", &format!("{DIRECTIVES}{{$DEFINE MORE}}\n"));
        let graph = project.sync();
        assert_eq!(
            language_of(graph, "defs.inc"),
            Some("php"),
            "{}",
            graph.report()
        );
    }

    /// KEEP-RUST: an override never claims a built-in extension, so a
    /// mapping of `.inc` to another language turns the content reading off
    /// and leaves `.inc` PHP (upstream would take the mapping).
    #[test]
    fn keep_rust_an_inc_mapping_to_another_language_leaves_it_php() {
        let project = Project::new()
            .file(
                ".codegraph/codegraph.json",
                "{ \"extensions\": { \".inc\": \"pascal\" } }\n",
            )
            .file("defs.inc", DIRECTIVES)
            .file("page.inc", "<?php echo 1;\n")
            .index();
        let graph = project.graph();
        assert_eq!(
            language_of(graph, "defs.inc"),
            Some("php"),
            "{}",
            graph.report()
        );
        assert_eq!(
            language_of(graph, "page.inc"),
            Some("php"),
            "{}",
            graph.report()
        );
    }
}
