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
        // `B` is in `App.Second`; the extraction still files it under the
        // file's first namespace until W4-CS-01 (#2220), the scope reader
        // does not.
        assert_no_edge!(project, "method App.First::B::Unseen" => calls => "method App.Hidden::HiddenExtensions::IsHidden");
        assert_unresolved!(project, "method App.First::B::Unseen" => calls => "type.IsHidden");
        // An enclosing namespace's extension is in scope in a nested one; a
        // nested one's is not in scope outside it (both files under `Outer`
        // by the extraction, see above).
        assert_edge!(project, "method Outer::Deep::Down" => calls => "method Outer::OuterExtensions::IsOuter");
        assert_no_edge!(project, "method Outer::Top::Up" => calls => "method Outer::InnerExtensions::IsInner");
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
    use super::support::{Graph, Project};

    /// `file: qualified` of every `calls` edge out of `file`'s nodes, sorted.
    fn calls_from(graph: &Graph, file: &str) -> Vec<String> {
        let mut out: Vec<String> = graph
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
        out.sort();
        out
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
}
