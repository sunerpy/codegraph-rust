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
            .index();

        // A C# file's usings are its namespace's references.
        assert_unresolved!(project, "namespace App @src/Svc.cs" => imports => "System");
        assert_unresolved!(project, "namespace App @src/Svc.cs" => imports => "System.Text");
        assert_unresolved!(project, "namespace App @src/Other.cs" => imports => "System.Text");
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
}
