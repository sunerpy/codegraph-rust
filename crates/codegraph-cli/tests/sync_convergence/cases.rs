//! The convergence cases, one function per case; `main.rs` registers each in
//! its `matrix!` list. Fixtures stay minimal: two or three files that hold one
//! cross-file edge the edit has to keep, move or drop.
//!
//! Selectors use the probe notation (`tests/probes/support.rs`). An
//! `edge_before`/`edge_after` expectation proves that the `before` index or a
//! fresh index of the final tree holds the relationship the case is about.

use std::sync::LazyLock;

use codegraph_core::types::Language;

use crate::{Case, Change};

// ----- a file appears that satisfies an existing import --------------------

pub fn typescript_named_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import { helper } from './util';\n\nexport function main(): number {\n  return helper();\n}\n",
        )
        .write(
            "src/util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        )
        .edge_after("function main", "calls", "function helper @src/util.ts")
        .edge_after("file:src/main.ts", "imports", "function helper")
}

pub fn typescript_default_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import Widget from './widget';\n\nexport function build(): Widget {\n  return new Widget();\n}\n",
        )
        .write("src/widget.ts", "export default class Widget {}\n")
        .edge_after("file:src/main.ts", "imports", "file:src/widget.ts")
        .edge_after("function build", "instantiates", "class Widget")
}

/// Upstream #2450: a reference through an aliased import binding links its
/// module only on a fresh index.
pub fn typescript_aliased_named_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import { Widget as Wrapper } from './widget';\n\nexport function build(): Wrapper {\n  return new Wrapper();\n}\n",
        )
        .write("src/widget.ts", "export class Widget {}\n")
        .edge_after("file:src/main.ts", "imports", "class Widget")
        .edge_after("function build", "instantiates", "class Widget")
}

/// Upstream #2450: an edit that gives the module the name an aliased binding
/// imports links the binding and its uses, which are written under the
/// alias.
pub fn typescript_aliased_named_import_gains_its_export() -> Case {
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import { Widget as Wrapper } from './widget';\n\nexport function build(): Wrapper {\n  return new Wrapper();\n}\n",
        )
        .file("src/widget.ts", "export class Placeholder {}\n")
        .write("src/widget.ts", "export class Widget {}\n")
        .edge_after("file:src/main.ts", "imports", "class Widget")
        .edge_after("function build", "instantiates", "class Widget")
}

/// Upstream #2450 (luci-go's milo): the same through a tsconfig alias, the
/// binding rendered as a JSX tag.
pub fn tsx_aliased_component_import_gains_its_export() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Tsx])
        .file(
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@/*\":[\"src/*\"]}}}",
        )
        .file(
            "src/pages/groups_page.test.tsx",
            "import { Component as GroupsPageWrapper } from '@/pages/groups_page';\nexport const page = <GroupsPageWrapper />;\n",
        )
        .file(
            "src/pages/groups_page.tsx",
            "export function Placeholder() { return null; }\n",
        )
        .write(
            "src/pages/groups_page.tsx",
            "export function Component() { return null; }\n",
        )
        .edge_after(
            "file:src/pages/groups_page.test.tsx",
            "imports",
            "function Component",
        )
        .edge_after("constant page", "references", "function Component")
}

/// Upstream #2450, found by W5-03's plan gate: while the module declares no
/// `Widget`, the binding `W` is linked by its name to a `W` elsewhere; once
/// the module gains `Widget`, the binding and its use leave that namesake.
pub fn typescript_aliased_named_import_leaves_a_namesake_when_its_module_gains_the_export() -> Case
{
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file(
            "src/draw.ts",
            "import { Widget as W } from './widgets/Widget';\n\nexport function paint() {\n  return new W();\n}\n",
        )
        .file("src/widgets/Widget.ts", "export class Placeholder {}\n")
        .file("src/other.ts", "export class W {}\n")
        .edge_before("function paint", "instantiates", "class W")
        .write("src/widgets/Widget.ts", "export class Widget {}\n")
        .edge_after("function paint", "instantiates", "class Widget")
        .no_edge_after("function paint", "instantiates", "class W")
}

/// Upstream #2450, found by W5-03's plan gate: the binding `W` is matched by
/// its name to the module's own `W` and its use to a `class W` elsewhere;
/// once the module gains `Widget`, both reach it.
pub fn typescript_aliased_named_import_leaves_an_in_module_namesake_when_its_module_gains_the_export()
-> Case {
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file(
            "src/draw.ts",
            "import { Widget as W } from './mod';

export function paint() {
  return new W();
}
",
        )
        .file(
            "src/mod.ts",
            "export const W = 1;
export class Placeholder {}
",
        )
        .file(
            "src/other.ts",
            "export class W {}
",
        )
        .edge_before("function paint", "instantiates", "class W")
        .write(
            "src/mod.ts",
            "export const W = 1;
export class Widget {}
",
        )
        .edge_after("file:src/draw.ts", "imports", "class Widget")
        .edge_after("function paint", "instantiates", "class Widget")
        .no_edge_after("function paint", "instantiates", "class W")
}

/// Upstream #2450, the other way: the module renames the declaration the
/// aliased binding imports, and the binding's uses leave it.
pub fn typescript_aliased_named_import_loses_its_export() -> Case {
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import { Widget as Wrapper } from './widget';\n\nexport function build(): Wrapper {\n  return new Wrapper();\n}\n",
        )
        .file("src/widget.ts", "export class Widget {}\n")
        .edge_before("function build", "instantiates", "class Widget")
        .write("src/widget.ts", "export class Gadget {}\n")
        .no_edge_after("function build", "instantiates", "class Gadget")
}

pub fn tsx_component_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Tsx])
        .file(
            "src/App.tsx",
            "import { Button } from './Button';\n\nexport function App() {\n  return <Button label=\"go\" />;\n}\n",
        )
        .write(
            "src/Button.tsx",
            "export function Button(props: { label: string }) {\n  return <button>{props.label}</button>;\n}\n",
        )
        .edge_after("function App", "references", "function Button")
}

/// Upstream #2392 (3): a failed `from pkg.mod import` is not retried when
/// `pkg/mod.py` appears.
pub fn python_from_import_of_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file(
            "main.py",
            "from pkg.mod import helper\n\n\ndef main():\n    return helper()\n",
        )
        .write("pkg/mod.py", "def helper():\n    return 1\n")
        .edge_after("file:main.py", "imports", "file:pkg/mod.py")
        .edge_after("function main", "calls", "function helper @pkg/mod.py")
}

/// Upstream #2392 (3): a failed PHP include is retried when the file it names
/// appears.
pub fn php_include_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Php])
        .file(
            "index.php",
            "<?php\nrequire 'inc/db.php';\n\nfunction boot() {\n    return connect();\n}\n",
        )
        .write(
            "inc/db.php",
            "<?php\nfunction connect() {\n    return 1;\n}\n",
        )
        .edge_after("file:index.php", "imports", "file:inc/db.php")
}

/// Upstream #2392 (3): a Dart import links the library it names once it appears.
pub fn dart_import_of_an_added_library() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/a.dart", "import 'b.dart';\n\nvoid main() {}\n")
        .write("lib/b.dart", "class B {}\n")
        .edge_after("file:lib/a.dart", "imports", "file:lib/b.dart")
}

/// Upstream #2392 (3): a C# `using` links the namespace a later file declares.
pub fn csharp_using_of_an_added_namespace() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file(
            "src/Svc.cs",
            "using App.Models;\nnamespace App { class Svc {} }\n",
        )
        .write(
            "src/Models.cs",
            "namespace App.Models { public class User {} }\n",
        )
        // A `using` outside a block namespace is its file's (upstream #2220).
        .edge_after("file:src/Svc.cs", "imports", "namespace App.Models")
}

/// Upstream #2392 (3): a Java import links the class a later file declares.
pub fn java_import_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Java])
        .file(
            "src/app/Main.java",
            "package app;\nimport app.util.Util;\nclass Main { void run() { Util.go(); } }\n",
        )
        .write(
            "src/app/util/Util.java",
            "package app.util;\npublic class Util { public static void go() {} }\n",
        )
        .edge_after(
            "namespace app @src/app/Main.java",
            "imports",
            "class app.util::Util",
        )
}

/// Upstream #2392 (2): a Lua `require` reaches a module file or nothing, so a
/// require of a module that appears later parks and links it (it used to bind
/// the local it is assigned to, which no sync retries).
pub fn lua_require_of_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Lua])
        .file(
            "main.lua",
            "local util = require('util')\n\nlocal function run()\n  return util.clamp(5, 0, 3)\nend\n\nreturn run\n",
        )
        .write(
            "util.lua",
            "local M = {}\n\nfunction M.clamp(value, low, high)\n  return math.max(low, math.min(high, value))\nend\n\nreturn M\n",
        )
        .edge_after("file:main.lua", "imports", "file:util.lua")
}

/// Upstream #2263 and #2392 (1): a side-effect import names a file, so one
/// that names no file yet parks and links it when it appears (it used to bind
/// its own import statement).
pub fn typescript_side_effect_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "import './polyfill';\n\nexport function start() {\n  return 1;\n}\n",
        )
        .write("src/polyfill.ts", "export const ready = true;\n")
        .edge_after("file:src/main.ts", "imports", "file:src/polyfill.ts")
}

/// Upstream #2120: a bare call to a name imported from a package outside the
/// repository never binds the project function of that name that appears
/// later, while a file that imports the project's own does.
pub fn typescript_out_of_repo_call_ignores_an_added_namesake() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"vitest\":\"*\"}}\n",
        )
        .file(
            "src/use.ts",
            "import { test } from './helpers';\n\nexport function run() {\n  return test('a');\n}\n",
        )
        .file(
            "src/a.test.ts",
            "import { test } from 'vitest';\n\ntest('works');\n",
        )
        .write(
            "src/helpers.ts",
            "export function test(name: string) {\n  return name;\n}\n",
        )
        .edge_after("function run", "calls", "function test @src/helpers.ts")
        .no_edge_after("file:src/a.test.ts", "calls", "function test")
}

/// Upstream #2136: a `super` call inside an override is the parent's, never a
/// self-call, before and after the override's body changes.
pub fn java_super_call_in_an_override_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base {\n    public void start() {\n    }\n}\n",
        )
        .file(
            "src/app/Service.java",
            "package app;\n\npublic class Service extends Base {\n    @Override\n    public void start() {\n        super.start();\n    }\n\n    void log() {\n    }\n}\n",
        )
        .write(
            "src/app/Service.java",
            "package app;\n\npublic class Service extends Base {\n    @Override\n    public void start() {\n        super.start();\n        log();\n    }\n\n    void log() {\n    }\n}\n",
        )
        .edge_after("method app::Service::start", "calls", "method app::Service::log")
        .no_edge_after(
            "method app::Service::start",
            "calls",
            "method app::Service::start",
        )
}

/// Upstream #2139: a Java `@Test` never decorates a `test()` method, whose
/// name differs only in case, before or after the annotated method's body
/// changes; an annotation the project declares keeps its edge.
pub fn java_annotation_ignores_a_case_variant_method_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/CharPredicate.java",
            "package app;\n\npublic interface CharPredicate {\n    boolean test(char c);\n}\n",
        )
        .file(
            "src/app/Marker.java",
            "package app;\n\npublic @interface Marker {\n}\n",
        )
        .file(
            "src/app/ParserTest.java",
            "package app;\n\nimport org.junit.jupiter.api.Test;\n\npublic class ParserTest {\n    @Test\n    @Marker\n    void parses() {\n    }\n}\n",
        )
        .write(
            "src/app/ParserTest.java",
            "package app;\n\nimport org.junit.jupiter.api.Test;\n\npublic class ParserTest {\n    @Test\n    @Marker\n    void parses() {\n        int checked = 1;\n    }\n}\n",
        )
        .edge_throughout("method app::ParserTest::parses", "decorates", "interface app::Marker")
        .no_edge_after(
            "method app::ParserTest::parses",
            "decorates",
            "method app::CharPredicate::test",
        )
}

/// Upstream #2166: `Object.assign(…)` stays the platform's when a project
/// class with an `assign` method appears; a call written on that class is the
/// project's.
pub fn typescript_outside_type_call_ignores_an_added_method() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/merge.ts",
            "export function merge(a: unknown) {\n  return Object.assign(a);\n}\n",
        )
        .file(
            "src/use.ts",
            "import { ObjectUtils } from './ObjectUtils';\n\nexport function use(a: unknown) {\n  return ObjectUtils.assign(a);\n}\n",
        )
        .write(
            "src/ObjectUtils.ts",
            "export class ObjectUtils {\n  static assign(a: unknown) {\n    return a;\n  }\n}\n",
        )
        .edge_after("function use", "calls", "method ObjectUtils::assign")
        .no_edge_after("function merge", "calls", "method ObjectUtils::assign")
}

/// Upstream #2169: a C++ receiver declared as one of the project's class
/// templates (`SkipList<int, int> list_`) keeps its method, before and after
/// the body changes.
pub fn cpp_class_template_receiver_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "db/skiplist.h",
            "namespace leveldb {\ntemplate <typename Key, class Comparator>\nclass SkipList {\n public:\n  void Insert(const Key& key) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/writer.cc",
            "#include \"db/skiplist.h\"\nnamespace leveldb {\nclass Writer {\n  SkipList<int, int> list_;\n\n public:\n  void WriteStep() { list_.Insert(1); }\n};\n}  // namespace leveldb\n",
        )
        .write(
            "db/writer.cc",
            "#include \"db/skiplist.h\"\nnamespace leveldb {\nclass Writer {\n  SkipList<int, int> list_;\n\n public:\n  void WriteStep() { list_.Insert(2); }\n};\n}  // namespace leveldb\n",
        )
        .edge_throughout(
            "method leveldb::Writer::WriteStep",
            "calls",
            "method leveldb::SkipList::Insert",
        )
}

/// W3-05c (KEEP-RUST): a C# call on an outside type keeps reaching the
/// project's extension method for it, before and after the body changes.
pub fn csharp_extension_method_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::CSharp])
        .file(
            "src/Utilities/TypeExtensions.cs",
            "using System;\n\nnamespace App.Utilities\n{\n    internal static class TypeExtensions\n    {\n        public static bool IsEnum(this Type type)\n        {\n            return false;\n        }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using System;\nusing App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public bool Check(Type objectType)\n        {\n            return objectType.IsEnum();\n        }\n    }\n}\n",
        )
        .write(
            "src/Reader.cs",
            "using System;\nusing App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public bool Check(Type objectType)\n        {\n            bool result = objectType.IsEnum();\n            return result;\n        }\n    }\n}\n",
        )
        .edge_throughout(
            "method App::Reader::Check",
            "calls",
            "method App.Utilities::TypeExtensions::IsEnum",
        )
}

/// W3-05c (KEEP-RUST): a static method that gains its `this` parameter, on
/// the same line, becomes the extension method an unchanged file's call on an
/// outside type reaches, as a fresh index of the edit finds.
pub fn csharp_method_becomes_an_extension_method() -> Case {
    Case::new(Change::BodyEdit, &[Language::CSharp])
        .file(
            "src/Utilities/TypeExtensions.cs",
            "using System;\n\nnamespace App.Utilities\n{\n    internal static class TypeExtensions\n    {\n        public static bool IsEnum(Type type)\n        {\n            return false;\n        }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using System;\nusing App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public bool Check(Type objectType)\n        {\n            return objectType.IsEnum();\n        }\n    }\n}\n",
        )
        .write(
            "src/Utilities/TypeExtensions.cs",
            "using System;\n\nnamespace App.Utilities\n{\n    internal static class TypeExtensions\n    {\n        public static bool IsEnum(this Type type)\n        {\n            return false;\n        }\n    }\n}\n",
        )
        .edge_after(
            "method App::Reader::Check",
            "calls",
            "method App.Utilities::TypeExtensions::IsEnum",
        )
}

/// W3-05c (KEEP-RUST): of a C# extension method's overloads, only those
/// extending the receiver's type compete, type parameters renamed or not: an
/// added `Open<U>(this Box<U>, U)` takes `box.Open(1)` from the `params`
/// overload, while the plain `Open(object)` and `Open(this Exception, …)`
/// never do, as a fresh index of the edit finds.
pub fn csharp_extension_overload_of_a_renamed_type_parameter_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Utilities/Boxes.cs",
            "using System;\n\nnamespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Open<T>(this Box<T> box, params T[] values)\n        {\n            return box;\n        }\n\n        public static object Open(object value)\n        {\n            return value;\n        }\n\n        public static object Open(this Exception error, object value)\n        {\n            return error;\n        }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public object Unbox(Box<int> box)\n        {\n            return box.Open(1);\n        }\n    }\n}\n",
        )
        .write(
            "src/Utilities/Boxes.cs",
            "using System;\n\nnamespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Open<T>(this Box<T> box, params T[] values)\n        {\n            return box;\n        }\n\n        public static object Open<U>(this Box<U> box, U value)\n        {\n            return box;\n        }\n\n        public static object Open(object value)\n        {\n            return value;\n        }\n\n        public static object Open(this Exception error, object value)\n        {\n            return error;\n        }\n    }\n}\n",
        )
        .edge_before(
            "method App::Reader::Unbox",
            "calls",
            "method App.Utilities::BoxExtensions::Open @src/Utilities/Boxes.cs:12",
        )
        .edge_after(
            "method App::Reader::Unbox",
            "calls",
            "method App.Utilities::BoxExtensions::Open @src/Utilities/Boxes.cs:17",
        )
        .no_edge_after(
            "method App::Reader::Unbox",
            "calls",
            "method App.Utilities::BoxExtensions::Open @src/Utilities/Boxes.cs:12",
        )
}

/// W3-05c (KEEP-RUST): an added C# extension overload whose `where` clause
/// takes no receiver the current one takes (`Seal<U>(this Box<U>, U) where
/// U : class` beside `Seal<T>(this Box<T>, params T[]) where T : struct`)
/// never takes `box.Seal(1)` on a `Box<int>`, as a fresh index of the edit
/// finds.
pub fn csharp_extension_overload_under_another_constraint_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Seal<T>(this Box<T> box, params T[] values) where T : struct\n        {\n            return box;\n        }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public object Close(Box<int> box)\n        {\n            return box.Seal(1);\n        }\n    }\n}\n",
        )
        .write(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Seal<T>(this Box<T> box, params T[] values) where T : struct\n        {\n            return box;\n        }\n\n        public static object Seal<U>(this Box<U> box, U value) where U : class\n        {\n            return box;\n        }\n    }\n}\n",
        )
        .edge_throughout(
            "method App::Reader::Close",
            "calls",
            "method App.Utilities::BoxExtensions::Seal @src/Utilities/Boxes.cs:10",
        )
        .no_edge_after(
            "method App::Reader::Close",
            "calls",
            "method App.Utilities::BoxExtensions::Seal @src/Utilities/Boxes.cs:15",
        )
}

/// W3-05c (KEEP-RUST): spacing and comments are no part of a C# extension
/// overload's receiver or constraints: an added `Wrap<T>(this Box< T > /* … */
/// box, T) where T : struct // …` takes `box.Wrap(1)` from
/// `Wrap<T>(this Box<T>, params T[]) where T: struct`, as a fresh index of
/// the edit finds.
pub fn csharp_extension_overload_spelled_with_other_spacing_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Wrap<T>(this Box<T> box, params T[] values) where T: struct\n        {\n            return box;\n        }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public object Fold(Box<int> box)\n        {\n            return box.Wrap(1);\n        }\n    }\n}\n",
        )
        .write(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Wrap<T>(this Box<T> box, params T[] values) where T: struct\n        {\n            return box;\n        }\n\n        public static object Wrap<T>(this Box< T > /* boxed */ box, T value) where T : struct // value types\n        {\n            return box;\n        }\n    }\n}\n",
        )
        .edge_before(
            "method App::Reader::Fold",
            "calls",
            "method App.Utilities::BoxExtensions::Wrap @src/Utilities/Boxes.cs:10",
        )
        .edge_after(
            "method App::Reader::Fold",
            "calls",
            "method App.Utilities::BoxExtensions::Wrap @src/Utilities/Boxes.cs:15",
        )
        .no_edge_after(
            "method App::Reader::Fold",
            "calls",
            "method App.Utilities::BoxExtensions::Wrap @src/Utilities/Boxes.cs:10",
        )
}

/// W3-05c (KEEP-RUST): a comment marker inside a raw string default value is
/// text: an added `Note<U>(this Box<U>, U, string note = """x" // …""") where
/// U : struct` keeps its `where` clause and takes `box.Note(1)` from the
/// `params` overload, as a fresh index of the edit finds.
pub fn csharp_extension_overload_with_a_raw_string_default_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Note<T>(this Box<T> box, params T[] values) where T : struct { return box; }\n    }\n}\n",
        )
        .file(
            "src/Reader.cs",
            "using App.Utilities;\n\nnamespace App\n{\n    public class Reader\n    {\n        public object Jot(Box<int> box)\n        {\n            return box.Note(1);\n        }\n    }\n}\n",
        )
        .write(
            "src/Utilities/Boxes.cs",
            "namespace App.Utilities\n{\n    public class Box<T>\n    {\n        public T Value;\n    }\n\n    public static class BoxExtensions\n    {\n        public static object Note<T>(this Box<T> box, params T[] values) where T : struct { return box; }\n\n        public static object Note<U>(this Box<U> box, U value, string note = \"\"\"x\" // literal text\"\"\") where U : struct { return box; }\n    }\n}\n",
        )
        .edge_before(
            "method App::Reader::Jot",
            "calls",
            "method App.Utilities::BoxExtensions::Note @src/Utilities/Boxes.cs:10",
        )
        .edge_after(
            "method App::Reader::Jot",
            "calls",
            "method App.Utilities::BoxExtensions::Note @src/Utilities/Boxes.cs:12",
        )
        .no_edge_after(
            "method App::Reader::Jot",
            "calls",
            "method App.Utilities::BoxExtensions::Note @src/Utilities/Boxes.cs:10",
        )
}

/// Upstream #2180: a call through a class name reaches the class method the
/// class inherits, before and after the calling body changes.
pub fn python_inherited_classmethod_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file(
            "pkg/base.py",
            "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.base import Base\n\n\nclass User(Base):\n    pass\n",
        )
        .file(
            "pkg/service.py",
            "from pkg.user import User\n\n\ndef make():\n    return None\n",
        )
        .write(
            "pkg/service.py",
            "from pkg.user import User\n\n\ndef make():\n    return User.create()\n",
        )
        .edge_after("function make", "calls", "method Base::create")
}

/// W5-05a (upstream #1240's tail retry): a qualified call `User.create()`
/// parked before its class exists links once a new file declares the class,
/// as a fresh index links it; the whole-name retry never matches it.
pub fn python_class_method_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file("pkg/service.py", "def make():\n    return User.create()\n")
        .write(
            "pkg/user.py",
            "class User:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .edge_after("function make", "calls", "method User::create")
}

/// W5-05a: a call through a subclass (`User.create()`) reaches the
/// classmethod of a base that a new file declares later, as a fresh index
/// reaches it; the caller's file is unchanged and its import already resolved.
pub fn python_inherited_classmethod_of_an_added_base() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file(
            "pkg/user.py",
            "from pkg.base import Base\n\n\nclass User(Base):\n    pass\n",
        )
        .file(
            "pkg/service.py",
            "from pkg.user import User\n\n\ndef make():\n    return User.create()\n",
        )
        .write(
            "pkg/base.py",
            "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .edge_after("function make", "calls", "method Base::create")
}

/// Upstream #2209: a test suite's `Record` that appears later is never what
/// production code's `Record<…>` names; shipped test support stays in reach.
pub fn typescript_test_suite_namesake_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file(
            "src/testing/stub.ts",
            "export function stubConnection(): number {\n  return 2;\n}\n",
        )
        .file(
            "src/columns.ts",
            "export function toJson(values: Record<string, number>): string {\n  return JSON.stringify(values);\n}\n\nexport function connect(): number {\n  return stubConnection();\n}\n",
        )
        .write(
            "test/functional/entity/Record.ts",
            "export class Record {\n  id = 0;\n}\n",
        )
        .edge_throughout("function connect", "calls", "function stubConnection")
        .no_edge_after("function toJson", "references", "class Record")
}

/// Upstream #2212: a method delegating to another overload of itself
/// reaches that overload, before and after its body changes.
pub fn java_overload_delegation_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Instants.java",
            "package app;\n\npublic class Instants {\n    public static Object toInstant(Object instant) {\n        return toInstant(instant, null);\n    }\n\n    public static Object toInstant(Object instant, Object fallback) {\n        return instant;\n    }\n}\n",
        )
        .write(
            "src/app/Instants.java",
            "package app;\n\npublic class Instants {\n    public static Object toInstant(Object instant) {\n        return toInstant(instant, instant);\n    }\n\n    public static Object toInstant(Object instant, Object fallback) {\n        return fallback;\n    }\n}\n",
        )
        .edge_after(
            "method app::Instants::toInstant @src/app/Instants.java:4",
            "calls",
            "method app::Instants::toInstant @src/app/Instants.java:8",
        )
        .no_edge_after(
            "method app::Instants::toInstant @src/app/Instants.java:4",
            "calls",
            "method app::Instants::toInstant @src/app/Instants.java:4",
        )
}

/// Upstream #2415: in a React app, a Python call never reaches a React hook
/// by the React resolver's rules, before or after its body changes; the
/// app's own call still does.
pub fn python_call_ignores_a_react_hook_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"react\":\"^19.0.0\"}}\n",
        )
        .file(
            "src/hooks/useSession.tsx",
            "export function useSession() {\n  return null;\n}\n",
        )
        .file(
            "src/App.tsx",
            "import { useSession } from './hooks/useSession';\n\nexport function App() {\n  const session = useSession();\n  return <div>{String(session)}</div>;\n}\n",
        )
        .file(
            "backend/session.py",
            "def handle():\n    return useSession()\n",
        )
        .write(
            "backend/session.py",
            "def handle():\n    value = useSession()\n    return value\n",
        )
        .edge_throughout("function App", "calls", "function useSession")
        .no_edge_after("function handle", "calls", "function useSession")
}

/// Upstream #2123: a type parameter the function declares in an edit is
/// that parameter, never the project class it shares a name with.
pub fn typescript_type_parameter_added_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/model.ts", "export class T {\n  id = 0;\n}\n")
        .file(
            "src/wrap.ts",
            "import { T } from './model';\n\nexport function wrap(x: T): T {\n  return x;\n}\n",
        )
        .write(
            "src/wrap.ts",
            "import { T } from './model';\n\nexport function wrap<T>(x: T): T {\n  return x;\n}\n",
        )
        .edge_before("function wrap", "references", "class T @src/model.ts")
        .no_edge_after("function wrap", "references", "class T @src/model.ts")
}

/// `util.js` with `u` on its first line and, below it, a data array: on one
/// line of 5,000 characters of code (a minified script), or split over many.
fn util_script(minified: bool) -> String {
    let separator = if minified { "" } else { "\n" };
    let data = format!("1,{separator}").repeat(2500);
    format!("function u(e){{return e}}\nvar data=[{separator}{data}0];\n")
}

static MINIFIED_UTIL: LazyLock<String> = LazyLock::new(|| util_script(true));
static READABLE_UTIL: LazyLock<String> = LazyLock::new(|| util_script(false));

/// Upstream #2204: a script that stops being minified, every declaration
/// where it was, is reachable by name again, as a fresh index of it is.
pub fn javascript_minified_script_becomes_readable() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file(
            "static/js/app.js",
            "function init() {\n  return u(\"x\");\n}\n",
        )
        .file("static/js/util.js", MINIFIED_UTIL.as_str())
        .write("static/js/util.js", READABLE_UTIL.as_str())
        .edge_after("function init", "calls", "function u @static/js/util.js")
}

/// Upstream #2204: a script that becomes minified, every declaration where
/// it was, is no longer reachable by name from another file.
pub fn javascript_readable_script_becomes_minified() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file(
            "static/js/app.js",
            "function init() {\n  return u(\"x\");\n}\n",
        )
        .file("static/js/util.js", READABLE_UTIL.as_str())
        .write("static/js/util.js", MINIFIED_UTIL.as_str())
        .edge_before("function init", "calls", "function u @static/js/util.js")
        .no_edge_after("function init", "calls", "function u @static/js/util.js")
}

/// Upstream #2219: a Python name an edit makes the function's own parameter
/// is that local's value, not the package's function of that name (the
/// same-file `Recorder.request` makes `request` a value candidate).
pub fn python_function_value_bound_by_an_added_parameter() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("app/api.py", "def request(method, url):\n    return method, url\n")
        .file(
            "app/views.py",
            "class Recorder:\n    def request(self):\n        return None\n\n\ndef view():\n    return handle(request)\n",
        )
        .write(
            "app/views.py",
            "class Recorder:\n    def request(self):\n        return None\n\n\ndef view(request):\n    return handle(request)\n",
        )
        .edge_before("function view", "references", "function request @app/api.py")
        .no_edge_after("function view", "references", "function request @app/api.py")
}

/// Upstream #2392 (3): a Go call through an imported package links once the
/// package appears.
pub fn go_import_of_an_added_package() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file(
            "main.go",
            "package main\n\nimport \"example.com/shop/util\"\n\nfunc main() { util.Go() }\n",
        )
        .write("util/util.go", "package util\n\nfunc Go() {}\n")
        .edge_after("function main", "calls", "function Go @util/util.go")
}

/// Upstream #2392 (3): a Solidity import links the file it names once it appears.
pub fn solidity_import_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Solidity])
        .file(
            "contracts/Token.sol",
            "pragma solidity ^0.8.0;\nimport \"./Lib.sol\";\ncontract Token {}\n",
        )
        .write(
            "contracts/Lib.sol",
            "pragma solidity ^0.8.0;\nlibrary Lib {}\n",
        )
        .edge_after(
            "file:contracts/Token.sol",
            "imports",
            "file:contracts/Lib.sol",
        )
}

/// Upstream #2128: a Svelte call imported through SvelteKit's `$lib` alias,
/// which no package.json declares, stays the project's and reaches the
/// module once it appears, while one from the framework's `$app/…` never
/// reaches a project namesake, as a fresh index of the edit finds.
pub fn svelte_lib_alias_call_reaches_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Svelte, Language::TypeScript])
        .file(
            "docs/package.json",
            "{\"name\":\"docs\",\"devDependencies\":{\"@sveltejs/kit\":\"*\"}}\n",
        )
        .file(
            "docs/src/routes/Design.svelte",
            "<script lang=\"ts\">\n  import { useDesignSystem } from '$lib/design';\n  import { goto } from '$app/navigation';\n  const ds = useDesignSystem();\n  goto('/');\n</script>\n\n<p>{ds}</p>\n",
        )
        .file(
            "docs/src/lib/router.ts",
            "export function goto(path: string) {\n  return path;\n}\n",
        )
        .write(
            "docs/src/lib/design.ts",
            "export function useDesignSystem() {\n  return 1;\n}\n",
        )
        .edge_after(
            "component docs/src/routes/Design.svelte::Design",
            "calls",
            "function useDesignSystem",
        )
        .no_edge_after(
            "component docs/src/routes/Design.svelte::Design",
            "calls",
            "function goto",
        )
}

/// Upstream #2128: a bare call in a Vue component follows the JS/TS rules: a
/// `t` destructured from a package's composable never reaches an interface's
/// `t` property that appears later, as a fresh index of the edit finds.
pub fn vue_bare_call_ignores_an_added_interface_property() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Vue, Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"vue-i18n\":\"*\"}}\n",
        )
        .file(
            "src/Labels.vue",
            "<script setup lang=\"ts\">\nimport { useI18n } from 'vue-i18n';\nconst { t } = useI18n();\nconst title = t('labels.title');\n</script>\n<template><h1>{{ title }}</h1></template>\n",
        )
        .write(
            "src/types/context.ts",
            "export interface ProvidersContext {\n  t: (key: string) => string;\n}\n",
        )
        .no_edge_after("component src/Labels.vue::Labels", "calls", "ProvidersContext::t")
}

/// Upstream #2162: an added ES module's unexported top-level function is
/// reached by its own module's call, never by another file's bare call of
/// its name, as a fresh index of the edit finds.
pub fn javascript_unexported_namesake_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::JavaScript])
        .file("package.json", "{\"name\":\"app\",\"type\":\"module\"}\n")
        .file(
            "src/use.js",
            "export async function handle(event) {\n  await resolve(event);\n}\n",
        )
        .write(
            "src/manifest.js",
            "import { join } from 'node:path';\nfunction resolve(event) {\n  return join(event);\n}\nexport function manifest() {\n  return resolve('x');\n}\n",
        )
        .edge_after("function manifest", "calls", "function resolve")
        .no_edge_after("function handle", "calls", "function resolve")
}

/// Upstream #2227: what an added Svelte component's instance script declares
/// is its own: another file's bare call of the name never reaches it, as a
/// fresh index of the edit finds.
pub fn svelte_instance_script_namesake_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Svelte, Language::TypeScript])
        .file(
            "src/extensions/code-block.ts",
            "export function active(state: unknown) {\n  return formatLabel(state);\n}\n",
        )
        .write(
            "src/components/Widget.svelte",
            "<script lang=\"ts\">\n  function formatLabel(value: unknown) {\n    return String(value);\n  }\n  const label = formatLabel(1);\n</script>\n\n<p>{label}</p>\n",
        )
        .edge_after(
            "component src/components/Widget.svelte::Widget",
            "calls",
            "function formatLabel",
        )
        .no_edge_after("function active", "calls", "function formatLabel")
}

/// Upstream #2226: a JS function's parameter shadows a same-named function
/// added elsewhere in its file: `mixin(object, source)` calling `object(…)`
/// calls its parameter, as a fresh index of the edit finds.
pub fn javascript_parameter_shadows_an_added_function() -> Case {
    Case::new(Change::AddCompetitor, &[Language::JavaScript])
        .file(
            "lodash.js",
            "function runInContext(context) {\n  function mixin(object, source) {\n    var result = object(source);\n    return result;\n  }\n  return { mixin };\n}\n",
        )
        .write(
            "lodash.js",
            "function runInContext(context) {\n  var baseCreate = (function() {\n    function object() {}\n    return object;\n  }());\n\n  function mixin(object, source) {\n    var result = object(source);\n    return result;\n  }\n  return { baseCreate, mixin };\n}\n",
        )
        .no_edge_after("runInContext::mixin", "calls", "runInContext::object")
}

/// Upstream #2163: a call through a name destructured from a composable's
/// result reaches the function the composable returns once its module
/// appears, as a fresh index of the edit finds.
pub fn typescript_destructured_composable_call_reaches_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/pages/index.ts",
            "import { useAuth } from '../hooks/useAuth';\nexport function go() {\n  const { login } = useAuth();\n  return login('ada');\n}\n",
        )
        .write(
            "src/hooks/useAuth.ts",
            "export function useAuth() {\n  function login(user: string) {\n    return user;\n  }\n  return { login };\n}\n",
        )
        .edge_after("function go", "calls", "function useAuth::login")
}

/// Upstream #2357 (a): a module-scope destructuring declaration's initializer
/// call is the file's, and an edit to it moves the edge.
pub fn typescript_module_destructuring_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "src/composables.ts",
            "export function useFoo(n: number) {\n  return { a: n };\n}\n\nexport function useBar(n: number) {\n  return { a: n };\n}\n",
        )
        .file(
            "src/app.ts",
            "import { useFoo, useBar } from './composables';\nconst { a } = useFoo(1);\nexport const value = a;\n",
        )
        .write(
            "src/app.ts",
            "import { useFoo, useBar } from './composables';\nconst { a } = useBar(1);\nexport const value = a;\n",
        )
        .edge_before("file:src/app.ts", "calls", "function useFoo")
        .edge_after("file:src/app.ts", "calls", "function useBar")
}

/// Upstream #2357 (d): a Vue page's own function, once added, shadows the
/// Nuxt auto-import of its name: the call that was the auto-import's (an
/// edge from the caller to itself) reaches the page's function, as a fresh
/// index of the edit finds.
pub fn vue_own_function_shadows_a_nuxt_auto_import() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue, Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"nuxt\":\"^3\",\"vue\":\"^3\"}}\n",
        )
        .file("nuxt.config.ts", "export default defineNuxtConfig({});\n")
        .file(
            "utils/errors.ts",
            "export function clearError(reset: boolean) {\n  return reset;\n}\n",
        )
        .file(
            "pages/lists.vue",
            "<script setup lang=\"ts\">\nfunction retry() {\n  return clearError(true);\n}\n</script>\n<template><button @click=\"retry\">retry</button></template>\n",
        )
        .write(
            "pages/lists.vue",
            "<script setup lang=\"ts\">\nfunction retry() {\n  return clearError(true);\n}\nfunction clearError(reset: boolean) {\n  return reset;\n}\n</script>\n<template><button @click=\"retry\">retry</button></template>\n",
        )
        .edge_before("function retry", "calls", "function retry")
        .edge_after("function retry", "calls", "function clearError @pages/lists.vue")
        .no_edge_after("function retry", "calls", "function retry")
}

/// Same-file value references (upstream #895) are extraction edges: a
/// namesake appearing in another file sends the unchanged reader file through
/// the whole-file re-resolution, which must keep `run`'s value reference to
/// its own `LIMIT_MAX`, as a fresh index of the edit has it.
pub fn typescript_value_reference_survives_an_added_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file(
            "src/a.ts",
            "const LIMIT_MAX = 3;\nexport function run() {\n  return LIMIT_MAX + 1;\n}\n",
        )
        .write("src/b.ts", "export const LIMIT_MAX = 5;\n")
        .edge_throughout("function run", "references", "constant LIMIT_MAX @src/a.ts")
}

/// Upstream #2363: a classic script's call through a global namespace
/// reaches the member once the script defining `window.WS = {…}` appears, as
/// a fresh index of the edit finds.
pub fn javascript_global_namespace_member_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::JavaScript])
        .file("use.js", "function viaGlobal() { return WS.wsM(); }\n")
        .write(
            "ns.js",
            "window.WS = { a() { return 1; }, wsM() { return 2; } };\n",
        )
        .edge_after("function viaGlobal", "calls", "function window.WS::wsM")
}

/// Upstream #2363: renaming a global namespace's member drops the call that
/// reached it from another script, as a fresh index of the edit finds.
pub fn javascript_global_namespace_member_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::JavaScript])
        .file(
            "ns.js",
            "window.WS = { a() { return 1; }, wsM() { return 2; } };\n",
        )
        .file("use.js", "function viaGlobal() { return WS.wsM(); }\n")
        .write("ns.js", "window.WS = { renamed() { return 4; } };\n")
        .edge_before("function viaGlobal", "calls", "function window.WS::wsM")
        .no_edge_after("function viaGlobal", "calls", "function window.WS::renamed")
}

/// Upstream #2363: a sibling's `this.render()` inside the methods of an
/// object hung on `window` follows an edit of the call to another member, as
/// a fresh index of the edit finds.
pub fn javascript_object_literal_this_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file(
            "app.js",
            "window.App = {\n  init: function () { this.render(); },\n  render: function () { return 1; },\n  draw: function () { return 2; },\n};\n",
        )
        .write(
            "app.js",
            "window.App = {\n  init: function () { this.draw(); },\n  render: function () { return 1; },\n  draw: function () { return 2; },\n};\n",
        )
        .edge_before("function window.App::init", "calls", "function window.App::render")
        .edge_after("function window.App::init", "calls", "function window.App::draw")
        .no_edge_after("function window.App::init", "calls", "function window.App::render")
}

/// Upstream #2442: a JSX tag whose name the component comes to bind as a
/// parameter renders that local, so the edge to another file's component of
/// the name goes, as a fresh index of the edit finds.
pub fn tsx_tag_becomes_a_parameter_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("package.json", "{\"dependencies\":{\"react\":\"^18.0.0\"}}\n")
        .file(
            "src/Widget.tsx",
            "export function Widget() {\n  return <div />;\n}\n",
        )
        .file(
            "src/Widgets.tsx",
            "export function Widgets({ widgets }) {\n  return <div>{widgets.map((item, index) => <Widget key={index} />)}</div>;\n}\n",
        )
        .write(
            "src/Widgets.tsx",
            "export function Widgets({ widgets }) {\n  return <div>{widgets.map((Widget, index) => <Widget key={index} />)}</div>;\n}\n",
        )
        .edge_before("function Widgets", "references", "function Widget @src/Widget.tsx")
        .no_edge_after("function Widgets", "references", "function Widget @src/Widget.tsx")
}

/// Upstream #841: a `<Route>` in a `.tsx` file is a route, and an edit to
/// the element it renders moves its edge, as a fresh index of the edit finds.
pub fn tsx_route_element_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file(
            "package.json",
            "{\"dependencies\":{\"react\":\"^18.0.0\",\"react-router-dom\":\"^6.0.0\"}}\n",
        )
        .file("src/Home.tsx", "export function Home() {\n  return <h1 />;\n}\n")
        .file("src/About.tsx", "export function About() {\n  return <h2 />;\n}\n")
        .file(
            "src/App.tsx",
            "import { Routes, Route } from 'react-router-dom';\nimport { Home } from './Home';\nimport { About } from './About';\n\nexport function App() {\n  return (\n    <Routes>\n      <Route path=\"/page\" element={<Home />} />\n    </Routes>\n  );\n}\n",
        )
        .write(
            "src/App.tsx",
            "import { Routes, Route } from 'react-router-dom';\nimport { Home } from './Home';\nimport { About } from './About';\n\nexport function App() {\n  return (\n    <Routes>\n      <Route path=\"/page\" element={<About />} />\n    </Routes>\n  );\n}\n",
        )
        .edge_before("route src/App.tsx::route:/page", "references", "function Home")
        .edge_after("route src/App.tsx::route:/page", "references", "function About")
        .no_edge_after("route src/App.tsx::route:/page", "references", "function Home")
}

/// Upstream #2420: a component value a `.tsx` file declares renders the
/// function it wraps, wherever the file uses it; an edit to what it wraps
/// moves the tag's edge, never to another file's namesake.
pub fn tsx_wrapper_value_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("package.json", "{\"dependencies\":{\"react\":\"^18.0.0\"}}\n")
        .file("src/other/Table.tsx", "export function Table() {\n  return <p />;\n}\n")
        .file(
            "src/App.tsx",
            "import { observer } from 'mobx-react';\n\nfunction TableInner() {\n  return <table />;\n}\nfunction TableOuter() {\n  return <section />;\n}\nconst Table = observer(TableInner);\n\nexport function App() {\n  return <Table />;\n}\n",
        )
        .write(
            "src/App.tsx",
            "import { observer } from 'mobx-react';\n\nfunction TableInner() {\n  return <table />;\n}\nfunction TableOuter() {\n  return <section />;\n}\nconst Table = observer(TableOuter);\n\nexport function App() {\n  return <Table />;\n}\n",
        )
        .edge_before("function App", "references", "function TableInner")
        .edge_after("function App", "references", "function TableOuter")
        .no_edge_after("function App", "references", "function TableInner")
        .no_edge_after("function App", "references", "function Table @src/other/Table.tsx")
}

/// W7-04b (upstream #2159): the edge the React resolver makes names it in
/// `metadata.framework`, and the file's re-extraction draws it again with the
/// name, as a fresh index does.
pub fn tsx_framework_edge_keeps_its_resolvers_name() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("package.json", "{\"dependencies\":{\"react\":\"^18.0.0\"}}\n")
        .file(
            "src/App.tsx",
            "import { observer } from 'mobx-react';\n\nfunction TableInner() {\n  return <table />;\n}\nconst Table = observer(TableInner);\n\nexport function App() {\n  return <Table />;\n}\n",
        )
        .write(
            "src/App.tsx",
            "import { observer } from 'mobx-react';\n\nfunction TableInner() {\n  return <table className=\"wide\" />;\n}\nconst Table = observer(TableInner);\n\nexport function App() {\n  return <Table />;\n}\n",
        )
        .edge_after("function App", "references", "function TableInner")
        .edge_metadata_after(
            ("function App", "references", "function TableInner"),
            "framework",
            "react",
        )
}

/// Upstream #2418: an interface moved into a `declare module 'x'` body is
/// exported, so another file's type reference reaches it by name, as a fresh
/// index of the edit finds.
pub fn typescript_interface_moves_into_a_declare_module_body() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "src/chart.registry.ts",
            "export {};\ninterface PluginOptions {\n  color?: string;\n}\n",
        )
        .file(
            "src/use.ts",
            "export function pick(options: PluginOptions) {\n  return options;\n}\n",
        )
        .write(
            "src/chart.registry.ts",
            "export {};\ndeclare module 'chart.js' {\n  interface PluginOptions {\n    color?: string;\n  }\n}\n",
        )
        .edge_after("function pick", "references", "interface PluginOptions")
}

/// Upstream #2140: an Options API method's `this.b()` follows an edit of
/// the call to another method of the component, as a fresh index of the
/// edit finds.
pub fn vue_options_api_this_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue])
        .file(
            "src/Form.vue",
            "<template><form /></template>\n<script>\nexport default {\n  methods: {\n    submit() {\n      this.validate()\n    },\n    validate() {\n      return true\n    },\n    reset() {\n      return false\n    }\n  }\n}\n</script>\n",
        )
        .write(
            "src/Form.vue",
            "<template><form /></template>\n<script>\nexport default {\n  methods: {\n    submit() {\n      this.reset()\n    },\n    validate() {\n      return true\n    },\n    reset() {\n      return false\n    }\n  }\n}\n</script>\n",
        )
        .edge_before("method Form::submit", "calls", "method Form::validate")
        .edge_after("method Form::submit", "calls", "method Form::reset")
        .no_edge_after("method Form::submit", "calls", "method Form::validate")
}

/// Upstream #2357 (b): a call in a component's template reaches the
/// composable once its module appears, as a fresh index of the edit finds.
pub fn vue_template_call_reaches_an_added_composable() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::Vue, Language::TypeScript],
    )
    .file(
        "package.json",
        "{\"name\":\"nuxt-app\",\"dependencies\":{\"nuxt\":\"^4.0.0\"}}\n",
    )
    .file(
        "components/Link.vue",
        "<template><NuxtLink :to=\"useBar(link.location)\">x</NuxtLink></template>\n",
    )
    .write(
        "composables/useBar.ts",
        "export function useBar(x: unknown) {\n  return String(x);\n}\n",
    )
    .edge_after(
        "component components/Link.vue::Link",
        "calls",
        "function useBar",
    )
}

/// Upstream #2177: `file.text()` on an untyped receiver is fetch's body
/// method, so a project class's `text` added later is never its target, as a
/// fresh index of the edit finds.
pub fn javascript_fetch_body_call_ignores_an_added_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::JavaScript])
        .file(
            "src/load.js",
            "export async function load(file) {\n  return file.text();\n}\n",
        )
        .write(
            "src/LazyFile.js",
            "export class LazyFile {\n  text() {\n    return '';\n  }\n}\n",
        )
        .no_edge_after("function load", "calls", "method LazyFile::text")
}

/// Upstream #2125: a bare Python call is no method, so a class with a
/// method of that name added later is never its target, as a fresh index of
/// the edit finds.
pub fn python_bare_call_ignores_an_added_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Python])
        .file(
            "app/views.py",
            "def bare_login():\n    return login(\"x\")\n",
        )
        .write(
            "app/tests.py",
            "class AuthTestCase:\n    def login(self, name):\n        return name\n",
        )
        .no_edge_after("function bare_login", "calls", "method AuthTestCase::login")
}

/// Upstream #2125: a name taken `from` a module outside the project is the
/// package's, so a project function of that name added later is never the
/// call's target, as a fresh index of the edit finds.
pub fn python_outside_import_ignores_an_added_function() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Python])
        .file(
            "app/pages.py",
            "from django.shortcuts import render\n\ndef page(request):\n    return render(request, \"page.html\")\n",
        )
        .write("app/tags.py", "def render(context):\n    return context\n")
        .no_edge_after("function page", "calls", "function render")
}

/// Upstream #2198: a bare call to a name the function comes to bind itself
/// is that value, so the edge to another file's function of the name goes,
/// as a fresh index of the edit finds.
pub fn python_call_becomes_a_local_binding_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("tests/test_testing.py", "def view(request):\n    return request\n")
        .file(
            "tests/test_versioning.py",
            "def test_version(request):\n    response = view(request)\n    return response\n",
        )
        .write(
            "tests/test_versioning.py",
            "def test_version(request):\n    view = MyView.as_view()\n    response = view(request)\n    return response\n",
        )
        .edge_before("function test_version", "calls", "function view")
        .no_edge_after("function test_version", "calls", "function view")
}

/// Upstream #2247: a name a package star-imports from a module reaches that
/// module's class once the module appears, ahead of a test's namesake.
pub fn python_star_reexport_of_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file("users/__init__.py", "")
        .file("users/models/__init__.py", "from .users import *\n")
        .file("extras/tests/test_tags.py", "class User:\n    pass\n")
        .file(
            "core/views.py",
            "from users.models import User\n\n\ndef make():\n    return User()\n",
        )
        .write("users/models/users.py", "class User:\n    pass\n")
        .edge_after(
            "function make",
            "instantiates",
            "class User @users/models/users.py",
        )
}

/// Upstream #2247: a package that comes to star-import a module re-exports
/// its class, so an unchanged importer's call binds it, as a fresh index of
/// the edit finds.
pub fn python_package_gains_a_star_reexport() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Python])
        .file("users/__init__.py", "")
        .file("users/models/__init__.py", "")
        .file("users/models/users.py", "class User:\n    pass\n")
        .file("extras/tests/test_tags.py", "class User:\n    pass\n")
        .file(
            "core/views.py",
            "from users.models import User\n\n\ndef make():\n    return User()\n",
        )
        .write("users/models/__init__.py", "from .users import *\n")
        .edge_after(
            "function make",
            "instantiates",
            "class User @users/models/users.py",
        )
}

/// Upstream #2247: a package that stops star-importing a module no longer
/// re-exports its class, so an unchanged importer's import-bound call goes,
/// as a fresh index of the edit finds.
pub fn python_package_drops_a_star_reexport() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Python])
        .file("users/__init__.py", "")
        .file("users/models/__init__.py", "from .users import *\n")
        .file("users/models/users.py", "class User:\n    pass\n")
        .file("extras/tests/test_tags.py", "class User:\n    pass\n")
        .file(
            "core/views.py",
            "from users.models import User\n\n\ndef make():\n    return User()\n",
        )
        .write("users/models/__init__.py", "")
        .edge_before(
            "function make",
            "instantiates",
            "class User @users/models/users.py",
        )
}

/// Upstream #2247: deleting the package module that re-exported a class
/// takes the import binding with it, as a fresh index of the edit finds.
pub fn python_reexporting_package_is_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Python])
        .file("users/__init__.py", "")
        .file("users/models/__init__.py", "from .tokens import Token\n")
        .file("users/models/tokens.py", "class Token:\n    pass\n")
        .file(
            "alembic/versions/0001_tokens.py",
            "class Token:\n    pass\n",
        )
        .file(
            "core/views.py",
            "from users.models import Token\n\n\ndef make():\n    return Token()\n",
        )
        .remove("users/models/__init__.py")
        .edge_before(
            "function make",
            "instantiates",
            "class Token @users/models/tokens.py",
        )
}

/// Upstream #2247 and #2291 (c): a package's named re-export moves to
/// another module, written as a parenthesized list, and an unchanged
/// importer's call follows it.
pub fn python_named_reexport_moves_to_another_module() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Python])
        .file("users/__init__.py", "")
        .file("users/models/__init__.py", "from .legacy import Token\n")
        .file("users/models/legacy.py", "class Token:\n    pass\n")
        .file("users/models/tokens.py", "class Token:\n    pass\n")
        .file(
            "core/views.py",
            "from users.models import Token\n\n\ndef make():\n    return Token()\n",
        )
        .write(
            "users/models/__init__.py",
            "from .tokens import (\n    Token,  # moved()\n)\n",
        )
        .edge_before(
            "function make",
            "instantiates",
            "class Token @users/models/legacy.py",
        )
        .edge_after(
            "function make",
            "instantiates",
            "class Token @users/models/tokens.py",
        )
        .no_edge_after(
            "function make",
            "instantiates",
            "class Token @users/models/legacy.py",
        )
}

/// Upstream #2248: a module member used through the imported module follows
/// a star re-export its package comes to make.
pub fn python_module_member_reexport_appears() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Python])
        .file("netbox/__init__.py", "")
        .file("netbox/views/__init__.py", "")
        .file("netbox/views/generic/__init__.py", "")
        .file(
            "netbox/views/generic/object_views.py",
            "class ObjectEditView:\n    pass\n",
        )
        .file("extras/views.py", "class ObjectEditView:\n    pass\n")
        .file(
            "dcim/views.py",
            "from netbox.views import generic\n\n\nclass DeviceEditView(generic.ObjectEditView):\n    pass\n",
        )
        .write(
            "netbox/views/generic/__init__.py",
            "from .object_views import *\n",
        )
        .edge_after(
            "class DeviceEditView",
            "extends",
            "class ObjectEditView @netbox/views/generic/object_views.py",
        )
}

/// Upstream #2291 (b): `alpha.foo.Client` through `import alpha.foo` is the
/// module's class once the module appears, so a method value on a subclass
/// reaches its method.
pub fn python_dotted_module_class_of_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file("alpha/__init__.py", "")
        .file(
            "one.py",
            "import alpha.foo\n\n\nclass Sub(alpha.foo.Client):\n    pass\n\n\ndef go(pool, s: Sub):\n    pool.submit(s.fetch)\n",
        )
        .write(
            "alpha/foo.py",
            "class Client:\n    def fetch(self):\n        return 1\n",
        )
        .edge_after("function go", "references", "method Client::fetch")
}

/// Upstream #2290: `self.get_ip()` stays the class's own method when the
/// module the file imports a `get_ip` from appears, as a fresh index of the
/// edit finds.
pub fn python_self_call_ignores_an_added_import_target() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Python])
        .file("pkg/__init__.py", "")
        .file(
            "pkg/adapter.py",
            "from pkg.httpkit import get_ip\n\n\nclass Adapter:\n    def get_ip(self, request):\n        return request\n\n    def notify(self, request):\n        return self.get_ip(request)\n",
        )
        .write(
            "pkg/httpkit.py",
            "def get_ip(request):\n    return \"1.2.3.4\"\n",
        )
        .edge_after("method Adapter::notify", "calls", "method Adapter::get_ip")
        .no_edge_after("method Adapter::notify", "calls", "function get_ip")
}

/// The models the pytest fixture cases type parameters with.
const PYTHON_FIXTURE_MODELS: &str = "class Forum:\n    def save(self):\n        return self\n\n\nclass Topic:\n    def save(self):\n        return self\n\n\nclass Board:\n    def pin(self):\n        return self\n\n\nclass Stray:\n    def pin(self):\n        return self\n";

/// Upstream #2228: a conftest fixture that comes to return another class
/// retypes an unchanged test's parameter, as a fresh index of the edit finds.
pub fn python_fixture_returns_another_class() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("app/models.py", PYTHON_FIXTURE_MODELS)
        .file(
            "tests/conftest.py",
            "import pytest\nfrom app.models import Forum, Topic\n\n\n@pytest.fixture\ndef account():\n    return Forum()\n",
        )
        .file(
            "tests/test_forum.py",
            "def test_forum(account):\n    account.save()\n",
        )
        .write(
            "tests/conftest.py",
            "import pytest\nfrom app.models import Forum, Topic\n\n\n@pytest.fixture\ndef account():\n    return Topic()\n",
        )
        .edge_before("function test_forum", "calls", "method Forum::save")
        .edge_after("function test_forum", "calls", "method Topic::save")
        .no_edge_after("function test_forum", "calls", "method Forum::save")
}

/// Upstream #2228: a conftest that comes to list a fixture module in
/// `pytest_plugins` puts its fixtures in reach of an unchanged test.
pub fn python_conftest_plugs_a_fixture_module() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("app/models.py", PYTHON_FIXTURE_MODELS)
        .file("tests/conftest.py", "import pytest\n")
        .file(
            "tests/fixtures/plugged.py",
            "import pytest\nfrom app.models import Board\n\n\n@pytest.fixture\ndef pinned():\n    return Board()\n",
        )
        .file("tests/test_board.py", "def test_board(pinned):\n    pinned.pin()\n")
        .write(
            "tests/conftest.py",
            "import pytest\n\npytest_plugins = [\"tests.fixtures.plugged\"]\n",
        )
        .edge_after("function test_board", "calls", "method Board::pin")
}

/// Upstream #2228: a fixture module a conftest pulls in that comes to
/// return another class retypes an unchanged test's parameter.
pub fn python_plugged_fixture_returns_another_class() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("app/models.py", PYTHON_FIXTURE_MODELS)
        .file("tests/conftest.py", "from tests.fixtures.plugged import *  # noqa\n")
        .file(
            "tests/fixtures/plugged.py",
            "import pytest\nfrom app.models import Board, Stray\n\n\n@pytest.fixture\ndef board():\n    return Board()\n",
        )
        .file("tests/test_board.py", "def test_board(board):\n    board.pin()\n")
        .write(
            "tests/fixtures/plugged.py",
            "import pytest\nfrom app.models import Board, Stray\n\n\n@pytest.fixture\ndef board():\n    return Stray()\n",
        )
        .edge_before("function test_board", "calls", "method Board::pin")
        .edge_after("function test_board", "calls", "method Stray::pin")
}

/// Upstream #2228: a fixture decorated through an alias (`from helpers
/// import fixture`, `@fixture`) in a module a conftest pulls in is a fixture
/// by its decorator alone, as on a fresh index; coming to return another
/// class, it retypes an unchanged test's parameter although the module never
/// names pytest.
pub fn python_aliased_plugged_fixture_returns_another_class() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file("app/models.py", PYTHON_FIXTURE_MODELS)
        .file("helpers.py", "from pytest import fixture\n")
        .file("tests/conftest.py", "from tests.fixtures.plugged import *  # noqa\n")
        .file(
            "tests/fixtures/plugged.py",
            "from helpers import fixture\nfrom app.models import Board, Stray\n\n\n@fixture\ndef board():\n    return Board()\n",
        )
        .file("tests/test_board.py", "def test_board(board):\n    board.pin()\n")
        .write(
            "tests/fixtures/plugged.py",
            "from helpers import fixture\nfrom app.models import Board, Stray\n\n\n@fixture\ndef board():\n    return Stray()\n",
        )
        .edge_before("function test_board", "calls", "method Board::pin")
        .edge_after("function test_board", "calls", "method Stray::pin")
}

/// Upstream #2228: a closer conftest's fixture of the same name, added
/// later, is what an unchanged test below it takes.
pub fn python_closer_conftest_overrides_a_fixture() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Python])
        .file("app/models.py", PYTHON_FIXTURE_MODELS)
        .file(
            "tests/conftest.py",
            "import pytest\nfrom app.models import Forum\n\n\n@pytest.fixture\ndef account():\n    return Forum()\n",
        )
        .file(
            "tests/sub/test_sub.py",
            "def test_sub(account):\n    account.save()\n",
        )
        .write(
            "tests/sub/conftest.py",
            "import pytest\nfrom app.models import Topic\n\n\n@pytest.fixture\ndef account():\n    return Topic()\n",
        )
        .edge_before("function test_sub", "calls", "method Forum::save")
        .edge_after("function test_sub", "calls", "method Topic::save")
}

/// Upstream #2175: a Django manager call that comes to build another model
/// types the local it binds.
pub fn python_django_manager_result_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file(
            "app/models.py",
            "class Site:\n    def save(self):\n        return self\n\n\nclass Rack:\n    def save(self):\n        return self\n",
        )
        .file(
            "tests/test_sites.py",
            "from app.models import Rack, Site\n\n\ndef test_record():\n    record = Site.objects.get(pk=1)\n    record.save()\n",
        )
        .write(
            "tests/test_sites.py",
            "from app.models import Rack, Site\n\n\ndef test_record():\n    record = Rack.objects.get(pk=1)\n    record.save()\n",
        )
        .edge_before("function test_record", "calls", "method Site::save")
        .edge_after("function test_record", "calls", "method Rack::save")
}

/// The files every module-global case starts from: two classes with a
/// `fetch`, a module whose `conn` its `init()` assigns, and two importers
/// reading `fetch` through it, by the module and by the name.
fn python_global_files(case: Case, init: &'static str) -> Case {
    case.file(
        "store.py",
        "class Store:\n    def fetch(self, ids):\n        return ids\n",
    )
    .file(
        "decoy.py",
        "class Decoy:\n    def fetch(self, ids):\n        return []\n",
    )
    .file("settings.py", init)
    .file(
        "consumer.py",
        "import settings\n\n\ndef cb(pool):\n    pool.submit(settings.conn.fetch)\n",
    )
    .file(
        "direct.py",
        "from settings import conn\n\n\ndef direct(pool):\n    pool.submit(conn.fetch)\n",
    )
}

/// Upstream #2291 (a): a module global its module comes to assign another
/// class takes that class's method for an unchanged importer's method value,
/// as a fresh index of the edit finds.
pub fn python_module_global_retyped_in_its_module() -> Case {
    python_global_files(
        Case::new(Change::BodyEdit, &[Language::Python]),
        "from store import Store\nfrom decoy import Decoy\nconn = None\n\n\ndef init():\n    global conn\n    conn = Store()\n",
    )
    .write(
        "settings.py",
        "from store import Store\nfrom decoy import Decoy\nconn = None\n\n\ndef init():\n    global conn\n    conn = Decoy()\n",
    )
    .edge_before("function cb", "references", "method Store::fetch")
    .edge_before("function direct", "references", "method Store::fetch")
    .edge_after("function cb", "references", "method Decoy::fetch")
    .edge_after("function direct", "references", "method Decoy::fetch")
    .no_edge_after("function cb", "references", "method Store::fetch")
}

/// Upstream #2291 (a): another module that comes to write a second class
/// into the global leaves its type with no shared declaration, so the
/// method values through it go, in its module and in an importer.
pub fn python_module_global_gains_an_external_write() -> Case {
    python_global_files(
        Case::new(Change::BodyEdit, &[Language::Python]),
        "from store import Store\nconn = None\n\n\ndef init():\n    global conn\n    conn = Store()\n\n\ndef same_file(pool):\n    pool.submit(conn.fetch)\n",
    )
    .file(
        "reset.py",
        "import settings\nfrom decoy import Decoy\n\n\ndef reset():\n    return Decoy\n",
    )
    .write(
        "reset.py",
        "import settings\nfrom decoy import Decoy\n\n\ndef reset():\n    settings.conn = Decoy()\n",
    )
    .edge_before("function cb", "references", "method Store::fetch")
    .edge_before("function same_file", "references", "method Store::fetch")
    .no_edge_after("function cb", "references", "method Store::fetch")
    .no_edge_after("function same_file", "references", "method Store::fetch")
}

/// Upstream #2291 (a): a write the type could not use goes, so the global's
/// type is its module's again and an importer's method value binds it.
pub fn python_module_global_loses_an_unknown_write() -> Case {
    python_global_files(
        Case::new(Change::BodyEdit, &[Language::Python]),
        "from store import Store\nconn = None\n\n\ndef init():\n    global conn\n    conn = Store()\n",
    )
    .file(
        "reset.py",
        "import settings\n\n\ndef reset():\n    settings.conn = make_conn()\n",
    )
    .write("reset.py", "import settings\n\n\ndef reset():\n    return None\n")
    .edge_after("function cb", "references", "method Store::fetch")
}

/// Upstream #2291 (a): deleting the module that wrote an unknown value into
/// the global restores its type for an importer's method value.
pub fn python_module_global_writer_is_deleted() -> Case {
    python_global_files(
        Case::new(Change::DeleteImportedModule, &[Language::Python]),
        "from store import Store\nimport reset\nconn = None\n\n\ndef init():\n    global conn\n    conn = Store()\n",
    )
    .file(
        "reset.py",
        "import settings\n\n\ndef reset():\n    settings.conn = make_conn()\n",
    )
    .remove("reset.py")
    .edge_after("function cb", "references", "method Store::fetch")
}

/// Upstream #2263: a CommonJS `require('./application')` imports that file once
/// it appears, though the binding is only ever used as a value.
pub fn javascript_require_imports_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::JavaScript])
        .file(
            "lib/express.js",
            "var proto = require('./application');\n\nfunction createApplication() {\n  return Object.create(proto);\n}\n\nmodule.exports = createApplication;\n",
        )
        .write(
            "lib/application.js",
            "var app = exports = module.exports = {};\napp.init = function init() {};\n",
        )
        .edge_after("file:lib/express.js", "imports", "file:lib/application.js")
}

/// Upstream #2189: `require('./utils').setCharset` is the module's
/// `setCharset` once the module appears, never its first export.
pub fn javascript_require_member_reaches_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::JavaScript])
        .file(
            "lib/response.js",
            "'use strict';\n\nvar setCharset = require('./utils').setCharset;\n\nfunction send(body) {\n  return setCharset(body, 'utf-8');\n}\n\nmodule.exports = { send };\n",
        )
        .write(
            "lib/utils.js",
            "'use strict';\n\nexports.normalizeType = function (type) {\n  return type;\n};\n\nexports.setCharset = function setCharset(type, charset) {\n  return type + charset;\n};\n",
        )
        .edge_after("function send", "calls", "function setCharset")
        .no_edge_after("function send", "calls", "function normalizeType")
}

/// Upstream #2433: `require('./widget').default` of an ES module is its
/// default export once the module appears, under whatever local name.
pub fn typescript_require_default_of_an_added_es_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/main.ts",
            "export function mount(): unknown {\n  const Ctor = require('./widget').default;\n  return new Ctor();\n}\n",
        )
        .write(
            "src/widget.ts",
            "function helper(): number {\n  return 1;\n}\n\nexport default class Widget {\n  size = helper();\n}\n",
        )
        .edge_after("function mount", "instantiates", "class Widget")
}

/// Upstream #2192: through `export * as core`, a namespace import's
/// `z.core.safeParse()` reaches the namespaced module's function once that
/// module appears, as a fresh index of the edit finds.
pub fn typescript_namespace_reexport_member_reaches_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/external.ts",
            "export * as core from './core';\n",
        )
        .file(
            "src/use.ts",
            "import * as z from './external';\n\nexport function check() {\n  return z.core.safeParse(1);\n}\n",
        )
        .write(
            "src/core.ts",
            "export function safeParse(value: unknown) {\n  return value;\n}\n",
        )
        .edge_after("function check", "calls", "function safeParse")
}

/// Two modules exporting the same function, for the re-export edit cases.
const JS_REEXPORT_A: &str = "export function safeParse(value: unknown) {\n  return value;\n}\n";
const JS_REEXPORT_B: &str = "export function safeParse(value: unknown) {\n  return [value];\n}\n";

/// Upstream #2192: a namespace re-export that comes to name another module
/// moves an unchanged caller's member call there, although the barrel holds
/// no node either side of the edit.
pub fn typescript_namespace_reexport_names_another_module() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file("src/a.ts", JS_REEXPORT_A)
        .file("src/b.ts", JS_REEXPORT_B)
        .file("src/external.ts", "export * as core from './a';\n")
        .file(
            "src/use.ts",
            "import * as z from './external';\n\nexport function check() {\n  return z.core.safeParse(1);\n}\n",
        )
        .write("src/external.ts", "export * as core from './b';\n")
        .edge_before("function check", "calls", "function safeParse @src/a.ts")
        .edge_after("function check", "calls", "function safeParse @src/b.ts")
}

/// A named re-export (`export { safeParse as parse } from …`) that comes to
/// name another module moves an unchanged importer's call there.
pub fn typescript_named_reexport_names_another_module() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file("src/a.ts", JS_REEXPORT_A)
        .file("src/b.ts", JS_REEXPORT_B)
        .file("src/external.ts", "export { safeParse as parse } from './a';\n")
        .file(
            "src/use.ts",
            "import { parse } from './external';\n\nexport function check() {\n  return parse(1);\n}\n",
        )
        .write("src/external.ts", "export { safeParse as parse } from './b';\n")
        .edge_before("function check", "calls", "function safeParse @src/a.ts")
        .edge_after("function check", "calls", "function safeParse @src/b.ts")
}

/// A wildcard re-export behind a second barrel that comes to name another
/// module moves a caller importing the outer barrel.
pub fn typescript_nested_wildcard_reexport_names_another_module() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file("src/a.ts", JS_REEXPORT_A)
        .file("src/b.ts", JS_REEXPORT_B)
        .file("src/inner.ts", "export * from './a';\n")
        .file("src/index.ts", "export * from './inner';\n")
        .file(
            "src/use.ts",
            "import { safeParse } from './index';\n\nexport function check() {\n  return safeParse(1);\n}\n",
        )
        .write("src/inner.ts", "export * from './b';\n")
        .edge_before("function check", "calls", "function safeParse @src/a.ts")
        .edge_after("function check", "calls", "function safeParse @src/b.ts")
}

/// A local export clause (`export { impl as api }`) and an `export default`
/// statement that come to name another declaration move unchanged
/// importers, although every declaration keeps its node.
pub fn typescript_local_export_alias_names_another_declaration() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file(
            "src/api.ts",
            "function impl() {\n  return 1;\n}\n\nfunction other() {\n  return 2;\n}\n\nexport { impl as api };\nexport default impl;\n",
        )
        .file(
            "src/use.ts",
            "import run, { api } from './api';\n\nexport function check() {\n  return api() + run();\n}\n",
        )
        .write(
            "src/api.ts",
            "function impl() {\n  return 1;\n}\n\nfunction other() {\n  return 2;\n}\n\nexport { other as api };\nexport default other;\n",
        )
        .edge_before("function check", "calls", "function impl")
        .edge_after("function check", "calls", "function other")
        .no_edge_after("function check", "calls", "function impl")
}

/// A barrel that comes to re-export a name binds an unchanged importer's
/// call that resolved nowhere before.
pub fn typescript_barrel_gains_a_named_reexport() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file("src/a.ts", JS_REEXPORT_A)
        .file("src/external.ts", "export const version = 1;\n")
        .file(
            "src/use.ts",
            "import { parse } from './external';\n\nexport function check() {\n  return parse(1);\n}\n",
        )
        .write(
            "src/external.ts",
            "export const version = 1;\nexport { safeParse as parse } from './a';\n",
        )
        .edge_after("function check", "calls", "function safeParse @src/a.ts")
}

/// Upstream #2162: a binding an export list stops naming is an unexported
/// module binding, out of another file's name match, although its node is
/// unchanged.
pub fn javascript_export_list_drops_a_binding() -> Case {
    Case::new(Change::ReexportEdit, &[Language::JavaScript])
        .file(
            "src/lib.js",
            "function helper() {\n  return 1;\n}\n\nexport { helper };\n",
        )
        .file(
            "src/use.js",
            "function run() {\n  return helper();\n}\n\nmodule.exports = run;\n",
        )
        .write(
            "src/lib.js",
            "function helper() {\n  return 1;\n}\n\nexport {};\n",
        )
        .edge_before("function run", "calls", "function helper")
}

/// Upstream #2162: a binding an export list comes to name is a module
/// export again, so another file's name match reaches it.
pub fn javascript_export_list_gains_a_binding() -> Case {
    Case::new(Change::ReexportEdit, &[Language::JavaScript])
        .file(
            "src/lib.js",
            "function helper() {\n  return 1;\n}\n\nexport {};\n",
        )
        .file(
            "src/use.js",
            "function run() {\n  return helper();\n}\n\nmodule.exports = run;\n",
        )
        .write(
            "src/lib.js",
            "function helper() {\n  return 1;\n}\n\nexport { helper };\n",
        )
        .edge_after("function run", "calls", "function helper")
}

/// Upstream #2189: `module.exports = require('./a')` that comes to forward
/// another module moves an unchanged CommonJS caller of the default.
pub fn javascript_commonjs_forward_names_another_module() -> Case {
    Case::new(Change::ReexportEdit, &[Language::JavaScript])
        .file("src/a.js", "module.exports = function safeParse(value) {\n  return value;\n};\n")
        .file("src/b.js", "module.exports = function safeParse(value) {\n  return [value];\n};\n")
        .file("src/external.js", "module.exports = require('./a');\n")
        .file(
            "src/use.js",
            "const parse = require('./external');\n\nfunction check() {\n  return parse(1);\n}\n\nmodule.exports = check;\n",
        )
        .write("src/external.js", "module.exports = require('./b');\n")
        .edge_before("function check", "calls", "function safeParse @src/a.js")
        .edge_after("function check", "calls", "function safeParse @src/b.js")
}

/// A sealed module (an import and no export) keeps its bindings from other
/// files; once it gains a CommonJS export (its import and its function's
/// node untouched), a classic script's `helper()` reaches it.
pub fn javascript_sealed_module_gains_a_commonjs_export() -> Case {
    Case::new(Change::ReexportEdit, &[Language::JavaScript])
        .file("src/x.js", "export const x = 1;\n")
        .file(
            "src/lib.js",
            "import { x } from './x';\nfunction helper() {\n  return 1;\n}\n",
        )
        .file("src/use.js", "function run() {\n  return helper();\n}\n")
        .write(
            "src/lib.js",
            "import { x } from './x';\nfunction helper() {\n  return 1;\n}\nmodule.exports = {};\n",
        )
        .edge_after("function run", "calls", "function helper")
}

/// Upstream #2227: what a Svelte component's instance `<script>` declares is
/// its own; once the block becomes `context="module"` (the function's node
/// untouched), another file's call reaches it.
pub fn svelte_instance_script_becomes_a_module_script() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Svelte, Language::TypeScript])
        .file("package.json", "{\"name\":\"web\",\"private\":true,\"dependencies\":{\"svelte\":\"^4\"}}\n")
        .file(
            "src/Widget.svelte",
            "<script>\n  export function helper() {\n    return 1;\n  }\n</script>\n\n<p>hi</p>\n",
        )
        .file("src/use.ts", "export function run() {\n  return helper();\n}\n")
        .write(
            "src/Widget.svelte",
            "<script context=\"module\">\n  export function helper() {\n    return 1;\n  }\n</script>\n\n<p>hi</p>\n",
        )
        .edge_after("function run", "calls", "function helper @src/Widget.svelte")
}

/// A barrel forwarded through a tsconfig `paths` alias (`@mid`) is deleted:
/// the outer barrel's alias source names no file any more, so an unchanged
/// caller's call through both is no import resolution, as on a fresh index
/// (which falls back to a name match). Only the target the outer barrel's
/// row recorded finds the caller.
pub fn typescript_alias_forwarded_barrel_is_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::TypeScript])
        .file(
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@mid\":[\"src/mid.ts\"]}}}\n",
        )
        .file("src/a.ts", JS_REEXPORT_A)
        .file("src/mid.ts", "export * from './a';\n")
        .file("src/index.ts", "export * from '@mid';\n")
        .file(
            "src/use.ts",
            "import { safeParse } from './index';\n\nexport function check() {\n  return safeParse(1);\n}\n",
        )
        .remove("src/mid.ts")
        .edge_before("function check", "calls", "function safeParse @src/a.ts")
}

/// A workspace package's entry barrel moves into a subdirectory: the
/// package name no longer names a file, so an unchanged caller importing a
/// barrel that forwards the package is no import resolution, as on a fresh
/// index.
pub fn typescript_workspace_entry_barrel_is_renamed() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"root\",\"private\":true,\"workspaces\":[\"packages/*\"]}\n")
        .file("packages/mid/package.json", "{\"name\":\"mid\",\"version\":\"1.0.0\"}\n")
        .file("packages/mid/index.ts", "export * from './a';\n")
        .file("packages/mid/a.ts", JS_REEXPORT_A)
        .file("packages/mid/lib/a.ts", JS_REEXPORT_B)
        .file("src/index.ts", "export * from 'mid';\n")
        .file(
            "src/use.ts",
            "import { safeParse } from './index';\n\nexport function check() {\n  return safeParse(1);\n}\n",
        )
        .rename("packages/mid/index.ts", "packages/mid/lib/index.ts")
        .edge_before("function check", "calls", "function safeParse @packages/mid/a.ts")
}

/// A barrel's source resolves by the language of the import it is followed
/// for: `./inner` from a JavaScript caller is `inner.js` first, from a
/// TypeScript one `inner.ts`. The node-less `inner.js` that a JavaScript
/// caller reaches through `outer.js` comes to forward another module, so the
/// caller moves although `inner.ts` is what a TypeScript import would see.
pub fn javascript_barrel_chain_follows_the_importer_language() -> Case {
    Case::new(Change::ReexportEdit, &[Language::JavaScript, Language::TypeScript])
        .file("src/a.js", "export function safeParse(value) {\n  return value;\n}\n")
        .file("src/b.js", "export function safeParse(value) {\n  return [value];\n}\n")
        .file("src/inner.js", "export * from './a';\n")
        .file("src/inner.ts", "export function other(): number {\n  return 1;\n}\n")
        .file("src/outer.js", "export * from './inner';\n")
        .file(
            "src/use.js",
            "import { safeParse } from './outer';\n\nexport function check() {\n  return safeParse(1);\n}\n",
        )
        .write("src/inner.js", "export * from './b';\n")
        .edge_before("function check", "calls", "function safeParse @src/a.js")
        .edge_after("function check", "calls", "function safeParse @src/b.js")
}

/// Upstream #2449: a class written around attribute macros (an annotation
/// after the parameter list, one after a field's name) keeps its method
/// through a body edit, and an unchanged caller keeps its call.
pub fn cpp_attribute_macro_class_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "src/counter.h",
            "namespace db {\n\nclass Counter {\n public:\n  void IncrementBy(int count) LOCKS_EXCLUDED(mu_) {\n    count_ += count;\n  }\n\n private:\n  int count_ GUARDED_BY(mu_);\n};\n\n}  // namespace db\n",
        )
        .file(
            "src/use.cc",
            "#include \"counter.h\"\n\nvoid run(db::Counter& counter) {\n  counter.IncrementBy(1);\n}\n",
        )
        .write(
            "src/counter.h",
            "namespace db {\n\nclass Counter {\n public:\n  void IncrementBy(int count) LOCKS_EXCLUDED(mu_) {\n    count_ += count * 2;\n  }\n\n private:\n  int count_ GUARDED_BY(mu_);\n};\n\n}  // namespace db\n",
        )
        .edge_before("function run", "calls", "method db::Counter::IncrementBy")
        .edge_after("function run", "calls", "method db::Counter::IncrementBy")
}

/// Upstream #2426: in a C++ file whose tree has errors (rocksdb's macro call
/// in a nested struct's head), the class after it keeps its namespaces
/// through a body edit, and an unchanged caller keeps its call into it.
pub fn cpp_broken_tree_class_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "cache/clock_cache.h",
            "namespace rocksdb {\nnamespace clock_cache {\n\nclass FixedTable : public BaseTable {\n public:\n  struct cacheline_aligned(64U) HandleImpl : public ClockHandle {\n    RelaxedAtomic<uint32_t> displacements{};\n    bool standalone = false;\n  };\n\n  struct Opts : public BaseOpts {\n    explicit Opts(size_t size) : BaseOpts(0), size(size) {}\n    size_t size;\n  };\n\n  void Insert(const Opts& opts);\n};\n\nclass AutoTable : public BaseTable {\n public:\n  void Erase() {\n    count_ = 0;\n  }\n\n private:\n  int count_;\n};\n\n}  // namespace clock_cache\n}  // namespace rocksdb\n",
        )
        .file(
            "src/use.cc",
            "#include \"clock_cache.h\"\n\nvoid run(rocksdb::clock_cache::AutoTable& table) {\n  table.Erase();\n}\n",
        )
        .write(
            "cache/clock_cache.h",
            "namespace rocksdb {\nnamespace clock_cache {\n\nclass FixedTable : public BaseTable {\n public:\n  struct cacheline_aligned(64U) HandleImpl : public ClockHandle {\n    RelaxedAtomic<uint32_t> displacements{};\n    bool standalone = false;\n  };\n\n  struct Opts : public BaseOpts {\n    explicit Opts(size_t size) : BaseOpts(0), size(size) {}\n    size_t size;\n  };\n\n  void Insert(const Opts& opts);\n};\n\nclass AutoTable : public BaseTable {\n public:\n  void Erase() {\n    count_ = 1;\n  }\n\n private:\n  int count_;\n};\n\n}  // namespace clock_cache\n}  // namespace rocksdb\n",
        )
        .edge_before("function run", "calls", "method rocksdb::clock_cache::AutoTable::Erase")
        .edge_after("function run", "calls", "method rocksdb::clock_cache::AutoTable::Erase")
}

/// Upstream #2399: an alias retargeted in a header (`typedef SkipList<…>
/// Table;` becoming `typedef OtherList<…> Table;`) moves an unchanged
/// caller's `table_.Insert(buf)` to the type the alias now names, as on a
/// fresh index, though the alias node keeps its identity: a name the unchanged
/// file uses now means another definition.
pub fn cpp_alias_retarget_moves_an_unchanged_receiver() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Cpp])
        .file(
            "db/lists.h",
            "namespace leveldb {\ntemplate <typename Key>\nclass SkipList {\n public:\n  void Insert(const Key& key) {}\n};\ntemplate <typename Key>\nclass OtherList {\n public:\n  void Insert(const Key& key) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/memtable.h",
            "#include \"db/lists.h\"\nnamespace leveldb {\nclass MemTable {\n public:\n  void Add(const char* buf);\n private:\n  typedef SkipList<const char*> Table;\n  Table table_;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/memtable.cc",
            "#include \"db/memtable.h\"\nnamespace leveldb {\nvoid MemTable::Add(const char* buf) {\n  table_.Insert(buf);\n}\n}  // namespace leveldb\n",
        )
        .write(
            "db/memtable.h",
            "#include \"db/lists.h\"\nnamespace leveldb {\nclass MemTable {\n public:\n  void Add(const char* buf);\n private:\n  typedef OtherList<const char*> Table;\n  Table table_;\n};\n}  // namespace leveldb\n",
        )
        .edge_before(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::SkipList::Insert",
        )
        .edge_after(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::OtherList::Insert",
        )
        .no_edge_after(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::SkipList::Insert",
        )
}

/// Upstream #2399: an alias template retargeted in a header
/// (`template <typename K> using Table = SkipList<K>;` naming `OtherList<K>`)
/// moves the call of an unchanged file that writes `Table<const char*>`,
/// though the file is no source of that header.
pub fn cpp_template_alias_retarget_moves_an_unchanged_receiver() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Cpp])
        .file(
            "db/lists.hpp",
            "namespace leveldb {\ntemplate <typename Key>\nclass SkipList {\n public:\n  void Insert(const Key& key) {}\n};\ntemplate <typename Key>\nclass OtherList {\n public:\n  void Insert(const Key& key) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/aliases.hpp",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\ntemplate <typename K>\nusing Table = SkipList<K>;\n}  // namespace leveldb\n",
        )
        .file(
            "db/use.cc",
            "#include \"db/aliases.hpp\"\nnamespace leveldb {\nvoid Fill() {\n  Table<const char*> table;\n  table.Insert(\"k\");\n}\n}  // namespace leveldb\n",
        )
        .write(
            "db/aliases.hpp",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\ntemplate <typename K>\nusing Table = OtherList<K>;\n}  // namespace leveldb\n",
        )
        .edge_before("function leveldb::Fill", "calls", "method leveldb::SkipList::Insert")
        .edge_after("function leveldb::Fill", "calls", "method leveldb::OtherList::Insert")
        .no_edge_after("function leveldb::Fill", "calls", "method leveldb::SkipList::Insert")
}

/// Upstream #2399: an alias reached through another alias
/// (`using Lookup = Table;`) moves an unchanged caller that writes only
/// `Lookup` when `Table` is retargeted, with the header declaring `Lookup`
/// rewritten in the same batch.
pub fn cpp_alias_chain_retarget_moves_an_unchanged_receiver() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Cpp])
        .file(
            "db/lists.hpp",
            "namespace leveldb {\nclass SkipList {\n public:\n  void Insert(int key) {}\n};\nclass OtherList {\n public:\n  void Insert(int key) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/table.hpp",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\nusing Table = SkipList;\n}  // namespace leveldb\n",
        )
        .file(
            "db/lookup.hpp",
            "#include \"db/table.hpp\"\nnamespace leveldb {\nusing Lookup = Table;\n}  // namespace leveldb\n",
        )
        .file(
            "db/fill.cc",
            "#include \"db/lookup.hpp\"\nnamespace leveldb {\nvoid Fill() {\n  Lookup lookup;\n  lookup.Insert(1);\n}\n}  // namespace leveldb\n",
        )
        .write(
            "db/lookup.hpp",
            "#include \"db/table.hpp\"\nnamespace leveldb {\n// The skip list's lookup.\nusing Lookup = Table;\n}  // namespace leveldb\n",
        )
        .write(
            "db/table.hpp",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\nusing Table = OtherList;\n}  // namespace leveldb\n",
        )
        .edge_before("function leveldb::Fill", "calls", "method leveldb::SkipList::Insert")
        .edge_after("function leveldb::Fill", "calls", "method leveldb::OtherList::Insert")
        .no_edge_after("function leveldb::Fill", "calls", "method leveldb::SkipList::Insert")
}

/// A member's type changed in a header (`SkipList table_;` becoming
/// `OtherList table_;`, no alias) moves the call of the header's unchanged
/// source, which receiver inference reads it from (v0.54.0 already diverged
/// here; owner 2026-10-12: fixed with W4-CPP-04).
pub fn cpp_header_member_type_moves_an_unchanged_receiver() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "db/lists.hpp",
            "namespace leveldb {\nclass SkipList {\n public:\n  void Insert(int key) {}\n};\nclass OtherList {\n public:\n  void Insert(int key) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/memtable.h",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\nclass MemTable {\n public:\n  void Add(int buf);\n private:\n  SkipList table_;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/memtable.cc",
            "#include \"db/memtable.h\"\nnamespace leveldb {\nvoid MemTable::Add(int buf) {\n  table_.Insert(buf);\n}\n}  // namespace leveldb\n",
        )
        .write(
            "db/memtable.h",
            "#include \"db/lists.hpp\"\nnamespace leveldb {\nclass MemTable {\n public:\n  void Add(int buf);\n private:\n  OtherList table_;\n};\n}  // namespace leveldb\n",
        )
        .edge_before(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::SkipList::Insert",
        )
        .edge_after(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::OtherList::Insert",
        )
        .no_edge_after(
            "method leveldb::MemTable::Add @db/memtable.cc",
            "calls",
            "method leveldb::SkipList::Insert",
        )
}

/// Upstream #2399: a class added in an inner scope hides an outer alias of
/// its name. `Holder::Use`, out of line in a file no header names as its
/// source, writes `Widget`; once `Holder` declares its own `Widget` (without
/// `Spin`), the call no longer reaches `Gadget::Spin` through the alias.
pub fn cpp_inner_class_added_hides_an_outer_alias() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cpp])
        .file(
            "app/gadget.hpp",
            "namespace app {\nclass Gadget {\n public:\n  void Spin() {}\n};\nusing Widget = Gadget;\n}  // namespace app\n",
        )
        .file(
            "app/holder.hpp",
            "#include \"app/gadget.hpp\"\nnamespace app {\nclass Holder {\n public:\n  void Use();\n};\n}  // namespace app\n",
        )
        .file(
            "app/use.cc",
            "#include \"app/holder.hpp\"\nnamespace app {\nvoid Holder::Use() {\n  Widget w;\n  w.Spin();\n}\n}  // namespace app\n",
        )
        .write(
            "app/holder.hpp",
            "#include \"app/gadget.hpp\"\nnamespace app {\nclass Holder {\n public:\n  class Widget {\n   public:\n    void Turn() {}\n  };\n  void Use();\n};\n}  // namespace app\n",
        )
        .edge_before("method app::Holder::Use", "calls", "method app::Gadget::Spin")
        .no_edge_after("method app::Holder::Use", "calls", "method app::Gadget::Spin")
}

/// Upstream #2399: a header the caller does not include declaring the same
/// alias for another type makes it ambiguous, so the caller no longer follows
/// it, as on a fresh index.
pub fn cpp_alias_in_an_unincluded_header_makes_it_ambiguous() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cpp])
        .file(
            "store/kinds.hpp",
            "namespace app {\nclass MemStore {\n public:\n  void Save() {}\n};\nclass DiskStore {\n public:\n  void Save() {}\n};\n}  // namespace app\n",
        )
        .file(
            "config/fast.hpp",
            "#include \"store/kinds.hpp\"\nnamespace app {\nusing Store = MemStore;\n}  // namespace app\n",
        )
        .file(
            "src/use.cc",
            "#include \"config/fast.hpp\"\nnamespace app {\nvoid Run() {\n  Store store;\n  store.Save();\n}\n}  // namespace app\n",
        )
        .write(
            "config/safe.hpp",
            "#include \"store/kinds.hpp\"\nnamespace app {\nusing Store = DiskStore;\n}  // namespace app\n",
        )
        .edge_before("function app::Run", "calls", "method app::MemStore::Save")
        .no_edge_after("function app::Run", "calls", "method app::MemStore::Save")
}

/// Upstream #2399: an alias a class inherits follows its base clause. Once
/// `ParseDescriptor` derives from `Proto3Descriptor` instead, its out-of-line
/// `NewMsg(Field f)` reaches the other base's `Field`.
pub fn cpp_base_change_moves_an_inherited_alias() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cpp])
        .file("proto/descriptor.hpp", "namespace proto {\nclass FieldDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\nclass OneofDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\n}  // namespace proto\n")
        .file(
            "proto/traits.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\nstruct Proto3Descriptor {\n  using Field = const OneofDescriptor*;\n};\nstruct ParseDescriptor : Proto2Descriptor {\n  static bool NewMsg(Field f);\n};\n}  // namespace proto\n",
        )
        .file(
            "proto/parse.cc",
            "#include \"proto/traits.hpp\"\nnamespace proto {\nbool ParseDescriptor::NewMsg(Field f) { return f->is_repeated(); }\n}  // namespace proto\n",
        )
        .write(
            "proto/traits.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\nstruct Proto3Descriptor {\n  using Field = const OneofDescriptor*;\n};\nstruct ParseDescriptor : Proto3Descriptor {\n  static bool NewMsg(Field f);\n};\n}  // namespace proto\n",
        )
        .edge_before(
            "method proto::ParseDescriptor::NewMsg @proto/parse.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
        .edge_after(
            "method proto::ParseDescriptor::NewMsg @proto/parse.cc",
            "calls",
            "method proto::OneofDescriptor::is_repeated",
        )
        .no_edge_after(
            "method proto::ParseDescriptor::NewMsg @proto/parse.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
}

/// Upstream #2399: a base two derivations up changing its own base moves the
/// inherited alias of `StrictParser::NewMsg`, whose out-of-line definition
/// names only `StrictParser`, in a file that is no header's source.
pub fn cpp_base_change_reaches_an_out_of_line_method_of_a_derived_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cpp])
        .file("proto/descriptor.hpp", "namespace proto {\nclass FieldDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\nclass OneofDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\n}  // namespace proto\n")
        .file(
            "proto/traits.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\nstruct Proto3Descriptor {\n  using Field = const OneofDescriptor*;\n};\nstruct BaseParser : Proto2Descriptor {};\n}  // namespace proto\n",
        )
        .file(
            "proto/parsers.hpp",
            "#include \"proto/traits.hpp\"\nnamespace proto {\nstruct Parser : BaseParser {};\nstruct StrictParser : Parser {\n  static bool NewMsg(Field f);\n};\n}  // namespace proto\n",
        )
        .file(
            "proto/impl.cc",
            "#include \"proto/parsers.hpp\"\nnamespace proto {\nbool StrictParser::NewMsg(Field f) { return f->is_repeated(); }\n}  // namespace proto\n",
        )
        .write(
            "proto/traits.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\nstruct Proto3Descriptor {\n  using Field = const OneofDescriptor*;\n};\nstruct BaseParser : Proto3Descriptor {};\n}  // namespace proto\n",
        )
        .edge_before(
            "method proto::StrictParser::NewMsg @proto/impl.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
        .edge_after(
            "method proto::StrictParser::NewMsg @proto/impl.cc",
            "calls",
            "method proto::OneofDescriptor::is_repeated",
        )
        .no_edge_after(
            "method proto::StrictParser::NewMsg @proto/impl.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
}

/// Upstream #2399, one batch: `base.hpp` is rewritten first (its nodes go,
/// and with them `Derived`'s `extends` edge), then `derived.hpp` drops `:
/// Proto2Descriptor`; the old surface of `derived.hpp` still shows the base,
/// so `Derived::NewMsg` moves to the namespace's `Field`.
pub fn cpp_batch_rewrites_a_base_before_dropping_a_derivation() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cpp])
        .file("proto/descriptor.hpp", "namespace proto {\nclass FieldDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\nclass OneofDescriptor {\n public:\n  bool is_repeated() const { return false; }\n};\n}  // namespace proto\n")
        .file(
            "proto/base.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\n}  // namespace proto\n",
        )
        .file(
            "proto/derived.hpp",
            "#include \"proto/base.hpp\"\nnamespace proto {\nusing Field = const OneofDescriptor*;\nstruct Derived : Proto2Descriptor {\n  static bool NewMsg(Field f);\n};\n}  // namespace proto\n",
        )
        .file(
            "proto/use.cc",
            "#include \"proto/derived.hpp\"\nnamespace proto {\nbool Derived::NewMsg(Field f) { return f->is_repeated(); }\n}  // namespace proto\n",
        )
        .write(
            "proto/base.hpp",
            "#include \"proto/descriptor.hpp\"\nnamespace proto {\n// Proto2's descriptor traits.\nstruct Proto2Descriptor {\n  using Field = const FieldDescriptor*;\n};\n}  // namespace proto\n",
        )
        .write(
            "proto/derived.hpp",
            "#include \"proto/base.hpp\"\nnamespace proto {\nusing Field = const OneofDescriptor*;\nstruct Derived {\n  static bool NewMsg(Field f);\n};\n}  // namespace proto\n",
        )
        .edge_before(
            "method proto::Derived::NewMsg @proto/use.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
        .edge_after(
            "method proto::Derived::NewMsg @proto/use.cc",
            "calls",
            "method proto::OneofDescriptor::is_repeated",
        )
        .no_edge_after(
            "method proto::Derived::NewMsg @proto/use.cc",
            "calls",
            "method proto::FieldDescriptor::is_repeated",
        )
}

/// Upstream #2407: a class another source file defines belongs to that
/// translation unit, so a namesake `leveldb::Comparator` added in a test
/// source never takes the base of `NumberComparator`, which keeps the header's
/// `Comparator`, as on a fresh index.
pub fn cpp_source_file_namesake_does_not_take_a_base() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cpp])
        .file(
            "include/leveldb/comparator.h",
            "namespace leveldb {\nclass Comparator {\n public:\n  virtual int Compare(const char* a, const char* b) const = 0;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/db_test.cc",
            "#include \"include/leveldb/comparator.h\"\nnamespace leveldb {\nclass NumberComparator : public Comparator {\n public:\n  int Compare(const char* a, const char* b) const override { return 0; }\n};\n}  // namespace leveldb\n",
        )
        .write(
            "db/skiplist_test.cc",
            "namespace leveldb {\nstruct Comparator {\n  int operator()(const int& a, const int& b) const { return a - b; }\n};\n}  // namespace leveldb\n",
        )
        .edge_before(
            "class leveldb::NumberComparator",
            "extends",
            "class leveldb::Comparator @include/leveldb/comparator.h",
        )
        .edge_after(
            "class leveldb::NumberComparator",
            "extends",
            "class leveldb::Comparator @include/leveldb/comparator.h",
        )
        .no_edge_after(
            "class leveldb::NumberComparator",
            "extends",
            "struct leveldb::Comparator @db/skiplist_test.cc",
        )
}

/// Upstream #2407: a using-declaration the deriving file adds before the
/// class names the base it means, so `public FilterPolicy` moves from the
/// file's only visible namesake to `leveldb::FilterPolicy`.
pub fn cpp_using_declaration_names_the_base() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/leveldb/filter_policy.h",
            "namespace leveldb {\nclass FilterPolicy {\n public:\n  virtual const char* Name() const = 0;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "include/other/filter_policy.h",
            "namespace other {\nclass FilterPolicy {\n public:\n  virtual const char* Name() const = 0;\n};\n}  // namespace other\n",
        )
        .file(
            "db/c.cc",
            "#include \"include/leveldb/filter_policy.h\"\nusing other::FilterPolicy;\nstruct leveldb_filterpolicy_t : public FilterPolicy {\n  const char* Name() const override { return \"c\"; }\n};\n",
        )
        .write(
            "db/c.cc",
            "#include \"include/leveldb/filter_policy.h\"\nusing leveldb::FilterPolicy;\nstruct leveldb_filterpolicy_t : public FilterPolicy {\n  const char* Name() const override { return \"c\"; }\n};\n",
        )
        .edge_before(
            "struct leveldb_filterpolicy_t",
            "extends",
            "class other::FilterPolicy",
        )
        .edge_after(
            "struct leveldb_filterpolicy_t",
            "extends",
            "class leveldb::FilterPolicy",
        )
        .no_edge_after(
            "struct leveldb_filterpolicy_t",
            "extends",
            "class other::FilterPolicy",
        )
}

/// Upstream #2413: once the header retypes a member from the project's
/// `Slice` to `std::string`, the unchanged source's `saved_slice_.clear()`
/// is the string's own member, not `Slice::clear`, which the receiver's
/// shared word would otherwise guess.
pub fn cpp_header_member_becomes_a_library_type() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/leveldb/slice.h",
            "namespace leveldb {\nclass Slice {\n public:\n  void clear() { size_ = 0; }\n private:\n  unsigned size_;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/db_iter.h",
            "#include <string>\n#include \"include/leveldb/slice.h\"\nnamespace leveldb {\nclass DBIter {\n public:\n  void Next();\n private:\n  Slice saved_slice_;\n};\n}  // namespace leveldb\n",
        )
        .file(
            "db/db_iter.cc",
            "#include \"db/db_iter.h\"\nnamespace leveldb {\nvoid DBIter::Next() {\n  saved_slice_.clear();\n}\n}  // namespace leveldb\n",
        )
        .write(
            "db/db_iter.h",
            "#include <string>\n#include \"include/leveldb/slice.h\"\nnamespace leveldb {\nclass DBIter {\n public:\n  void Next();\n private:\n  std::string saved_slice_;\n};\n}  // namespace leveldb\n",
        )
        .edge_before(
            "method leveldb::DBIter::Next @db/db_iter.cc",
            "calls",
            "method leveldb::Slice::clear",
        )
        .no_edge_after(
            "method leveldb::DBIter::Next @db/db_iter.cc",
            "calls",
            "method leveldb::Slice::clear",
        )
}

/// Upstream #2413: a lowercase type is from outside the project only while
/// the project declares no type of that name. Once a vendored
/// `absl::flat_hash_set` appears, the unchanged caller's `arena.insert(1)`
/// is that class's method, never the decoy `Arena::insert`.
pub fn cpp_project_declares_a_lowercase_outside_type() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cpp])
        .file(
            "util/arena.h",
            "namespace leveldb {\nclass Arena {\n public:\n  void insert(int block) {}\n};\n}  // namespace leveldb\n",
        )
        .file(
            "util/use.cc",
            "#include \"absl/container/flat_hash_set.h\"\n#include \"util/arena.h\"\nnamespace leveldb {\nvoid Use() {\n  absl::flat_hash_set<int> arena;\n  arena.insert(1);\n}\n}  // namespace leveldb\n",
        )
        .write(
            "absl/container/flat_hash_set.h",
            "namespace absl {\ntemplate <class T>\nclass flat_hash_set {\n public:\n  void insert(const T& value) {}\n};\n}  // namespace absl\n",
        )
        .edge_after(
            "function leveldb::Use",
            "calls",
            "method absl::flat_hash_set::insert",
        )
        .no_edge_after("function leveldb::Use", "calls", "method leveldb::Arena::insert")
}

/// Upstream #2443: a member declared with nested template arguments is its
/// class template's, so retyping it in the header moves the unchanged
/// source's call from `autovector::clear` to `smallvector::clear`.
pub fn cpp_nested_template_member_type_moves_an_unchanged_receiver() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "util/vectors.h",
            "namespace rocksdb {\ntemplate <class T>\nclass autovector {\n public:\n  void clear() {}\n};\ntemplate <class T>\nclass smallvector {\n public:\n  void clear() {}\n};\nstruct FileMetaData {};\n}  // namespace rocksdb\n",
        )
        .file(
            "db/version_set.h",
            "#include \"util/vectors.h\"\nnamespace rocksdb {\nclass VersionStorageInfo {\n public:\n  void Reset();\n private:\n  autovector<std::pair<int, FileMetaData*>> files_;\n};\n}  // namespace rocksdb\n",
        )
        .file(
            "db/version_set.cc",
            "#include \"db/version_set.h\"\nnamespace rocksdb {\nvoid VersionStorageInfo::Reset() {\n  files_.clear();\n}\n}  // namespace rocksdb\n",
        )
        .write(
            "db/version_set.h",
            "#include \"util/vectors.h\"\nnamespace rocksdb {\nclass VersionStorageInfo {\n public:\n  void Reset();\n private:\n  smallvector<std::pair<int, FileMetaData*>> files_;\n};\n}  // namespace rocksdb\n",
        )
        .edge_before(
            "method rocksdb::VersionStorageInfo::Reset @db/version_set.cc",
            "calls",
            "method rocksdb::autovector::clear",
        )
        .edge_after(
            "method rocksdb::VersionStorageInfo::Reset @db/version_set.cc",
            "calls",
            "method rocksdb::smallvector::clear",
        )
        .no_edge_after(
            "method rocksdb::VersionStorageInfo::Reset @db/version_set.cc",
            "calls",
            "method rocksdb::autovector::clear",
        )
}

/// Upstream #2176 (W4-CPP-07): a namespace alias another header writes
/// moves, so the unchanged caller's `py::print("hi")` reaches the other
/// namespace's `print`.
pub fn cpp_cross_file_namespace_alias_retarget() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/pybind11/print.h",
            "#pragma once\nnamespace pybind11 {\ninline void print(const char* text) {}\n}  // namespace pybind11\n",
        )
        .file(
            "include/other/print.h",
            "#pragma once\nnamespace other {\ninline void print(const char* text) {}\n}  // namespace other\n",
        )
        .file(
            "include/aliases.h",
            "#pragma once\nnamespace py = pybind11;\n",
        )
        .file(
            "src/main.cc",
            "#include \"aliases.h\"\nint main() {\n  py::print(\"hi\");\n  return 0;\n}\n",
        )
        .write(
            "include/aliases.h",
            "#pragma once\nnamespace py = other;\n",
        )
        .edge_before("function main", "calls", "function pybind11::print")
        .edge_after("function main", "calls", "function other::print")
        .no_edge_after("function main", "calls", "function pybind11::print")
}

/// Upstream #2176 (W4-CPP-07): the namespace an opener macro opens moves, so
/// the unchanged caller's `lib::run()` names nothing the header declares.
pub fn cpp_macro_opener_opens_another_namespace() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/lib/config.h",
            "#pragma once\n#define LIB_BEGIN namespace lib {\n#define LIB_END }\n",
        )
        .file(
            "include/lib/run.h",
            "#pragma once\n#include \"lib/config.h\"\nLIB_BEGIN\ninline int run() { return 0; }\nLIB_END\n",
        )
        .file(
            "src/main.cc",
            "#include \"lib/run.h\"\nint main() { return lib::run(); }\n",
        )
        .write(
            "include/lib/config.h",
            "#pragma once\n#define LIB_BEGIN namespace mylib {\n#define LIB_END }\n",
        )
        .edge_before("function main", "calls", "function run")
        .no_edge_after("function main", "calls", "function run")
}

/// Upstream #2176 (W4-CPP-07): only the header defining a macro alias
/// changes; the opener another header defines through it, and the header
/// using that opener, are unchanged, and the caller's `lib::run()` follows.
pub fn cpp_macro_alias_moves_an_opener_it_feeds() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/lib/name.h",
            "#pragma once\n#define LIB_NAME lib\n",
        )
        .file(
            "include/lib/config.h",
            "#pragma once\n#include \"lib/name.h\"\n#define LIB_BEGIN namespace LIB_NAME {\n#define LIB_END }\n",
        )
        .file(
            "include/lib/run.h",
            "#pragma once\n#include \"lib/config.h\"\nLIB_BEGIN\ninline int run() { return 0; }\nLIB_END\n",
        )
        .file(
            "src/main.cc",
            "#include \"lib/run.h\"\nint main() { return lib::run(); }\n",
        )
        .write(
            "include/lib/name.h",
            "#pragma once\n#define LIB_NAME otherlib\n",
        )
        .edge_before("function main", "calls", "function run")
        .no_edge_after("function main", "calls", "function run")
}

/// Upstream #2445 (W4-CPP-07): a closer moved above an unchanged
/// declaration line takes it out of the macro's namespace, so the
/// unchanged caller's `::pipe(fds)` reaches it (a global name skips a
/// declaration a macro namespace holds).
pub fn cpp_macro_closer_moves_a_declaration_out_of_the_namespace() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/lib/os.hpp",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\nLIB_BEGIN_NAMESPACE\n\ninline int pipe(int* fds) { return 0; }\nLIB_END_NAMESPACE\n",
        )
        .file(
            "src/run.cc",
            "#include \"lib/os.hpp\"\nint run(int* fds) { return ::pipe(fds); }\n",
        )
        .write(
            "include/lib/os.hpp",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\nLIB_BEGIN_NAMESPACE\nLIB_END_NAMESPACE\ninline int pipe(int* fds) { return 0; }\n\n",
        )
        .edge_after("function run", "calls", "function pipe")
}

/// The same for a free `operator<<` whose declaration line stays put: the
/// name lane scans for `operator`, which `::operator<<(…)` writes.
pub fn cpp_free_operator_moves_out_of_the_macro_namespace() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/lib/stream.hpp",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\nstruct Stream {};\nstruct Value {};\nLIB_BEGIN_NAMESPACE\n\ninline Stream& operator<<(Stream& out, const Value& value) { return out; }\nLIB_END_NAMESPACE\n",
        )
        .file(
            "src/print.cc",
            "#include \"lib/stream.hpp\"\nvoid print(Stream& out, const Value& value) { ::operator<<(out, value); }\n",
        )
        .write(
            "include/lib/stream.hpp",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\nstruct Stream {};\nstruct Value {};\nLIB_BEGIN_NAMESPACE\nLIB_END_NAMESPACE\ninline Stream& operator<<(Stream& out, const Value& value) { return out; }\n\n",
        )
        .edge_after("function print", "calls", "function operator<<")
}

/// Upstream #2445 (W4-CPP-07): only the header defining the opener and
/// closer changes, and they no longer open a namespace; the unchanged
/// header using them now declares `pipe` globally, and the unchanged
/// caller's `::pipe(fds)` reaches it.
pub fn cpp_opener_definition_stops_opening_a_namespace() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "include/lib/config.h",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE namespace lib {\n#define LIB_END_NAMESPACE }\n",
        )
        .file(
            "include/lib/os.h",
            "#pragma once\n#include \"lib/config.h\"\nLIB_BEGIN_NAMESPACE\ninline int pipe(int* fds) { return 0; }\nLIB_END_NAMESPACE\n",
        )
        .file(
            "src/run.cc",
            "#include \"lib/os.h\"\nint run(int* fds) { return ::pipe(fds); }\n",
        )
        .write(
            "include/lib/config.h",
            "#pragma once\n#define LIB_BEGIN_NAMESPACE\n#define LIB_END_NAMESPACE\n",
        )
        .edge_after("function run", "calls", "function pipe")
}

/// Upstream #2234 (W4-CPP-08): a C++ header adding a method named like a C
/// file's call never gives that call a method: C has no methods.
pub fn c_call_ignores_an_added_cpp_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::C, Language::Cpp])
        .file(
            "src/util.c",
            "#include <unistd.h>\n\nlong slurp(int fd, char *buf, long n) {\n    return read(fd, buf, n);\n}\n",
        )
        .write(
            "deps/hiredis/adapters/qt.hpp",
            "class RedisQtAdapter {\npublic:\n    void read() { }\n};\n",
        )
        .no_edge_after("function slurp", "calls", "method RedisQtAdapter::read")
}

/// Upstream #2150's C-call half (W4-CPP-08): an Objective-C class adding a
/// method named like a block a method calls in C call syntax never takes
/// that call.
pub fn objc_c_call_ignores_an_added_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::ObjC])
        .file(
            "Core/SDCache.h",
            "#import <Foundation/Foundation.h>\n@interface SDCache : NSObject\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image;\n@end\n",
        )
        .file(
            "Core/SDCache.m",
            "#import \"SDCache.h\"\n@implementation SDCache\n- (void)storeWithCompletion:(void (^)(void))completionBlock image:(id)image {\n    completionBlock();\n}\n@end\n",
        )
        .write(
            "Core/SDOperation.m",
            "#import <Foundation/Foundation.h>\n@interface SDOperation : NSObject\n- (void)completionBlock;\n@end\n@implementation SDOperation\n- (void)completionBlock {\n}\n@end\n",
        )
        .no_edge_after(
            "method SDCache::storeWithCompletion:image:",
            "calls",
            "method SDOperation::completionBlock",
        )
}

/// Layers of the deep include graph of [`cpp_macro_visibility_through_a_deep_include_graph`].
const DEEP_INCLUDE_LAYERS: usize = 20;

/// Two value-guarded headers per layer (`#define H_3_a 1`, a guard the walk
/// cannot decide from the guard alone), each including both headers of the
/// next layer: without upstream #2292's re-entry skip a walk rescans them on
/// every one of the 2^depth inclusion paths.
static DEEP_INCLUDE_HEADERS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let mut headers = Vec::new();
    for depth in 0..DEEP_INCLUDE_LAYERS {
        for side in ["a", "b"] {
            let guard = format!("H_{depth}_{side}");
            let next = if depth + 1 < DEEP_INCLUDE_LAYERS {
                format!("#include \"h{0}_a.h\"\n#include \"h{0}_b.h\"\n", depth + 1)
            } else {
                String::new()
            };
            headers.push((
                format!("src/h{depth}_{side}.h"),
                format!(
                    "#ifndef {guard}\n#define {guard} 1\n{next}#define M_{depth}_{side}(x) (x)\nint f_{depth}_{side}(int);\n#endif\n"
                ),
            ));
        }
    }
    headers
});

/// Upstream #2292: the macro-visibility walk bounded by re-entry skips and an
/// event budget gives the answers of the full walk, so a header that defines
/// `TRACE` leaving the project, behind a deep value-guarded include graph,
/// turns the unchanged unit's `TRACE(1)` from a macro call into a call of the
/// project's `TRACE`, after the sync as on a fresh index.
pub fn cpp_macro_visibility_through_a_deep_include_graph() -> Case {
    let mut case = Case::new(Change::DeleteImportedModule, &[Language::Cpp]);
    for (path, contents) in DEEP_INCLUDE_HEADERS.iter() {
        case = case.file(path.as_str(), contents.as_str());
    }
    case.file("src/trace.h", "#define TRACE(v) ((void)(v))\n")
        .file(
            "src/unit.cpp",
            "#include \"h0_a.h\"\n#include \"h0_b.h\"\n#include \"trace.h\"\nvoid unit() { TRACE(1); }\n",
        )
        .file(
            "src/decoy.cpp",
            "void TRACE(int v) {}\nvoid caller() { TRACE(2); }\n",
        )
        .remove("src/trace.h")
        .edge_throughout("function caller", "calls", "function TRACE @src/decoy.cpp")
        .edge_after("function unit", "calls", "function TRACE @src/decoy.cpp")
}

/// Upstream #946 (W4-CPP-10): a class annotated with a lowercase macro
/// mints no phantom function, before and after an edit of a body in the
/// same file, and an unchanged caller of the real function keeps its edge.
pub fn cpp_lowercase_macro_class_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "src/shapes.cpp",
            "class my_export Foo : public Base {\n public:\n  void run() {}\n};\nint helper() { return 1; }\n",
        )
        .file(
            "src/use.cpp",
            "int helper();\nint use() { return helper(); }\n",
        )
        .write(
            "src/shapes.cpp",
            "class my_export Foo : public Base {\n public:\n  void run() { int x = 0; (void)x; }\n};\nint helper() { return 2; }\n",
        )
        .edge_throughout("function use", "calls", "function helper @src/shapes.cpp")
}

/// Upstream #1353 (W4-CPP-11): a C23 `auto` local no longer hides the call
/// in its initializer, before and after a body edit.
pub fn c_auto_local_initializer_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file("src/hb.c", "int get_hb(void) { return 1; }\n")
        .file(
            "src/q.c",
            "int get_hb(void);\nvoid q(void) { auto hb = get_hb(); (void)hb; }\n",
        )
        .write(
            "src/q.c",
            "int get_hb(void);\nvoid q(void) { auto hb = get_hb(); (void)hb; (void)0; }\n",
        )
        .edge_throughout("function q", "calls", "function get_hb @src/hb.c")
}

/// Upstream #1789's prerequisite phase: once `Child` gains `extends Base`
/// in the file that also calls `c.run()` on a `Child`, the call reaches
/// `Base::run`, the extends edge resolved before the call, as on a fresh
/// index.
pub fn java_subclass_gains_a_base_its_caller_reaches() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base {\n    public void run() { }\n}\n",
        )
        .file(
            "src/app/Use.java",
            "package app;\n\nclass Child {\n}\n\npublic class Use {\n    public void go() {\n        Child c = new Child();\n        c.run();\n    }\n}\n",
        )
        .write(
            "src/app/Use.java",
            "package app;\n\nclass Child extends Base {\n}\n\npublic class Use {\n    public void go() {\n        Child c = new Child();\n        c.run();\n    }\n}\n",
        )
        .edge_after("method app::Use::go", "calls", "method app::Base::run")
}

/// A Java class gains a second interface: `implements Repo, Store` is a
/// `type_list` read type by type, so both resolve, as on a fresh index.
pub fn java_class_gains_a_second_interface() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/Repo.java",
            "package app;\n\npublic interface Repo {\n    void save();\n}\n",
        )
        .file(
            "src/app/Store.java",
            "package app;\n\npublic interface Store {\n    void load();\n}\n",
        )
        .file(
            "src/app/JpaRepo.java",
            "package app;\n\npublic class JpaRepo implements Repo {\n    public void save() { }\n    public void load() { }\n}\n",
        )
        .write(
            "src/app/JpaRepo.java",
            "package app;\n\npublic class JpaRepo implements Repo, Store {\n    public void save() { }\n    public void load() { }\n}\n",
        )
        .edge_before("class app::JpaRepo", "implements", "interface app::Repo")
        .edge_after("class app::JpaRepo", "implements", "interface app::Repo")
        .edge_after("class app::JpaRepo", "implements", "interface app::Store")
}

/// Upstream #2172: a bare Kotlin call reaches a member of what the class
/// around it inherits, read from the declarations' heads by name, so a base
/// changed in another file moves the call of an unchanged subclass, as on a
/// fresh index.
pub fn kotlin_bare_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "src/app/Root.kt",
            "package app\n\nopen class Root {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Other.kt",
            "package app\n\nopen class Other {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Base.kt",
            "package app\n\nopen class Base : Root()\n",
        )
        .file(
            "src/app/Child.kt",
            "package app\n\nclass Child : Base() {\n    fun go() {\n        assist()\n    }\n}\n",
        )
        .write(
            "src/app/Base.kt",
            "package app\n\nopen class Base : Other()\n",
        )
        .edge_before("method app::Child::go", "calls", "method app::Root::assist")
        .edge_after(
            "method app::Child::go",
            "calls",
            "method app::Other::assist",
        )
}

/// Upstream #2172: the members of a type some project function type takes
/// as a receiver are in reach of every bare call, so a file appearing with
/// `Helper.() -> Unit` makes an unchanged file's bare `ping()` reach
/// `Helper::ping`, as on a fresh index.
pub fn kotlin_receiver_type_appears_in_another_file() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Kotlin])
        .file(
            "src/app/Helper.kt",
            "package app\n\nclass Helper {\n    fun ping() { }\n}\n",
        )
        .file(
            "src/app/Use.kt",
            "package app\n\nclass Use {\n    fun go() {\n        ping()\n    }\n}\n",
        )
        .write(
            "src/app/Dsl.kt",
            "package app\n\nfun helping(block: Helper.() -> Unit) { }\n",
        )
        .edge_after("method app::Use::go", "calls", "method app::Helper::ping")
}

/// Upstream #2174: a Kotlin infix call is a call, so an infix function
/// appearing in another file gives an unchanged file's `Column().id eq 3`
/// its callee, as on a fresh index.
pub fn kotlin_infix_function_appears_in_another_file() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Kotlin])
        .file(
            "src/app/Column.kt",
            "package app\n\nclass Column {\n    val id: Int = 1\n}\n",
        )
        .file(
            "src/app/Use.kt",
            "package app\n\nfun same(): Boolean = Column().id eq 3\n",
        )
        .write(
            "src/app/Column.kt",
            "package app\n\nclass Column {\n    val id: Int = 1\n\n    infix fun eq(other: Int): Boolean = true\n}\n",
        )
        .edge_after("function app::same", "calls", "method app::Column::eq")
}

/// Upstream #2174: a project's bitwise extension on a number type appearing
/// in another file never takes an unchanged file's `x.inc() and 0xff`, while
/// another type's infix `and` written alongside it does, as on a fresh index.
pub fn kotlin_number_bitwise_extension_appears_in_another_file() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Kotlin])
        .file(
            "src/app/Use.kt",
            "package app\n\nfun low(x: Int): Int = x.inc() and 0xff\n",
        )
        .write(
            "src/app/Bits.kt",
            "package app\n\ninfix fun Int.and(mask: Long): Long = 0\n\nclass Mask {\n    infix fun and(other: Int): Int = other\n}\n",
        )
        .no_edge_after("function app::low", "calls", "method Int::and")
        .edge_after("function app::low", "calls", "method app::Mask::and")
}

/// Upstream #2182: a `when` guard written into a class keeps the class and
/// its methods, so an unchanged file's call into it stays, as on a fresh
/// index.
pub fn kotlin_when_guard_keeps_the_class_around_it() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/app/Dialect.kt",
            "package app\n\nclass Dialect(private val mode: Int) {\n    fun sql(value: Any?): String = when (value) {\n        is String -> \"text\"\n        else -> \"other\"\n    }\n\n    fun after(): Int = 1\n}\n",
        )
        .file(
            "src/app/Use.kt",
            "package app\n\nfun use(d: Dialect): Int = d.after()\n",
        )
        .write(
            "src/app/Dialect.kt",
            "package app\n\nclass Dialect(private val mode: Int) {\n    fun sql(value: Any?): String = when (value) {\n        is String if mode == 2 -> \"text\"\n        else -> $$\"other $mode\"\n    }\n\n    fun after(): Int = 1\n}\n",
        )
        .edge_before("function app::use", "calls", "method app::Dialect::after")
        .edge_after("function app::use", "calls", "method app::Dialect::after")
}

/// Upstream #2206: a lambda runs on the receiver its function's last
/// parameter type names, so that function's declaration changing in another
/// file moves the bare call inside an unchanged lambda, as on a fresh index.
pub fn kotlin_lambda_receiver_changes_in_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/app/Scopes.kt",
            "package app\n\nclass Scope {\n    fun get(): Int = 1\n}\n\nclass Koin {\n    fun get(): Int = 2\n}\n",
        )
        .file(
            "src/app/Module.kt",
            "package app\n\nclass Module {\n    fun single(definition: Scope.() -> Int) { }\n}\n",
        )
        .file(
            "src/app/Perfs.kt",
            "package app\n\nfun perfModule(m: Module) {\n    m.single { get() }\n}\n",
        )
        .write(
            "src/app/Module.kt",
            "package app\n\nclass Module {\n    fun single(definition: Koin.() -> Int) { }\n}\n",
        )
        .edge_before("function app::perfModule", "calls", "method app::Scope::get")
        .edge_after("function app::perfModule", "calls", "method app::Koin::get")
}

/// Upstream #2196: the outside-import rule reads the project's packages for
/// Kotlin too: a Kotlin file appearing in `com.lib` makes an unchanged
/// Kotlin file's `Tools.make()` under `import com.lib.Tools` a project name,
/// as on a fresh index.
pub fn kotlin_import_package_appears_in_the_project() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Kotlin])
        .file(
            "src/app/Tools.kt",
            "package app\n\nobject Tools {\n    fun make() { }\n}\n",
        )
        .file(
            "src/app/Use.kt",
            "package app\n\nimport com.lib.Tools\n\nclass Use {\n    fun go() {\n        Tools.make()\n    }\n}\n",
        )
        .write(
            "src/com/lib/Other.kt",
            "package com.lib\n\nclass Other\n",
        )
        .edge_after("method app::Use::go", "calls", "method app::Tools::make")
}

/// Upstream #2232: a Gradle script's bare call reaches a member only of an
/// owner the project declares no class for, so a class of that name
/// appearing makes an unchanged script's call to an extension on it stop,
/// as on a fresh index.
pub fn kotlin_gradle_script_owner_becomes_a_project_class() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Kotlin])
        .file(
            "build-support/src/main/kotlin/SourceSets.kt",
            "fun NamedThings.createSourceSet(name: String) {\n}\n",
        )
        .file(
            "app/build.gradle.kts",
            "kotlin {\n    createSourceSet(\"nonJvm\")\n}\n",
        )
        .write(
            "build-support/src/main/kotlin/NamedThings.kt",
            "class NamedThings\n",
        )
        .edge_before(
            "file:app/build.gradle.kts",
            "calls",
            "method NamedThings::createSourceSet",
        )
        .no_edge_after(
            "file:app/build.gradle.kts",
            "calls",
            "method NamedThings::createSourceSet",
        )
}

/// Upstream #2172: the hierarchy walk takes Android's platform ancestry
/// between project types, so a project `Context` changing its base moves the
/// bare call of an unchanged `Screen : MyActivity`, `MyActivity :
/// AppCompatActivity`, as on a fresh index.
pub fn kotlin_platform_ancestry_chain_follows_a_project_head_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "src/app/Root.kt",
            "package app\n\nopen class Root {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Other.kt",
            "package app\n\nopen class Other {\n    fun assist() { }\n}\n",
        )
        .file("src/app/Context.kt", "package app\n\nopen class Context : Root()\n")
        .file(
            "src/app/MyActivity.kt",
            "package app\n\nopen class MyActivity : AppCompatActivity()\n",
        )
        .file(
            "src/app/Screen.kt",
            "package app\n\nclass Screen : MyActivity() {\n    fun go() {\n        assist()\n    }\n}\n",
        )
        .write("src/app/Context.kt", "package app\n\nopen class Context : Other()\n")
        .edge_before("method app::Screen::go", "calls", "method app::Root::assist")
        .edge_after("method app::Screen::go", "calls", "method app::Other::assist")
}

/// A Kotlin function body edit moves no head, receiver or lambda type: an
/// unchanged subclass's bare call stays where it was, as on a fresh index.
pub fn kotlin_base_body_edit_keeps_a_bare_call() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/app/Base.kt",
            "package app\n\nopen class Base {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Child.kt",
            "package app\n\nclass Child : Base() {\n    fun go() {\n        assist()\n    }\n}\n",
        )
        .write(
            "src/app/Base.kt",
            "package app\n\nopen class Base {\n    fun assist() {\n        println(1)\n    }\n}\n",
        )
        .edge_throughout("method app::Child::go", "calls", "method app::Base::assist")
}

/// Upstream #2172 in a batch: the base and the type between it and an
/// unchanged subclass both change in one sync; the subclass's bare call
/// reaches what the new middle type names, as on a fresh index.
pub fn kotlin_batch_rewrites_a_base_and_a_middle_type() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "src/app/Root.kt",
            "package app\n\nopen class Root {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Other.kt",
            "package app\n\nopen class Other {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Spare.kt",
            "package app\n\nopen class Spare {\n    fun assist() { }\n}\n",
        )
        .file(
            "src/app/Base.kt",
            "package app\n\nopen class Base : Root()\n",
        )
        .file("src/app/Mid.kt", "package app\n\nopen class Mid : Base()\n")
        .file(
            "src/app/Child.kt",
            "package app\n\nclass Child : Mid() {\n    fun go() {\n        assist()\n    }\n}\n",
        )
        .write(
            "src/app/Base.kt",
            "package app\n\nopen class Base : Other()\n",
        )
        .write(
            "src/app/Mid.kt",
            "package app\n\nopen class Mid : Spare()\n",
        )
        .edge_before("method app::Child::go", "calls", "method app::Root::assist")
        .edge_after(
            "method app::Child::go",
            "calls",
            "method app::Spare::assist",
        )
}

/// Upstream #2206 through a typealias: the alias changing the receiver it
/// names moves the bare call inside an unchanged lambda, as on a fresh index.
pub fn kotlin_lambda_receiver_changes_through_a_typealias() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/app/Scopes.kt",
            "package app\n\nclass Scope {\n    fun get(): Int = 1\n}\n\nclass Koin {\n    fun get(): Int = 2\n}\n",
        )
        .file(
            "src/app/Definition.kt",
            "package app\n\ntypealias Definition = Scope.() -> Int\n",
        )
        .file(
            "src/app/Module.kt",
            "package app\n\nclass Module {\n    fun single(definition: Definition) { }\n}\n",
        )
        .file(
            "src/app/Perfs.kt",
            "package app\n\nfun perfModule(m: Module) {\n    m.single { get() }\n}\n",
        )
        .write(
            "src/app/Definition.kt",
            "package app\n\ntypealias Definition = Koin.() -> Int\n",
        )
        .edge_before("function app::perfModule", "calls", "method app::Scope::get")
        .edge_after("function app::perfModule", "calls", "method app::Koin::get")
}

/// Upstream #2172 and #2232: a Gradle build script's walk starts from
/// `Project`, so a project class of that name changing its base moves which
/// outside type's extension an unchanged script's bare call takes, as on a
/// fresh index.
pub fn kotlin_gradle_script_follows_a_project_head_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "build-logic/src/main/kotlin/Ext.kt",
            "fun Base.foo() { }\n\nfun Other.foo() { }\n",
        )
        .file(
            "build-logic/src/main/kotlin/Project.kt",
            "abstract class Project : Base()\n",
        )
        .file("app/build.gradle.kts", "foo()\n")
        .write(
            "build-logic/src/main/kotlin/Project.kt",
            "abstract class Project : Other()\n",
        )
        .edge_before("file:app/build.gradle.kts", "calls", "method Base::foo")
        .edge_after("file:app/build.gradle.kts", "calls", "method Other::foo")
}

/// Upstream #912: Lombok's members are nodes of the class's file, so `@Data`
/// written onto a class gives an unchanged file's `user.getName()` its
/// target, as on a fresh index.
pub fn java_lombok_data_appears_on_a_class() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Java])
        .file(
            "model/User.java",
            "package model;\n\npublic class User {\n    private String name;\n}\n",
        )
        .file(
            "svc/UserService.java",
            "package svc;\n\nimport model.User;\n\nclass UserService {\n    String describe(User user) {\n        return user.getName();\n    }\n}\n",
        )
        .write(
            "model/User.java",
            "package model;\n\nimport lombok.Data;\n\n@Data\npublic class User {\n    private String name;\n}\n",
        )
        .edge_after(
            "method svc::UserService::describe",
            "calls",
            "method model::User::getName",
        )
}

/// Upstream #912: a hand-written accessor replaces the one Lombok would
/// generate, so an unchanged caller's edge moves to it, as on a fresh index.
pub fn java_lombok_hand_written_getter_replaces_the_generated_one() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "model/Account.java",
            "package model;\n\nimport lombok.Getter;\n\n@Getter\npublic class Account {\n    private int balance;\n}\n",
        )
        .file(
            "svc/Report.java",
            "package svc;\n\nimport model.Account;\n\nclass Report {\n    int total(Account account) {\n        return account.getBalance();\n    }\n}\n",
        )
        .write(
            "model/Account.java",
            "package model;\n\nimport lombok.Getter;\n\n@Getter\npublic class Account {\n    private int balance;\n\n    public int getBalance() { return balance < 0 ? 0 : balance; }\n}\n",
        )
        .edge_before("method svc::Report::total", "calls", "method model::Account::getBalance")
        .edge_after("method svc::Report::total", "calls", "method model::Account::getBalance")
}

/// Upstream #912: a field leaving a `@Data` class takes its accessors with
/// it, so an unchanged caller's edge goes, as on a fresh index (where the
/// accessor no longer exists).
pub fn java_lombok_field_removal_drops_its_accessor() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "model/User.java",
            "package model;\n\nimport lombok.Data;\n\n@Data\npublic class User {\n    private String name;\n    private int age;\n}\n",
        )
        .file(
            "svc/UserService.java",
            "package svc;\n\nimport model.User;\n\nclass UserService {\n    int age(User user) {\n        return user.getAge();\n    }\n}\n",
        )
        .write(
            "model/User.java",
            "package model;\n\nimport lombok.Data;\n\n@Data\npublic class User {\n    private String name;\n}\n",
        )
        .edge_before("method svc::UserService::age", "calls", "method model::User::getAge")
}

/// Upstream #2194: a Java enum constant's body declares methods, so an edit
/// to that body gains the call it adds, as on a fresh index.
pub fn java_enum_constant_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Util.java",
            "package app;\n\npublic class Util {\n    public static int twice(int x) { return 2 * x; }\n}\n",
        )
        .file(
            "src/app/Op.java",
            "package app;\n\npublic enum Op {\n    PLUS {\n        int apply(int a) { return a; }\n    };\n\n    abstract int apply(int a);\n}\n",
        )
        .write(
            "src/app/Op.java",
            "package app;\n\npublic enum Op {\n    PLUS {\n        int apply(int a) { return Util.twice(a); }\n    };\n\n    abstract int apply(int a);\n}\n",
        )
        .edge_after("method app::Op::PLUS::apply", "calls", "method app::Util::twice")
}

/// Upstream #2194: a Kotlin enum entry's body declares methods, so an edit to
/// that body gains the call it adds, as on a fresh index.
pub fn kotlin_enum_entry_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/app/Tools.kt",
            "package app\n\nfun size(): Int = 3\n",
        )
        .file(
            "src/app/Factory.kt",
            "package app\n\nenum class Factory {\n    NewBuffer {\n        override fun pipe(): Int = 1\n    };\n\n    abstract fun pipe(): Int\n}\n",
        )
        .write(
            "src/app/Factory.kt",
            "package app\n\nenum class Factory {\n    NewBuffer {\n        override fun pipe(): Int = size()\n    };\n\n    abstract fun pipe(): Int\n}\n",
        )
        .edge_after("method app::Factory::NewBuffer::pipe", "calls", "function app::size")
}

/// Upstream #2126: a bare call reaches a method of a supertype the classes
/// around it name in their heads, so a base changed in another file moves the
/// call of an unchanged subclass, as on a fresh index.
pub fn java_bare_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/Root.java",
            "package app;\n\npublic class Root {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Other.java",
            "package app;\n\npublic class Other {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base extends Root {\n}\n",
        )
        .file(
            "src/app/Child.java",
            "package app;\n\npublic class Child extends Base {\n    void go() {\n        assist();\n    }\n}\n",
        )
        .write(
            "src/app/Base.java",
            "package app;\n\npublic class Base extends Other {\n}\n",
        )
        .edge_before("method app::Child::go", "calls", "method app::Root::assist")
        .edge_after("method app::Child::go", "calls", "method app::Other::assist")
}

/// Upstream #2126: the scope walk reads every declaration of a type's name,
/// so a head changed in another package's same-named class moves the bare
/// call of an unchanged class of that name, as on a fresh index.
pub fn java_bare_call_follows_a_same_named_head_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/lib/Root.java",
            "package lib;\n\npublic class Root {\n    public void assist() { }\n}\n",
        )
        .file(
            "src/lib/Other.java",
            "package lib;\n\npublic class Other {\n    public void assist() { }\n}\n",
        )
        .file(
            "src/lib/Node.java",
            "package lib;\n\npublic class Node extends Root {\n}\n",
        )
        .file(
            "src/app/Node.java",
            "package app;\n\npublic class Node {\n    void go() {\n        assist();\n    }\n}\n",
        )
        .write(
            "src/lib/Node.java",
            "package lib;\n\npublic class Node extends Other {\n}\n",
        )
        .edge_before("method app::Node::go", "calls", "method lib::Root::assist")
        .edge_after("method app::Node::go", "calls", "method lib::Other::assist")
}

/// #2126 with an anonymous class (KEEP-RUST): it derives from what it
/// instantiates, so that interface changing its base in another file moves
/// a bare call in the unchanged anonymous class, as on a fresh index.
pub fn java_anonymous_class_follows_an_interface_head_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/lib/Left.java",
            "package lib;\n\npublic interface Left {\n    default void step() { }\n}\n",
        )
        .file(
            "src/lib/Right.java",
            "package lib;\n\npublic interface Right {\n    default void step() { }\n}\n",
        )
        .file(
            "src/lib/Walker.java",
            "package lib;\n\npublic interface Walker extends Left {\n}\n",
        )
        .file(
            "src/app/Tree.java",
            "package app;\n\nimport lib.Walker;\n\npublic class Tree {\n    Object walk() {\n        return new Walker() {\n            void next() {\n                step();\n            }\n        };\n    }\n}\n",
        )
        .write(
            "src/lib/Walker.java",
            "package lib;\n\npublic interface Walker extends Right {\n}\n",
        )
        .edge_before(
            "method app::Tree::walk::<Walker$anon@7>::next",
            "calls",
            "method lib::Left::step",
        )
        .edge_after(
            "method app::Tree::walk::<Walker$anon@7>::next",
            "calls",
            "method lib::Right::step",
        )
}

/// Upstream #2126 reads a head's six lines as written, a `? extends X` in
/// the class body's first lines included: editing that body text moves the
/// bare call of an unchanged subclass, though no declaration's `extends`
/// changed, as on a fresh index.
pub fn java_head_window_text_moves_a_subclass_bare_call() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Root.java",
            "package app;\n\npublic class Root {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Other.java",
            "package app;\n\npublic class Other {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base {\n    java.util.List<? extends Root> items;\n    void touch() {\n    }\n}\n",
        )
        .file(
            "src/app/Child.java",
            "package app;\n\npublic class Child extends Base {\n    void go() {\n        assist();\n    }\n}\n",
        )
        .write(
            "src/app/Base.java",
            "package app;\n\npublic class Base {\n    java.util.List<? extends Other> items;\n    void touch() {\n    }\n}\n",
        )
        .edge_before("method app::Child::go", "calls", "method app::Root::assist")
        .edge_after("method app::Child::go", "calls", "method app::Other::assist")
}

/// Upstream #2126 in a batch: the base and the type between it and an
/// unchanged subclass both change in one sync; the subclass's bare call
/// reaches what the new middle type names, as on a fresh index.
pub fn java_batch_rewrites_a_base_and_a_middle_type() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/Root.java",
            "package app;\n\npublic class Root {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Other.java",
            "package app;\n\npublic class Other {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Spare.java",
            "package app;\n\npublic class Spare {\n    protected void assist() { }\n}\n",
        )
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base extends Root {\n}\n",
        )
        .file(
            "src/app/Mid.java",
            "package app;\n\npublic class Mid extends Base {\n}\n",
        )
        .file(
            "src/app/Child.java",
            "package app;\n\npublic class Child extends Mid {\n    void go() {\n        assist();\n    }\n}\n",
        )
        .write(
            "src/app/Base.java",
            "package app;\n\npublic class Base extends Other {\n}\n",
        )
        .write(
            "src/app/Mid.java",
            "package app;\n\npublic class Mid extends Spare {\n}\n",
        )
        .edge_before("method app::Child::go", "calls", "method app::Root::assist")
        .edge_after("method app::Child::go", "calls", "method app::Spare::assist")
}

/// A file with two same-named interfaces, each extending thirty names: the
/// scope walk of a bare call reads them in file order and stops at forty
/// names, so whichever comes first is the one whose names it reaches.
fn heads_at_the_limit(b_first: bool) -> String {
    let names = |prefix: char| {
        (1..=30)
            .map(|n| format!("{prefix}{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let a = format!(
        "class Front {{\n    interface Node extends {} {{ }}\n}}\n",
        names('A')
    );
    let b = format!(
        "class Back {{\n    interface Node extends {} {{ }}\n}}\n",
        names('B')
    );
    let (first, second) = if b_first { (b, a) } else { (a, b) };
    format!("package app;\n\n{first}\n{second}")
}

static HEADS_B_FIRST: LazyLock<String> = LazyLock::new(|| heads_at_the_limit(true));
static HEADS_A_FIRST: LazyLock<String> = LazyLock::new(|| heads_at_the_limit(false));

/// Upstream #2126's walk stops at forty names: two same-named interfaces in
/// one file trading places, nothing else changing, decide which of their
/// supertypes an unchanged caller's bare call can reach, as on a fresh
/// index.
pub fn java_same_named_heads_trade_places_at_the_walk_limit() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/B20.java",
            "package app;\n\npublic interface B20 {\n    default void reachB() { }\n}\n",
        )
        .file(
            "src/app/Caller.java",
            "package app;\n\npublic class Caller implements Node {\n    void go() {\n        reachB();\n    }\n}\n",
        )
        .file("src/app/Heads.java", HEADS_B_FIRST.as_str())
        .write("src/app/Heads.java", HEADS_A_FIRST.as_str())
        .edge_before("method app::Caller::go", "calls", "method app::B20::reachB")
        .no_edge_after("method app::Caller::go", "calls", "method app::B20::reachB")
}

/// Upstream #2236: a nested type is in reach from a subclass of its owner,
/// the heads read from source, so a base changed in another file moves the
/// type reference of an unchanged subclass, as on a fresh index.
pub fn java_nested_type_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/lib/Widget.java",
            "package lib;\n\npublic class Widget {\n    public static class Part { }\n}\n",
        )
        .file(
            "src/other/Holder.java",
            "package other;\n\npublic class Holder {\n    public static class Part { }\n}\n",
        )
        .file(
            "src/app/Mid.java",
            "package app;\n\npublic class Mid extends lib.Widget {\n}\n",
        )
        .file(
            "src/app/Sub.java",
            "package app;\n\npublic class Sub extends Mid {\n    Part part;\n}\n",
        )
        .write(
            "src/app/Mid.java",
            "package app;\n\npublic class Mid extends other.Holder {\n}\n",
        )
        .edge_before(
            "field app::Sub::part",
            "references",
            "class lib::Widget::Part",
        )
        .edge_after(
            "field app::Sub::part",
            "references",
            "class other::Holder::Part",
        )
}

/// Upstream #2236: a nested type of an unrelated class is no candidate, so a
/// namesake appearing in another package leaves the subclass's reference
/// where it was.
pub fn java_nested_type_ignores_an_added_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Java])
        .file(
            "src/lib/Widget.java",
            "package lib;\n\npublic class Widget {\n    public static class Part { }\n}\n",
        )
        .file(
            "src/app/Sub.java",
            "package app;\n\npublic class Sub extends lib.Widget {\n    Part part;\n}\n",
        )
        .write(
            "src/app/Holder.java",
            "package app;\n\npublic class Holder {\n    public static class Part { }\n}\n",
        )
        .edge_throughout(
            "field app::Sub::part",
            "references",
            "class lib::Widget::Part",
        )
}

/// Upstream #2446: a base class defined in a variable declaration (`class
/// Base { … } base_instance;`) is a node, so a heritage edit in another file
/// moves the `extends` edge from one such class to another, as on a fresh
/// index.
pub fn cpp_declared_base_class_heritage_edit() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cpp])
        .file(
            "src/bases.hpp",
            "namespace app {\nclass Base { public: virtual int run() { return 1; } } base_instance;\nclass Other { public: virtual int run() { return 2; } } other_instance;\n}  // namespace app\n",
        )
        .file(
            "src/worker.cc",
            "#include \"bases.hpp\"\n\nnamespace app {\nclass Worker : public Base {\n public:\n  int run() override { return 3; }\n};\n}  // namespace app\n",
        )
        .write(
            "src/worker.cc",
            "#include \"bases.hpp\"\n\nnamespace app {\nclass Worker : public Other {\n public:\n  int run() override { return 3; }\n};\n}  // namespace app\n",
        )
        .edge_before("class app::Worker", "extends", "class app::Base")
        .edge_after("class app::Worker", "extends", "class app::Other")
        .no_edge_after("class app::Worker", "extends", "class app::Base")
}

/// Upstream #2378's identifier grammar: `export default $fmt;` that comes
/// to name `$other` moves an unchanged default importer, as on a fresh
/// index, rather than both falling back to the first exported function.
pub fn typescript_dollar_default_export_names_another_declaration() -> Case {
    Case::new(Change::ReexportEdit, &[Language::TypeScript])
        .file(
            "src/fmt.ts",
            "export function helper(value: string) {\n  return value;\n}\n\nconst $fmt = (value: string) => value + '!';\nconst $other = (value: string) => value + '?';\n\nexport default $fmt;\n",
        )
        .file(
            "src/use.ts",
            "import fmt from './fmt';\n\nexport function show(value: string) {\n  return fmt(value);\n}\n",
        )
        .write(
            "src/fmt.ts",
            "export function helper(value: string) {\n  return value;\n}\n\nconst $fmt = (value: string) => value + '!';\nconst $other = (value: string) => value + '?';\n\nexport default $other;\n",
        )
        .edge_before("function show", "calls", "function $fmt")
        .edge_after("function show", "calls", "function $other")
        .no_edge_after("function show", "calls", "function helper")
}

/// Upstream #2378: a `$`-named default import binds its module's default
/// export once the module appears, ahead of another file's namesake.
pub fn typescript_dollar_import_reaches_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file(
            "src/a/other.ts",
            "export function $fmt(value: string) {\n  return value + '!';\n}\n",
        )
        .file(
            "src/use.ts",
            "import $fmt from './fmt';\n\nexport function show(value: string) {\n  return $fmt(value);\n}\n",
        )
        .write(
            "src/fmt.ts",
            "export default function $fmt(value: string) {\n  return value;\n}\n",
        )
        .edge_after("function show", "calls", "function $fmt @src/fmt.ts")
        .no_edge_after("function show", "calls", "function $fmt @src/a/other.ts")
}

/// Upstream #2392 (3): a Svelte import links the component file once it appears.
pub fn svelte_import_of_an_added_component() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Svelte])
        .file(
            "src/App.svelte",
            "<script>\n  import Child from './Child.svelte';\n</script>\n<Child />\n",
        )
        .write("src/Child.svelte", "<p>child</p>\n")
        .edge_after("file:src/App.svelte", "imports", "file:src/Child.svelte")
}

/// Upstream #2392 (3): a Vue import links the component file once it appears.
pub fn vue_import_of_an_added_component() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Vue])
        .file(
            "src/App.vue",
            "<script setup>\nimport Foo from './Foo.vue';\n</script>\n<template><Foo /></template>\n",
        )
        .write("src/Foo.vue", "<template><p>foo</p></template>\n")
        .edge_after("file:src/App.vue", "imports", "file:src/Foo.vue")
}

pub fn go_call_into_an_added_file_of_the_same_package() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file(
            "main.go",
            "package main\n\nfunc main() {\n\tprintln(Total(2, 3))\n}\n",
        )
        .write(
            "total.go",
            "package main\n\nfunc Total(a, b int) int {\n\treturn a + b\n}\n",
        )
        .edge_after("function main", "calls", "function Total @total.go")
}

/// A file whose parse collapsed records the error on its row (the status
/// report's `filesWithParseErrors`); once fixed, the synced row and symbols
/// equal a fresh index's, error cleared.
pub fn typescript_collapsed_parse_is_fixed() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript])
        .file("src/util.ts", "@@@ ### ((( === }}} <<<\n")
        .file(
            "src/main.ts",
            "import { helper } from './util';\n\nexport function main(): number {\n  return helper();\n}\n",
        )
        .write(
            "src/util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        )
        .edge_after("function main", "calls", "function helper @src/util.ts")
}

/// G2: a Go composite literal instantiates its struct, which appears later.
pub fn go_composite_literal_of_an_added_struct() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file(
            "main.go",
            "package main\n\nfunc main() {\n\tw := Widget{N: 1}\n\tprintln(w.N)\n}\n",
        )
        .write(
            "widget.go",
            "package main\n\ntype Widget struct {\n\tN int\n}\n",
        )
        .edge_after("function main", "instantiates", "struct Widget @widget.go")
}

/// G3: a Go parameter type is a reference to a struct that appears later.
pub fn go_parameter_type_of_an_added_struct() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file(
            "main.go",
            "package main\n\nfunc Ship(o Order) int {\n\treturn 1\n}\n",
        )
        .write(
            "order.go",
            "package main\n\ntype Order struct {\n\tID int\n}\n",
        )
        .edge_after("function Ship", "references", "struct Order @order.go")
}

/// G3: a Java parameter type is a reference to a class that appears later in
/// the same package.
pub fn java_parameter_type_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Java])
        .file(
            "src/zoo/Keeper.java",
            "package zoo;\n\npublic class Keeper {\n    public int feed(Animal animal) {\n        return 1;\n    }\n}\n",
        )
        .write(
            "src/zoo/Animal.java",
            "package zoo;\n\npublic class Animal {\n}\n",
        )
        .edge_after("method zoo::Keeper::feed", "references", "class zoo::Animal")
}

/// G3: a C# parameter type is a reference to a class that appears later.
pub fn csharp_parameter_type_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file(
            "src/Keeper.cs",
            "namespace Zoo\n{\n    public class Keeper\n    {\n        public int Feed(Animal animal) { return 1; }\n    }\n}\n",
        )
        .write(
            "src/Animal.cs",
            "namespace Zoo\n{\n    public class Animal { }\n}\n",
        )
        .edge_after("method Zoo::Keeper::Feed", "references", "class Zoo::Animal")
}

/// W2-03d: a Kotlin property's declared type is a reference from the property
/// to a class that appears later in the same package.
pub fn kotlin_property_type_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Kotlin])
        .file(
            "src/zoo/Keeper.kt",
            "package zoo\n\nclass Keeper {\n    private val animal: Animal? = null\n}\n",
        )
        .write("src/zoo/Animal.kt", "package zoo\n\nclass Animal\n")
        .edge_after(
            "field zoo::Keeper::animal",
            "references",
            "class zoo::Animal",
        )
}

/// W2-03d: a PHP property's declared type is a reference from its field to a
/// class that appears later in the same namespace.
pub fn php_property_type_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Php])
        .file(
            "src/Keeper.php",
            "<?php\nnamespace Zoo;\n\nclass Keeper\n{\n    private ?Animal $animal = null;\n}\n",
        )
        .write(
            "src/Animal.php",
            "<?php\nnamespace Zoo;\n\nclass Animal\n{\n}\n",
        )
        .edge_after(
            "field Zoo::Keeper::animal",
            "references",
            "class Zoo::Animal",
        )
}

/// Upstream #2392 notes that C includes already converge.
pub fn c_include_of_an_added_header() -> Case {
    Case::new(Change::AddImportTarget, &[Language::C])
        .file(
            "main.c",
            "#include \"util.h\"\n\nint main(void) {\n    return helper(1);\n}\n",
        )
        .file(
            "util.c",
            "int helper(int value) {\n    return value + 1;\n}\n",
        )
        .write("util.h", "int helper(int value);\n")
        .edge_after("file:main.c", "imports", "file:util.h")
        .edge_after("function main", "calls", "function helper @util.c")
}

/// Upstream #2403: a Liquid `render` links the snippet a sync adds later.
pub fn liquid_render_of_an_added_snippet() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Liquid])
        .file(
            "templates/product.liquid",
            "<div class=\"product\">\n  {% render 'price' %}\n</div>\n",
        )
        .write(
            "snippets/price.liquid",
            "<span class=\"price\">{{ product.price }}</span>\n",
        )
        .edge_after(
            "file:templates/product.liquid",
            "references",
            "file:snippets/price.liquid",
        )
}

/// Upstream #2403: a JSON template's section `type` links the section a sync
/// adds later, inside the template's theme.
pub fn liquid_json_template_section_of_an_added_section() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Liquid])
        .file("layout/theme.liquid", "{{ content_for_layout }}\n")
        .file(
            "templates/404.json",
            "{\"sections\":{\"main\":{\"type\":\"404\"}},\"order\":[\"main\"]}",
        )
        .write("sections/404.liquid", "<h1>404</h1>\n")
        .edge_after(
            "file:templates/404.json",
            "references",
            "file:sections/404.liquid",
        )
}

pub fn razor_reference_to_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Razor, Language::CSharp])
        .file(
            "Pages/Index.cshtml",
            "@page\n@{\n    var total = Calculator.Add(1, 2);\n}\n<p>@total</p>\n",
        )
        .write(
            "Pages/Calculator.cs",
            "public static class Calculator\n{\n    public static int Add(int a, int b) { return a + b; }\n}\n",
        )
        .edge_after(
            "component Pages/Index.cshtml::Index",
            "references",
            "class Calculator",
        )
}

pub fn r_source_of_an_added_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::R])
        .file(
            "analysis.R",
            "source(\"utils.R\")\n\nsummarise <- function(values) {\n  scale_values(values)\n}\n",
        )
        .write(
            "utils.R",
            "scale_values <- function(values) {\n  values / max(values)\n}\n",
        )
        .edge_after("file:analysis.R", "imports", "file:utils.R")
        .edge_after("function summarise", "calls", "function scale_values")
}

const ERLANG_MAIN: &str = "-module(main).\n-export([run/1]).\n\nrun(X) -> util:double(X).\n";
const ERLANG_UTIL: &str = "-module(util).\n-export([double/1]).\n\ndouble(X) -> X * 2.\n";
const USER_MAPPER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.UserMapper\">\n  <select id=\"findAll\" resultType=\"app.User\">\n    SELECT <include refid=\"app.CommonMapper.userColumns\"/> FROM users\n  </select>\n</mapper>\n";
const COMMON_MAPPER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.CommonMapper\">\n  <sql id=\"userColumns\">id, name, email</sql>\n</mapper>\n";

/// Found by W0-04: the call is parked as `util::double/1`, which no added
/// node is named; it is retried by its tail (W5-05a) and by its whole
/// qualified name (W5-05).
pub fn erlang_remote_call_into_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Erlang])
        .file("src/main.erl", ERLANG_MAIN)
        .write("src/util.erl", ERLANG_UTIL)
        .edge_after("function main::run/1", "calls", "function util::double/1")
}

/// Found by W0-04: the include is parked as `app.CommonMapper::userColumns`,
/// which no added node is named; it is retried by its tail (W5-05a) and by
/// its whole qualified name (W5-05).
pub fn mybatis_include_of_an_added_fragment() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Xml])
        .file("mapper/UserMapper.xml", USER_MAPPER)
        .write("mapper/CommonMapper.xml", COMMON_MAPPER)
        .edge_after(
            "method app.UserMapper::findAll",
            "references",
            "method app.CommonMapper::userColumns",
        )
}

/// More unresolved `….double` and `….userColumns` calls than one tail
/// retries in a sync: CFML calls on receivers nothing is named after.
static UNTYPED_DOUBLE_CALLS: LazyLock<String> = LazyLock::new(|| untyped_calls("double"));
static UNTYPED_USER_COLUMNS_CALLS: LazyLock<String> =
    LazyLock::new(|| untyped_calls("userColumns"));

fn untyped_calls(method: &str) -> String {
    let mut text = String::from("component {\n  function noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("    n{index}.{method}({index});\n"));
    }
    text.push_str("    return 0;\n  }\n}\n");
    text
}

/// W5-05: the parked remote call is found by its whole qualified name when
/// more parked calls share its tail than one tail retries.
pub fn erlang_remote_call_into_an_added_module_past_the_tail_ceiling() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Erlang, Language::Cfml])
        .file("src/main.erl", ERLANG_MAIN)
        .file("noise/Noise.cfc", UNTYPED_DOUBLE_CALLS.as_str())
        .write("src/util.erl", ERLANG_UTIL)
        .edge_after("function main::run/1", "calls", "function util::double/1")
}

/// W5-05, found by its plan gate: the module renamed in place keeps every
/// node id (an id holds no qualified name), and the parked call is found by
/// the qualified name the rename gave the function, past the tail ceiling.
pub fn erlang_remote_call_follows_a_module_renamed_in_place_past_the_tail_ceiling() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Erlang, Language::Cfml])
        .file("src/main.erl", ERLANG_MAIN)
        .file(
            "src/util.erl",
            "-module(utils).\n-export([double/1]).\n\ndouble(X) -> X * 2.\n",
        )
        .file("noise/Noise.cfc", UNTYPED_DOUBLE_CALLS.as_str())
        .write("src/util.erl", ERLANG_UTIL)
        .edge_after("function main::run/1", "calls", "function util::double/1")
}

/// W5-05, found by its plan gate: a mapper namespace renamed in place, past
/// the tail ceiling.
pub fn mybatis_include_follows_a_namespace_renamed_in_place_past_the_tail_ceiling() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Xml, Language::Cfml])
        .file("mapper/UserMapper.xml", USER_MAPPER)
        .file(
            "mapper/CommonMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.Other\">\n  <sql id=\"userColumns\">id, name, email</sql>\n</mapper>\n",
        )
        .file("noise/Noise.cfc", UNTYPED_USER_COLUMNS_CALLS.as_str())
        .write("mapper/CommonMapper.xml", COMMON_MAPPER)
        .edge_after(
            "method app.UserMapper::findAll",
            "references",
            "method app.CommonMapper::userColumns",
        )
}

/// W5-05: the module the remote call reaches is deleted.
pub fn erlang_remote_call_into_a_deleted_module() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Erlang])
        .file("src/main.erl", ERLANG_MAIN)
        .file("src/util.erl", ERLANG_UTIL)
        .edge_before("function main::run/1", "calls", "function util::double/1")
        .remove("src/util.erl")
}

/// W5-05: the mapper the include reaches is deleted.
pub fn mybatis_include_of_a_deleted_fragment() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Xml])
        .file("mapper/UserMapper.xml", USER_MAPPER)
        .file("mapper/CommonMapper.xml", COMMON_MAPPER)
        .edge_before(
            "method app.UserMapper::findAll",
            "references",
            "method app.CommonMapper::userColumns",
        )
        .remove("mapper/CommonMapper.xml")
}

// ----- a body changes, every declaration keeps its identity ----------------

/// Upstream #2288: same-named methods of one file keep their typed callers
/// across a body-only edit of that file.
pub fn typescript_pr2288_same_named_methods_keep_typed_callers() -> Case {
    const RUNNERS: &str = "export class Alpha {\n  run(): number {\n    return 1;\n  }\n}\n\nexport class Beta {\n  run(): number {\n    return 2;\n  }\n}\n";
    const RUNNERS_EDITED: &str = "export class Alpha {\n  run(): number {\n    return 10;\n  }\n}\n\nexport class Beta {\n  run(): number {\n    return 20;\n  }\n}\n";
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/runners.ts", RUNNERS)
        .file(
            "src/main.ts",
            "import { Alpha, Beta } from './runners';\n\nexport function runBoth(alpha: Alpha, beta: Beta): number {\n  return alpha.run() + beta.run();\n}\n",
        )
        .write("src/runners.ts", RUNNERS_EDITED)
        .edge_throughout("function runBoth", "calls", "method Alpha::run")
        .edge_throughout("function runBoth", "calls", "method Beta::run")
}

/// Upstream #2288's Java half: overloads keep their cross-file callers across
/// a body-only edit.
pub fn java_overloads_keep_their_callers() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Picker.java",
            "package app;\n\npublic class Picker {\n    public int pick(int value) {\n        return value;\n    }\n\n    public String pick(String value) {\n        return value;\n    }\n}\n",
        )
        .file(
            "src/app/Main.java",
            "package app;\n\npublic class Main {\n    public static void main(String[] args) {\n        Picker picker = new Picker();\n        picker.pick(1);\n        picker.pick(\"one\");\n    }\n}\n",
        )
        .write(
            "src/app/Picker.java",
            "package app;\n\npublic class Picker {\n    public int pick(int value) {\n        return value + 1;\n    }\n\n    public String pick(String value) {\n        return value.trim();\n    }\n}\n",
        )
        .edge_throughout(
            "method app::Main::main",
            "calls",
            "method app::Picker::pick @src/app/Picker.java:4",
        )
}

pub fn jsx_component_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Jsx])
        .file(
            "src/Card.jsx",
            "export function Card(props) {\n  return <div>{props.title}</div>;\n}\n",
        )
        .file(
            "src/Page.jsx",
            "import { Card } from './Card';\n\nexport function Page() {\n  return <Card title=\"hello\" />;\n}\n",
        )
        .write(
            "src/Card.jsx",
            "export function Card(props) {\n  return <section>{props.title}</section>;\n}\n",
        )
        .edge_throughout("function Page", "references", "function Card")
}

pub fn swift_method_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift])
        .file(
            "Sources/Counter.swift",
            "class Counter {\n    var value = 0\n\n    func increment() -> Int {\n        value += 1\n        return value\n    }\n}\n",
        )
        .file(
            "Sources/App.swift",
            "func tick() -> Int {\n    let counter = Counter()\n    return counter.increment()\n}\n",
        )
        .write(
            "Sources/Counter.swift",
            "class Counter {\n    var value = 0\n\n    func increment() -> Int {\n        value += 2\n        return value\n    }\n}\n",
        )
        .edge_throughout("function tick", "calls", "method Counter::increment")
        .edge_throughout("function tick", "instantiates", "class Counter")
}

/// Upstream #2107 (W4-SW-01): an extension is not the type, so a reference
/// beside it reaches the declaration elsewhere; turned into a class of the
/// same name it keeps its node id, yet it is now the nearer declaration the
/// unchanged reference reaches.
pub fn swift_extension_becomes_a_declaration() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "App/Models/Account.swift",
            "struct Account {\n    let id: String\n}\n",
        )
        .file(
            "App/Other/Account+Display.swift",
            "extension Account {\n    var display: String { id }\n}\n",
        )
        .file(
            "App/Other/AccountRow.swift",
            "struct AccountRow {\n    let account: Account\n}\n",
        )
        .write(
            "App/Other/Account+Display.swift",
            "class Account {\n    var display: String { \"x\" }\n}\n",
        )
        .edge_before(
            "struct AccountRow",
            "references",
            "struct Account @App/Models/Account.swift",
        )
        .edge_after(
            "struct AccountRow",
            "references",
            "class Account @App/Other/Account+Display.swift",
        )
        .no_edge_after(
            "struct AccountRow",
            "references",
            "struct Account @App/Models/Account.swift",
        )
}

/// Upstream #2107 (W4-SW-01): a subclass names a nested type of its
/// superclass's superclass bare; once the middle class's head names another
/// superclass in its own file, the unchanged subclass's reference is out of
/// scope.
pub fn swift_subclass_scope_follows_a_superclass_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "Sources/Core/Channel.swift",
            "class BaseChannel {\n    enum ReadResult { case some, none }\n}\nclass OtherChannel {\n}\n",
        )
        .file(
            "Sources/Core/Middle.swift",
            "class MiddleChannel: BaseChannel {\n}\n",
        )
        .file(
            "Sources/Core/Stream.swift",
            "final class StreamChannel: MiddleChannel {\n    var last: ReadResult?\n}\n",
        )
        .write(
            "Sources/Core/Middle.swift",
            "class MiddleChannel: OtherChannel {\n}\n",
        )
        .edge_before(
            "class StreamChannel",
            "references",
            "enum BaseChannel::ReadResult",
        )
        .no_edge_after(
            "class StreamChannel",
            "references",
            "enum BaseChannel::ReadResult",
        )
}

/// Upstream #2107 (W4-SW-01): a conformance to a type the project only
/// extends is read from the conformer's head, so an unchanged caller's
/// typed call follows the head to the other extension's method.
pub fn swift_conformer_head_moves_an_extension_method_call() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "App/Card.swift",
            "extension View {\n    func cardStyle() -> Int { 1 }\n}\n",
        )
        .file(
            "App/ShapeCard.swift",
            "extension Shape {\n    func cardStyle() -> Int { 2 }\n}\n",
        )
        .file("App/Home.swift", "struct HomeView: View {\n}\n")
        .file(
            "App/Main.swift",
            "func makeHome() -> Int {\n    return HomeView().cardStyle()\n}\n",
        )
        .write("App/Home.swift", "struct HomeView: Shape {\n}\n")
        .edge_before("function makeHome", "calls", "method View::cardStyle")
        .edge_after("function makeHome", "calls", "method Shape::cardStyle")
        .no_edge_after("function makeHome", "calls", "method View::cardStyle")
}

/// Upstream #2107 (W4-SW-01): a conformance to a type the project only
/// extends is no edge, until a declaration of the type appears.
pub fn swift_declaration_of_an_extended_type_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Swift])
        .file(
            "App/Card.swift",
            "extension View {\n    func cardStyle() -> Int { 1 }\n}\n",
        )
        .file("App/Home.swift", "struct HomeView: View {\n}\n")
        .write("App/View.swift", "protocol View {\n}\n")
        .no_edge_after("struct HomeView", "extends", "class View @App/Card.swift")
        .edge_after(
            "struct HomeView",
            "implements",
            "interface View @App/View.swift",
        )
}

/// Upstream #2107 (W4-SW-01): an extension appearing beside an unchanged
/// reference to its type is not the type; the reference keeps the
/// declaration.
pub fn swift_extension_beside_a_reference_keeps_the_declaration() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Swift])
        .file(
            "App/Models/Account.swift",
            "struct Account {\n    let id: String\n}\n",
        )
        .file(
            "App/Rows/AccountRow.swift",
            "struct AccountRow {\n    let account: Account\n}\n",
        )
        .write(
            "App/Rows/Account+Display.swift",
            "extension Account {\n    var display: String { id }\n}\n",
        )
        .edge_throughout(
            "struct AccountRow",
            "references",
            "struct Account @App/Models/Account.swift",
        )
}

/// Upstream #2118 (W4-SW-02): a type-path call's member is owned through
/// the path its extension writes; an edit to that path in another file keeps
/// every node id, yet the unchanged call no longer names the type.
pub fn swift_type_path_call_follows_an_extension_path_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "Sources/App/API.swift",
            "enum API {\n    enum Ctl {}\n}\nenum Other {\n    enum Ctl {}\n}\nextension API.Ctl {\n    enum GetRoute {\n        static func query() -> Int { 1 }\n    }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.Ctl.GetRoute.query() }\n}\n",
        )
        .write(
            "Sources/App/API.swift",
            "enum API {\n    enum Ctl {}\n}\nenum Other {\n    enum Ctl {}\n}\nextension Other.Ctl {\n    enum GetRoute {\n        static func query() -> Int { 1 }\n    }\n}\n",
        )
        .edge_before("method Health::one", "calls", "method Ctl::GetRoute::query")
        .no_edge_after("method Health::one", "calls", "method Ctl::GetRoute::query")
}

/// Upstream #2118 (W4-SW-02): an unresolved type-path call waits for the
/// path an extension in a third file writes to name its type; no node id
/// moves, and the call's own qualifier binds a file that does not change.
pub fn swift_type_path_call_reaches_an_extension_that_now_writes_its_path() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "Sources/App/API.swift",
            "enum API {\n    enum Ctl {}\n}\nenum Other {\n    enum Ctl {}\n}\n",
        )
        .file(
            "Sources/App/Routes.swift",
            "extension Other.Ctl {\n    enum GetRoute {\n        static func query() -> Int { 1 }\n    }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.Ctl.GetRoute.query() }\n}\n",
        )
        .write(
            "Sources/App/Routes.swift",
            "extension API.Ctl {\n    enum GetRoute {\n        static func query() -> Int { 1 }\n    }\n}\n",
        )
        .edge_after("method Health::one", "calls", "method Ctl::GetRoute::query")
}

/// More unresolved `….query` calls than one tail retries in a sync (the
/// ceiling counts every language's): CFML calls on receivers no `query` is
/// named after, cheap to resolve.
static UNTYPED_QUERY_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("component {\n  function noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("    n{index}.query({index});\n"));
    }
    text.push_str("    return 0;\n  }\n}\n");
    text
});

/// Upstream #2118 (W4-SW-02): a member a standalone extension gains
/// settles the parked type-path call of its name in a third file, however
/// many parked calls share its tail.
pub fn swift_type_path_call_reaches_a_member_an_extension_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift, Language::Cfml])
        .file("Sources/App/API.swift", "enum API {\n    enum GetRoute {}\n}\n")
        .file(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.GetRoute.query() }\n}\n",
        )
        .file("noise/Noise.cfc", UNTYPED_QUERY_CALLS.as_str())
        .write(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n    static func query() -> Int { 2 }\n}\n",
        )
        .edge_after("method Health::one", "calls", "method GetRoute::query")
}

/// Upstream #2118 (W4-SW-02): a standalone extension already declaring a
/// `query` under another owner gains the path's own, and later loses it: the
/// parked type-path call in a third file follows both, however many parked
/// calls share the tail.
pub fn swift_type_path_call_follows_a_second_namesake_an_extension_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift, Language::Cfml])
        .file("Sources/App/API.swift", "enum API {\n    enum GetRoute {}\n}\n")
        .file(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n}\nenum Elsewhere {\n    static func query() -> Int { 0 }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.GetRoute.query() }\n}\n",
        )
        .file("noise/Noise.cfc", UNTYPED_QUERY_CALLS.as_str())
        .write(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n    static func query() -> Int { 2 }\n}\nenum Elsewhere {\n    static func query() -> Int { 0 }\n}\n",
        )
        .edge_after("method Health::one", "calls", "method GetRoute::query")
        .no_edge_after("method Health::one", "calls", "method Elsewhere::query")
}

/// Upstream #2118 (W4-SW-02): the reverse: the path's own `query` leaves the
/// extension, the other owner's stays.
pub fn swift_type_path_call_drops_a_namesake_an_extension_loses() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift, Language::Cfml])
        .file("Sources/App/API.swift", "enum API {\n    enum GetRoute {}\n}\n")
        .file(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n    static func query() -> Int { 2 }\n}\nenum Elsewhere {\n    static func query() -> Int { 0 }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.GetRoute.query() }\n}\n",
        )
        .file("noise/Noise.cfc", UNTYPED_QUERY_CALLS.as_str())
        .write(
            "Sources/App/Routes.swift",
            "extension API.GetRoute {\n    static func list() -> Int { 1 }\n}\nenum Elsewhere {\n    static func query() -> Int { 0 }\n}\n",
        )
        .edge_before("method Health::one", "calls", "method GetRoute::query")
        .no_edge_after("method Health::one", "calls", "method Elsewhere::query")
}

/// Upstream #2118 (W4-SW-02): a type-path call waits for the member the
/// path's type gains.
pub fn swift_type_path_call_reaches_a_member_its_type_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift])
        .file(
            "Sources/App/API.swift",
            "enum API {\n    enum GetRoute {\n        static func list() -> Int { 1 }\n    }\n}\nenum Other {\n    static func query() -> Int { 2 }\n}\n",
        )
        .file(
            "Sources/App/Health.swift",
            "enum Health {\n    static func one() -> Int { API.GetRoute.query() }\n}\n",
        )
        .write(
            "Sources/App/API.swift",
            "enum API {\n    enum GetRoute {\n        static func list() -> Int { 1 }\n        static func query() -> Int { 3 }\n    }\n}\nenum Other {\n    static func query() -> Int { 2 }\n}\n",
        )
        .edge_after("method Health::one", "calls", "method API::GetRoute::query")
        .no_edge_after("method Health::one", "calls", "method Other::query")
}

/// Upstream #2170 (W4-SW-02): a bare call reaches the members of the types
/// the class around it inherits, read from their heads two levels up; the
/// middle class's head naming another base in its own file moves the
/// unchanged grandchild's call, though nothing the grandchild writes names
/// the middle class.
pub fn swift_bare_call_follows_a_superclass_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "Sources/Charts/Base.swift",
            "class ChartViewBase {\n    func reset() {}\n}\nclass OtherBase {\n    func reset() {}\n}\n",
        )
        .file("Sources/Charts/Middle.swift", "class Middle: ChartViewBase {\n}\n")
        .file("Sources/Charts/Leaf.swift", "class Leaf: Middle {\n}\n")
        .file(
            "Sources/Charts/Grand.swift",
            "class Grand: Leaf {\n    func tap() {\n        reset()\n    }\n}\n",
        )
        .write("Sources/Charts/Middle.swift", "class Middle: OtherBase {\n}\n")
        .edge_before("method Grand::tap", "calls", "method ChartViewBase::reset")
        .edge_after("method Grand::tap", "calls", "method OtherBase::reset")
        .no_edge_after("method Grand::tap", "calls", "method ChartViewBase::reset")
}

/// Upstream #2205 (W4-SW-02): a call on a property a superclass in another
/// file declares with a type follows the declared type.
pub fn swift_typed_property_follows_an_inherited_declaration() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "Sources/Cache/Caches.swift",
            "class ImageCache {\n    func imageCachedType(forKey key: String) -> Int { 0 }\n}\nclass DiskBackend {\n    func imageCachedType(forKey key: String) -> Int { 1 }\n}\n",
        )
        .file(
            "Tests/AppTests/BaseTests.swift",
            "class BaseTests {\n    var store: ImageCache!\n}\n",
        )
        .file(
            "Tests/AppTests/StoreTests.swift",
            "class StoreTests: BaseTests {\n    func testType() {\n        _ = store.imageCachedType(forKey: \"a\")\n    }\n}\n",
        )
        .write(
            "Tests/AppTests/BaseTests.swift",
            "class BaseTests {\n    var store: DiskBackend!\n}\n",
        )
        .edge_before(
            "method StoreTests::testType",
            "calls",
            "method ImageCache::imageCachedType",
        )
        .edge_after(
            "method StoreTests::testType",
            "calls",
            "method DiskBackend::imageCachedType",
        )
        .no_edge_after(
            "method StoreTests::testType",
            "calls",
            "method ImageCache::imageCachedType",
        )
}

/// Upstream #2323 (W4-GO-01): a method a receiver's type promotes from a
/// type it embeds follows the embedding, changed in another file of the
/// package.
pub fn go_embedded_method_follows_an_embedding_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "h/base.go",
            "package h\n\ntype BaseAPI struct{}\n\nfunc (b *BaseAPI) SendError() {}\n\ntype OtherAPI struct{}\n\nfunc (o *OtherAPI) SendError() {}\n",
        )
        .file("h/server.go", "package h\n\ntype server struct {\n\tBaseAPI\n}\n")
        .file(
            "h/run.go",
            "package h\n\nfunc (s *server) Run() {\n\ts.SendError()\n}\n",
        )
        .write("h/server.go", "package h\n\ntype server struct {\n\tOtherAPI\n}\n")
        .edge_before("method server::Run", "calls", "method BaseAPI::SendError")
        .edge_after("method server::Run", "calls", "method OtherAPI::SendError")
        .no_edge_after("method server::Run", "calls", "method BaseAPI::SendError")
}

/// Upstream #1276 / #2361 (W4-GO-01): a call through a field chain follows
/// the field's declared type, changed in another file of the package.
pub fn go_field_chain_follows_a_field_type_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "store/store.go",
            "package store\n\ntype Manager struct{}\n\nfunc (m *Manager) CreateItem(name string) error { return nil }\n\ntype Archive struct{}\n\nfunc (a *Archive) CreateItem(name string) error { return nil }\n",
        )
        .file(
            "domain/service.go",
            "package domain\n\nimport \"example.com/app/store\"\n\ntype Service struct {\n\tdb *store.Manager\n}\n",
        )
        .file(
            "domain/add.go",
            "package domain\n\nfunc (s *Service) Add(name string) error {\n\treturn s.db.CreateItem(name)\n}\n",
        )
        .write(
            "domain/service.go",
            "package domain\n\nimport \"example.com/app/store\"\n\ntype Service struct {\n\tdb *store.Archive\n}\n",
        )
        .edge_before("method Service::Add", "calls", "method Manager::CreateItem")
        .edge_after("method Service::Add", "calls", "method Archive::CreateItem")
        .no_edge_after("method Service::Add", "calls", "method Manager::CreateItem")
}

/// Upstream #2361 (W4-GO-01): a field typed through a package qualifier
/// follows the import its file retargets under the same name (plan gate
/// round 1).
pub fn go_field_chain_follows_an_import_retarget_in_the_declaring_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "store1/store.go",
            "package store1\n\ntype Manager struct{}\n\nfunc (m *Manager) CreateItem(name string) error { return nil }\n",
        )
        .file(
            "store2/store.go",
            "package store2\n\ntype Manager struct{}\n\nfunc (m *Manager) CreateItem(name string) error { return nil }\n",
        )
        .file(
            "domain/service.go",
            "package domain\n\nimport dep \"example.com/app/store1\"\n\ntype Service struct {\n\tdb *dep.Manager\n}\n",
        )
        .file(
            "domain/add.go",
            "package domain\n\nfunc (s *Service) Add(name string) error {\n\treturn s.db.CreateItem(name)\n}\n",
        )
        .write(
            "domain/service.go",
            "package domain\n\nimport dep \"example.com/app/store2\"\n\ntype Service struct {\n\tdb *dep.Manager\n}\n",
        )
        .edge_before(
            "method Service::Add",
            "calls",
            "method Manager::CreateItem @store1/store.go",
        )
        .edge_after(
            "method Service::Add",
            "calls",
            "method Manager::CreateItem @store2/store.go",
        )
        .no_edge_after(
            "method Service::Add",
            "calls",
            "method Manager::CreateItem @store1/store.go",
        )
}

/// Upstream #2361 (W4-GO-01): an embedding written through a package
/// qualifier follows the import its file retargets under the same name.
pub fn go_embedded_method_follows_an_import_retarget_in_the_declaring_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "base1/api.go",
            "package base1\n\ntype API struct{}\n\nfunc (a *API) Send() {}\n",
        )
        .file(
            "base2/api.go",
            "package base2\n\ntype API struct{}\n\nfunc (a *API) Send() {}\n",
        )
        .file(
            "h/server.go",
            "package h\n\nimport base \"example.com/app/base1\"\n\ntype server struct {\n\tbase.API\n}\n",
        )
        .file("h/run.go", "package h\n\nfunc (s *server) Run() {\n\ts.Send()\n}\n")
        .write(
            "h/server.go",
            "package h\n\nimport base \"example.com/app/base2\"\n\ntype server struct {\n\tbase.API\n}\n",
        )
        .edge_before("method server::Run", "calls", "method API::Send @base1/api.go")
        .edge_after("method server::Run", "calls", "method API::Send @base2/api.go")
        .no_edge_after("method server::Run", "calls", "method API::Send @base1/api.go")
}

/// More unresolved `….CreateItem` calls than one tail retries in a sync:
/// CFML calls on receivers no `CreateItem` is named after, cheap to resolve.
static UNTYPED_CREATE_ITEM_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("component {\n  function noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("    n{index}.CreateItem({index});\n"));
    }
    text.push_str("    return 0;\n  }\n}\n");
    text
});

/// Upstream #1276 / #2361 (W4-GO-01): a field retyped in another file to a
/// project type settles the parked call through it, however many parked
/// calls share the tail.
pub fn go_field_chain_reaches_a_retyped_field_past_the_tail_ceiling() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go, Language::Cfml])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "store/store.go",
            "package store\n\ntype Archive struct{}\n\nfunc (a *Archive) CreateItem(name string) error { return nil }\n",
        )
        .file(
            "domain/service.go",
            "package domain\n\nimport \"example.com/app/store\"\n\ntype Service struct {\n\tdb *store.Missing\n}\n",
        )
        .file(
            "domain/add.go",
            "package domain\n\nfunc (s *Service) Add(name string) error {\n\treturn s.db.CreateItem(name)\n}\n",
        )
        .file("noise/Noise.cfc", UNTYPED_CREATE_ITEM_CALLS.as_str())
        .write(
            "domain/service.go",
            "package domain\n\nimport \"example.com/app/store\"\n\ntype Service struct {\n\tdb *store.Archive\n}\n",
        )
        .edge_after("method Service::Add", "calls", "method Archive::CreateItem")
}

/// Upstream #2323 (W4-GO-01): a method an unexported receiver's own package
/// gains is what its call reaches, never another package's same-named
/// type's.
pub fn go_unexported_receiver_reaches_a_method_its_package_gains() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "handlers/server.go",
            "package handlers\n\ntype server struct{}\n\nfunc (s *server) Create() {\n\ts.Close()\n}\n",
        )
        .file(
            "admin/server.go",
            "package admin\n\ntype server struct{}\n\nfunc (s *server) Close() {}\n",
        )
        .write(
            "handlers/close.go",
            "package handlers\n\nfunc (s *server) Close() {}\n",
        )
        .edge_after(
            "method server::Create",
            "calls",
            "method server::Close @handlers/close.go",
        )
        .no_edge_after(
            "method server::Create",
            "calls",
            "method server::Close @admin/server.go",
        )
}

/// Upstream #2322 (W4-GO-01): a type a reference names through a package
/// lands on that package's type once it appears, never on a same-named
/// method beside the reference.
pub fn go_qualified_type_reference_reaches_a_package_type_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "impl/context.go",
            "package impl\n\nimport \"example.com/app/job\"\n\ntype Context struct{}\n\nfunc (c *Context) OPCommand() (job.OPCommand, bool) {\n\treturn \"\", false\n}\n",
        )
        .file("job/doc.go", "package job\n")
        .write("job/op.go", "package job\n\ntype OPCommand string\n")
        .edge_after("method Context::OPCommand", "references", "type_alias OPCommand")
        .no_edge_after("method Context::OPCommand", "references", "method Context::OPCommand")
}

/// Upstream #2417 (W4-GO-02): a method called on an alias is the aliased
/// type's, so it follows the alias retargeted in another file of the
/// package.
pub fn go_alias_method_follows_an_alias_target_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.24\n")
        .file(
            "a/event.go",
            "package a\n\ntype Event struct{}\n\nfunc (e *Event) IsCreate() bool { return true }\n",
        )
        .file(
            "b/event.go",
            "package b\n\ntype Event struct{}\n\nfunc (e *Event) IsCreate() bool { return false }\n",
        )
        .file(
            "w/alias.go",
            "package w\n\nimport \"example.com/app/a\"\n\ntype Event = a.Event\n",
        )
        .file(
            "w/use.go",
            "package w\n\nfunc isCreate(e *Event) bool { return e.IsCreate() }\n",
        )
        .write(
            "w/alias.go",
            "package w\n\nimport \"example.com/app/b\"\n\ntype Event = b.Event\n",
        )
        .edge_before("function isCreate", "calls", "method Event::IsCreate @a/event.go")
        .edge_after("function isCreate", "calls", "method Event::IsCreate @b/event.go")
        .no_edge_after("function isCreate", "calls", "method Event::IsCreate @a/event.go")
}

/// Upstream #2417 (W4-GO-02): a method called through two aliases follows
/// the middle one, changed in another file of the package.
pub fn go_alias_chain_follows_a_middle_alias_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.24\n")
        .file(
            "w/types.go",
            "package w\n\ntype A struct{}\n\nfunc (a *A) Err() error { return nil }\n\ntype B struct{}\n\nfunc (b *B) Err() error { return nil }\n",
        )
        .file("w/ptr.go", "package w\n\ntype Ptr = *Local\n")
        .file("w/local.go", "package w\n\ntype Local = A\n")
        .file(
            "w/use.go",
            "package w\n\nfunc check(p Ptr) error { return p.Err() }\n",
        )
        .write("w/local.go", "package w\n\ntype Local = B\n")
        .edge_before("function check", "calls", "method A::Err")
        .edge_after("function check", "calls", "method B::Err")
        .no_edge_after("function check", "calls", "method A::Err")
}

/// Upstream #2417 (W4-GO-02): a method called on a type an alias that
/// appears declares is the aliased type's, never a namesake's.
pub fn go_alias_method_reaches_the_aliased_type_once_the_alias_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.24\n")
        .file(
            "a/event.go",
            "package a\n\ntype Event struct{}\n\nfunc (e *Event) IsCreate() bool { return true }\n",
        )
        .file(
            "b/event.go",
            "package b\n\ntype Event struct{}\n\nfunc (e *Event) IsCreate() bool { return false }\n",
        )
        .file(
            "w/use.go",
            "package w\n\nfunc isCreate(e *Event) bool { return e.IsCreate() }\n",
        )
        .write(
            "w/alias.go",
            "package w\n\nimport \"example.com/app/b\"\n\ntype Event = b.Event\n",
        )
        .edge_after("function isCreate", "calls", "method Event::IsCreate @b/event.go")
        .no_edge_after("function isCreate", "calls", "method Event::IsCreate @a/event.go")
}

/// Upstream #2441 (W4-GO-02): a defined type references the type its own
/// package gains, never another package's namesake.
pub fn go_defined_type_reaches_a_type_its_package_gains() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.24\n")
        .file(
            "alpha/names.go",
            "package alpha\n\ntype WatchResponse struct{}\n",
        )
        .file(
            "w/chan.go",
            "package w\n\ntype WatchChan <-chan WatchResponse\n",
        )
        .write("w/resp.go", "package w\n\ntype WatchResponse struct{}\n")
        .edge_after(
            "type_alias WatchChan",
            "references",
            "struct WatchResponse @w/resp.go",
        )
        .no_edge_after(
            "type_alias WatchChan",
            "references",
            "struct WatchResponse @alpha/names.go",
        )
}

/// Upstream #2233 (W4-GO-02): a name written through a package from
/// outside the project's modules never reaches a namesake that appears.
pub fn go_external_qualified_reference_never_reaches_a_namesake_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.24\n")
        .file(
            "w/handler.go",
            "package w\n\nimport \"context\"\n\ntype Handler func(ctx context.Context) error\n\ntype Ctx = context.Context\n",
        )
        .write("w/context.go", "package w\n\ntype Context struct{}\n")
        .no_edge_after("type_alias Handler", "references", "struct Context")
        .no_edge_after("type_alias Ctx", "references", "struct Context")
}

/// Upstream #2444 (W4-GO-03): a call through a type assertion reaches the
/// asserted type's promoted method, so it follows the type's embedding,
/// changed in another file.
pub fn go_asserted_call_follows_an_embedding_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "w/base.go",
            "package w\n\ntype Base struct{}\n\nfunc (b *Base) Flush() error { return nil }\n\ntype Other struct{}\n\nfunc (o *Other) Flush() error { return nil }\n",
        )
        .file("w/wrapper.go", "package w\n\ntype Wrapper struct {\n\t*Base\n}\n")
        .file("w/use.go", "package w\n\nfunc use(v any) {\n\tv.(*Wrapper).Flush()\n}\n")
        .write("w/wrapper.go", "package w\n\ntype Wrapper struct {\n\t*Other\n}\n")
        .edge_before("function use", "calls", "method Base::Flush")
        .edge_after("function use", "calls", "method Other::Flush")
        .no_edge_after("function use", "calls", "method Base::Flush")
}

/// Upstream #2444 (W4-GO-03): a call through a type assertion reaches a
/// method the asserted interface gains, never a stub's namesake.
pub fn go_asserted_call_reaches_a_method_the_asserted_type_gains() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "pb/stub.go",
            "package pb\n\ntype UnimplementedKVServer struct{}\n\nfunc (UnimplementedKVServer) Range() error { return nil }\n",
        )
        .file("pb/server.go", "package pb\n\ntype KVServer interface {\n\tPut() error\n}\n")
        .file(
            "pb/handler.go",
            "package pb\n\nfunc handle(srv interface{}) error {\n\treturn srv.(KVServer).Range()\n}\n",
        )
        .write(
            "pb/server.go",
            "package pb\n\ntype KVServer interface {\n\tPut() error\n\tRange() error\n}\n",
        )
        .edge_after("function handle", "calls", "method KVServer::Range")
        .no_edge_after("function handle", "calls", "method UnimplementedKVServer::Range")
}

/// Upstream #2416 (W4-GO-03): a type position reaches the type its package
/// gains, never the method of its name beside it.
pub fn go_type_position_reaches_a_type_its_package_gains() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "mvcc/index.go",
            "package mvcc\n\ntype treeIndex struct{}\n\nfunc (ti *treeIndex) keyIndex() {}\n\nfunc (ti *treeIndex) KeyIndex(keyi *keyIndex) {}\n",
        )
        .write("mvcc/key_index.go", "package mvcc\n\ntype keyIndex struct{}\n")
        .edge_after(
            "method treeIndex::KeyIndex",
            "references",
            "struct keyIndex @mvcc/key_index.go",
        )
        .no_edge_after(
            "method treeIndex::KeyIndex",
            "references",
            "method treeIndex::keyIndex",
        )
}

/// Upstream #2150 (W4-SW-04): a message to `self` reaches the sender's
/// hierarchy, so it follows a middle class's superclass changed in another
/// file.
pub fn objc_self_send_follows_a_superclass_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "Core/SDBase.m",
            "@interface SDBase : NSObject\n@end\n@implementation SDBase\n- (void)prepare {\n}\n@end\n",
        )
        .file(
            "Core/SDOther.m",
            "@interface SDOther : NSObject\n@end\n@implementation SDOther\n- (void)prepare {\n}\n@end\n",
        )
        .file("Core/SDMiddle.h", "@interface SDMiddle : SDBase\n@end\n")
        .file(
            "Core/SDCache.m",
            "@interface SDCache : SDMiddle\n@end\n@implementation SDCache\n- (void)run {\n    [self prepare];\n}\n@end\n",
        )
        .write("Core/SDMiddle.h", "@interface SDMiddle : SDOther\n@end\n")
        .edge_before("method SDCache::run", "calls", "method SDBase::prepare")
        .edge_after("method SDCache::run", "calls", "method SDOther::prepare")
        .no_edge_after("method SDCache::run", "calls", "method SDBase::prepare")
}

/// Upstream #2179 (W4-SW-04): `[super init]` goes past the sender, to the
/// superclass its header, changed apart from the implementation, names.
pub fn objc_super_send_follows_the_header_of_its_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "Base.m",
            "@interface Base : NSObject\n@end\n@implementation Base\n- (instancetype)init {\n    return self;\n}\n@end\n",
        )
        .file(
            "Mid.m",
            "@interface Mid : NSObject\n@end\n@implementation Mid\n- (instancetype)init {\n    return self;\n}\n@end\n",
        )
        .file("Child.h", "@interface Child : Base\n@end\n")
        .file(
            "Child.m",
            "#import \"Child.h\"\n@implementation Child\n- (instancetype)init {\n    self = [super init];\n    return self;\n}\n@end\n",
        )
        .write("Child.h", "@interface Child : Mid\n@end\n")
        .edge_before("method Child::init", "calls", "method Base::init")
        .edge_after("method Child::init", "calls", "method Mid::init")
        .no_edge_after("method Child::init", "calls", "method Base::init")
}

/// Upstream #2150 (W4-SW-04): an untyped receiver ending in a property keeps
/// the one method of its name while the property's declared type has the
/// method's owner, and loses it when another file retypes the property.
pub fn objc_typed_property_receiver_follows_a_declared_type_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "RLMSet.m",
            "@interface RLMSet : NSObject\n@end\n@implementation RLMSet\n- (BOOL)containsObject:(id)object {\n    return NO;\n}\n@end\n",
        )
        .file(
            "AllTypesObject.h",
            "@interface AllTypesObject : NSObject\n@property (nonatomic) RLMSet<NSString *> *anyDataObj;\n@end\n",
        )
        .file(
            "Tests.m",
            "@interface Tests : NSObject\n@end\n@implementation Tests\n- (void)run:(AllTypesObject *)managed {\n    [managed.anyDataObj containsObject:@\"x\"];\n}\n@end\n",
        )
        .write(
            "AllTypesObject.h",
            "@interface AllTypesObject : NSObject\n@property (nonatomic) NSSet<NSString *> *anyDataObj;\n@end\n",
        )
        .edge_before("method Tests::run:", "calls", "method RLMSet::containsObject:")
        .no_edge_after("method Tests::run:", "calls", "method RLMSet::containsObject:")
}

/// Upstream #2150 (W4-SW-04): a property's declared type that comes to
/// inherit the method's owner in another file settles the parked message on
/// the property, written with a keyword selector.
pub fn objc_typed_property_receiver_reaches_an_owner_its_type_comes_to_inherit() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "RLMCollection.m",
            "@interface RLMCollection : NSObject\n@end\n@implementation RLMCollection\n- (BOOL)containsObject:(id)object {\n    return NO;\n}\n@end\n",
        )
        .file("RLMArray.h", "@interface RLMArray : NSObject\n@end\n")
        .file(
            "Owner.h",
            "@interface Owner : NSObject\n@property (nonatomic) RLMArray *items;\n@end\n",
        )
        .file(
            "Tests.m",
            "@interface Tests : NSObject\n@end\n@implementation Tests\n- (void)run:(Owner *)owner {\n    [owner.items containsObject:@\"x\"];\n}\n@end\n",
        )
        .write("RLMArray.h", "@interface RLMArray : RLMCollection\n@end\n")
        .edge_after("method Tests::run:", "calls", "method RLMCollection::containsObject:")
}

/// More unresolved `….next` calls than one tail retries in a sync: CFML
/// calls on receivers no `next` is named after, cheap to resolve.
static UNTYPED_NEXT_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("component {\n  function noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("    n{index}.next({index});\n"));
    }
    text.push_str("    return 0;\n  }\n}\n");
    text
});

/// Upstream #2150 (W4-SW-04): a local typed by its declaration
/// (`FMResultSet *rs`) reaches the method its type comes to inherit in
/// another file, however many parked calls share the tail.
pub fn objc_typed_local_reaches_an_inherited_method_past_the_tail_ceiling() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC, Language::Cfml])
        .file(
            "FMBase.m",
            "@interface FMBase : NSObject\n@end\n@implementation FMBase\n- (BOOL)next {\n    return NO;\n}\n@end\n",
        )
        .file("FMResultSet.h", "@interface FMResultSet : NSObject\n@end\n")
        .file(
            "Reader.m",
            "@interface Reader : NSObject\n@end\n@implementation Reader\n- (void)read {\n    FMResultSet *rs = nil;\n    [rs next];\n}\n@end\n",
        )
        .file("noise/Noise.cfc", UNTYPED_NEXT_CALLS.as_str())
        .write("FMResultSet.h", "@interface FMResultSet : FMBase\n@end\n")
        .edge_after("method Reader::read", "calls", "method FMBase::next")
}

/// Upstream #2150's system table (W4-SW-04): a UIView category's method
/// that appears is what a UIImageView category's `[self …]` sends to.
pub fn objc_category_send_reaches_a_system_superclass_category_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::ObjC])
        .file(
            "SDDecoy.m",
            "@interface SDDecoy : NSObject\n@end\n@implementation SDDecoy\n- (void)sd_internalSetImageWithURL:(NSURL *)url {\n}\n@end\n",
        )
        .file(
            "UIImageView+WebCache.m",
            "@implementation UIImageView (WebCache)\n- (void)sd_setImageWithURL:(NSURL *)url {\n    [self sd_internalSetImageWithURL:url];\n}\n@end\n",
        )
        .write(
            "UIView+WebCache.m",
            "@implementation UIView (WebCache)\n- (void)sd_internalSetImageWithURL:(NSURL *)url {\n}\n@end\n",
        )
        .edge_after(
            "method UIImageView::sd_setImageWithURL:",
            "calls",
            "method UIView::sd_internalSetImageWithURL:",
        )
        .no_edge_after(
            "method UIImageView::sd_setImageWithURL:",
            "calls",
            "method SDDecoy::sd_internalSetImageWithURL:",
        )
}

/// Upstream #2242 (W4-SW-04): a class conforming to its namesake protocol
/// keeps implementing its own header's when a copy of the header appears.
pub fn objc_namesake_protocol_keeps_its_own_header_when_a_copy_appears() -> Case {
    const HEADER: &str = "@protocol SDDiskCache <NSObject>\n- (NSData *)dataForKey:(NSString *)key;\n@end\n\n@interface SDDiskCache : NSObject <SDDiskCache>\n@end\n";
    Case::new(Change::AddCompetitor, &[Language::ObjC])
        .file("Core/SDDiskCache.h", HEADER)
        .write("include/SDDiskCache.h", HEADER)
        .edge_throughout(
            "class SDDiskCache @Core/SDDiskCache.h",
            "implements",
            "protocol SDDiskCache @Core/SDDiskCache.h",
        )
        .no_edge_after(
            "class SDDiskCache @Core/SDDiskCache.h",
            "implements",
            "protocol SDDiskCache @include/SDDiskCache.h",
        )
}

/// A line added above a function moves its id: the edge into it follows it,
/// and an edge to a namesake in another file stays as a fresh index has it
/// (a moved TypeScript node still touches its name; W4-PHP-01's move-aware
/// touch is PHP's only).
pub fn typescript_line_shift_above_a_function_keeps_namesake_edges_elsewhere() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/a.ts", "export function run() {\n  return 1;\n}\n")
        .file("src/b.ts", "export function run() {\n  return 2;\n}\n")
        .file(
            "src/main.ts",
            "import { run } from './b';\n\nexport function main() {\n  return run();\n}\n",
        )
        .file(
            "src/other.ts",
            "import { run } from './a';\n\nexport function other() {\n  return run();\n}\n",
        )
        .write(
            "src/a.ts",
            "// One line above the function moves its id.\nexport function run() {\n  return 1;\n}\n",
        )
        .edge_throughout("function main", "calls", "function run @src/b.ts")
        .edge_throughout("function other", "calls", "function run @src/a.ts")
}

/// A C function's `static` is read from its declaration's text, which no node
/// records: a blank line added above a `static` function that loses `static`
/// moves its id and nothing its node holds, and makes it visible to another
/// file's parked call, so a moved C node keeps touching its name (W4-PHP-01's
/// move-aware touch is PHP's only). A comment there would not do: it becomes
/// the node's docstring.
pub fn c_static_function_becomes_visible_with_a_line_added_above() -> Case {
    Case::new(Change::AddCompetitor, &[Language::C])
        .file("a.c", "static int f(void) {\n  return 1;\n}\n")
        .file("b.c", "int g(void) {\n  return f();\n}\n")
        .write("a.c", "\nint f(void) {\n  return 1;\n}\n")
        .edge_after("function g", "calls", "function f @a.c")
}

/// The same for a PHP base class: a line above its methods moves their ids;
/// the subclass's `$this->prepare()` follows the moved method.
pub fn php_line_shift_in_a_base_class_keeps_its_callers() -> Case {
    Case::new(Change::BodyEdit, &[Language::Php])
        .file(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    public function prepare() {}\n}\n",
        )
        .file(
            "src/Other.php",
            "<?php\nnamespace App;\n\nclass Other\n{\n    public function prepare() {}\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Base\n{\n    public function run()\n    {\n        $this->prepare();\n    }\n}\n",
        )
        .write(
            "src/Base.php",
            "<?php\nnamespace App;\n\n/** The base. */\nclass Base\n{\n    public function prepare() {}\n}\n",
        )
        .edge_throughout("method App::Child::run", "calls", "method App::Base::prepare")
}

/// Upstream #2142 (W4-PHP-01): `$this->m()` reaches the class's ancestry, so
/// it follows a middle class's `extends` changed in another file.
pub fn php_this_call_follows_an_ancestor_extends_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    public function prepare() {}\n}\n",
        )
        .file(
            "src/Other.php",
            "<?php\nnamespace App;\n\nclass Other\n{\n    public function prepare() {}\n}\n",
        )
        .file(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle extends Base\n{\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Middle\n{\n    public function run()\n    {\n        $this->prepare();\n    }\n}\n",
        )
        .write(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle extends Other\n{\n}\n",
        )
        .edge_before("method App::Child::run", "calls", "method App::Base::prepare")
        .edge_after("method App::Child::run", "calls", "method App::Other::prepare")
        .no_edge_after("method App::Child::run", "calls", "method App::Base::prepare")
}

/// Upstream #2142 (W4-PHP-01): a base class's `$this->m()` reaches a
/// subclass's method, so it leaves a deeper subclass's method once a middle
/// class between them is rebased in another file.
pub fn php_base_this_call_leaves_a_deeper_subclass_method_once_a_middle_class_is_rebased() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Entity.php",
            "<?php\nnamespace App;\n\nabstract class Entity\n{\n    public function parentBook()\n    {\n        return $this->book();\n    }\n}\n",
        )
        .file(
            "src/Other.php",
            "<?php\nnamespace App;\n\nclass Other\n{\n}\n",
        )
        .file(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle extends Entity\n{\n}\n",
        )
        .file(
            "src/Page.php",
            "<?php\nnamespace App;\n\nclass Page extends Middle\n{\n    public function book() { return null; }\n}\n",
        )
        .write(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle extends Other\n{\n}\n",
        )
        .edge_before("method App::Entity::parentBook", "calls", "method App::Page::book")
        .no_edge_after("method App::Entity::parentBook", "calls", "method App::Page::book")
}

/// Upstream #2142 (W4-PHP-01): a parked `$this->m()` resolves once a middle
/// class in another file comes to extend the method's class.
pub fn php_this_call_reaches_a_method_once_a_middle_class_extends_its_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    public function prepare() {}\n}\n",
        )
        .file(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle\n{\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Middle\n{\n    public function run()\n    {\n        $this->prepare();\n    }\n}\n",
        )
        .write(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nclass Middle extends Base\n{\n}\n",
        )
        .edge_after("method App::Child::run", "calls", "method App::Base::prepare")
}

/// Upstream #2142 (W4-PHP-01): a trait an ancestor `use`s is in the
/// ancestry, so `$this->m()` follows the trait an ancestor's body switches
/// to.
pub fn php_this_call_follows_a_trait_use_change_in_an_ancestor() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Greets.php",
            "<?php\nnamespace App;\n\ntrait Greets\n{\n    public function hello() {}\n}\n",
        )
        .file(
            "src/Waves.php",
            "<?php\nnamespace App;\n\ntrait Waves\n{\n    public function hello() {}\n}\n",
        )
        .file(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    use Greets;\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Base\n{\n    public function run()\n    {\n        $this->hello();\n    }\n}\n",
        )
        .write(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    use Waves;\n}\n",
        )
        .edge_before("method App::Child::run", "calls", "method App::Greets::hello")
        .edge_after("method App::Child::run", "calls", "method App::Waves::hello")
        .no_edge_after("method App::Child::run", "calls", "method App::Greets::hello")
}

/// Upstream #2142 (W4-PHP-01): an ancestor's `extends` names a class through
/// its own file's `use` imports, so `parent::m()` follows an import an
/// ancestor's file retargets.
pub fn php_parent_call_follows_an_import_retarget_in_an_ancestor_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/One/Base.php",
            "<?php\nnamespace App\\One;\n\nclass Base\n{\n    public function boot() {}\n}\n",
        )
        .file(
            "src/Two/Base.php",
            "<?php\nnamespace App\\Two;\n\nclass Base\n{\n    public function boot() {}\n}\n",
        )
        .file(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nuse App\\One\\Base;\n\nclass Middle extends Base\n{\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Middle\n{\n    public function boot()\n    {\n        parent::boot();\n    }\n}\n",
        )
        .write(
            "src/Middle.php",
            "<?php\nnamespace App;\n\nuse App\\Two\\Base;\n\nclass Middle extends Base\n{\n}\n",
        )
        .edge_before("method App::Child::boot", "calls", "method App\\One::Base::boot")
        .edge_after("method App::Child::boot", "calls", "method App\\Two::Base::boot")
        .no_edge_after("method App::Child::boot", "calls", "method App\\One::Base::boot")
}

/// Upstream #2142 (W4-PHP-01): past an ancestor outside the repository a
/// repository trait's method is in reach, so `$this->m()` leaves the trait
/// once the ancestor's declaration appears.
pub fn php_this_call_leaves_a_repository_trait_once_the_outside_base_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Php])
        .file(
            "src/Testing/InteractsWithTime.php",
            "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\n\ntrait InteractsWithTime\n{\n    public function travelTo($date) {}\n}\n",
        )
        .file(
            "tests/QueueTest.php",
            "<?php\nnamespace Illuminate\\Tests\\Integration;\n\nuse Orchestra\\Testbench\\TestCase;\n\nclass QueueTest extends TestCase\n{\n    public function testDelay()\n    {\n        $this->travelTo('2000-01-02');\n    }\n}\n",
        )
        .write(
            "testbench/TestCase.php",
            "<?php\nnamespace Orchestra\\Testbench;\n\nclass TestCase\n{\n}\n",
        )
        .edge_before(
            "method Illuminate\\Tests\\Integration::QueueTest::testDelay",
            "calls",
            "method Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo",
        )
        .no_edge_after(
            "method Illuminate\\Tests\\Integration::QueueTest::testDelay",
            "calls",
            "method Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo",
        )
}

/// Upstream #2142 (W4-PHP-01): past an ancestor outside the repository only
/// a trait's method is in reach, so `$this->m()` leaves the method once its
/// trait becomes a class.
pub fn php_this_call_leaves_a_method_once_its_trait_becomes_a_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Testing/InteractsWithTime.php",
            "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\n\ntrait InteractsWithTime\n{\n    public function travelTo($date) {}\n}\n",
        )
        .file(
            "tests/QueueTest.php",
            "<?php\nnamespace Illuminate\\Tests\\Integration;\n\nuse Orchestra\\Testbench\\TestCase;\n\nclass QueueTest extends TestCase\n{\n    public function testDelay()\n    {\n        $this->travelTo('2000-01-02');\n    }\n}\n",
        )
        .write(
            "src/Testing/InteractsWithTime.php",
            "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\n\nclass InteractsWithTime\n{\n    public function travelTo($date) {}\n}\n",
        )
        .edge_before(
            "method Illuminate\\Tests\\Integration::QueueTest::testDelay",
            "calls",
            "method Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo",
        )
        .no_edge_after(
            "method Illuminate\\Tests\\Integration::QueueTest::testDelay",
            "calls",
            "method Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo",
        )
}

/// Upstream #2142 (W4-PHP-01): the other way, `$this->m()` reaches the
/// method once its class becomes a trait. The call was parked, so nothing
/// resolved into the changed file, and the method itself did not change.
pub fn php_this_call_reaches_a_method_once_its_class_becomes_a_trait() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Testing/InteractsWithTime.php",
            "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\n\nclass InteractsWithTime\n{\n    public function travelTo($date) {}\n}\n",
        )
        .file(
            "tests/QueueTest.php",
            "<?php\nnamespace Illuminate\\Tests\\Integration;\n\nuse Orchestra\\Testbench\\TestCase;\n\nclass QueueTest extends TestCase\n{\n    public function testDelay()\n    {\n        $this->travelTo('2000-01-02');\n    }\n}\n",
        )
        .write(
            "src/Testing/InteractsWithTime.php",
            "<?php\nnamespace Illuminate\\Foundation\\Testing\\Concerns;\n\ntrait InteractsWithTime\n{\n    public function travelTo($date) {}\n}\n",
        )
        .edge_after(
            "method Illuminate\\Tests\\Integration::QueueTest::testDelay",
            "calls",
            "method Illuminate\\Foundation\\Testing\\Concerns::InteractsWithTime::travelTo",
        )
}

/// Upstream #2142 (W4-PHP-01): the same rule the other way, through a
/// removal: once the file declaring an imported base is deleted, the
/// ancestry leaves the repository and a repository trait's method is in
/// reach.
pub fn php_this_call_reaches_a_repository_trait_once_its_imported_base_is_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Php])
        .file(
            "src/Concerns/Travels.php",
            "<?php\nnamespace App\\Concerns;\n\ntrait Travels\n{\n    public function travel() {}\n}\n",
        )
        .file(
            "src/Base/Middle.php",
            "<?php\nnamespace App\\Base;\n\nclass Middle\n{\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nuse App\\Base\\Middle;\n\nclass Child extends Middle\n{\n    public function run()\n    {\n        $this->travel();\n    }\n}\n",
        )
        .remove("src/Base/Middle.php")
        .edge_after(
            "method App::Child::run",
            "calls",
            "method App\\Concerns::Travels::travel",
        )
}

/// Upstream #2152 (W4-PHP-01): an untyped receiver keeps the one method of
/// its name while the class it is named after inherits it, so it leaves the
/// method when that class's `extends` changes in another file.
pub fn php_untyped_receiver_leaves_a_method_its_class_stops_inheriting() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Models/Entity.php",
            "<?php\nnamespace App\\Models;\n\nabstract class Entity\n{\n    public function save(): bool { return true; }\n}\n",
        )
        .file(
            "src/Models/Record.php",
            "<?php\nnamespace App\\Models;\n\nclass Record\n{\n}\n",
        )
        .file(
            "src/Models/Page.php",
            "<?php\nnamespace App\\Models;\n\nclass Page extends Entity\n{\n}\n",
        )
        .file(
            "src/Controller.php",
            "<?php\nnamespace App;\n\nfunction handle($page)\n{\n    $page->save();\n}\n",
        )
        .write(
            "src/Models/Page.php",
            "<?php\nnamespace App\\Models;\n\nclass Page extends Record\n{\n}\n",
        )
        .edge_before("function App::handle", "calls", "method App\\Models::Entity::save")
        .no_edge_after("function App::handle", "calls", "method App\\Models::Entity::save")
}

/// Unresolved PHP calls through receivers nothing names a class after, all
/// written `$nN->save()`: 501 of them share the tail `save`, one past the
/// name lane's per-tail ceiling.
static UNTYPED_SAVE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("<?php\nnamespace Noise;\n\nfunction noise()\n{\n");
    for index in 0..501 {
        text.push_str(&format!("    $n{index}->save();\n"));
    }
    text.push_str("}\n");
    text
});

/// Upstream #2152 (W4-PHP-01): an untyped receiver reaches a method its
/// class's ancestor gains, with no head changing, however many parked calls
/// share the tail.
pub fn php_untyped_receiver_reaches_a_method_its_ancestor_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Php])
        .file(
            "src/Models/Entity.php",
            "<?php\nnamespace App\\Models;\n\nabstract class Entity\n{\n    public function load(): void {}\n}\n",
        )
        .file(
            "src/Models/Page.php",
            "<?php\nnamespace App\\Models;\n\nclass Page extends Entity\n{\n}\n",
        )
        .file(
            "src/Controller.php",
            "<?php\nnamespace App;\n\nfunction handle($page)\n{\n    $page->save();\n}\n",
        )
        .file("noise/Noise.php", UNTYPED_SAVE_CALLS.as_str())
        .write(
            "src/Models/Entity.php",
            "<?php\nnamespace App\\Models;\n\nabstract class Entity\n{\n    public function load(): void {}\n    public function save(): bool { return true; }\n}\n",
        )
        .edge_after("function App::handle", "calls", "method App\\Models::Entity::save")
}

/// Traits `B1` … `B78` used by a trait `B`, the five `S1` … `S5`, and `B77`'s
/// method `m77`: with `Widget extends S { use B; }` the resolver's 80-type
/// walk from `Widget` visits `B77` last, until `S` uses the five traits.
static WIDGET_TRAITS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("<?php\nnamespace App;\n\n");
    for index in 1..=78 {
        if index == 77 {
            text.push_str("trait B77\n{\n    public function m77() {}\n}\n\n");
        } else {
            text.push_str(&format!("trait B{index}\n{{\n}}\n\n"));
        }
    }
    for index in 1..=5 {
        text.push_str(&format!("trait S{index}\n{{\n}}\n\n"));
    }
    let uses: Vec<String> = (1..=78).map(|index| format!("B{index}")).collect();
    text.push_str(&format!("trait B\n{{\n    use {};\n}}\n", uses.join(", ")));
    text
});

/// Upstream #2152 (W4-PHP-01) at the walk limit: a seed whose ancestry grows
/// pushes another branch of a subclass's ancestry past the resolver's 80
/// types, so a receiver named after the subclass leaves that branch's method.
pub fn php_untyped_receiver_leaves_a_method_a_seed_pushes_past_the_walk_limit() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file("src/Traits.php", WIDGET_TRAITS.as_str())
        .file("src/S.php", "<?php\nnamespace App;\n\nclass S\n{\n}\n")
        .file(
            "src/Widget.php",
            "<?php\nnamespace App;\n\nclass Widget extends S\n{\n    use B;\n}\n",
        )
        .file(
            "src/Controller.php",
            "<?php\nnamespace App;\n\nfunction handle($widget)\n{\n    $widget->m77();\n}\n",
        )
        .write(
            "src/S.php",
            "<?php\nnamespace App;\n\nclass S\n{\n    use S1, S2, S3, S4, S5;\n}\n",
        )
        .edge_before("function App::handle", "calls", "method App::B77::m77")
        .no_edge_after("function App::handle", "calls", "method App::B77::m77")
}

/// Upstream #2152 (W4-PHP-01): an untyped receiver reaches the method the
/// class it is named after comes to inherit in another file, however many
/// parked calls share the tail.
pub fn php_untyped_receiver_reaches_an_inherited_method_past_the_tail_ceiling() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Models/Entity.php",
            "<?php\nnamespace App\\Models;\n\nabstract class Entity\n{\n    public function save(): bool { return true; }\n}\n",
        )
        .file(
            "src/Models/Page.php",
            "<?php\nnamespace App\\Models;\n\nclass Page\n{\n}\n",
        )
        .file(
            "src/Controller.php",
            "<?php\nnamespace App;\n\nfunction handle($page)\n{\n    $page->save();\n}\n",
        )
        .file("noise/Noise.php", UNTYPED_SAVE_CALLS.as_str())
        .write(
            "src/Models/Page.php",
            "<?php\nnamespace App\\Models;\n\nclass Page extends Entity\n{\n}\n",
        )
        .edge_after("function App::handle", "calls", "method App\\Models::Entity::save")
}

/// Upstream #2103 (W4-PHP-02): a bare PHP call is a function call, so it
/// stays parked beside a same-named method and links the function once a
/// file declares it.
pub fn php_bare_call_reaches_a_function_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "app/Http/Controllers/DocsController.php",
            "<?php\nclass DocsController\n{\n    public function redirect() { return 1; }\n}\n",
        )
        .file(
            "app/Http/Controllers/BookController.php",
            "<?php\nclass BookController\n{\n    public function show()\n    {\n        return redirect('/books');\n    }\n}\n",
        )
        .write(
            "app/helpers.php",
            "<?php\nfunction redirect($to) { return $to; }\n",
        )
        .no_edge_after(
            "method BookController::show",
            "calls",
            "method DocsController::redirect",
        )
        .edge_after("method BookController::show", "calls", "function redirect")
}

/// Upstream #2235 (W4-PHP-02): a bare PHP class name is its namespace's
/// class or the imported one, so `extends Request` waits for the class its
/// own namespace declares, past another namespace's `Request`.
pub fn php_bare_class_name_reaches_a_class_added_to_its_namespace() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "app/Http/Requests/API/Interaction/Request.php",
            "<?php\n\nnamespace App\\Http\\Requests\\API\\Interaction;\n\nabstract class Request {}\n",
        )
        .file(
            "app/Http/Requests/API/DeleteSongsRequest.php",
            "<?php\n\nnamespace App\\Http\\Requests\\API;\n\nclass DeleteSongsRequest extends Request {}\n",
        )
        .write(
            "app/Http/Requests/API/Request.php",
            "<?php\n\nnamespace App\\Http\\Requests\\API;\n\nabstract class Request {}\n",
        )
        .no_edge_after(
            "class App\\Http\\Requests\\API::DeleteSongsRequest",
            "extends",
            "class App\\Http\\Requests\\API\\Interaction::Request",
        )
        .edge_after(
            "class App\\Http\\Requests\\API::DeleteSongsRequest",
            "extends",
            "class App\\Http\\Requests\\API::Request",
        )
}

/// Upstream #2286 (W4-PHP-02): `new Field\FirstName()` through a namespace
/// alias names one class, never a same-named class elsewhere; it links once
/// a file declares the named one.
pub fn php_namespace_alias_instantiation_reaches_a_class_that_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Php])
        .file(
            "app/Other/FirstName.php",
            "<?php\nnamespace App\\Other;\n\nclass FirstName\n{\n}\n",
        )
        .file(
            "app/Personal.php",
            "<?php\nnamespace App;\n\nuse App\\Fields as Field;\n\nclass Personal\n{\n    public function fields()\n    {\n        return [new Field\\FirstName()];\n    }\n}\n",
        )
        .write(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Fields;\n\nclass FirstName\n{\n}\n",
        )
        .no_edge_after(
            "method App::Personal::fields",
            "instantiates",
            "class App\\Other::FirstName",
        )
        .edge_after(
            "method App::Personal::fields",
            "instantiates",
            "class App\\Fields::FirstName",
        )
}

/// Upstream #2286 (W4-PHP-02): `F\FirstName::make()` leaves the method once
/// its class moves to another namespace, and no same-named method takes it.
pub fn php_qualified_static_call_leaves_a_class_moved_to_another_namespace() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Php])
        .file(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Fields;\n\nclass FirstName\n{\n    public static function make() { return new static(); }\n}\n",
        )
        .file(
            "app/Personal.php",
            "<?php\nnamespace App;\n\nuse App\\Fields as F;\n\nclass Personal\n{\n    public function build()\n    {\n        return F\\FirstName::make();\n    }\n}\n",
        )
        .write(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Moved;\n\nclass FirstName\n{\n    public static function make() { return new static(); }\n}\n",
        )
        .edge_before(
            "method App::Personal::build",
            "calls",
            "method App\\Fields::FirstName::make",
        )
        .no_edge_after(
            "method App::Personal::build",
            "calls",
            "method App\\Moved::FirstName::make",
        )
}

/// 501 parked static calls of `make` through classes the project lacks, so
/// the shared tail retry skips that tail.
static QUALIFIED_MAKE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("<?php\nnamespace Noise;\n\nfunction noise()\n{\n");
    for index in 0..501 {
        text.push_str(&format!("    Missing{index}::make();\n"));
    }
    text.push_str("}\n");
    text
});

/// Upstream #2286 (W4-PHP-02): `F\FirstName::make()` links the class once a
/// file declares it, however many parked calls share the `make` tail.
pub fn php_qualified_static_call_reaches_a_class_that_appears_past_the_tail_ceiling() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Php])
        .file(
            "app/Personal.php",
            "<?php\nnamespace App;\n\nuse App\\Fields as F;\n\nclass Personal\n{\n    public function build()\n    {\n        return F\\FirstName::make();\n    }\n}\n",
        )
        .file("noise/Noise.php", QUALIFIED_MAKE_CALLS.as_str())
        .write(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Fields;\n\nclass FirstName\n{\n    public static function make() { return new static(); }\n}\n",
        )
        .edge_after(
            "method App::Personal::build",
            "calls",
            "method App\\Fields::FirstName::make",
        )
}

/// Upstream #2286 (W4-PHP-02): `F\FirstName::make()` links the method its
/// class gains, however many parked calls share the `make` tail.
pub fn php_qualified_static_call_reaches_a_method_its_class_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Php])
        .file(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Fields;\n\nclass FirstName\n{\n}\n",
        )
        .file(
            "app/Personal.php",
            "<?php\nnamespace App;\n\nuse App\\Fields as F;\n\nclass Personal\n{\n    public function build()\n    {\n        return F\\FirstName::make();\n    }\n}\n",
        )
        .file("noise/Noise.php", QUALIFIED_MAKE_CALLS.as_str())
        .write(
            "app/Fields/FirstName.php",
            "<?php\nnamespace App\\Fields;\n\nclass FirstName\n{\n    public static function make() { return new static(); }\n}\n",
        )
        .edge_after(
            "method App::Personal::build",
            "calls",
            "method App\\Fields::FirstName::make",
        )
}

/// Upstream #2103's form (W4-PHP-02): `\helper()` links the global function
/// once a file declares it (KEEP-RUST: the reference keeps the `\` in its
/// name, so it waits for `helper` under `\helper`).
pub fn php_function_called_from_the_global_namespace_reaches_it_once_it_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "app/Decoy.php",
            "<?php\nclass Decoy\n{\n    public function helper() { return 2; }\n}\n",
        )
        .file(
            "app/Page.php",
            "<?php\nnamespace App;\n\nclass Page\n{\n    public function show()\n    {\n        return \\helper();\n    }\n}\n",
        )
        .write("app/helpers.php", "<?php\nfunction helper() { return 1; }\n")
        .no_edge_after("method App::Page::show", "calls", "method Decoy::helper")
        .edge_after("method App::Page::show", "calls", "function helper")
}

/// Upstream #2141 (a) (W4-PHP-03): `formatprice(10)` links `FormatPrice`
/// once a file declares it, as PHP names functions in any case.
pub fn php_call_in_another_case_reaches_a_function_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "src/view.php",
            "<?php\nfunction render() { return formatprice(10); }\n",
        )
        .write(
            "src/helpers.php",
            "<?php\nfunction FormatPrice($amount) { return $amount; }\n",
        )
        .edge_after("function render", "calls", "function FormatPrice")
}

/// The same for Pascal (W4-PHP-03): `formatprice(10)` links `FormatPrice`
/// once a unit declares it once.
pub fn pascal_call_in_another_case_reaches_a_function_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Pascal])
        .file(
            "View.pas",
            "unit View;\ninterface\nfunction Render: Integer;\nimplementation\nfunction Render: Integer;\nbegin\n  Result := formatprice(10);\nend;\nend.\n",
        )
        .write(
            "Prices.pas",
            "unit Prices;\ninterface\nimplementation\nfunction FormatPrice(Amount: Integer): Integer;\nbegin\n  Result := Amount;\nend;\nend.\n",
        )
        .edge_after("file:View.pas", "calls", "function FormatPrice")
}

/// The same call leaves `FormatPrice` once a method named `formatPrice`
/// appears elsewhere: the name now matches two nodes in some case, and the
/// last-resort match takes only a single one.
pub fn php_call_in_another_case_leaves_a_function_once_a_namesake_in_another_case_appears() -> Case
{
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "src/helpers.php",
            "<?php\nfunction FormatPrice($amount) { return $amount; }\n",
        )
        .file(
            "src/view.php",
            "<?php\nfunction render() { return formatprice(10); }\n",
        )
        .write(
            "src/Money.php",
            "<?php\nclass Money\n{\n    public function formatPrice() { return 0; }\n}\n",
        )
        .edge_before("function render", "calls", "function FormatPrice")
        .no_edge_after("function render", "calls", "function FormatPrice")
}

/// W4-PHP-02's rooted form in another case (W4-PHP-03): `\HELPER()` links
/// `helper` once a file declares it.
pub fn php_function_called_in_another_case_from_the_global_namespace_reaches_it_once_it_appears()
-> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "app/Page.php",
            "<?php\nnamespace App;\n\nclass Page\n{\n    public function show()\n    {\n        return \\HELPER();\n    }\n}\n",
        )
        .write("app/helpers.php", "<?php\nfunction helper() { return 1; }\n")
        .edge_after("method App::Page::show", "calls", "function helper")
}

/// Upstream #2143 (W4-LUA-01): a Lua function another file calls leaves its
/// callers once it is declared `local`, on the same line, so no node of it
/// changes.
pub fn lua_function_made_local_leaves_its_callers_in_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "spec/helpers.lua",
            "function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .file("spec/plugin_spec.lua", "helper()\n")
        .write(
            "spec/helpers.lua",
            "local function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .edge_before("file:spec/plugin_spec.lua", "calls", "function helper")
        .no_edge_after("file:spec/plugin_spec.lua", "calls", "function helper")
}

/// The reverse: a `local` function another file calls is reached once it is
/// made global.
pub fn lua_local_function_made_global_reaches_its_callers_in_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "spec/helpers.lua",
            "local function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .file("spec/plugin_spec.lua", "helper()\n")
        .write(
            "spec/helpers.lua",
            "function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .edge_after("file:spec/plugin_spec.lua", "calls", "function helper")
}

/// Upstream #2188 (W4-LUA-01): a call through `local cp = phases.check`
/// reaches the function the module starts handing out under `check`, though
/// no node of the module changes.
pub fn lua_alias_reaches_a_member_its_module_starts_exporting() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n}\n",
        )
        .file(
            "kong/handler.lua",
            "local phases = require \"kong.pdk.phases\"\nlocal cp = phases.check\n\nlocal function run()\n  cp(\"access\")\nend\n\nreturn { run = run }\n",
        )
        .write(
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
        )
        .edge_after("function run", "calls", "function check_phase")
}

/// The same two modules away: `helpers` re-exports `misc.unindent`, and
/// `misc` starts handing out its `undent` under `unindent`.
pub fn lua_alias_reaches_a_member_re_exported_from_a_module_two_away() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "spec/internal/misc.lua",
            "local function undent(s)\n  return s\nend\n\nreturn {}\n",
        )
        .file(
            "spec/helpers.lua",
            "local misc = require(\"spec.internal.misc\")\n\nreturn {\n  unindent = misc.unindent,\n}\n",
        )
        .file(
            "spec/handler_spec.lua",
            "local helpers = require \"spec.helpers\"\nlocal unindent = helpers.unindent\n\nunindent(\"a\")\n",
        )
        .write(
            "spec/internal/misc.lua",
            "local function undent(s)\n  return s\nend\n\nreturn { unindent = undent }\n",
        )
        .edge_after("file:spec/handler_spec.lua", "calls", "function undent")
}

/// A call through an alias of a module that appears later reaches the
/// module's member (the alias is not named after it).
pub fn lua_alias_reaches_a_member_of_a_module_that_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Lua])
        .file(
            "kong/handler.lua",
            "local phases = require \"kong.pdk.phases\"\nlocal cp = phases.check\n\nlocal function run()\n  cp(\"access\")\nend\n\nreturn { run = run }\n",
        )
        .write(
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
        )
        .edge_after("function run", "calls", "function check_phase")
}

/// A call through `local snooze = ngx.sleep` reaches the project's own
/// `function ngx.sleep` once one appears: the alias reads every node named
/// `sleep`, and the call is named `snooze`.
pub fn lua_alias_of_a_library_function_reaches_the_patch_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Lua])
        .file(
            "app/main.lua",
            "local snooze = ngx.sleep\n\nlocal function run()\n  snooze(1)\nend\n\nreturn { run = run }\n",
        )
        .write(
            "lib/patch.lua",
            "function ngx.sleep(seconds)\n  return seconds\nend\n",
        )
        .edge_after("function run", "calls", "method ngx::sleep")
}

/// The reverse: the call leaves the patch once a second one appears, since
/// an alias of a library function names it only while the project has one.
pub fn lua_alias_of_a_library_function_leaves_the_patch_once_a_second_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Lua])
        .file(
            "app/main.lua",
            "local snooze = ngx.sleep\n\nlocal function run()\n  snooze(1)\nend\n\nreturn { run = run }\n",
        )
        .file(
            "lib/patch.lua",
            "function ngx.sleep(seconds)\n  return seconds\nend\n",
        )
        .write(
            "lib/other_patch.lua",
            "function ngx.sleep(seconds)\n  return seconds + 1\nend\n",
        )
        .edge_before("function run", "calls", "method ngx::sleep")
        .no_edge_after("function run", "calls", "method ngx::sleep")
}

/// The same through a module: `util` hands out `M.snooze = ngx.sleep`, so a
/// call through `local nap = util.snooze` reaches the patch that appears
/// though neither file changes, and the caller's own text never names
/// `sleep`.
pub fn lua_alias_through_a_module_reaches_the_library_patch_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Lua])
        .file("app/util.lua", "local M = {}\nM.snooze = ngx.sleep\nreturn M\n")
        .file(
            "app/main.lua",
            "local util = require \"app.util\"\nlocal nap = util.snooze\n\nlocal function run()\n  nap(1)\nend\n\nreturn { run = run }\n",
        )
        .write(
            "lib/patch.lua",
            "function ngx.sleep(seconds)\n  return seconds\nend\n",
        )
        .edge_after("function run", "calls", "method ngx::sleep")
}

/// The same with the module itself edited in that sync, its surface kept:
/// its callers still reach the patch that appears beside it.
pub fn lua_alias_through_a_changed_module_reaches_the_library_patch_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Lua])
        .file("app/util.lua", "local M = {}\nM.snooze = ngx.sleep\nreturn M\n")
        .file(
            "app/main.lua",
            "local util = require \"app.util\"\nlocal nap = util.snooze\n\nlocal function run()\n  nap(1)\nend\n\nreturn { run = run }\n",
        )
        .write(
            "app/util.lua",
            "local M = {}\nM.snooze = ngx.sleep\nM.idle = 1\nreturn M\n",
        )
        .write(
            "lib/patch.lua",
            "function ngx.sleep(seconds)\n  return seconds\nend\n",
        )
        .edge_after("function run", "calls", "method ngx::sleep")
}

/// A call through `local respond = kong.response.exit` reaches the one
/// method `exit` of a table named for `response` once it appears.
pub fn lua_alias_of_a_host_table_member_reaches_the_method_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Lua])
        .file(
            "app/main.lua",
            "local respond = kong.response.exit\n\nlocal function run()\n  respond(200)\nend\n\nreturn { run = run }\n",
        )
        .write(
            "kong/pdk/response.lua",
            "local response = {}\n\nfunction response.exit(status)\n  return status\nend\n\nreturn response\n",
        )
        .edge_after("function run", "calls", "method response::exit")
}

/// A `require` an alias reads through, written with a trailing comment,
/// spread over lines, or as one target of several: the module starting to
/// export the member reaches every call, as the resolver reads each one.
pub fn lua_alias_through_a_commented_spread_or_multi_target_require_reaches_a_new_export() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n}\n",
        )
        .file(
            "kong/handler.lua",
            "local p1 = require \"kong.pdk.phases\" -- phases\nlocal p2 = require(\n  \"kong.pdk.phases\"\n)\nlocal n, p3 = 1, require \"kong.pdk.phases\"\nlocal c1 = p1.check\nlocal c2 = p2.check\nlocal c3 = p3.check\n\nlocal function run()\n  c1(\"a\")\n  c2(\"b\")\n  c3(\"c\")\nend\n\nreturn { run = run }\n",
        )
        .write(
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
        )
        .edge_after("function run", "calls", "function check_phase")
}

/// A module two away starts handing out the member while the module in the
/// middle, which re-exports it, changes in the same sync without its
/// surface changing: the unchanged caller still reaches the member.
pub fn lua_alias_reaches_a_new_export_through_a_middle_module_edited_in_the_same_sync() -> Case {
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "spec/internal/misc.lua",
            "local function undent(s)\n  return s\nend\n\nreturn {}\n",
        )
        .file(
            "spec/helpers.lua",
            "local misc = require(\"spec.internal.misc\")\n\nreturn {\n  unindent = misc.unindent,\n}\n",
        )
        .file(
            "spec/handler_spec.lua",
            "local helpers = require \"spec.helpers\"\nlocal unindent = helpers.unindent\n\nunindent(\"a\")\n",
        )
        .write(
            "spec/internal/misc.lua",
            "local function undent(s)\n  return s\nend\n\nreturn { unindent = undent }\n",
        )
        .write(
            "spec/helpers.lua",
            "local misc = require(\"spec.internal.misc\")\n\nreturn {\n  unindent = misc.unindent,\n}\n-- reviewed\n",
        )
        .edge_after("file:spec/handler_spec.lua", "calls", "function undent")
}

/// Of two `local function helper` in one file, the first, which a name in
/// another file matches first, is made global: that file's call reaches it,
/// though the file still declares a local `helper`.
pub fn lua_the_first_of_two_local_namesakes_made_global_reaches_its_callers_in_another_file() -> Case
{
    Case::new(Change::BodyEdit, &[Language::Lua])
        .file(
            "spec/helpers.lua",
            "local function helper()\n  return 0\nend\n\nlocal function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .file("spec/plugin_spec.lua", "helper()\n")
        .write(
            "spec/helpers.lua",
            "function helper()\n  return 0\nend\n\nlocal function helper()\n  return 1\nend\n\nreturn {}\n",
        )
        .edge_after("file:spec/plugin_spec.lua", "calls", "function helper")
}

/// A module re-exports the first of two same-named `local` aliases; the two
/// swap places, so a caller through it reaches the other module's member,
/// though the module declares the same aliases as before.
pub fn lua_alias_follows_the_first_of_two_same_named_aliases_after_they_swap() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Lua])
        .file(
            "spec/internal/misc.lua",
            "local function undent(s)\n  return s\nend\n\nreturn { unindent = undent }\n",
        )
        .file(
            "spec/internal/other.lua",
            "local function dedent(s)\n  return s\nend\n\nreturn { unindent = dedent }\n",
        )
        .file(
            "spec/helpers.lua",
            "local unindent = require(\"spec.internal.misc\").unindent\nlocal unindent = require(\"spec.internal.other\").unindent\n\nreturn {}\n",
        )
        .file(
            "spec/handler_spec.lua",
            "local helpers = require \"spec.helpers\"\nlocal unindent = helpers.unindent\n\nunindent(\"a\")\n",
        )
        .write(
            "spec/helpers.lua",
            "local unindent = require(\"spec.internal.other\").unindent\nlocal unindent = require(\"spec.internal.misc\").unindent\n\nreturn {}\n",
        )
        .edge_before("file:spec/handler_spec.lua", "calls", "function undent")
        .edge_after("file:spec/handler_spec.lua", "calls", "function dedent")
}

/// Upstream #2144 (W4-R-01): a variable a test file assigns at its top
/// level stays out of another test's call of its name when it appears.
pub fn r_test_file_variable_stays_out_of_another_test_that_calls_its_name() -> Case {
    Case::new(Change::AddCompetitor, &[Language::R])
        .file("DESCRIPTION", "Package: demo\n")
        .file(
            "tests/testthat/test-other.R",
            "test_that(\"other\", {\n  fixture()\n})\n",
        )
        .write(
            "tests/testthat/test-scales.R",
            "fixture <- data.frame(y = 2)\ntest_that(\"works\", {\n  expect_true(TRUE)\n})\n",
        )
        .no_edge_after(
            "file:tests/testthat/test-other.R",
            "calls",
            "variable fixture",
        )
}

/// The same variable, its file renamed to a `helper-*.R` every test
/// sources: the other test's call reaches it.
pub fn r_test_variable_moved_to_a_helper_reaches_the_other_tests() -> Case {
    Case::new(Change::RenameSymbol, &[Language::R])
        .file("DESCRIPTION", "Package: demo\n")
        .file(
            "tests/testthat/test-data.R",
            "fixture <- data.frame(y = 2)\n",
        )
        .file(
            "tests/testthat/test-other.R",
            "test_that(\"other\", {\n  fixture()\n})\n",
        )
        .rename("tests/testthat/test-data.R", "tests/testthat/helper-data.R")
        .edge_after(
            "file:tests/testthat/test-other.R",
            "calls",
            "variable fixture",
        )
}

/// Upstream #2239 (W4-R-01): a binding named like a base function is what a
/// call of the name means only while its line holds a function; the line
/// gaining one (`c <- list(f = function(x) x)`, still the same variable)
/// brings the call in another file to it.
pub fn r_base_named_binding_whose_line_gains_a_function_reaches_its_callers() -> Case {
    Case::new(Change::BodyEdit, &[Language::R])
        .file("DESCRIPTION", "Package: demo\n")
        .file("R/data.R", "c <- list(f = 1)\n")
        .file("R/use.R", "use <- function() {\n  c(1, 2)\n}\n")
        .write("R/data.R", "c <- list(f = function(x) x)\n")
        .edge_after("function use", "calls", "variable c")
}

/// The reverse: the line losing its function takes the call off it.
pub fn r_base_named_binding_whose_line_loses_its_function_leaves_its_callers() -> Case {
    Case::new(Change::BodyEdit, &[Language::R])
        .file("DESCRIPTION", "Package: demo\n")
        .file("R/data.R", "c <- list(f = function(x) x)\n")
        .file("R/use.R", "use <- function() {\n  c(1, 2)\n}\n")
        .write("R/data.R", "c <- list(f = 1)\n")
        .edge_before("function use", "calls", "variable c")
        .no_edge_after("function use", "calls", "variable c")
}

/// Upstream #2177, the R part (W4-R-01): a plain call stays off an object's
/// method of its name when one appears.
pub fn r_bare_call_stays_off_an_objects_method_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::R])
        .file(
            "R/scale.R",
            "scale_limits <- function(x) {\n  range(x)\n}\n",
        )
        .write(
            "R/coord.R",
            "Coord <- ggproto(\"Coord\",\n  range = function(panel_params) {\n    list(x = panel_params$x$dimension())\n  }\n)\n",
        )
        .no_edge_after("function scale_limits", "calls", "method Coord::range")
}

/// Upstream #2145 (W4-DART-01): a bare Dart call in a subclass reaches the
/// method of a base its superclass, in another file, starts to extend: the
/// hierarchy is read from the class heads, and no node of `emit` changed.
pub fn dart_bare_call_reaches_a_method_of_a_base_its_superclass_starts_to_extend() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {\n  void emit(int x) {}\n}\n")
        .file("lib/cubit.dart", "class Cubit {}\n")
        .file(
            "lib/sub.dart",
            "class Sub extends Cubit {\n  void run() {\n    emit(1);\n  }\n}\n",
        )
        .write("lib/cubit.dart", "class Cubit extends Base {}\n")
        .edge_after("method Sub::run", "calls", "method Base::emit")
}

/// The reverse: the superclass stops extending the base, so the call leaves
/// its method.
pub fn dart_bare_call_leaves_a_method_of_a_base_its_superclass_stops_extending() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {\n  void emit(int x) {}\n}\n")
        .file("lib/cubit.dart", "class Cubit extends Base {}\n")
        .file(
            "lib/sub.dart",
            "class Sub extends Cubit {\n  void run() {\n    emit(1);\n  }\n}\n",
        )
        .write("lib/cubit.dart", "class Cubit {}\n")
        .edge_before("method Sub::run", "calls", "method Base::emit")
        .no_edge_after("method Sub::run", "calls", "method Base::emit")
}

/// An extension in another file comes to sit `on` the caller's base: a bare
/// call of its member there reaches it.
pub fn dart_bare_call_reaches_an_extension_member_once_it_is_on_the_callers_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {}\n\nclass Other {}\n")
        .file(
            "lib/ext.dart",
            "extension Helpers on Other {\n  void helper() {}\n}\n",
        )
        .file(
            "lib/sub.dart",
            "class Sub extends Base {\n  void run() {\n    helper();\n  }\n}\n",
        )
        .write(
            "lib/ext.dart",
            "extension Helpers on Base {\n  void helper() {}\n}\n",
        )
        .edge_after("method Sub::run", "calls", "method Helpers::helper")
}

/// A superclass in another file starts to extend the type an extension sits
/// `on`: a bare call of the extension's member in the subclass reaches it,
/// though neither the extension nor the call changed.
pub fn dart_bare_call_reaches_an_extension_member_once_its_superclass_extends_the_on_type() -> Case
{
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {}\n")
        .file(
            "lib/ext.dart",
            "extension Helpers on Base {\n  void helper() {}\n}\n",
        )
        .file("lib/cubit.dart", "class Cubit {}\n")
        .file(
            "lib/sub.dart",
            "class Sub extends Cubit {\n  void run() {\n    helper();\n  }\n}\n",
        )
        .write("lib/cubit.dart", "class Cubit extends Base {}\n")
        .edge_after("method Sub::run", "calls", "method Helpers::helper")
}

/// A superclass in another file moves a base nearer without the base
/// coming or going: the subclass's bare call now reaches that base's member
/// rather than a farther one through its mixin.
pub fn dart_bare_call_reaches_a_member_whose_base_its_superclass_brings_nearer() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/c.dart", "class C {\n  void m() {}\n}\n")
        .file(
            "lib/b.dart",
            "class B2 extends C {}\n\nclass B extends B2 {}\n",
        )
        .file(
            "lib/d.dart",
            "class D {\n  void m() {}\n}\n\nclass F extends D {}\n\nclass E extends F {}\n",
        )
        .file("lib/a.dart", "class A extends B {}\n")
        .file(
            "lib/sub.dart",
            "class Sub extends A with E {\n  void run() {\n    m();\n  }\n}\n",
        )
        .write("lib/a.dart", "class A extends C {}\n")
        .edge_before("method Sub::run", "calls", "method D::m")
        .edge_after("method Sub::run", "calls", "method C::m")
}

/// Forty types, so that a head naming them all pushes the rest of a
/// hierarchy past the resolver's walk limit.
static FORTY_TYPES: LazyLock<String> = LazyLock::new(|| {
    (1..=40)
        .map(|index| format!("class X{index} {{}}\n"))
        .collect()
});

/// A superclass in another file comes to implement forty types: the walk
/// from the subclass stops before reaching its mixin's base, so the bare
/// call leaves that base's member, though no member came or went.
pub fn dart_bare_call_leaves_a_member_pushed_past_the_walk_limit() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/m.dart", "class M1 {\n  void m() {}\n}\n\nclass M extends M1 {}\n")
        .file("lib/x.dart", FORTY_TYPES.as_str())
        .file("lib/mid.dart", "class Mid {}\n")
        .file(
            "lib/sub.dart",
            "class Sub extends Mid with M {\n  void run() {\n    m();\n  }\n}\n",
        )
        .write(
            "lib/mid.dart",
            "class Mid implements X1, X2, X3, X4, X5, X6, X7, X8, X9, X10, X11, X12, X13, X14, X15, X16, X17, X18, X19, X20, X21, X22, X23, X24, X25, X26, X27, X28, X29, X30, X31, X32, X33, X34, X35, X36, X37, X38, X39, X40 {}\n",
        )
        .edge_before("method Sub::run", "calls", "method M1::m")
        .no_edge_after("method Sub::run", "calls", "method M1::m")
}

/// Upstream #2207 (W4-DART-01): a core method name on an untyped receiver
/// reaches an extension's member only through a receiver named after the
/// type it is `on`; the extension moving to another type takes the call off
/// it, though no node of `endsWith` changed.
pub fn dart_core_method_call_leaves_an_extension_moved_to_another_type() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/rx.dart",
            "extension RxStringExt on Rx<String> {\n  bool endsWith(String s) => true;\n}\n",
        )
        .file(
            "lib/use.dart",
            "bool isVideo() {\n  var stringValue = load();\n  return stringValue.endsWith(\".avi\");\n}\n",
        )
        .write(
            "lib/rx.dart",
            "extension RxStringExt on Rx<int> {\n  bool endsWith(String s) => true;\n}\n",
        )
        .edge_before("function isVideo", "calls", "method RxStringExt::endsWith")
        .no_edge_after("function isVideo", "calls", "method RxStringExt::endsWith")
}

/// 501 parked calls of `endsWith` through receivers named after nothing,
/// so the shared tail retry skips that tail.
static ENDS_WITH_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("void noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("  missing{index}.endsWith('x');\n"));
    }
    text.push_str("}\n");
    text
});

/// The reverse, past the tail ceiling: the extension moving onto `Rx<String>`
/// brings `stringValue.endsWith(…)` to it, however many parked calls share
/// the `endsWith` tail.
pub fn dart_core_method_call_reaches_an_extension_moved_onto_its_type_past_the_tail_ceiling() -> Case
{
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/rx.dart",
            "extension RxStringExt on Rx<int> {\n  bool endsWith(String s) => true;\n}\n",
        )
        .file(
            "lib/use.dart",
            "bool isVideo() {\n  var stringValue = load();\n  return stringValue.endsWith(\".avi\");\n}\n",
        )
        .file("noise/noise.dart", ENDS_WITH_CALLS.as_str())
        .write(
            "lib/rx.dart",
            "extension RxStringExt on Rx<String> {\n  bool endsWith(String s) => true;\n}\n",
        )
        .edge_after("function isVideo", "calls", "method RxStringExt::endsWith")
}

/// Upstream #2237 (W4-DART-01): an extension losing its name applies only in
/// its own file, so a subclass's bare call in another file leaves its member.
pub fn dart_bare_call_leaves_an_extension_member_once_the_extension_is_unnamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/foo_type.dart", "class Foo {}\n")
        .file(
            "lib/foo.dart",
            "extension Helpers on Foo {\n  void helper() {}\n}\n",
        )
        .file(
            "lib/bar.dart",
            "class Bar extends Foo {\n  void run() {\n    helper();\n  }\n}\n",
        )
        .write(
            "lib/foo.dart",
            "extension on Foo {\n  void helper() {}\n}\n",
        )
        .edge_before("method Bar::run", "calls", "method Helpers::helper")
        .no_edge_after("method Bar::run", "calls", "method <anonymous>::helper")
}

// ----- a call through a class name and the heads it reads (W5-07) ------------
//
// Upstream #2180 (W3-06): a call through a class name reaches the method the
// class inherits, walking the supertypes each class's head names, read from
// source (`class_heads::declared_supertypes`). The `class_heads` lane records
// those heads, so a head changed in one file moves such a call in an
// unchanged file (W5-07).

/// A PHP class head spread over more lines: `extends Base` past the head's
/// third line, which `declared_supertypes` reads, though within the fifth,
/// which the `php_heads` rows record. A fresh index no longer walks to `Base`
/// for `Child::method()` and resolves it by the method's name instead.
pub fn php_inherited_static_call_follows_a_class_head_spread_past_its_third_line() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Base.php",
            "<?php\nnamespace App;\n\nclass Base\n{\n    public function method() {}\n}\n",
        )
        .file(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child extends Base\n{\n}\n",
        )
        .file(
            "src/User.php",
            "<?php\nnamespace App;\n\nclass User\n{\n    public function run()\n    {\n        Child::method();\n    }\n}\n",
        )
        .write(
            "src/Child.php",
            "<?php\nnamespace App;\n\nclass Child\n\n\nextends Base\n{\n}\n",
        )
        .edge_throughout("method App::User::run", "calls", "method App::Base::method")
}

/// A Python class's base changed in its own file moves `User.create()` in
/// an unchanged file to the new base's classmethod.
pub fn python_inherited_classmethod_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file(
            "pkg/base.py",
            "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .file(
            "pkg/other.py",
            "class Other:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass User(Base):\n    pass\n",
        )
        .file(
            "pkg/service.py",
            "from pkg.user import User\n\n\ndef make():\n    return User.create()\n",
        )
        .write(
            "pkg/user.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass User(Other):\n    pass\n",
        )
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// The same with the call beside its old target: the unchanged file holds
/// both `Base.create` and `User.create()`, so the edge to move is a same-file
/// one.
pub fn python_inherited_classmethod_follows_a_base_change_beside_its_old_target() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file(
            "pkg/service.py",
            "from pkg.user import User\n\n\nclass Base:\n    @classmethod\n    def create(cls):\n        return cls()\n\n\ndef make():\n    return User.create()\n",
        )
        .file(
            "pkg/other.py",
            "class Other:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.service import Base\nfrom pkg.other import Other\n\n\nclass User(Base):\n    pass\n",
        )
        .write(
            "pkg/user.py",
            "from pkg.service import Base\nfrom pkg.other import Other\n\n\nclass User(Other):\n    pass\n",
        )
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// The same for a Java class's `extends`.
pub fn java_inherited_static_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/app/Base.java",
            "package app;\n\npublic class Base {\n    public static int create() { return 1; }\n}\n",
        )
        .file(
            "src/app/Other.java",
            "package app;\n\npublic class Other {\n    public static int create() { return 2; }\n}\n",
        )
        .file(
            "src/app/User.java",
            "package app;\n\npublic class User extends Base {\n}\n",
        )
        .file(
            "src/app/Service.java",
            "package app;\n\npublic class Service {\n    public int make() {\n        return User.create();\n    }\n}\n",
        )
        .write(
            "src/app/User.java",
            "package app;\n\npublic class User extends Other {\n}\n",
        )
        .edge_before("method app::Service::make", "calls", "method app::Base::create")
        .edge_after("method app::Service::make", "calls", "method app::Other::create")
}

/// The same for a C# class's base list.
pub fn csharp_inherited_static_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Base.cs",
            "namespace App;\n\npublic class Base\n{\n    public static int Create() => 1;\n}\n",
        )
        .file(
            "src/Other.cs",
            "namespace App;\n\npublic class Other\n{\n    public static int Create() => 2;\n}\n",
        )
        .file(
            "src/User.cs",
            "namespace App;\n\npublic class User : Base\n{\n}\n",
        )
        .file(
            "src/Service.cs",
            "namespace App;\n\npublic class Service\n{\n    public int Make()\n    {\n        return User.Create();\n    }\n}\n",
        )
        .write(
            "src/User.cs",
            "namespace App;\n\npublic class User : Other\n{\n}\n",
        )
        .edge_before("method App::Service::Make", "calls", "method App::Base::Create")
        .edge_after("method App::Service::Make", "calls", "method App::Other::Create")
}

/// The same for a Swift class's superclass.
pub fn swift_inherited_class_method_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Swift])
        .file(
            "src/Base.swift",
            "class Base {\n    class func create() -> Int { return 1 }\n}\n",
        )
        .file(
            "src/Other.swift",
            "class Other {\n    class func create() -> Int { return 2 }\n}\n",
        )
        .file("src/User.swift", "class User: Base {\n}\n")
        .file(
            "src/Service.swift",
            "func make() -> Int {\n    return User.create()\n}\n",
        )
        .write("src/User.swift", "class User: Other {\n}\n")
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// The same for a Ruby class's `<` (converged before W5-07: the Ruby lane's
/// rows hold the same heads).
pub fn ruby_class_method_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Ruby])
        .file(
            "lib/base.rb",
            "class Base\n  def self.create\n    new\n  end\nend\n",
        )
        .file(
            "lib/other.rb",
            "class Other\n  def self.create\n    new\n  end\nend\n",
        )
        .file("lib/user.rb", "class User < Base\nend\n")
        .file("lib/service.rb", "def make\n  User.create\nend\n")
        .write("lib/user.rb", "class User < Other\nend\n")
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// The same for a Kotlin class's supertype list.
pub fn kotlin_class_name_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "src/Base.kt",
            "open class Base {\n    fun create(): Int = 1\n}\n",
        )
        .file(
            "src/Other.kt",
            "open class Other {\n    fun create(): Int = 2\n}\n",
        )
        .file("src/User.kt", "class User : Base()\n")
        .file("src/Service.kt", "fun make(): Int = User.create()\n")
        .write("src/User.kt", "class User : Other()\n")
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// Not through the walk: a Dart call through a type's name reaches that
/// type's own static members only (W4-DART-03c), so `User.create()` stays
/// unresolved whichever base `User` extends.
pub fn dart_class_name_call_reaches_no_inherited_static_member() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file(
            "lib/base.dart",
            "class Base {\n  static int create() => 1;\n}\n",
        )
        .file(
            "lib/other.dart",
            "class Other {\n  static int create() => 2;\n}\n",
        )
        .file(
            "lib/user.dart",
            "import 'base.dart';\nimport 'other.dart';\n\nclass User extends Base {}\n",
        )
        .file(
            "lib/service.dart",
            "import 'user.dart';\n\nint make() => User.create();\n",
        )
        .write(
            "lib/user.dart",
            "import 'base.dart';\nimport 'other.dart';\n\nclass User extends Other {}\n",
        )
        .no_edge_after("function make", "calls", "method Base::create")
        .no_edge_after("function make", "calls", "method Other::create")
}

/// The same for a Scala object's `extends`.
pub fn scala_object_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/Base.scala",
            "abstract class Base {\n  def create(): Int = 1\n}\n",
        )
        .file(
            "src/Other.scala",
            "abstract class Other {\n  def create(): Int = 2\n}\n",
        )
        .file("src/User.scala", "object User extends Base\n")
        .file(
            "src/Service.scala",
            "object Service {\n  def make(): Int = User.create()\n}\n",
        )
        .write("src/User.scala", "object User extends Other\n")
        .edge_before("method Service::make", "calls", "method Base::create")
        .edge_after("method Service::make", "calls", "method Other::create")
}

/// The same for an Objective-C `@interface`'s superclass (converged before
/// W5-07: the Objective-C lane's rows hold the same superclass).
pub fn objc_class_message_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "src/Base.m",
            "@interface Base : NSObject\n+ (int)create;\n@end\n\n@implementation Base\n+ (int)create { return 1; }\n@end\n",
        )
        .file(
            "src/Other.m",
            "@interface Other : NSObject\n+ (int)create;\n@end\n\n@implementation Other\n+ (int)create { return 2; }\n@end\n",
        )
        .file(
            "src/User.m",
            "@interface User : Base\n@end\n\n@implementation User\n@end\n",
        )
        .file(
            "src/Service.m",
            "int make(void) {\n    return [User create];\n}\n",
        )
        .write(
            "src/User.m",
            "@interface User : Other\n@end\n\n@implementation User\n@end\n",
        )
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// The same for a Pascal `class(…)` head.
pub fn pascal_class_function_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Pascal])
        .file(
            "Base.pas",
            "unit Base;\ninterface\ntype\n  TBase = class\n    class function Make: Integer;\n  end;\nimplementation\nclass function TBase.Make: Integer;\nbegin\n  Result := 1;\nend;\nend.\n",
        )
        .file(
            "Other.pas",
            "unit Other;\ninterface\ntype\n  TOther = class\n    class function Make: Integer;\n  end;\nimplementation\nclass function TOther.Make: Integer;\nbegin\n  Result := 2;\nend;\nend.\n",
        )
        .file(
            "User.pas",
            "unit User;\ninterface\nuses Base, Other;\ntype\n  TUser = class(TBase)\n  end;\nimplementation\nend.\n",
        )
        .file(
            "Service.pas",
            "unit Service;\ninterface\nuses User;\nfunction Run: Integer;\nimplementation\nfunction Run: Integer;\nbegin\n  Result := TUser.Make();\nend;\nend.\n",
        )
        .write(
            "User.pas",
            "unit User;\ninterface\nuses Base, Other;\ntype\n  TUser = class(TOther)\n  end;\nimplementation\nend.\n",
        )
        .edge_before("file:Service.pas", "calls", "method TBase::Make")
        .edge_after("file:Service.pas", "calls", "method TOther::Make")
}

/// Not through the walk: a TS call of a static method through an imported
/// class reads as an instantiation of that class, whichever base it
/// extends.
pub fn typescript_static_call_through_an_imported_subclass_reads_no_head() -> Case {
    Case::new(Change::EditHeritage, &[Language::TypeScript])
        .file(
            "src/base.ts",
            "export class Base {\n  static create(): number {\n    return 1;\n  }\n}\n",
        )
        .file(
            "src/other.ts",
            "export class Other {\n  static create(): number {\n    return 2;\n  }\n}\n",
        )
        .file(
            "src/user.ts",
            "import { Base } from './base';\nimport { Other } from './other';\n\nexport class User extends Base {}\n",
        )
        .file(
            "src/service.ts",
            "import { User } from './user';\n\nexport function make(): number {\n  return User.create();\n}\n",
        )
        .write(
            "src/user.ts",
            "import { Base } from './base';\nimport { Other } from './other';\n\nexport class User extends Other {}\n",
        )
        .edge_throughout("function make", "instantiates", "class User")
}

const PY_BASE: &str = "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n";
const PY_OTHER: &str =
    "class Other:\n    @classmethod\n    def create(cls):\n        return cls()\n";
const PY_SERVICE: &str = "from pkg.user import User\n\n\ndef make():\n    return User.create()\n";

/// The base's file deleted: `User.create()` leaves its classmethod.
pub fn python_inherited_classmethod_leaves_a_deleted_base() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Python])
        .file("pkg/base.py", PY_BASE)
        .file(
            "pkg/user.py",
            "from pkg.base import Base\n\n\nclass User(Base):\n    pass\n",
        )
        .file("pkg/service.py", PY_SERVICE)
        .edge_before("function make", "calls", "method Base::create")
        .remove("pkg/base.py")
}

/// Two levels up: the middle class's base changes, and `User.create()`
/// through it follows.
pub fn python_inherited_classmethod_follows_a_middle_class_base_change() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file("pkg/base.py", PY_BASE)
        .file("pkg/other.py", PY_OTHER)
        .file(
            "pkg/mid.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass Mid(Base):\n    pass\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.mid import Mid\n\n\nclass User(Mid):\n    pass\n",
        )
        .file("pkg/service.py", PY_SERVICE)
        .write(
            "pkg/mid.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass Mid(Other):\n    pass\n",
        )
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// A call parked while no class above `User` has `create` is retried once
/// the middle class's new base has it: the walk from `User` reads the middle
/// class's head, one level down from the change.
pub fn python_parked_class_method_reaches_a_base_its_middle_class_gains() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file("pkg/base.py", "class Base:\n    pass\n")
        .file("pkg/other.py", PY_OTHER)
        .file(
            "pkg/mid.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass Mid(Base):\n    pass\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.mid import Mid\n\n\nclass User(Mid):\n    pass\n",
        )
        .file("pkg/service.py", PY_SERVICE)
        .write(
            "pkg/mid.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass Mid(Other):\n    pass\n",
        )
        .edge_after("function make", "calls", "method Other::create")
}

/// The changed head's old and new targets two levels above it: `User`
/// moves from a middle class over `Base` to one over `Other`.
pub fn python_inherited_classmethod_follows_a_base_two_levels_above_the_changed_head() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file("pkg/base.py", PY_BASE)
        .file("pkg/other.py", PY_OTHER)
        .file(
            "pkg/mids.py",
            "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass Mid1(Base):\n    pass\n\n\nclass Mid2(Other):\n    pass\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.mids import Mid1, Mid2\n\n\nclass User(Mid1):\n    pass\n",
        )
        .file("pkg/service.py", PY_SERVICE)
        .write(
            "pkg/user.py",
            "from pkg.mids import Mid1, Mid2\n\n\nclass User(Mid2):\n    pass\n",
        )
        .edge_before("function make", "calls", "method Base::create")
        .edge_after("function make", "calls", "method Other::create")
}

/// Five levels up the base is in reach; the fourth class's head changed to
/// one more level puts it at the sixth, past the walk.
pub fn python_inherited_classmethod_leaves_a_base_pushed_past_five_levels() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file("pkg/base.py", PY_BASE)
        .file(
            "pkg/levels.py",
            "from pkg.base import Base\n\n\nclass L5(Base):\n    pass\n\n\nclass L3(L4):\n    pass\n\n\nclass L2(L3):\n    pass\n\n\nclass L1(L2):\n    pass\n",
        )
        .file(
            "pkg/l4.py",
            "from pkg.base import Base\n\n\nclass L4(Base):\n    pass\n",
        )
        .file(
            "pkg/user.py",
            "from pkg.levels import L1\n\n\nclass User(L1):\n    pass\n",
        )
        .file("pkg/service.py", PY_SERVICE)
        .edge_before("function make", "calls", "method Base::create")
        .write(
            "pkg/l4.py",
            "from pkg.levels import L5\n\n\nclass L4(L5):\n    pass\n",
        )
}

/// #897: a Kotlin function's value reference follows the object constant it
/// reads.
pub fn kotlin_object_constant_reader_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Kotlin])
        .file(
            "src/Config.kt",
            "object Config {\n    const val MAX_RETRIES = 3\n    const val MAX_WAIT = 30\n\n    fun limit(): Int = MAX_RETRIES\n}\n",
        )
        .write(
            "src/Config.kt",
            "object Config {\n    const val MAX_RETRIES = 3\n    const val MAX_WAIT = 30\n\n    fun limit(): Int = MAX_WAIT\n}\n",
        )
        .edge_before("method Config::limit", "references", "constant Config::MAX_RETRIES")
        .edge_after("method Config::limit", "references", "constant Config::MAX_WAIT")
}

/// #897: a Ruby method's value reference follows the class constant it reads.
pub fn ruby_class_constant_reader_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Ruby])
        .file(
            "lib/config.rb",
            "class Config\n  MAX_RETRIES = 3\n  MAX_WAIT = 30\n\n  def limit\n    MAX_RETRIES\n  end\nend\n",
        )
        .write(
            "lib/config.rb",
            "class Config\n  MAX_RETRIES = 3\n  MAX_WAIT = 30\n\n  def limit\n    MAX_WAIT\n  end\nend\n",
        )
        .edge_before("method Config::limit", "references", "variable Config::MAX_RETRIES")
        .edge_after("method Config::limit", "references", "variable Config::MAX_WAIT")
}

/// G4: a static field read references the class it reads through.
pub fn java_static_field_read_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file(
            "src/app/Limits.java",
            "package app;\n\nclass Limits {\n  static final int MAX = 10;\n}\n",
        )
        .file(
            "src/app/Bounds.java",
            "package app;\n\nclass Bounds {\n  static final int MAX = 20;\n}\n",
        )
        .file(
            "src/app/Reader.java",
            "package app;\n\nclass Reader {\n  int peek() {\n    return Limits.MAX;\n  }\n}\n",
        )
        .write(
            "src/app/Reader.java",
            "package app;\n\nclass Reader {\n  int peek() {\n    return Bounds.MAX;\n  }\n}\n",
        )
        .edge_before(
            "method app::Reader::peek",
            "references",
            "class app::Limits",
        )
        .edge_after(
            "method app::Reader::peek",
            "references",
            "class app::Bounds",
        )
}

/// G4: a C++ read through a capitalized scope references the type it names.
pub fn cpp_scoped_read_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "src/colors.hpp",
            "enum Color { RED, GREEN };\nenum Shade { DARK, LIGHT };\n",
        )
        .file(
            "src/paint.cpp",
            "#include \"colors.hpp\"\n\nint paint() {\n  return Color::RED;\n}\n",
        )
        .write(
            "src/paint.cpp",
            "#include \"colors.hpp\"\n\nint paint() {\n  return Shade::DARK;\n}\n",
        )
        .edge_before("function paint", "references", "enum Color")
        .edge_after("function paint", "references", "enum Shade")
}

/// #895: a Go package value reads what its initializer names.
pub fn go_value_initializer_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", "module example.com/limits\n\ngo 1.22\n")
        .file(
            "limits.go",
            "package limits\n\nconst BaseLimit = 10\nconst WideLimit = 50\n\nvar ScaledLimit = BaseLimit * 2\n",
        )
        .write(
            "limits.go",
            "package limits\n\nconst BaseLimit = 10\nconst WideLimit = 50\n\nvar ScaledLimit = WideLimit * 2\n",
        )
        .edge_before("variable ScaledLimit", "references", "constant BaseLimit")
        .edge_after("variable ScaledLimit", "references", "constant WideLimit")
}

/// #2268: an Astro frontmatter's top-level call is the component's.
pub fn astro_frontmatter_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Astro])
        .file(
            "src/lib/posts.ts",
            "export function getPosts(): string[] {\n  return [];\n}\n\nexport function getDrafts(): string[] {\n  return [];\n}\n",
        )
        .file(
            "src/pages/index.astro",
            "---\nimport { getPosts, getDrafts } from '../lib/posts';\nconst posts = getPosts();\n---\n<ul>{posts.length}</ul>\n",
        )
        .write(
            "src/pages/index.astro",
            "---\nimport { getPosts, getDrafts } from '../lib/posts';\nconst posts = getDrafts();\n---\n<ul>{posts.length}</ul>\n",
        )
        .edge_before(
            "component src/pages/index.astro::index",
            "calls",
            "function getPosts",
        )
        .edge_after(
            "component src/pages/index.astro::index",
            "calls",
            "function getDrafts",
        )
}

/// #895: a function's value reference follows the constant its body reads.
pub fn typescript_value_reader_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "src/paging.ts",
            "export const PAGE_SIZE = 20;\nexport const PAGE_LIMIT = 50;\n\nexport function pages(total: number): number {\n  return Math.ceil(total / PAGE_SIZE);\n}\n",
        )
        .write(
            "src/paging.ts",
            "export const PAGE_SIZE = 20;\nexport const PAGE_LIMIT = 50;\n\nexport function pages(total: number): number {\n  return Math.ceil(total / PAGE_LIMIT);\n}\n",
        )
        .edge_before("function pages", "references", "constant PAGE_SIZE")
        .edge_after("function pages", "references", "constant PAGE_LIMIT")
}

/// G13: a C++ call keeps its scope, so `ns::compute()` and `alt::compute()`
/// reach different functions of one name.
pub fn cpp_qualified_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "src/compute.hpp",
            "namespace ns {\nint compute();\n}\n\nnamespace alt {\nint compute();\n}\n",
        )
        .file(
            "src/compute.cpp",
            "#include \"compute.hpp\"\n\nnamespace ns {\nint compute() { return 1; }\n}\n\nnamespace alt {\nint compute() { return 2; }\n}\n",
        )
        .file(
            "src/run.cpp",
            "#include \"compute.hpp\"\n\nint run() {\n  return ns::compute();\n}\n",
        )
        .write(
            "src/run.cpp",
            "#include \"compute.hpp\"\n\nint run() {\n  return alt::compute();\n}\n",
        )
        .edge_before("function run", "calls", "function ns::compute")
        .edge_after("function run", "calls", "function alt::compute")
}

pub fn vue_component_template_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue])
        .file("package.json", "{\"name\":\"shop\",\"dependencies\":{\"vue\":\"3.4.0\"}}\n")
        .file(
            "src/components/Badge.vue",
            "<script setup lang=\"ts\">\ndefineProps<{ label: string }>();\n</script>\n\n<template>\n  <span class=\"badge\">{{ label }}</span>\n</template>\n",
        )
        .file(
            "src/App.vue",
            "<script setup lang=\"ts\">\nimport Badge from './components/Badge.vue';\n</script>\n\n<template>\n  <Badge label=\"new\" />\n</template>\n",
        )
        .write(
            "src/components/Badge.vue",
            "<script setup lang=\"ts\">\ndefineProps<{ label: string }>();\n</script>\n\n<template>\n  <strong class=\"badge\">{{ label }}</strong>\n</template>\n",
        )
        .edge_throughout(
            "component src/App.vue::App",
            "references",
            "component src/components/Badge.vue::Badge",
        )
}

pub fn liquid_snippet_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Liquid])
        .file(
            "templates/product.liquid",
            "<div class=\"product\">\n  {% render 'price' %}\n</div>\n",
        )
        .file(
            "snippets/price.liquid",
            "<span class=\"price\">{{ product.price }}</span>\n",
        )
        .write(
            "snippets/price.liquid",
            "<strong class=\"price\">{{ product.price | money }}</strong>\n",
        )
        .edge_throughout(
            "file:templates/product.liquid",
            "references",
            "file:snippets/price.liquid",
        )
}

pub fn luau_module_function_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Luau])
        .file(
            "src/Inventory.luau",
            "local Inventory = {}\n\nfunction Inventory.count(items: { string }): number\n  return #items\nend\n\nreturn Inventory\n",
        )
        .file(
            "src/Shop.luau",
            "local Inventory = require(script.Parent.Inventory)\n\nlocal function stock(items: { string }): number\n  return Inventory.count(items)\nend\n\nreturn stock\n",
        )
        .write(
            "src/Inventory.luau",
            "local Inventory = {}\n\nfunction Inventory.count(items: { string }): number\n  return #items + 0\nend\n\nreturn Inventory\n",
        )
        .edge_throughout("function stock", "calls", "method Inventory::count")
        .edge_throughout("file:src/Shop.luau", "imports", "file:src/Inventory.luau")
}

/// G8: a Vue script's function bodies are extracted, so editing a call in one
/// moves its edge.
pub fn vue_script_function_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue, Language::TypeScript])
        .file(
            "src/actions.ts",
            "export function run(): number {\n  return 1;\n}\n\nexport function stop(): number {\n  return 0;\n}\n",
        )
        .file(
            "src/App.vue",
            "<template>\n  <button @click=\"go\">go</button>\n</template>\n\n<script setup lang=\"ts\">\nimport { run, stop } from './actions';\n\nfunction go() {\n  return run();\n}\n</script>\n",
        )
        .write(
            "src/App.vue",
            "<template>\n  <button @click=\"go\">go</button>\n</template>\n\n<script setup lang=\"ts\">\nimport { run, stop } from './actions';\n\nfunction go() {\n  return stop();\n}\n</script>\n",
        )
        .edge_before("function go", "calls", "function run")
        .edge_after("function go", "calls", "function stop")
}

/// G9 / upstream #2268: a Svelte instance script's top-level call is the
/// component's.
pub fn svelte_instance_script_call_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Svelte, Language::TypeScript])
        .file(
            "src/setup.ts",
            "export function doSetup(): void {}\n\nexport function doTeardown(): void {}\n",
        )
        .file(
            "src/Widget.svelte",
            "<script lang=\"ts\">\n  import { doSetup, doTeardown } from './setup';\n  doSetup();\n</script>\n\n<p>hi</p>\n",
        )
        .write(
            "src/Widget.svelte",
            "<script lang=\"ts\">\n  import { doSetup, doTeardown } from './setup';\n  doTeardown();\n</script>\n\n<p>hi</p>\n",
        )
        .edge_before("component src/Widget.svelte::Widget", "calls", "function doSetup")
        .edge_after("component src/Widget.svelte::Widget", "calls", "function doTeardown")
}

/// G11: a Ruby call keeps its receiver, so a local built by `Logger.new`
/// types it (ahead of `Audit`'s same-named methods), and an edit moves the
/// edge to the other method.
pub fn ruby_receiver_call_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Ruby])
        .file(
            "audit.rb",
            "class Audit\n  def log(m)\n    m\n  end\n\n  def warn(m)\n    m\n  end\nend\n",
        )
        .file(
            "logger.rb",
            "class Logger\n  def log(m)\n    m\n  end\n\n  def warn(m)\n    m\n  end\nend\n",
        )
        .file(
            "service.rb",
            "require_relative \"logger\"\n\nclass Service\n  def run\n    lg = Logger.new\n    lg.log(1)\n  end\nend\n",
        )
        .write(
            "service.rb",
            "require_relative \"logger\"\n\nclass Service\n  def run\n    lg = Logger.new\n    lg.warn(1)\n  end\nend\n",
        )
        .edge_before("method Service::run", "calls", "method Logger::log")
        .edge_after("method Service::run", "calls", "method Logger::warn")
}

pub fn objc_implementation_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::ObjC])
        .file(
            "Greeter.h",
            "#import <Foundation/Foundation.h>\n\n@interface Greeter : NSObject\n- (NSString *)greet:(NSString *)name;\n@end\n",
        )
        .file(
            "Greeter.m",
            "#import \"Greeter.h\"\n\n@implementation Greeter\n- (NSString *)greet:(NSString *)name {\n    return name;\n}\n@end\n",
        )
        .file(
            "main.m",
            "#import \"Greeter.h\"\n\nint main(void) {\n    Greeter *greeter = [[Greeter alloc] init];\n    [greeter greet:@\"there\"];\n    return 0;\n}\n",
        )
        .write(
            "Greeter.m",
            "#import \"Greeter.h\"\n\n@implementation Greeter\n- (NSString *)greet:(NSString *)name {\n    return [name copy];\n}\n@end\n",
        )
        .edge_throughout("file:main.m", "imports", "file:Greeter.h")
        // `[[Greeter alloc] init]` is a message to the class: a reference to
        // it (G5), as upstream records it.
        .edge_throughout("function main", "references", "class Greeter")
}

pub fn gdscript_autoload_method_body_edit() -> Case {
    Case::new(
        Change::BodyEdit,
        &[Language::Gdscript, Language::GodotProject, Language::GodotScene],
    )
    .file(
        "project.godot",
        "config_version=5\n\n[application]\n\nconfig/name=\"Fixture\"\n\n[autoload]\n\nGameFlow=\"*res://game_flow.gd\"\n",
    )
    .file(
        "main.tscn",
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://stage.gd\" id=\"1_stage\"]\n\n[node name=\"Main\" type=\"Node\"]\nscript = ExtResource(\"1_stage\")\n",
    )
    .file(
        "game_flow.gd",
        "extends Node\n\nfunc return_to_map() -> void:\n\tpass\n",
    )
    .file(
        "stage.gd",
        "extends Node\n\nfunc _goto_map() -> void:\n\tGameFlow.return_to_map()\n",
    )
    .write(
        "game_flow.gd",
        "extends Node\n\nfunc return_to_map() -> void:\n\tprint(\"map\")\n",
    )
    .edge_throughout("function _goto_map", "calls", "function return_to_map")
}

// ----- a declaration is renamed under unchanged references -----------------

pub fn typescript_rename_an_imported_function() -> Case {
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file(
            "src/util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { helper } from './util';\n\nexport function main(): number {\n  return helper();\n}\n",
        )
        .write(
            "src/util.ts",
            "export function assist(): number {\n  return 1;\n}\n",
        )
        .edge_before("function main", "calls", "function helper")
}

pub fn javascript_rename_a_function_used_by_import_and_require() -> Case {
    Case::new(Change::RenameSymbol, &[Language::JavaScript])
        .file(
            "lib/util.js",
            "export function format(value) {\n  return String(value);\n}\n",
        )
        .file(
            "main.js",
            "import { format } from './lib/util.js';\n\nexport function show(value) {\n  return format(value);\n}\n",
        )
        .file(
            "legacy.js",
            "const util = require('./lib/util.js');\n\nfunction legacy(value) {\n  return util.format(value);\n}\n\nmodule.exports = { legacy };\n",
        )
        .write(
            "lib/util.js",
            "export function render(value) {\n  return String(value);\n}\n",
        )
        .edge_before("function show", "calls", "function format")
        .edge_before("function legacy", "calls", "function format")
}

pub fn cpp_rename_a_member_function() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Cpp])
        .file(
            "widget.hpp",
            "class Widget {\npublic:\n    int size() const;\n};\n",
        )
        .file(
            "widget.cpp",
            "#include \"widget.hpp\"\n\nint Widget::size() const {\n    return 1;\n}\n",
        )
        .file(
            "main.cpp",
            "#include \"widget.hpp\"\n\nint measure(const Widget &widget) {\n    return widget.size();\n}\n",
        )
        .write(
            "widget.hpp",
            "class Widget {\npublic:\n    int area() const;\n};\n",
        )
        .write(
            "widget.cpp",
            "#include \"widget.hpp\"\n\nint Widget::area() const {\n    return 1;\n}\n",
        )
        .edge_before("function measure", "calls", "method Widget::size")
}

pub fn csharp_rename_a_called_method() -> Case {
    Case::new(Change::RenameSymbol, &[Language::CSharp])
        .file(
            "Models/Widget.cs",
            "namespace Shop.Models\n{\n    public class Widget\n    {\n        public int Size() { return 1; }\n    }\n}\n",
        )
        .file(
            "Models/Factory.cs",
            "namespace Shop.Models\n{\n    public class Factory\n    {\n        public int Build()\n        {\n            var widget = new Widget();\n            return widget.Size();\n        }\n    }\n}\n",
        )
        .write(
            "Models/Widget.cs",
            "namespace Shop.Models\n{\n    public class Widget\n    {\n        public int Measure() { return 1; }\n    }\n}\n",
        )
        .edge_before(
            "method Shop.Models::Factory::Build",
            "calls",
            "method Shop.Models::Widget::Size",
        )
}

pub fn svelte_rename_an_imported_function() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Svelte])
        .file(
            "src/format.ts",
            "export function formatName(name: string): string {\n  return name.trim();\n}\n",
        )
        .file(
            "src/Hello.svelte",
            "<script lang=\"ts\">\n  import { formatName } from './format';\n  export let name: string;\n  const shown = formatName(name);\n</script>\n\n<h1>Hello {shown}</h1>\n",
        )
        .write(
            "src/format.ts",
            "export function tidyName(name: string): string {\n  return name.trim();\n}\n",
        )
        // An instance script's top-level constant's initializer is the
        // component's call (upstream #2268).
        .edge_before("component src/Hello.svelte::Hello", "calls", "function formatName")
}

pub fn pascal_rename_a_unit_function() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Pascal])
        .file(
            "Utils.pas",
            "unit Utils;\n\ninterface\n\nfunction Double(Value: Integer): Integer;\n\nimplementation\n\nfunction Double(Value: Integer): Integer;\nbegin\n  Result := Value * 2;\nend;\n\nend.\n",
        )
        .file(
            "Main.pas",
            "unit Main;\n\ninterface\n\nuses Utils;\n\nfunction Quadruple(Value: Integer): Integer;\n\nimplementation\n\nfunction Quadruple(Value: Integer): Integer;\nbegin\n  Result := Double(Double(Value));\nend;\n\nend.\n",
        )
        .write(
            "Utils.pas",
            "unit Utils;\n\ninterface\n\nfunction Twice(Value: Integer): Integer;\n\nimplementation\n\nfunction Twice(Value: Integer): Integer;\nbegin\n  Result := Value * 2;\nend;\n\nend.\n",
        )
        .edge_before("file:Main.pas", "calls", "function Double")
}

pub fn scala_rename_a_called_method() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Scala])
        .file(
            "src/Shapes.scala",
            "package shapes\n\nclass Circle(radius: Double) {\n  def area(): Double = radius * radius * 3.14\n}\n",
        )
        .file(
            "src/Main.scala",
            "package shapes\n\nobject Main {\n  def total(): Double = {\n    val circle = new Circle(1.0)\n    circle.area()\n  }\n}\n",
        )
        .write(
            "src/Shapes.scala",
            "package shapes\n\nclass Circle(radius: Double) {\n  def surface(): Double = radius * radius * 3.14\n}\n",
        )
        .edge_before("method Main::total", "calls", "method Circle::area")
}

pub fn terraform_rename_a_variable() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Terraform])
        .file(
            "variables.tf",
            "variable \"region\" {\n  type    = string\n  default = \"us-east-1\"\n}\n",
        )
        .file(
            "main.tf",
            "provider \"aws\" {\n  region = var.region\n}\n\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"logs-${var.region}\"\n}\n",
        )
        .write(
            "variables.tf",
            "variable \"aws_region\" {\n  type    = string\n  default = \"us-east-1\"\n}\n",
        )
        .edge_before(
            "class aws_s3_bucket.logs",
            "references",
            "variable var.region",
        )
}

/// Upstream #2178 (b): a bare call another module's function answers only
/// through `-import`: the imported module gains it, and the call links it,
/// never the namesake of a module it does not import.
pub fn erlang_imported_call_reaches_a_function_its_module_gains() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Erlang])
        .file(
            "test/req_SUITE.erl",
            "-module(req_SUITE).\n-export([other/0]).\n\nother() -> ok.\n",
        )
        .file(
            "test/compress_SUITE.erl",
            "-module(compress_SUITE).\n-export([do_get/3]).\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n",
        )
        .file(
            "test/static_handler_SUITE.erl",
            "-module(static_handler_SUITE).\n-export([etag/1]).\n-import(req_SUITE, [do_get/3]).\n\netag(Config) -> do_get(\"/etag\", [], Config).\n",
        )
        .write(
            "test/req_SUITE.erl",
            "-module(req_SUITE).\n-export([other/0, do_get/3]).\n\nother() -> ok.\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n",
        )
        .edge_after(
            "function static_handler_SUITE::etag/1",
            "calls",
            "function req_SUITE::do_get/3",
        )
        .no_edge_after(
            "function static_handler_SUITE::etag/1",
            "calls",
            "function compress_SUITE::do_get/3",
        )
}

/// A bare call with no `-import` reaches a function a header gains, and no
/// other module's.
pub fn erlang_bare_call_reaches_a_function_its_header_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Erlang])
        .file("include/helpers.hrl", "-define(TIMEOUT, 100).\n")
        .file(
            "src/other.erl",
            "-module(other).\n-export([helper/1]).\n\nhelper(Config) -> Config.\n",
        )
        .file(
            "src/plain.erl",
            "-module(plain).\n-export([run/1]).\n-include(\"../include/helpers.hrl\").\n\nrun(Config) -> helper(Config).\n",
        )
        .write(
            "include/helpers.hrl",
            "-define(TIMEOUT, 100).\n\nhelper(Config) -> Config.\n",
        )
        .edge_after("function plain::run/1", "calls", "function helper/1")
        .no_edge_after("function plain::run/1", "calls", "function other::helper/1")
}

/// 501 parked bare calls of `do_get/3` that no module imports.
static PARKED_DO_GET_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("-module(noise).\n-export([]).\n\n");
    for index in 0..501 {
        text.push_str(&format!("n{index}(C) -> do_get(\"/{index}\", [], C).\n"));
    }
    text
});

/// The imported module gains the function past the tail ceiling: 501 parked
/// calls share the tail `do_get`.
pub fn erlang_imported_call_reaches_a_function_its_module_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Erlang])
        .file(
            "test/req_SUITE.erl",
            "-module(req_SUITE).\n-export([other/0]).\n\nother() -> ok.\n",
        )
        .file(
            "test/static_handler_SUITE.erl",
            "-module(static_handler_SUITE).\n-export([etag/1]).\n-import(req_SUITE, [do_get/3]).\n\netag(Config) -> do_get(\"/etag\", [], Config).\n",
        )
        .file("test/noise.erl", PARKED_DO_GET_CALLS.as_str())
        .write(
            "test/req_SUITE.erl",
            "-module(req_SUITE).\n-export([other/0, do_get/3]).\n\nother() -> ok.\n\ndo_get(Path, Headers, Config) -> {Path, Headers, Config}.\n",
        )
        .edge_after(
            "function static_handler_SUITE::etag/1",
            "calls",
            "function req_SUITE::do_get/3",
        )
}

pub fn erlang_rename_a_remotely_called_function() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Erlang])
        .file(
            "src/util.erl",
            "-module(util).\n-export([double/1]).\n\ndouble(X) -> X * 2.\n",
        )
        .file(
            "src/main.erl",
            "-module(main).\n-export([run/1]).\n\nrun(X) -> util:double(X).\n",
        )
        .write(
            "src/util.erl",
            "-module(util).\n-export([twice/1]).\n\ntwice(X) -> X * 2.\n",
        )
        .edge_before("function main::run/1", "calls", "function util::double/1")
}

pub fn mybatis_rename_an_included_fragment() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Xml])
        .file(
            "mapper/CommonMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.CommonMapper\">\n  <sql id=\"userColumns\">id, name, email</sql>\n</mapper>\n",
        )
        .file(
            "mapper/UserMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.UserMapper\">\n  <select id=\"findAll\" resultType=\"app.User\">\n    SELECT <include refid=\"app.CommonMapper.userColumns\"/> FROM users\n  </select>\n</mapper>\n",
        )
        .write(
            "mapper/CommonMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.CommonMapper\">\n  <sql id=\"baseColumns\">id, name, email</sql>\n</mapper>\n",
        )
        .edge_before(
            "method app.UserMapper::findAll",
            "references",
            "method app.CommonMapper::userColumns",
        )
}

// ----- a module an unchanged file imports is deleted -----------------------

pub fn typescript_delete_an_imported_module() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::TypeScript])
        .file(
            "src/util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { helper } from './util';\n\nexport function main(): number {\n  return helper();\n}\n",
        )
        .remove("src/util.ts")
        .edge_before("function main", "calls", "function helper")
}

pub fn rust_delete_a_used_module() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Rust])
        .file(
            "src/lib.rs",
            "mod util;\n\nuse crate::util::helper;\n\npub fn run() -> u32 {\n    helper(1)\n}\n",
        )
        .file(
            "src/util.rs",
            "pub fn helper(value: u32) -> u32 {\n    value + 1\n}\n",
        )
        .remove("src/util.rs")
        .edge_before("function run", "calls", "function helper")
        .edge_before("file:src/lib.rs", "imports", "function helper")
}

/// Upstream #2214 (W4-RS-01): another file's item is in scope through a glob
/// over its module, so a bare call links the item that module gains.
pub fn rust_glob_module_gains_an_item() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod util;\nmod other;\nmod run;\n")
        .file("src/util.rs", "pub fn other_helper() {}\n")
        .file("src/other.rs", "pub fn helper() {}\n")
        .file(
            "src/run.rs",
            "use crate::util::*;\n\npub fn run() {\n    helper();\n}\n",
        )
        .write("src/util.rs", "pub fn helper() {}\n")
        .edge_after("function run", "calls", "function helper @src/util.rs")
        .no_edge_after("function run", "calls", "function helper @src/other.rs")
}

/// Upstream #2214 (W4-RS-01): a namesake that appears in a module the file
/// neither imports nor globs is out of scope.
pub fn rust_out_of_scope_namesake_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod util;\nmod other;\nmod run;\n")
        .file("src/util.rs", "pub fn helper() {}\n")
        .file("src/other.rs", "pub fn unrelated() {}\n")
        .file(
            "src/run.rs",
            "use crate::util::*;\n\npub fn run() {\n    helper();\n}\n",
        )
        .write("src/other.rs", "pub fn helper() {}\n")
        .edge_before("function run", "calls", "function helper @src/util.rs")
        .edge_after("function run", "calls", "function helper @src/util.rs")
        .no_edge_after("function run", "calls", "function helper @src/other.rs")
}

/// Upstream #2124 (W4-RS-01): a bare prelude name is the prelude's, so a
/// project `drop` that appears without a `use` of it is not what it calls.
pub fn rust_prelude_name_ignores_a_project_namesake_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod util;\nmod release;\n")
        .file("src/util.rs", "pub fn keep() {}\n")
        .file(
            "src/release.rs",
            "pub fn release(v: Vec<u8>) {\n    drop(v);\n}\n",
        )
        .write(
            "src/util.rs",
            "pub fn keep() {}\n\npub fn drop<T>(_value: T) {}\n",
        )
        .no_edge_after("function release", "calls", "function drop")
}

/// Upstream #2124 (W4-RS-01): an enum variant is in scope through a glob over
/// its enum, so renaming the enum in another file takes it out of scope.
pub fn rust_glob_variant_leaves_when_its_enum_is_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod lowargs;\nmod modes;\n")
        .file(
            "src/lowargs.rs",
            "pub enum Mode {\n    Auto,\n    Some(u8),\n}\n",
        )
        .file(
            "src/modes.rs",
            "use crate::lowargs::Mode::*;\n\npub fn pick() {\n    let _ = Some(1);\n}\n",
        )
        .write(
            "src/lowargs.rs",
            "pub enum Kind {\n    Auto,\n    Some(u8),\n}\n",
        )
        .edge_before("function pick", "calls", "enum_member Mode::Some")
        .no_edge_after("function pick", "calls", "enum_member Kind::Some")
}

/// Upstream #2354 (W4-RS-02): a variant path references the enum only once
/// the enum, in another file, declares that variant.
pub fn rust_variant_path_reaches_the_enum_once_it_declares_the_variant() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod mode;\nmod pick;\n")
        .file("src/mode.rs", "pub enum Mode {\n    A,\n    B,\n}\n")
        .file(
            "src/pick.rs",
            "use crate::mode::Mode;\n\npub fn pick() {\n    let _ = Mode::C;\n}\n",
        )
        .write(
            "src/mode.rs",
            "pub enum Mode {\n    A,\n    B,\n    C,\n}\n",
        )
        .edge_after("function pick", "references", "enum Mode")
}

/// Upstream #2354 (W4-RS-02): a variant path leaves the enum when the enum,
/// in another file, renames that variant.
pub fn rust_variant_path_leaves_the_enum_when_the_variant_is_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod mode;\nmod pick;\n")
        .file(
            "src/mode.rs",
            "pub enum Mode {\n    A,\n    B,\n    C,\n}\n",
        )
        .file(
            "src/pick.rs",
            "use crate::mode::Mode;\n\npub fn pick() {\n    let _ = Mode::C;\n}\n",
        )
        .write(
            "src/mode.rs",
            "pub enum Mode {\n    A,\n    B,\n    D,\n}\n",
        )
        .edge_before("function pick", "references", "enum Mode")
        .no_edge_after("function pick", "references", "enum Mode")
}

/// Upstream #2177 (W4-RS-03): a CamelCase path receiver the project does
/// not declare is an outside type; once another file declares it, the call
/// is the project's.
pub fn rust_outside_type_receiver_reaches_a_type_the_project_gains() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod config;\nmod run;\nmod builder;\n")
        .file(
            "src/config.rs",
            "pub struct BuilderConfig;\n\nimpl BuilderConfig {\n    pub fn new() -> Self {\n        BuilderConfig\n    }\n}\n",
        )
        .file("src/builder.rs", "pub fn unrelated() {}\n")
        .file(
            "src/run.rs",
            "use crate::builder::Builder;\n\npub fn run() {\n    let _b = Builder::new();\n}\n",
        )
        .write(
            "src/builder.rs",
            "pub struct Builder;\n\nimpl Builder {\n    pub fn new() -> Self {\n        Builder\n    }\n}\n",
        )
        .no_edge_after("function run", "calls", "method BuilderConfig::new")
        .edge_after("function run", "calls", "method Builder::new")
}

/// Upstream #2177 (W4-RS-03): once the project type a path receiver names is
/// renamed away, the receiver is an outside type, and the call reaches no
/// project method of another type by word overlap.
pub fn rust_outside_type_receiver_leaves_when_the_project_type_is_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod config;\nmod run;\nmod builder;\n")
        .file(
            "src/config.rs",
            "pub struct BuilderConfig;\n\nimpl BuilderConfig {\n    pub fn new() -> Self {\n        BuilderConfig\n    }\n}\n",
        )
        .file(
            "src/builder.rs",
            "pub struct Builder;\n\nimpl Builder {\n    pub fn new() -> Self {\n        Builder\n    }\n}\n",
        )
        .file(
            "src/run.rs",
            "use crate::builder::Builder;\n\npub fn run() {\n    let _b = Builder::new();\n}\n",
        )
        .write(
            "src/builder.rs",
            "pub struct Assembler;\n\nimpl Assembler {\n    pub fn new() -> Self {\n        Assembler\n    }\n}\n",
        )
        .edge_before("function run", "calls", "method Builder::new")
        .no_edge_after("function run", "calls", "method BuilderConfig::new")
        .no_edge_after("function run", "calls", "method Assembler::new")
}

/// Upstream #2177 (W4-RS-03): a path receiver is a declared type once a
/// type alias of its name appears in another file, which changes no node of
/// the method's name.
pub fn rust_alias_receiver_reaches_a_method_once_the_alias_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod widget;\nmod alias;\nmod run;\n")
        .file(
            "src/widget.rs",
            "pub struct Widget;\n\nimpl Widget {\n    pub fn fresh() -> Self {\n        Widget\n    }\n}\n",
        )
        .file("src/alias.rs", "pub fn unrelated() {}\n")
        .file(
            "src/run.rs",
            "use crate::alias::WidgetRef;\n\npub fn run() {\n    let _w = WidgetRef::fresh();\n}\n",
        )
        .write(
            "src/alias.rs",
            "pub type WidgetRef = crate::widget::Widget;\n\npub fn unrelated() {}\n",
        )
        .edge_after("function run", "calls", "method Widget::fresh")
}

/// Upstream #2177 (W4-RS-03): a path receiver is an outside type once the
/// type alias of its name leaves another file, which changes no node of the
/// method's name.
pub fn rust_alias_receiver_leaves_the_method_when_the_alias_goes() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod widget;\nmod alias;\nmod run;\n")
        .file(
            "src/widget.rs",
            "pub struct Widget;\n\nimpl Widget {\n    pub fn fresh() -> Self {\n        Widget\n    }\n}\n",
        )
        .file(
            "src/alias.rs",
            "pub type WidgetRef = crate::widget::Widget;\n\npub fn unrelated() {}\n",
        )
        .file(
            "src/run.rs",
            "use crate::alias::WidgetRef;\n\npub fn run() {\n    let _w = WidgetRef::fresh();\n}\n",
        )
        .write("src/alias.rs", "pub fn unrelated() {}\n")
        .edge_before("function run", "calls", "method Widget::fresh")
        .no_edge_after("function run", "calls", "method Widget::fresh")
}

/// Upstream #2214 (W4-RS-01): a crate the manifest comes to depend on is
/// outside the project, so a name the file imports from it is no project
/// item. `Cargo.toml` is not a source file; W5-06 makes its change
/// re-resolve the project's Rust files.
pub fn rust_cargo_dependency_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .file("src/lib.rs", "mod stream;\nmod source;\n")
        .file("src/stream.rs", "pub trait Stream {}\n")
        .file(
            "src/source.rs",
            "use futures::Stream;\n\npub fn drain(s: impl Stream) {\n    let _ = s;\n}\n",
        )
        .write(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfutures = \"0.3\"\n",
        )
        .edge_before("function drain", "references", "trait Stream")
        .no_edge_after("function drain", "references", "trait Stream")
}

/// W5-06: the dependency leaving the manifest, the name is the project's
/// item again.
pub fn rust_cargo_dependency_leaves() -> Case {
    Case::new(Change::ControlFile, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfutures = \"0.3\"\n",
        )
        .file("src/lib.rs", "mod stream;\nmod source;\n")
        .file("src/stream.rs", "pub trait Stream {}\n")
        .file(
            "src/source.rs",
            "use futures::Stream;\n\npub fn drain(s: impl Stream) {\n    let _ = s;\n}\n",
        )
        .write(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .edge_after("function drain", "references", "trait Stream")
}

/// W5-06: a workspace member's manifest appearing makes its crate the
/// project's, which the other member depended on as an outside one.
pub fn rust_workspace_member_becomes_a_project_crate() -> Case {
    Case::new(Change::ControlFile, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        )
        .file(
            "crates/a/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n\n[dependencies]\nbeta = \"0.1\"\n",
        )
        .file(
            "crates/a/src/lib.rs",
            "use beta::Thing;\n\npub fn make() -> Thing {\n    Thing\n}\n",
        )
        .file("crates/b/src/lib.rs", "pub struct Thing;\n")
        .write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
        )
        .edge_after("function make", "references", "struct Thing")
}

const GAMMA_MAIN: &str =
    "mod thing;\nuse gamma::Thing;\n\npub fn make() -> Thing {\n    Thing\n}\n";

/// W5-06: the first `.rs` file under an existing nested manifest makes that
/// manifest's crate the project's: an unchanged file's `use` of it resolves.
/// The name lane retries `Thing`, the added struct's name, as well, so this
/// case converges without the control lane; the deletion below needs it.
pub fn rust_first_source_under_a_nested_manifest_makes_its_crate_the_projects() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngamma = \"0.1\"\n",
        )
        .file(
            "crates/c/Cargo.toml",
            "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\n",
        )
        .file(
            "src/main.rs",
            "use gamma::Thing;\n\npub fn make() -> Thing {\n    Thing\n}\n",
        )
        .write("crates/c/src/lib.rs", "pub struct Thing;\n")
        .edge_after("function make", "references", "struct Thing")
}

/// W5-06: the last `.rs` file under a nested manifest deleted: its crate is
/// an outside dependency again, so the `use` no longer binds the project's
/// namesake.
pub fn rust_last_source_under_a_nested_manifest_leaves_its_crate_outside() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Rust])
        .file(
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngamma = \"0.1\"\n",
        )
        .file(
            "crates/c/Cargo.toml",
            "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\n",
        )
        .file("crates/c/src/lib.rs", "pub struct Other;\n")
        .file("src/thing.rs", "pub struct Thing;\n")
        .file("src/main.rs", GAMMA_MAIN)
        .edge_before("function make", "references", "struct Thing")
        .remove("crates/c/src/lib.rs")
        .no_edge_after("function make", "references", "struct Thing")
}

pub fn dart_delete_an_imported_library() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Dart])
        .file(
            "lib/math.dart",
            "int square(int value) {\n  return value * value;\n}\n",
        )
        .file(
            "lib/main.dart",
            "import 'math.dart';\n\nint area(int side) {\n  return square(side);\n}\n",
        )
        .remove("lib/math.dart")
        .edge_before("file:lib/main.dart", "imports", "file:lib/math.dart")
        .edge_before("function area", "calls", "function square")
}

pub fn astro_delete_an_imported_module() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Astro])
        .file(
            "src/lib/date.ts",
            "export function formatDate(date: Date): string {\n  return date.toISOString();\n}\n",
        )
        .file(
            "src/pages/index.astro",
            "---\nimport { formatDate } from '../lib/date';\nconst today = formatDate(new Date());\n---\n<p>{today}</p>\n",
        )
        .remove("src/lib/date.ts")
        // The frontmatter runs per render: its top-level constant's
        // initializer call is the component's (#2268).
        .edge_before(
            "component src/pages/index.astro::index",
            "calls",
            "function formatDate",
        )
}

pub fn lua_delete_a_required_module() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Lua])
        .file(
            "util.lua",
            "local M = {}\n\nfunction M.clamp(value, low, high)\n  return math.max(low, math.min(high, value))\nend\n\nreturn M\n",
        )
        .file(
            "main.lua",
            "local util = require('util')\n\nlocal function run()\n  return util.clamp(5, 0, 3)\nend\n\nreturn run\n",
        )
        .remove("util.lua")
        .edge_before("file:main.lua", "imports", "file:util.lua")
}

pub fn nix_delete_an_imported_file() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Nix])
        .file("lib.nix", "{\n  double = x: x * 2;\n}\n")
        .file(
            "default.nix",
            "let\n  lib = import ./lib.nix;\nin\n{\n  value = lib.double 21;\n}\n",
        )
        .remove("lib.nix")
        .edge_before("file:default.nix", "imports", "file:lib.nix")
}

pub fn python_move_an_imported_module_away() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Python])
        .file("lib.py", "def helper():\n    return 1\n")
        .file(
            "main.py",
            "from lib import helper\n\n\ndef main():\n    return helper()\n",
        )
        .rename("lib.py", "tools.py")
        .edge_before("function main", "calls", "function helper @lib.py")
}

// ----- an extends/implements clause changes --------------------------------

pub fn typescript_change_a_superclass() -> Case {
    Case::new(Change::EditHeritage, &[Language::TypeScript])
        .file(
            "src/base.ts",
            "export class Animal {\n  speak(): string {\n    return '...';\n  }\n}\n\nexport class Robot {\n  speak(): string {\n    return 'beep';\n  }\n}\n",
        )
        .file(
            "src/dog.ts",
            "import { Animal, Robot } from './base';\n\nexport class Dog extends Animal {}\n",
        )
        .write(
            "src/dog.ts",
            "import { Animal, Robot } from './base';\n\nexport class Dog extends Robot {}\n",
        )
        .edge_before("class Dog", "extends", "class Animal")
        .edge_after("class Dog", "extends", "class Robot")
}

/// Upstream #2195: an import is the project's when a project file declares
/// its package. A file appearing in that package makes an unchanged file's
/// `Tools.make()` under `import com.lib.Tools;` a project name again, as on a
/// fresh index.
pub fn java_import_package_appears_in_the_project() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Java])
        .file(
            "src/app/Tools.java",
            "package app;\n\npublic class Tools {\n    public static void make() { }\n}\n",
        )
        .file(
            "src/app/Use.java",
            "package app;\n\nimport com.lib.Tools;\n\npublic class Use {\n    void go() {\n        Tools.make();\n    }\n}\n",
        )
        .write(
            "src/com/lib/Other.java",
            "package com.lib;\n\npublic class Other { }\n",
        )
        .edge_after("method app::Use::go", "calls", "method app::Tools::make")
}

/// Upstream #2195: the only file of a package moving to another package
/// makes an import from the first an outside one, which owns its name in an
/// unchanged file, as on a fresh index.
pub fn java_import_package_is_renamed_away() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Java])
        .file(
            "src/app/Tools.java",
            "package app;\n\npublic class Tools {\n    public static void make() { }\n}\n",
        )
        .file(
            "src/app/Use.java",
            "package app;\n\nimport com.lib.Tools;\n\npublic class Use {\n    void go() {\n        Tools.make();\n    }\n}\n",
        )
        .file(
            "src/com/lib/Other.java",
            "package com.lib;\n\npublic class Other { }\n",
        )
        .write(
            "src/com/lib/Other.java",
            "package com.misc;\n\npublic class Other { }\n",
        )
        .edge_before("method app::Use::go", "calls", "method app::Tools::make")
        .no_edge_after("method app::Use::go", "calls", "method app::Tools::make")
}

/// Upstream #2195: the last file of a package leaving makes an import from it
/// an outside one, which owns its name in an unchanged file, as on a fresh
/// index.
pub fn java_import_package_leaves_the_project() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Java])
        .file(
            "src/app/Tools.java",
            "package app;\n\npublic class Tools {\n    public static void make() { }\n}\n",
        )
        .file(
            "src/app/Use.java",
            "package app;\n\nimport com.lib.Tools;\n\npublic class Use {\n    void go() {\n        Tools.make();\n    }\n}\n",
        )
        .file(
            "src/com/lib/Other.java",
            "package com.lib;\n\npublic class Other { }\n",
        )
        .remove("src/com/lib/Other.java")
        .edge_before("method app::Use::go", "calls", "method app::Tools::make")
        .no_edge_after("method app::Use::go", "calls", "method app::Tools::make")
}

pub fn java_change_a_superclass_and_interface() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/zoo/Animal.java",
            "package zoo;\n\npublic class Animal {\n    public String sound() {\n        return \"...\";\n    }\n}\n",
        )
        .file(
            "src/zoo/Creature.java",
            "package zoo;\n\npublic class Creature {\n    public String sound() {\n        return \"?\";\n    }\n}\n",
        )
        .file(
            "src/zoo/Pet.java",
            "package zoo;\n\npublic interface Pet {\n    String name();\n}\n",
        )
        .file(
            "src/zoo/Named.java",
            "package zoo;\n\npublic interface Named {\n    String name();\n}\n",
        )
        .file(
            "src/zoo/Dog.java",
            "package zoo;\n\npublic class Dog extends Animal implements Pet {\n    public String name() {\n        return \"rex\";\n    }\n}\n",
        )
        .write(
            "src/zoo/Dog.java",
            "package zoo;\n\npublic class Dog extends Creature implements Named {\n    public String name() {\n        return \"rex\";\n    }\n}\n",
        )
        .edge_before("class zoo::Dog", "extends", "class zoo::Animal")
        .edge_before("class zoo::Dog", "implements", "interface zoo::Pet")
        .edge_after("class zoo::Dog", "extends", "class zoo::Creature")
        .edge_after("class zoo::Dog", "implements", "interface zoo::Named")
}

/// G1: a C# `base_list` names the base class and the interfaces in one list.
pub fn csharp_base_list_gains_an_interface() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Shapes.cs",
            "namespace Shapes\n{\n    public class Shape { }\n\n    public interface IDrawable { }\n}\n",
        )
        .file(
            "src/Square.cs",
            "namespace Shapes\n{\n    public class Square : Shape { }\n}\n",
        )
        .write(
            "src/Square.cs",
            "namespace Shapes\n{\n    public class Square : Shape, IDrawable { }\n}\n",
        )
        .edge_throughout("class Shapes::Square", "extends", "class Shapes::Shape")
        .edge_after("class Shapes::Square", "implements", "interface Shapes::IDrawable")
}

/// G5: an Objective-C `@interface` names its superclass.
pub fn objc_change_a_superclass() -> Case {
    Case::new(Change::EditHeritage, &[Language::ObjC])
        .file(
            "Sources/Base.h",
            "@interface Base\n- (void)ping;\n@end\n\n@interface Other\n- (void)ping;\n@end\n",
        )
        .file(
            "Sources/Sub.m",
            "#import \"Base.h\"\n\n@interface Sub : Base\n@end\n",
        )
        .write(
            "Sources/Sub.m",
            "#import \"Base.h\"\n\n@interface Sub : Other\n@end\n",
        )
        .edge_before("class Sub", "extends", "class Base")
        .edge_after("class Sub", "extends", "class Other")
}

/// G6: a Python class's bases are its supertypes.
pub fn python_change_a_base_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::Python])
        .file(
            "zoo/base.py",
            "class Animal:\n    def speak(self):\n        return '...'\n\n\nclass Robot:\n    def speak(self):\n        return 'beep'\n",
        )
        .file(
            "zoo/dog.py",
            "from zoo.base import Animal, Robot\n\n\nclass Dog(Animal):\n    pass\n",
        )
        .write(
            "zoo/dog.py",
            "from zoo.base import Animal, Robot\n\n\nclass Dog(Robot):\n    pass\n",
        )
        .edge_before("class Dog", "extends", "class Animal")
        .edge_after("class Dog", "extends", "class Robot")
}

/// G7: a PHP class extends its `base_clause` parent.
pub fn php_change_a_parent_class() -> Case {
    Case::new(Change::EditHeritage, &[Language::Php])
        .file(
            "src/Shapes.php",
            "<?php\n\nnamespace App;\n\nclass Shape\n{\n}\n\nclass Figure\n{\n}\n",
        )
        .file(
            "src/Square.php",
            "<?php\n\nnamespace App;\n\nclass Square extends Shape\n{\n}\n",
        )
        .write(
            "src/Square.php",
            "<?php\n\nnamespace App;\n\nclass Square extends Figure\n{\n}\n",
        )
        .edge_before("class App::Square", "extends", "class App::Shape")
        .edge_after("class App::Square", "extends", "class App::Figure")
}

/// G10: a Go struct's embedded type is its supertype.
pub fn go_change_an_embedded_struct() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/db\n\ngo 1.22\n")
        .file(
            "parts.go",
            "package db\n\ntype Head struct{}\n\ntype Tail struct{}\n",
        )
        .file("db.go", "package db\n\ntype DB struct {\n\t*Head\n}\n")
        .write("db.go", "package db\n\ntype DB struct {\n\t*Tail\n}\n")
        .edge_before("struct DB", "extends", "struct Head")
        .edge_after("struct DB", "extends", "struct Tail")
}

/// G12: a Dart class implements each `with` mixin; its base stays `extends`.
pub fn dart_change_a_mixin() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file(
            "lib/parts.dart",
            "class Bird {}\n\nmixin Walker {}\n\nmixin Swimmer {}\n",
        )
        .file(
            "lib/duck.dart",
            "import 'parts.dart';\n\nclass Duck extends Bird with Walker {}\n",
        )
        .write(
            "lib/duck.dart",
            "import 'parts.dart';\n\nclass Duck extends Bird with Swimmer {}\n",
        )
        .edge_before("class Duck", "implements", "class Walker")
        .edge_after("class Duck", "implements", "class Swimmer")
        .edge_throughout("class Duck", "extends", "class Bird")
}

/// Upstream #2160: a bare Scala call reaches the members of the supertypes
/// the heads of the types around it name, read from source by name, so
/// another file changing a base's head moves an unchanged file's call, as
/// on a fresh index.
pub fn scala_bare_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/app/Base.scala",
            "package app\n\ntrait Base extends HelperA\n",
        )
        .file(
            "src/app/HelperA.scala",
            "package app\n\ntrait HelperA {\n  def helper(): Int = 1\n}\n",
        )
        .file(
            "src/app/HelperB.scala",
            "package app\n\ntrait HelperB {\n  def helper(): Int = 2\n}\n",
        )
        .file(
            "src/app/Use.scala",
            "package app\n\nobject Use extends Base {\n  def local(): Int = helper()\n}\n",
        )
        .write(
            "src/app/Base.scala",
            "package app\n\ntrait Base extends HelperB\n",
        )
        .edge_before("method Use::local", "calls", "method HelperA::helper")
        .edge_after("method Use::local", "calls", "method HelperB::helper")
}

/// Upstream #2193: an imported package object's supertypes are in scope, and
/// the graph holds no node for `package object api extends Api`, so a change
/// of what it extends moves an unchanged importer's bare call, as on a fresh
/// index.
pub fn scala_imported_package_object_changes_its_supertype() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/app/Api.scala",
            "package app\n\ntrait Api {\n  def multipart(s: String): String = s\n}\n",
        )
        .file(
            "src/app/Api2.scala",
            "package app\n\ntrait Api2 {\n  def multipart(s: String): String = s\n}\n",
        )
        .file(
            "src/app/api/package.scala",
            "package app\n\npackage object api extends Api\n",
        )
        .file(
            "src/app/client/Client.scala",
            "package app.client\n\nimport app.api._\n\nclass Client {\n  def send(): String = multipart(\"x\")\n}\n",
        )
        .write(
            "src/app/api/package.scala",
            "package app\n\npackage object api extends Api2\n",
        )
        .edge_before("method Client::send", "calls", "method Api::multipart")
        .edge_after("method Client::send", "calls", "method Api2::multipart")
}

/// Upstream #2193 through a package object: an imported package object's
/// supertypes are walked by their heads, so a head change of what the
/// package object extends, in another file, moves an unchanged importer's
/// bare call, as on a fresh index (the closure runs through the package
/// object's entry).
pub fn scala_package_object_supertype_head_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/app/BaseA.scala",
            "package app\n\ntrait BaseA {\n  def multipart(s: String): String = s\n}\n",
        )
        .file(
            "src/app/BaseB.scala",
            "package app\n\ntrait BaseB {\n  def multipart(s: String): String = s\n}\n",
        )
        .file("src/app/Api.scala", "package app\n\ntrait Api extends BaseA\n")
        .file(
            "src/app/api/package.scala",
            "package app\n\npackage object api extends Api\n",
        )
        .file(
            "src/app/client/Client.scala",
            "package app.client\n\nimport app.api._\n\nclass Client {\n  def send(): String = multipart(\"x\")\n}\n",
        )
        .write("src/app/Api.scala", "package app\n\ntrait Api extends BaseB\n")
        .edge_before("method Client::send", "calls", "method BaseA::multipart")
        .edge_after("method Client::send", "calls", "method BaseB::multipart")
}

// ----- C# namespaces, usings and nested types (W4-CS-01) --------------------

const UNIT_CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n";
// `public`: until W4-CS-03 (#2171) reads a C# type without a modifier as
// `internal`, it is `private` to other files.
const UNIT_SOME: &str = "namespace Unit.Support;\n\npublic static class Some\n{\n    public static int InformationEvent() => 1;\n}\n";
const UNIT_LOG_TESTS: &str =
    "namespace Unit.Context;\n\nclass LogTests\n{\n    Some Make() => null;\n}\n";

/// Upstream #2216/#2220: a project's `global using` brings a namespace into
/// every file of the project, so a file that comes to declare one makes a
/// bare type name in an unchanged file of that project visible, as on a fresh
/// index.
pub fn csharp_global_using_appears_for_its_project() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file("test/Unit/Unit.csproj", UNIT_CSPROJ)
        .file("test/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/Unit/LogTests.cs", UNIT_LOG_TESTS)
        .write("test/Unit/GlobalUsings.cs", "global using Unit.Support;\n")
        .edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// Upstream #2216/#2220: the file holding a project's `global using` leaves,
/// and the bare type name it made visible in an unchanged file is no longer
/// in reach.
pub fn csharp_global_using_leaves_its_project() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::CSharp])
        .file("test/Unit/Unit.csproj", UNIT_CSPROJ)
        .file("test/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/Unit/LogTests.cs", UNIT_LOG_TESTS)
        .file("test/Unit/GlobalUsings.cs", "global using Unit.Support;\n")
        .remove("test/Unit/GlobalUsings.cs")
        .edge_before(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
        .no_edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// Upstream #2216: a project file's `<Using Include>` imports a namespace
/// into the project's files. A `.csproj` is no source file, so a sync that
/// sees only it re-resolves nothing.
pub fn csharp_csproj_using_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("test/Unit/Unit.csproj", UNIT_CSPROJ)
        .file("test/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/Unit/LogTests.cs", UNIT_LOG_TESTS)
        .write(
            "test/Unit/Unit.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Using Include=\"Unit.Support\" />\n  </ItemGroup>\n</Project>\n",
        )
        .edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// Upstream #2220: a nested type is reached by bare name from a type
/// deriving from its owner through any partial part, so a partial part in
/// another file gaining the base brings the unchanged part's reference in.
pub fn csharp_partial_part_gains_a_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Lib/Reader.cs",
            "namespace Lib\n{\n    public abstract class Reader\n    {\n        internal enum State { Start, Done }\n    }\n\n    public partial class TextReader\n    {\n    }\n}\n",
        )
        .file(
            "src/Lib/TextReader.Async.cs",
            "namespace Lib\n{\n    public partial class TextReader\n    {\n        object Peek() => State.Start;\n    }\n}\n",
        )
        .write(
            "src/Lib/Reader.cs",
            "namespace Lib\n{\n    public abstract class Reader\n    {\n        internal enum State { Start, Done }\n    }\n\n    public partial class TextReader : Reader\n    {\n    }\n}\n",
        )
        .edge_after(
            "method Lib::TextReader::Peek",
            "references",
            "enum Lib::Reader::State",
        )
}

/// Upstream #2220 through a middle type: the type between an unchanged
/// subclass and the owner of a nested type drops its base, so the subclass's
/// reference to the nested type leaves, though neither its file nor the
/// owner's changed.
pub fn csharp_middle_type_drops_its_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Lib/Base.cs",
            "namespace Lib\n{\n    public class Reader\n    {\n        internal enum State { Start, Done }\n    }\n}\n",
        )
        .file(
            "src/Lib/Middle.cs",
            "namespace Lib\n{\n    public class Middle : Reader\n    {\n    }\n}\n",
        )
        .file(
            "src/Lib/Leaf.cs",
            "namespace Lib\n{\n    public class Leaf : Middle\n    {\n        object Peek() => State.Start;\n    }\n}\n",
        )
        .write(
            "src/Lib/Middle.cs",
            "namespace Lib\n{\n    public class Middle\n    {\n    }\n}\n",
        )
        .edge_before(
            "method Lib::Leaf::Peek",
            "references",
            "enum Lib::Reader::State",
        )
        .no_edge_after(
            "method Lib::Leaf::Peek",
            "references",
            "enum Lib::Reader::State",
        )
}

/// Upstream #2220: a type declared in a class is nested, one declared in a
/// namespace of the same name is not. The class becomes the namespace; the
/// declaration keeps its node and qualified name, and an unchanged file in a
/// namespace under it now reaches it, as on a fresh index.
pub fn csharp_nested_owner_becomes_a_namespace() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Owner.cs",
            "class Owner\n{\n    public class Nested { }\n}\n",
        )
        .file(
            "src/Use.cs",
            "namespace Owner.Users\n{\n    class Use\n    {\n        Nested Make() => null;\n    }\n}\n",
        )
        .write(
            "src/Owner.cs",
            "namespace Owner\n{\n    public class Nested { }\n}\n",
        )
        .edge_after(
            "method Owner.Users::Use::Make",
            "references",
            "class Owner::Nested",
        )
}

/// Upstream #2216: a type moving into a namespace an unchanged file imports
/// comes into its reach (its qualified name changes, which the name lane
/// already follows).
pub fn csharp_type_moves_into_an_imported_namespace() -> Case {
    Case::new(Change::RenameSymbol, &[Language::CSharp])
        .file(
            "src/Models/User.cs",
            "namespace App.Old\n{\n    public class User { }\n}\n",
        )
        .file(
            "src/Svc.cs",
            "using App.Models;\n\nnamespace App.Services\n{\n    class Svc\n    {\n        User Find() => null;\n    }\n}\n",
        )
        .write(
            "src/Models/User.cs",
            "namespace App.Models\n{\n    public class User { }\n}\n",
        )
        .edge_after(
            "method App.Services::Svc::Find",
            "references",
            "class App.Models::User",
        )
}

const SPEC_A: &str =
    "namespace App.Specs;\n\npublic abstract class SpecA\n{\n    protected void Check() { }\n}\n";
const SPEC_B: &str =
    "namespace App.Specs;\n\npublic abstract class SpecB\n{\n    protected void Check() { }\n}\n";
const SPEC_LEAF: &str = "namespace App.Specs;\n\npublic class Leaf : Middle\n{\n    public void Runs()\n    {\n        Check();\n    }\n}\n";

/// Upstream #2151: a bare C# call reaches a member of the bases the heads
/// around it list, so a middle type in another file changing its base moves
/// an unchanged subclass's call.
pub fn csharp_bare_call_follows_a_base_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file("src/Specs/SpecA.cs", SPEC_A)
        .file("src/Specs/SpecB.cs", SPEC_B)
        .file(
            "src/Specs/Middle.cs",
            "namespace App.Specs;\n\npublic abstract class Middle : SpecA\n{\n}\n",
        )
        .file("src/Specs/Leaf.cs", SPEC_LEAF)
        .write(
            "src/Specs/Middle.cs",
            "namespace App.Specs;\n\npublic abstract class Middle : SpecB\n{\n}\n",
        )
        .edge_before(
            "method App.Specs::Leaf::Runs",
            "calls",
            "method App.Specs::SpecA::Check",
        )
        .edge_after(
            "method App.Specs::Leaf::Runs",
            "calls",
            "method App.Specs::SpecB::Check",
        )
}

/// Upstream #2151 reads a head nine lines deep, two more than #2220's
/// reader: a base list on a head's eighth line moves an unchanged
/// subclass's bare call.
pub fn csharp_bare_call_follows_a_long_head_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file("src/Specs/SpecA.cs", SPEC_A)
        .file("src/Specs/SpecB.cs", SPEC_B)
        .file(
            "src/Specs/Middle.cs",
            "namespace App.Specs;\n\npublic abstract class Middle\n\n\n\n\n\n\n    : SpecA\n{\n}\n",
        )
        .file("src/Specs/Leaf.cs", SPEC_LEAF)
        .write(
            "src/Specs/Middle.cs",
            "namespace App.Specs;\n\npublic abstract class Middle\n\n\n\n\n\n\n    : SpecB\n{\n}\n",
        )
        .edge_before(
            "method App.Specs::Leaf::Runs",
            "calls",
            "method App.Specs::SpecA::Check",
        )
        .edge_after(
            "method App.Specs::Leaf::Runs",
            "calls",
            "method App.Specs::SpecB::Check",
        )
}

const BUILDER: &str = "namespace App.Execution;\n\npublic static class ExpressionBuilder\n{\n    public static object Call(object target) => target;\n}\n";
const CATALOG: &str = "namespace App.Specs;\n\npublic class CatalogSpec\n{\n    public object Runs(object token)\n    {\n        return Call(token);\n    }\n}\n";

/// Upstream #2151: a `global using static` in any file brings its type's
/// members into reach of every file, so it appearing reaches an unchanged
/// file's bare call.
pub fn csharp_global_using_static_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file("src/Execution/ExpressionBuilder.cs", BUILDER)
        .file("src/Specs/CatalogSpec.cs", CATALOG)
        .write(
            "src/GlobalUsings.cs",
            "global using static App.Execution.ExpressionBuilder;\n",
        )
        .edge_after(
            "method App.Specs::CatalogSpec::Runs",
            "calls",
            "method App.Execution::ExpressionBuilder::Call",
        )
}

/// Upstream #2151: the file holding a `global using static` leaves, and the
/// unchanged file's bare call is out of reach again.
pub fn csharp_global_using_static_leaves() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::CSharp])
        .file("src/Execution/ExpressionBuilder.cs", BUILDER)
        .file("src/Specs/CatalogSpec.cs", CATALOG)
        .file(
            "src/GlobalUsings.cs",
            "global using static App.Execution.ExpressionBuilder;\n",
        )
        .remove("src/GlobalUsings.cs")
        .edge_before(
            "method App.Specs::CatalogSpec::Runs",
            "calls",
            "method App.Execution::ExpressionBuilder::Call",
        )
        .no_edge_after(
            "method App.Specs::CatalogSpec::Runs",
            "calls",
            "method App.Execution::ExpressionBuilder::Call",
        )
}

/// Upstream #2151: a project file's `<Using … Static="true"/>` brings its
/// type's members into reach. A `.csproj` is no source file, so a sync that
/// sees only it re-resolves nothing.
pub fn csharp_csproj_static_using_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("src/App.csproj", UNIT_CSPROJ)
        .file("src/Execution/ExpressionBuilder.cs", BUILDER)
        .file("src/Specs/CatalogSpec.cs", CATALOG)
        .write(
            "src/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Using Include=\"App.Execution.ExpressionBuilder\" Static=\"true\"/>\n  </ItemGroup>\n</Project>\n",
        )
        .edge_after(
            "method App.Specs::CatalogSpec::Runs",
            "calls",
            "method App.Execution::ExpressionBuilder::Call",
        )
}

/// W5-06: a `Directory.Build.props` gaining a `<Using Include>` imports the
/// namespace into the project's files.
pub fn csharp_props_using_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("Directory.Build.props", "<Project>\n</Project>\n")
        .file("test/Unit/Unit.csproj", UNIT_CSPROJ)
        .file("test/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/Unit/LogTests.cs", UNIT_LOG_TESTS)
        .write(
            "Directory.Build.props",
            "<Project>\n  <ItemGroup>\n    <Using Include=\"Unit.Support\" />\n  </ItemGroup>\n</Project>\n",
        )
        .edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// W5-06: `<ImplicitUsings>` turned on brings the SDK's namespaces in, a
/// project type in one of them with them.
pub fn csharp_implicit_usings_turn_on() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("src/App.csproj", UNIT_CSPROJ)
        .file(
            "src/Tasks/Helper.cs",
            "namespace System.Threading.Tasks;\n\npublic class Helper\n{\n}\n",
        )
        .file(
            "src/App/Consumer.cs",
            "namespace App;\n\npublic class Consumer\n{\n    Helper Make() => null;\n}\n",
        )
        .write(
            "src/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>\n  </PropertyGroup>\n</Project>\n",
        )
        .edge_after(
            "method App::Consumer::Make",
            "references",
            "class System.Threading.Tasks::Helper",
        )
}

const GLOBALS: &str = "global using Unit.Support;\n";
const TEST_LOG: &str =
    "namespace Unit.Context;\n\nclass LogTests\n{\n    Some Make() => null;\n}\n";

/// W5-06: a new `.csproj` makes its folder a project of its own, so another
/// project's `global using` no longer reaches its files.
pub fn csharp_new_csproj_hides_another_projects_global_using() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("src/App.csproj", UNIT_CSPROJ)
        .file("src/Globals.cs", GLOBALS)
        .file("src/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/LogTests.cs", TEST_LOG)
        .write("test/Tests.csproj", UNIT_CSPROJ)
        .edge_before(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
        .no_edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// W5-06: the folder's `.csproj` deleted, the other project's `global using`
/// reaches its files again.
pub fn csharp_deleted_csproj_lets_another_projects_global_using_in() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("src/App.csproj", UNIT_CSPROJ)
        .file("src/Globals.cs", GLOBALS)
        .file("src/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/LogTests.cs", TEST_LOG)
        .file("test/Tests.csproj", UNIT_CSPROJ)
        .remove("test/Tests.csproj")
        .edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// W5-06: the project file renamed (here to a `.props`), its `<Using>` read
/// under the new name.
pub fn csharp_renamed_project_file_keeps_its_using() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file(
            "test/Unit/Unit.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Using Include=\"Unit.Support\" />\n  </ItemGroup>\n</Project>\n",
        )
        .file("test/Unit/Support/Some.cs", UNIT_SOME)
        .file("test/Unit/LogTests.cs", UNIT_LOG_TESTS)
        .rename("test/Unit/Unit.csproj", "test/Unit/Renamed.props")
        .edge_throughout(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// W5-06: C# files named with an upper-case `.CS` are read as C# from the
/// index, and their folder's project file is an input like any other's.
pub fn csharp_upper_case_files_read_their_project_file() -> Case {
    Case::new(Change::ControlFile, &[Language::CSharp])
        .file("test/Unit/Unit.csproj", UNIT_CSPROJ)
        .file("test/Unit/Support/Some.CS", UNIT_SOME)
        .file("test/Unit/LogTests.CS", UNIT_LOG_TESTS)
        .write(
            "test/Unit/Unit.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Using Include=\"Unit.Support\" />\n  </ItemGroup>\n</Project>\n",
        )
        .edge_after(
            "method Unit.Context::LogTests::Make",
            "references",
            "class Unit.Support::Some",
        )
}

/// Upstream #2151: a bare name reaches only a member in scope, so another
/// class gaining a property of that name is no competitor.
pub fn csharp_bare_name_ignores_an_added_property() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Specs/SpecBase.cs",
            "namespace App.Specs;\n\npublic abstract class SpecBase\n{\n    protected TimeSpan Level { get; set; }\n}\n",
        )
        .file(
            "src/Specs/CatalogSpec.cs",
            "namespace App.Specs;\n\npublic class CatalogSpec : SpecBase\n{\n    public int Runs()\n    {\n        return Level.Seconds;\n    }\n}\n",
        )
        .write(
            "tests/Other.cs",
            "namespace App.Tests;\n\npublic class Other\n{\n    public TimeSpan Level { get; set; }\n}\n",
        )
        .edge_throughout(
            "method App.Specs::CatalogSpec::Runs",
            "references",
            "property App.Specs::SpecBase::Level",
        )
        .no_edge_after(
            "method App.Specs::CatalogSpec::Runs",
            "references",
            "property App.Tests::Other::Level",
        )
}

const CS_JSON_WRITER: &str = "namespace App.Json;\n\npublic class JsonWriter\n{\n    public void WriteValue(string value) { }\n}\n";
const CS_OTHER_WRITER: &str = "namespace App.Json;\n\npublic class OtherWriter\n{\n    public void WriteValue(string value) { }\n}\n";
const CS_TRACE_WRITER: &str = "namespace App.Json;\n\npublic class TraceWriter : WriterBase\n{\n    public void Trace(string value)\n    {\n        Writer.WriteValue(value);\n    }\n}\n";

/// Upstream #2171: a call through a member a class inherits is a call on
/// the type the base declares it with, so the base changing that type in
/// another file moves an unchanged subclass's call.
pub fn csharp_inherited_member_type_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file("src/Json/JsonWriter.cs", CS_JSON_WRITER)
        .file("src/Json/OtherWriter.cs", CS_OTHER_WRITER)
        .file(
            "src/Json/WriterBase.cs",
            "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected JsonWriter Writer { get; set; }\n}\n",
        )
        .file("src/Json/TraceWriter.cs", CS_TRACE_WRITER)
        .write(
            "src/Json/WriterBase.cs",
            "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected OtherWriter Writer { get; set; }\n}\n",
        )
        .edge_before(
            "method App.Json::TraceWriter::Trace",
            "calls",
            "method App.Json::JsonWriter::WriteValue",
        )
        .edge_after(
            "method App.Json::TraceWriter::Trace",
            "calls",
            "method App.Json::OtherWriter::WriteValue",
        )
}

/// Upstream #2171: a method body edit in the base changes no member line,
/// so no unchanged file re-resolves and the subclass's typed call stays.
pub fn csharp_inherited_member_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::CSharp])
        .file("src/Json/JsonWriter.cs", CS_JSON_WRITER)
        .file(
            "src/Json/WriterBase.cs",
            "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected JsonWriter Writer { get; set; }\n\n    protected int Depth()\n    {\n        return 1;\n    }\n}\n",
        )
        .file("src/Json/TraceWriter.cs", CS_TRACE_WRITER)
        .write(
            "src/Json/WriterBase.cs",
            "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected JsonWriter Writer { get; set; }\n\n    protected int Depth()\n    {\n        return 2;\n    }\n}\n",
        )
        .edge_throughout(
            "method App.Json::TraceWriter::Trace",
            "calls",
            "method App.Json::JsonWriter::WriteValue",
        )
}

/// Upstream #2171: a member typed by a base's type parameter is the type
/// argument a class between it and the caller gives, so that middle class
/// changing its argument in another file moves an unchanged leaf's call.
pub fn csharp_middle_type_argument_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "src/Tests/IntegrationTest.cs",
            "namespace App.Tests;\n\npublic abstract class IntegrationTest<TFixture> where TFixture : new()\n{\n    protected TFixture Fixture { get; private set; }\n}\n",
        )
        .file(
            "src/Tests/FirstInit.cs",
            "namespace App.Tests;\n\npublic class FirstInit\n{\n    public object CreateContext() => null;\n}\n",
        )
        .file(
            "src/Tests/SecondInit.cs",
            "namespace App.Tests;\n\npublic class SecondInit\n{\n    public object CreateContext() => null;\n}\n",
        )
        .file(
            "src/Tests/Middle.cs",
            "namespace App.Tests;\n\npublic abstract class Middle : IntegrationTest<FirstInit>\n{\n}\n",
        )
        .file(
            "src/Tests/Leaf.cs",
            "namespace App.Tests;\n\npublic class Leaf : Middle\n{\n    public object Runs()\n    {\n        return Fixture.CreateContext();\n    }\n}\n",
        )
        .write(
            "src/Tests/Middle.cs",
            "namespace App.Tests;\n\npublic abstract class Middle : IntegrationTest<SecondInit>\n{\n}\n",
        )
        .edge_before(
            "method App.Tests::Leaf::Runs",
            "calls",
            "method App.Tests::FirstInit::CreateContext",
        )
        .edge_after(
            "method App.Tests::Leaf::Runs",
            "calls",
            "method App.Tests::SecondInit::CreateContext",
        )
}

/// Upstream #2171, Java: a member typed by a type parameter the subclass
/// gives no argument for is the parameter's bound, so the base changing the
/// bound in another file moves an unchanged subclass's call.
pub fn java_type_parameter_bound_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/main/java/app/IntegrationTest.java",
            "package app;\n\npublic abstract class IntegrationTest<T extends FirstInit> {\n    protected T fixture;\n}\n",
        )
        .file(
            "src/main/java/app/FirstInit.java",
            "package app;\n\npublic class FirstInit {\n    public Object createContext() { return null; }\n}\n",
        )
        .file(
            "src/main/java/app/SecondInit.java",
            "package app;\n\npublic class SecondInit {\n    public Object createContext() { return null; }\n}\n",
        )
        .file(
            "src/main/java/app/Leaf.java",
            "package app;\n\npublic class Leaf extends IntegrationTest {\n    public Object runs() {\n        return fixture.createContext();\n    }\n}\n",
        )
        .write(
            "src/main/java/app/IntegrationTest.java",
            "package app;\n\npublic abstract class IntegrationTest<T extends SecondInit> {\n    protected T fixture;\n}\n",
        )
        .edge_before("method app::Leaf::runs", "calls", "method app::FirstInit::createContext")
        .edge_after("method app::Leaf::runs", "calls", "method app::SecondInit::createContext")
}

/// Upstream #2345: a C# property's accessor body is walked as the property,
/// so its call on a class added later in another file links after the sync,
/// as on a fresh index.
pub fn csharp_accessor_body_call_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file(
            "src/Box.cs",
            "namespace App\n{\n    public class Box\n    {\n        public int Value\n        {\n            get { return Helper.Compute(1); }\n        }\n        public int Arrow => Helper.Compute(2);\n    }\n}\n",
        )
        .write(
            "src/Helper.cs",
            "namespace App\n{\n    public static class Helper\n    {\n        public static int Compute(int n) => n;\n    }\n}\n",
        )
        .edge_after("property App::Box::Value", "calls", "method App::Helper::Compute")
        .edge_after("property App::Box::Arrow", "calls", "method App::Helper::Compute")
}

/// Upstream #2348: a C# field's initializer is walked as the field, so
/// editing which method it calls moves the field's edge.
pub fn csharp_field_initializer_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::CSharp])
        .file(
            "src/Helper.cs",
            "namespace App\n{\n    public static class Helper\n    {\n        public static int Compute(int n) => n;\n        public static int Seed() => 1;\n    }\n}\n",
        )
        .file(
            "src/Box.cs",
            "namespace App\n{\n    public class Box\n    {\n        private int _a = 1, _b = Helper.Compute(2);\n        public int Seeded { get; } = Helper.Compute(3);\n    }\n}\n",
        )
        .write(
            "src/Box.cs",
            "namespace App\n{\n    public class Box\n    {\n        private int _a = 1, _b = Helper.Seed();\n        public int Seeded { get; } = Helper.Seed();\n    }\n}\n",
        )
        .edge_before("field App::Box::_b", "calls", "method App::Helper::Compute")
        .edge_before("property App::Box::Seeded", "calls", "method App::Helper::Compute")
        .edge_after("field App::Box::_b", "calls", "method App::Helper::Seed")
        .edge_after("property App::Box::Seeded", "calls", "method App::Helper::Seed")
        .no_edge_after("field App::Box::_b", "calls", "method App::Helper::Compute")
}

/// Upstream #2348: a C# target-typed `new()` initializing a field or
/// property instantiates the declared type, so a class of that name added
/// later in another file is instantiated after the sync, as on a fresh index.
pub fn csharp_target_typed_new_of_an_added_class() -> Case {
    Case::new(Change::AddImportTarget, &[Language::CSharp])
        .file(
            "src/Box.cs",
            "namespace App\n{\n    public class Box\n    {\n        private readonly Widget _w = new();\n        public Widget? Made { get; } = new();\n    }\n}\n",
        )
        .write(
            "src/Widget.cs",
            "namespace App\n{\n    public class Widget { }\n}\n",
        )
        .edge_after("field App::Box::_w", "instantiates", "class App::Widget")
        .edge_after("property App::Box::Made", "instantiates", "class App::Widget")
}

/// Upstream #2348 (issue #2337, its three-file repro): a C# type position
/// never names a constant, so a `private const string Station` added beside
/// the consumer leaves `new Station { … }` on the class it imports, after the
/// sync as on a fresh index.
pub fn csharp_type_position_ignores_an_added_constant() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/Core/Station.cs",
            "namespace Demo.Core;\n\npublic class Station\n{\n    public string Id { get; set; }\n}\n",
        )
        .file(
            "tests/App.Tests/Consumer.cs",
            "using Demo.Core;\n\nnamespace Demo.Tests\n{\n    public class Consumer\n    {\n        public Station Make() => new Station { Id = \"a\" };\n    }\n}\n",
        )
        .write(
            "tests/App.Tests/ConstHolder.cs",
            "namespace Demo.Tests\n{\n    public class ConstHolder\n    {\n        private const string Station = \"S1\";\n\n        public string Get() => Station;\n    }\n}\n",
        )
        .edge_throughout(
            "method Demo.Tests::Consumer::Make",
            "instantiates",
            "class Demo.Core::Station",
        )
        .no_edge_after(
            "method Demo.Tests::Consumer::Make",
            "instantiates",
            "constant Demo.Tests::ConstHolder::Station",
        )
}

/// Upstream #2171, Java: a field the base declares types the subclass's
/// call through it, so the base changing the field's type in another file
/// moves the unchanged subclass's call.
pub fn java_inherited_field_type_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Java])
        .file(
            "src/main/java/app/JsonWriter.java",
            "package app;\n\npublic class JsonWriter {\n    public void writeValue(String value) { }\n}\n",
        )
        .file(
            "src/main/java/app/OtherWriter.java",
            "package app;\n\npublic class OtherWriter {\n    public void writeValue(String value) { }\n}\n",
        )
        .file(
            "src/main/java/app/WriterBase.java",
            "package app;\n\npublic abstract class WriterBase {\n    protected JsonWriter writer;\n}\n",
        )
        .file(
            "src/main/java/app/TraceWriter.java",
            "package app;\n\npublic class TraceWriter extends WriterBase {\n    public void trace(String value) {\n        writer.writeValue(value);\n    }\n}\n",
        )
        .write(
            "src/main/java/app/WriterBase.java",
            "package app;\n\npublic abstract class WriterBase {\n    protected OtherWriter writer;\n}\n",
        )
        .edge_before(
            "method app::TraceWriter::trace",
            "calls",
            "method app::JsonWriter::writeValue",
        )
        .edge_after(
            "method app::TraceWriter::trace",
            "calls",
            "method app::OtherWriter::writeValue",
        )
}

/// Upstream #2171, Kotlin: a property the base declares types the
/// subclass's call through it, across files.
pub fn kotlin_inherited_property_type_changes_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Kotlin])
        .file(
            "src/main/kotlin/app/JsonWriter.kt",
            "package app\n\nclass JsonWriter {\n    fun writeValue(value: String) {}\n}\n",
        )
        .file(
            "src/main/kotlin/app/OtherWriter.kt",
            "package app\n\nclass OtherWriter {\n    fun writeValue(value: String) {}\n}\n",
        )
        .file(
            "src/main/kotlin/app/WriterBase.kt",
            "package app\n\nopen class WriterBase {\n    protected val writer: JsonWriter = JsonWriter()\n}\n",
        )
        .file(
            "src/main/kotlin/app/TraceWriter.kt",
            "package app\n\nclass TraceWriter : WriterBase() {\n    fun trace(value: String) {\n        writer.writeValue(value)\n    }\n}\n",
        )
        .write(
            "src/main/kotlin/app/WriterBase.kt",
            "package app\n\nopen class WriterBase {\n    protected val writer: OtherWriter = OtherWriter()\n}\n",
        )
        .edge_before(
            "method app::TraceWriter::trace",
            "calls",
            "method app::JsonWriter::writeValue",
        )
        .edge_after(
            "method app::TraceWriter::trace",
            "calls",
            "method app::OtherWriter::writeValue",
        )
}

/// Upstream #2121: a type position names a type, so a property of the same
/// name appearing elsewhere is no competitor.
pub fn csharp_type_position_ignores_an_added_property() -> Case {
    Case::new(Change::AddCompetitor, &[Language::CSharp])
        .file(
            "src/TypeMap.cs",
            "namespace App;\npublic class TypeMap { }\n",
        )
        .file(
            "src/Profile.cs",
            "namespace App;\npublic class Profile {\n    public void Configure(TypeMap typeMap) { }\n}\n",
        )
        .write(
            "src/Holder.cs",
            "namespace App;\npublic class Holder {\n    public TypeMap TypeMap { get; set; }\n}\n",
        )
        .edge_throughout(
            "method App::Profile::Configure",
            "references",
            "class App::TypeMap",
        )
        .no_edge_after(
            "method App::Profile::Configure",
            "references",
            "property App::Holder::TypeMap",
        )
}

/// Upstream #2230: a package object's member is in scope through an import
/// of its package, read from its file's text, so renaming the package object
/// (its members keep their nodes) drops an unchanged importer's reference,
/// as on a fresh index.
pub fn scala_package_object_is_renamed_under_its_members() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Scala])
        .file(
            "src/algebra/package.scala",
            "package object algebra {\n  type Eq[A] = Int\n}\n",
        )
        .file(
            "src/algebra/laws/RingLaws.scala",
            "package algebra.laws\n\nimport algebra._\n\ntrait RingLaws {\n  def eqv[A](eq: Eq[A]): Boolean = true\n}\n",
        )
        .write(
            "src/algebra/package.scala",
            "package object algebra2 {\n  type Eq[A] = Int\n}\n",
        )
        .edge_before("method RingLaws::eqv", "references", "type_alias Eq")
}

/// Upstream #2199: a type as a chain link is named by what holds it, a
/// top-level type's file's package, read from its text, so moving the type's
/// file to another package (its node unchanged) drops an unchanged caller's
/// `app.client.Response(200)`, as on a fresh index.
pub fn scala_chain_link_type_changes_its_package() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/client/Response.scala",
            "package app.client\n\ncase class Response(code: Int)\n",
        )
        .file(
            "src/app/Use.scala",
            "package app\n\nclass Use {\n  def make(): Any = app.client.Response(200)\n}\n",
        )
        .write(
            "src/app/client/Response.scala",
            "package app.clients\n\ncase class Response(code: Int)\n",
        )
        .edge_before("method Use::make", "instantiates", "class Response")
}

/// Upstream #2193: a Scala `val` written in a block of a class body is that
/// block's, read from the class's braces, so wrapping a member `val` in a
/// block (its node unchanged) drops a subclass's call in another file, as on
/// a fresh index.
pub fn scala_member_val_becomes_block_local() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/ASuite.scala",
            "package app\n\nclass ASuite {\n  def test(name: String)(body: => Unit): Unit = ()\n  // block\n    val f = (i: Int) => i\n  // end\n}\n",
        )
        .file(
            "src/app/BSuite.scala",
            "package app\n\nclass BSuite extends ASuite {\n  def go(): Int = f(1)\n}\n",
        )
        .write(
            "src/app/ASuite.scala",
            "package app\n\nclass ASuite {\n  def test(name: String)(body: => Unit): Unit = ()\n  test(\"a\") {\n    val f = (i: Int) => i\n  }\n}\n",
        )
        .edge_before("method BSuite::go", "calls", "field ASuite::f")
}

/// Upstream #2160: a chain link reaches a package object's function only
/// when it is an `extension` method, read from the lines above it, so an
/// `extension` line appearing above an unchanged function lets an unchanged
/// file's chain link reach it, as on a fresh index.
pub fn scala_function_becomes_an_extension_method() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/tools/package.scala",
            "package app\n\npackage object tools {\n  val x = 1\n  def double(n: Int): Int = n * 2\n}\n",
        )
        .file(
            "src/app/Use.scala",
            "package app\n\nimport app.tools._\n\nobject Use {\n  def run(): Int = List(1).double(2)\n}\n",
        )
        .write(
            "src/app/tools/package.scala",
            "package app\n\npackage object tools {\n  extension (s: String) def shout: String = s\n  def double(n: Int): Int = n * 2\n}\n",
        )
        .edge_after("method Use::run", "calls", "function double")
}

/// Upstream #2193: two same-named `val f` of one file, `A`'s and `C`'s,
/// trade their block and member states with every node in place; a
/// subclass of `C` in another file gains its call to `C::f`, as on a fresh
/// index (the facts compare by node id, not by name).
pub fn scala_same_named_vals_trade_block_and_member_states() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/Suites.scala",
            "package app\n\nclass A {\n  def test(n: String)(b: => Unit): Unit = ()\n  // block\n    val f = (i: Int) => i\n  // end\n}\n\nclass C {\n  def test(n: String)(b: => Unit): Unit = ()\n  test(\"c\") {\n    val f = (i: Int) => i\n  }\n}\n",
        )
        .file(
            "src/app/D.scala",
            "package app\n\nclass D extends C {\n  def go(): Int = f(1)\n}\n",
        )
        .write(
            "src/app/Suites.scala",
            "package app\n\nclass A {\n  def test(n: String)(b: => Unit): Unit = ()\n  test(\"a\") {\n    val f = (i: Int) => i\n  }\n}\n\nclass C {\n  def test(n: String)(b: => Unit): Unit = ()\n  // block\n    val f = (i: Int) => i\n  // end\n}\n",
        )
        .edge_after("method D::go", "calls", "field C::f")
}

/// Upstream #2199 past the name lane's per-tail ceiling: 501 parked
/// `app.client.Response(…)` calls all reach `Response` once its file moves
/// into `app.client`, as on a fresh index (the lane re-resolves their file
/// whole, however many share the tail).
pub fn scala_holder_change_reaches_past_the_tail_ceiling() -> Case {
    static CALLS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut calls = String::from("package app\n\nclass Use {\n");
        for i in 0..501 {
            calls.push_str(&format!(
                "  def make{i}(): Any = app.client.Response({i})\n"
            ));
        }
        calls.push_str("}\n");
        calls
    });
    let calls: &'static str = CALLS.as_str();
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/client/Response.scala",
            "package app.clients\n\ncase class Response(code: Int)\n",
        )
        .file("src/app/Use.scala", calls)
        .write(
            "src/app/client/Response.scala",
            "package app.client\n\ncase class Response(code: Int)\n",
        )
        .edge_after("method Use::make500", "instantiates", "class Response")
}

/// Upstream #2160: a body edit in a base changes no head, so no unchanged
/// file re-resolves, and the subclass's bare call stays.
pub fn scala_base_body_edit_keeps_a_bare_call() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/Base.scala",
            "package app\n\ntrait Base {\n  def assist(): Int = 1\n}\n",
        )
        .file(
            "src/app/Child.scala",
            "package app\n\nclass Child extends Base {\n  def go(): Int = assist()\n}\n",
        )
        .write(
            "src/app/Base.scala",
            "package app\n\ntrait Base {\n  def assist(): Int = {\n    2\n  }\n}\n",
        )
        .edge_throughout("method Child::go", "calls", "method Base::assist")
}

/// Upstream #2160 in a batch: the base and the type between it and an
/// unchanged subclass both change in one sync; the subclass's bare call
/// reaches what the new middle type names, as on a fresh index.
pub fn scala_batch_rewrites_a_base_and_a_middle_type() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/app/Root.scala",
            "package app\n\ntrait Root {\n  def assist(): Int = 1\n}\n",
        )
        .file(
            "src/app/Other.scala",
            "package app\n\ntrait Other {\n  def assist(): Int = 2\n}\n",
        )
        .file(
            "src/app/Spare.scala",
            "package app\n\ntrait Spare {\n  def assist(): Int = 3\n}\n",
        )
        .file(
            "src/app/Base.scala",
            "package app\n\ntrait Base extends Root\n",
        )
        .file(
            "src/app/Mid.scala",
            "package app\n\ntrait Mid extends Base\n",
        )
        .file(
            "src/app/Child.scala",
            "package app\n\nclass Child extends Mid {\n  def go(): Int = assist()\n}\n",
        )
        .write(
            "src/app/Base.scala",
            "package app\n\ntrait Base extends Other\n",
        )
        .write(
            "src/app/Mid.scala",
            "package app\n\ntrait Mid extends Spare\n",
        )
        .edge_before("method Child::go", "calls", "method Root::assist")
        .edge_after("method Child::go", "calls", "method Spare::assist")
}

/// Upstream #2230, the other way round: a package object taking the name
/// an unchanged file imports (its members keep their nodes) gives that
/// file's reference its target, as on a fresh index.
pub fn scala_package_object_takes_an_imported_name() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Scala])
        .file(
            "src/algebra/package.scala",
            "package object algebra2 {\n  type Eq[A] = Int\n}\n",
        )
        .file(
            "src/algebra/laws/RingLaws.scala",
            "package algebra.laws\n\nimport algebra._\n\ntrait RingLaws {\n  def eqv[A](eq: Eq[A]): Boolean = true\n}\n",
        )
        .write(
            "src/algebra/package.scala",
            "package object algebra {\n  type Eq[A] = Int\n}\n",
        )
        .edge_after("method RingLaws::eqv", "references", "type_alias Eq")
}

/// Upstream #2199, the other way round: a type's file moving into the
/// package an unchanged caller's chain names (the type's node unchanged)
/// gives `app.client.Response(200)` its target, as on a fresh index.
pub fn scala_chain_link_type_moves_into_the_named_package() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/client/Response.scala",
            "package app.clients\n\ncase class Response(code: Int)\n",
        )
        .file(
            "src/app/Use.scala",
            "package app\n\nclass Use {\n  def make(): Any = app.client.Response(200)\n}\n",
        )
        .write(
            "src/app/client/Response.scala",
            "package app.client\n\ncase class Response(code: Int)\n",
        )
        .edge_after("method Use::make", "instantiates", "class Response")
}

/// Upstream #2193, the other way round: a block's `val` becoming a member of
/// the class body (its node unchanged) gives a subclass's call in another
/// file its target, as on a fresh index.
pub fn scala_block_local_val_becomes_a_member() -> Case {
    Case::new(Change::BodyEdit, &[Language::Scala])
        .file(
            "src/app/ASuite.scala",
            "package app\n\nclass ASuite {\n  def test(name: String)(body: => Unit): Unit = ()\n  test(\"a\") {\n    val f = (i: Int) => i\n  }\n}\n",
        )
        .file(
            "src/app/BSuite.scala",
            "package app\n\nclass BSuite extends ASuite {\n  def go(): Int = f(1)\n}\n",
        )
        .write(
            "src/app/ASuite.scala",
            "package app\n\nclass ASuite {\n  def test(name: String)(body: => Unit): Unit = ()\n  // block\n    val f = (i: Int) => i\n  // end\n}\n",
        )
        .edge_after("method BSuite::go", "calls", "field ASuite::f")
}

/// Upstream #2123's type scope reads the heads of every declaration named
/// like a type around the reference, another file's included, so a
/// same-named class's head changing in another file moves an unchanged
/// file's type reference, as on a fresh index.
pub fn scala_type_reference_follows_a_namesake_head_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/app/HasA.scala",
            "package app\n\ntrait HasA {\n  type A = Int\n}\n",
        )
        .file(
            "src/app/HasB.scala",
            "package app\n\ntrait HasB {\n  type A = Long\n}\n",
        )
        .file(
            "src/other/Child.scala",
            "package other\n\nclass Child extends HasA\n",
        )
        .file(
            "src/app/Child.scala",
            "package app\n\nclass Child {\n  def get(): A = ???\n}\n",
        )
        .write(
            "src/other/Child.scala",
            "package other\n\nclass Child extends HasB\n",
        )
        .edge_before("method Child::get", "references", "type_alias HasA::A")
        .edge_after("method Child::get", "references", "type_alias HasB::A")
}

pub fn scala_change_a_parent_trait() -> Case {
    Case::new(Change::EditHeritage, &[Language::Scala])
        .file(
            "src/Shape.scala",
            "package shapes\n\ntrait Shape {\n  def area(): Double\n}\n\ntrait Figure {\n  def area(): Double\n}\n",
        )
        .file(
            "src/Square.scala",
            "package shapes\n\nclass Square(side: Double) extends Shape {\n  def area(): Double = side * side\n}\n",
        )
        .write(
            "src/Square.scala",
            "package shapes\n\nclass Square(side: Double) extends Figure {\n  def area(): Double = side * side\n}\n",
        )
        .edge_before("class Square", "extends", "trait Shape")
        .edge_after("class Square", "extends", "trait Figure")
}

pub fn solidity_change_a_base_contract() -> Case {
    Case::new(Change::EditHeritage, &[Language::Solidity])
        .file(
            "contracts/Ownable.sol",
            "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n\ncontract Ownable {\n    address internal owner;\n\n    function transferOwnership(address next) public {\n        owner = next;\n    }\n}\n",
        )
        .file(
            "contracts/Pausable.sol",
            "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n\ncontract Pausable {\n    bool internal paused;\n\n    function pause() public {\n        paused = true;\n    }\n}\n",
        )
        .file(
            "contracts/Token.sol",
            "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n\nimport \"./Ownable.sol\";\nimport \"./Pausable.sol\";\n\ncontract Token is Ownable {\n    function handOver(address next) public {\n        transferOwnership(next);\n    }\n}\n",
        )
        .write(
            "contracts/Token.sol",
            "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n\nimport \"./Ownable.sol\";\nimport \"./Pausable.sol\";\n\ncontract Token is Pausable {\n    function handOver(address next) public {\n        transferOwnership(next);\n    }\n}\n",
        )
        .edge_before("class Token", "extends", "class Ownable")
        .edge_after("class Token", "extends", "class Pausable")
}

/// OpenZeppelin's `Context` and its `ERC2771Context` override, which a bare
/// call reaches only through the caller's `is` chain (upstream #2178).
fn solidity_context_case(change: Change, middle: &'static str) -> Case {
    Case::new(change, &[Language::Solidity])
        .file(
            "contracts/utils/Context.sol",
            "pragma solidity ^0.8.20;\n\nabstract contract Context {\n    function _msgSender() internal view virtual returns (address) {\n        return msg.sender;\n    }\n}\n",
        )
        .file(
            "contracts/metatx/ERC2771Context.sol",
            "pragma solidity ^0.8.20;\n\nimport {Context} from \"../utils/Context.sol\";\n\nabstract contract ERC2771Context is Context {\n    function _msgSender() internal view virtual override returns (address) {\n        return msg.sender;\n    }\n}\n",
        )
        .file("contracts/Middle.sol", middle)
        .file(
            "contracts/Inner.sol",
            "pragma solidity ^0.8.20;\n\nimport \"./Middle.sol\";\n\nabstract contract Inner is Middle {}\n",
        )
        .file(
            "contracts/governance/Governor.sol",
            "pragma solidity ^0.8.20;\n\nimport \"../Inner.sol\";\n\nabstract contract Governor is Inner {\n    function castVote() public returns (address) {\n        return _msgSender();\n    }\n}\n",
        )
}

const MIDDLE_ALONE: &str = "pragma solidity ^0.8.20;\n\nabstract contract Middle {}\n";
const MIDDLE_IS_CONTEXT: &str = "pragma solidity ^0.8.20;\n\nimport \"./utils/Context.sol\";\n\nabstract contract Middle is Context {}\n";
const LEGACY_MIDDLE_IS_CONTEXT: &str = "pragma solidity ^0.8.20;\n\nimport \"../utils/Context.sol\";\n\nabstract contract Middle is Context {}\n";

/// A base two steps up an unchanged contract's chain comes to inherit
/// `Context`: the unchanged contract's bare call reaches Context's function,
/// never the override of a contract it does not inherit (upstream #2178).
pub fn solidity_bare_call_reaches_a_function_its_chain_comes_to_inherit() -> Case {
    solidity_context_case(Change::EditHeritage, MIDDLE_ALONE)
        .write("contracts/Middle.sol", MIDDLE_IS_CONTEXT)
        .edge_after(
            "method Governor::castVote",
            "calls",
            "method Context::_msgSender",
        )
        .no_edge_after(
            "method Governor::castVote",
            "calls",
            "method ERC2771Context::_msgSender",
        )
}

/// The reverse: the base stops inheriting it, and the call links nothing.
pub fn solidity_bare_call_leaves_a_function_its_chain_stops_inheriting() -> Case {
    solidity_context_case(Change::EditHeritage, MIDDLE_IS_CONTEXT)
        .write("contracts/Middle.sol", MIDDLE_ALONE)
        .edge_before(
            "method Governor::castVote",
            "calls",
            "method Context::_msgSender",
        )
        .no_edge_after(
            "method Governor::castVote",
            "calls",
            "method Context::_msgSender",
        )
        .no_edge_after(
            "method Governor::castVote",
            "calls",
            "method ERC2771Context::_msgSender",
        )
}

/// A second declaration of a base's name appears in another file: the chain
/// reads every declaration of a name, so the unchanged contract's call
/// reaches what the new one inherits.
pub fn solidity_bare_call_follows_a_namesake_base_that_appears() -> Case {
    solidity_context_case(Change::AddCompetitor, MIDDLE_ALONE)
        .write("contracts/legacy/Middle.sol", LEGACY_MIDDLE_IS_CONTEXT)
        .edge_after(
            "method Governor::castVote",
            "calls",
            "method Context::_msgSender",
        )
}

/// Upstream #2148: a bare call in a component reaches the components its
/// `extends` chain names, read from their source, so a middle component
/// extending another base in another file moves an unchanged leaf's call.
pub fn cfml_bare_call_follows_an_extends_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cfml])
        .file(
            "system/First.cfc",
            "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
        )
        .file(
            "system/Second.cfc",
            "component {\n  function getSetting( name ) {\n    return 2;\n  }\n}\n",
        )
        .file("system/Middle.cfc", "component extends=\"First\" {\n}\n")
        .file(
            "system/Handler.cfc",
            "component extends=\"Middle\" {\n  function index() {\n    return getSetting( \"appName\" );\n  }\n}\n",
        )
        .write("system/Middle.cfc", "component extends=\"Second\" {\n}\n")
        .edge_before("method Handler::index", "calls", "method First::getSetting")
        .edge_after("method Handler::index", "calls", "method Second::getSetting")
}

/// Upstream #2148: a bare `extends` name means a component of the same
/// directory first, so one appearing beside the handler takes the chain
/// over, and the call to the farther base's method is gone, as on a fresh
/// index.
pub fn cfml_bare_extends_takes_a_component_that_appears_beside_it() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file(
            "lib/Base.cfc",
            "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
        )
        .file(
            "app/Handler.cfc",
            "component extends=\"Base\" {\n  function index() {\n    return getSetting( \"appName\" );\n  }\n}\n",
        )
        .write("app/Base.cfc", "component {\n}\n")
        .edge_before("method Handler::index", "calls", "method Base::getSetting")
        .no_edge_after("method Handler::index", "calls", "method Base::getSetting")
}

/// Upstream #2148: a bare call in a component never reaches a method
/// outside its chain, so a date helper's `now` added in another file leaves
/// the built-in `now()` unresolved.
pub fn cfml_bare_call_ignores_an_added_built_in_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file(
            "system/Base.cfc",
            "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
        )
        .file(
            "system/Handler.cfc",
            "component extends=\"Base\" {\n  function index() {\n    var stamp = now();\n    return getSetting( \"appName\" );\n  }\n}\n",
        )
        .write(
            "system/core/DateTimeHelper.cfc",
            "component {\n  function now() {\n    return 0;\n  }\n}\n",
        )
        .edge_throughout("method Handler::index", "calls", "method Base::getSetting")
        .no_edge_after("method Handler::index", "calls", "method DateTimeHelper::now")
}

/// Upstream #2148: a CFML call through an untyped receiver takes the one
/// method of its name only when the receiver is named after its component,
/// so an interceptor buffer's lone `keyExists` added in another file is not
/// `server.keyExists`.
pub fn cfml_untyped_receiver_ignores_a_lone_added_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file(
            "system/models/UserService.cfc",
            "component {\n  function list() {\n    return [];\n  }\n}\n",
        )
        .file(
            "system/Handler.cfc",
            "component {\n  function index( userService ) {\n    var lucee = server.keyExists( \"lucee\" );\n    return userService.list();\n  }\n}\n",
        )
        .write(
            "system/core/InterceptorBuffer.cfc",
            "component {\n  function keyExists( required key ) {\n    return false;\n  }\n}\n",
        )
        .edge_throughout("method Handler::index", "calls", "method UserService::list")
        .no_edge_after(
            "method Handler::index",
            "calls",
            "method InterceptorBuffer::keyExists",
        )
}

/// Upstream #2148's relaxation: a receiver whose whole name spells a
/// component (`cbsecurity` -> CBSecurity) takes that component's method once
/// it appears in another file.
pub fn cfml_receiver_named_after_an_added_component() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Cfml])
        .file(
            "system/Guard.cfc",
            "component {\n  function guard( cbsecurity ) {\n    return cbsecurity.secure( \"x\" );\n  }\n}\n",
        )
        .write(
            "system/security/CBSecurity.cfc",
            "component {\n  function secure( event ) {\n    return true;\n  }\n}\n",
        )
        .edge_after("method Guard::guard", "calls", "method CBSecurity::secure")
}

/// Upstream #2148: an inherited method's body changing leaves the unchanged
/// component's bare call where it was.
pub fn cfml_inherited_method_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "system/Base.cfc",
            "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
        )
        .file(
            "system/Handler.cfc",
            "component extends=\"app.system.Base\" {\n  function index() {\n    return getSetting( \"appName\" );\n  }\n}\n",
        )
        .write(
            "system/Base.cfc",
            "component {\n  function getSetting( name ) {\n    return 2;\n  }\n}\n",
        )
        .edge_throughout("method Handler::index", "calls", "method Base::getSetting")
}

/// Upstream #1152: a dotted component path names its supertype by
/// directory, so the component it names appearing in another file links the
/// unchanged subclass, as on a fresh index.
pub fn cfml_dotted_extends_of_an_added_component() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Cfml])
        .file(
            "handlers/Main.cfc",
            "component extends=\"coldbox.system.web.Controller\" {\n  function index() {\n    return handle();\n  }\n}\n",
        )
        .write(
            "system/web/Controller.cfc",
            "component {\n  function handle() {\n    return 1;\n  }\n}\n",
        )
        .edge_after(
            "class handlers/Main.cfc::Main",
            "extends",
            "class system/web/Controller.cfc::Controller",
        )
}

/// Upstream #1152: among same-named components the one whose directories
/// corroborate the most of the path wins, so a better-corroborated one
/// appearing in another file takes the unchanged subclass's `extends`.
pub fn cfml_dotted_extends_takes_a_better_corroborated_component() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file("web/Controller.cfc", "component {\n}\n")
        .file(
            "handlers/Main.cfc",
            "component extends=\"coldbox.system.web.Controller\" {\n}\n",
        )
        .write("system/web/Controller.cfc", "component {\n}\n")
        .edge_before(
            "class handlers/Main.cfc::Main",
            "extends",
            "class web/Controller.cfc::Controller",
        )
        .edge_after(
            "class handlers/Main.cfc::Main",
            "extends",
            "class system/web/Controller.cfc::Controller",
        )
        .no_edge_after(
            "class handlers/Main.cfc::Main",
            "extends",
            "class web/Controller.cfc::Controller",
        )
}

/// Upstream #1152 / #1154: a relative component path (FW/1's `../base`)
/// resolves against the referencing file once the component appears.
pub fn cfml_relative_extends_of_an_added_component() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Cfml])
        .file(
            "examples/sub/app.cfc",
            "component extends=\"../base\" {\n}\n",
        )
        .write(
            "examples/base.cfc",
            "component {\n  function shared() {\n    return 1;\n  }\n}\n",
        )
        .edge_after(
            "class examples/sub/app.cfc::app",
            "extends",
            "class examples/base.cfc::base",
        )
}

/// Upstream #1155: a receiver typed by its declaration reaches the method its
/// component declares, so the method appearing in that component's file
/// links the unchanged call, and never the same-named method of another.
pub fn cfml_typed_receiver_reaches_a_method_its_type_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "svc/UserService.cfc",
            "component {\n  function load(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "svc/OrderService.cfc",
            "component {\n  function save(any o) {\n    return o;\n  }\n}\n",
        )
        .file(
            "handlers/Main.cfc",
            "component {\n  function bare() {\n    var svc = new UserService();\n    return svc.save(1);\n  }\n}\n",
        )
        .write(
            "svc/UserService.cfc",
            "component {\n  function load(any u) {\n    return u;\n  }\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .edge_after("method Main::bare", "calls", "method UserService::save")
        .no_edge_after("method Main::bare", "calls", "method OrderService::save")
}

/// W4-CF-02: a same-named component appearing in another directory leaves
/// the unchanged caller's bare type name ambiguous, so its call loses its
/// edge, while a written path still tells the components apart.
pub fn cfml_typed_receiver_namesake_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file(
            "a/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "handlers/Main.cfc",
            "component {\n  function bare() {\n    var svc = new UserService();\n    return svc.save(1);\n  }\n  function dotted() {\n    var svc = new a.UserService();\n    return svc.save(2);\n  }\n}\n",
        )
        .write(
            "b/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .edge_before(
            "method Main::bare",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
        .no_edge_after(
            "method Main::bare",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
        .no_edge_after(
            "method Main::bare",
            "calls",
            "method UserService::save @b/UserService.cfc",
        )
        .edge_after(
            "method Main::dotted",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
}

/// W4-CF-02: once the namesake leaves, the unchanged caller's bare type
/// name names one component again, and its call, unresolved while the two
/// stood side by side, reaches it.
pub fn cfml_typed_receiver_namesake_leaves() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Cfml])
        .file(
            "a/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "b/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "handlers/Main.cfc",
            "component {\n  function bare() {\n    var svc = new UserService();\n    return svc.save(1);\n  }\n}\n",
        )
        .remove("b/UserService.cfc")
        .edge_after(
            "method Main::bare",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
}

/// More unresolved `….save` calls than one tail retries in a sync
/// (`NAME_TAIL_RETRY_CEILING`, 500): untyped receivers no `save` is named
/// after.
static CFML_UNTYPED_SAVE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("component {\n  function noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("    n{index}.save({index});\n"));
    }
    text.push_str("    return 0;\n  }\n}\n");
    text
});

/// W4-CF-02: the namesake leaving settles the unchanged caller's parked
/// typed call even when more parked calls share its tail than the name-tail
/// retry wakes.
pub fn cfml_typed_receiver_namesake_leaves_past_the_tail_ceiling() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Cfml])
        .file(
            "a/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "b/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "handlers/Main.cfc",
            "component {\n  function bare() {\n    var svc = new UserService();\n    return svc.save(1);\n  }\n}\n",
        )
        .file("handlers/Noise.cfc", CFML_UNTYPED_SAVE_CALLS.as_str())
        .remove("b/UserService.cfc")
        .edge_after(
            "method Main::bare",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
}

/// W4-CF-02: a namesake whose directories corroborate more of the written
/// path takes the unchanged caller's typed call.
pub fn cfml_typed_receiver_takes_a_better_corroborated_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Cfml])
        .file(
            "a/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .file(
            "handlers/Main.cfc",
            "component {\n  function dotted() {\n    var svc = new x.a.UserService();\n    return svc.save(1);\n  }\n}\n",
        )
        .write(
            "x/a/UserService.cfc",
            "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
        )
        .edge_before(
            "method Main::dotted",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
        .edge_after(
            "method Main::dotted",
            "calls",
            "method UserService::save @x/a/UserService.cfc",
        )
        .no_edge_after(
            "method Main::dotted",
            "calls",
            "method UserService::save @a/UserService.cfc",
        )
}

/// Upstream #2296: a `<cfscript>` block at component scope declares the
/// component's methods, read with the cfscript grammar in place; editing a
/// method's body moves its call.
pub fn cfml_script_tag_method_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "mod/Helpers.cfc",
            "component {\n  function first() {\n    return 1;\n  }\n  function second() {\n    return 2;\n  }\n}\n",
        )
        .file(
            "mod/ModuleConfig.cfc",
            "<cfcomponent>\n<cfscript>\nfunction configure() {\n  var helpers = new Helpers();\n  return helpers.first();\n}\n</cfscript>\n</cfcomponent>\n",
        )
        .write(
            "mod/ModuleConfig.cfc",
            "<cfcomponent>\n<cfscript>\nfunction configure() {\n  var helpers = new Helpers();\n  return helpers.second();\n}\n</cfscript>\n</cfcomponent>\n",
        )
        .edge_before("method ModuleConfig::configure", "calls", "method Helpers::first")
        .edge_after("method ModuleConfig::configure", "calls", "method Helpers::second")
}

/// Upstream #2296: a `<cfquery>` body's `#hash#` calls and a method wrapped
/// in `<cfprocessingdirective>` are the method's; an edit to the SQL moves
/// the call.
pub fn cfml_query_body_in_a_wrapped_method_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "Application.cfc",
            "<cfcomponent>\n<cfprocessingdirective suppresswhitespace=\"true\">\n<cffunction name=\"onRequestStart\">\n<cfquery name=\"q\">SELECT #firstColumn()# FROM t</cfquery>\n</cffunction>\n<cffunction name=\"firstColumn\"><cfreturn \"a\"></cffunction>\n<cffunction name=\"secondColumn\"><cfreturn \"b\"></cffunction>\n</cfprocessingdirective>\n</cfcomponent>\n",
        )
        .write(
            "Application.cfc",
            "<cfcomponent>\n<cfprocessingdirective suppresswhitespace=\"true\">\n<cffunction name=\"onRequestStart\">\n<cfquery name=\"q\">SELECT #secondColumn()# FROM t</cfquery>\n</cffunction>\n<cffunction name=\"firstColumn\"><cfreturn \"a\"></cffunction>\n<cffunction name=\"secondColumn\"><cfreturn \"b\"></cffunction>\n</cfprocessingdirective>\n</cfcomponent>\n",
        )
        .edge_before(
            "method Application::onRequestStart",
            "calls",
            "method Application::firstColumn",
        )
        .edge_after(
            "method Application::onRequestStart",
            "calls",
            "method Application::secondColumn",
        )
}

/// `(((…1)))` nested 400 deep: past the walker's 256-level limit in a
/// re-parsed `<cfscript>` or `<cfquery>` body, inside a shallow tag tree.
fn cfml_deep_parentheses() -> String {
    format!("{}1{}", "(".repeat(400), ")".repeat(400))
}

static DEEP_SCRIPT_MODULE_CONFIG: LazyLock<String> = LazyLock::new(|| {
    format!(
        "<cfcomponent>\n<cfscript>\nfunction configure() {{\n  var helpers = new Helpers();\n  var x = {};\n  return helpers.first();\n}}\n</cfscript>\n</cfcomponent>\n",
        cfml_deep_parentheses()
    )
});

static DEEP_QUERY_MODULE_CONFIG: LazyLock<String> = LazyLock::new(|| {
    format!(
        "<cfcomponent>\n<cffunction name=\"configure\">\n<cfquery name=\"q\">SELECT #{}# FROM t</cfquery>\n<cfreturn first()>\n</cffunction>\n<cffunction name=\"first\"><cfreturn 1></cffunction>\n</cfcomponent>\n",
        cfml_deep_parentheses()
    )
});

/// A `<cfscript>` body nested past the walker's depth limit fails its whole
/// file: the component's class and methods leave the index, so another
/// component's `extends` of its name takes the namesake elsewhere, as a fresh
/// index has it.
pub fn cfml_script_tag_body_nested_past_the_depth_limit_drops_the_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "mod/Helpers.cfc",
            "component {\n  function first() {\n    return 1;\n  }\n}\n",
        )
        .file(
            "mod/ModuleConfig.cfc",
            "<cfcomponent>\n<cfscript>\nfunction configure() {\n  var helpers = new Helpers();\n  var x = (1);\n  return helpers.first();\n}\n</cfscript>\n</cfcomponent>\n",
        )
        .file(
            "other/ModuleConfig.cfc",
            "component {\n  function configure() {\n    return 0;\n  }\n}\n",
        )
        .file("mod/Child.cfc", "component extends=\"ModuleConfig\" {\n}\n")
        .write("mod/ModuleConfig.cfc", DEEP_SCRIPT_MODULE_CONFIG.as_str())
        .edge_before(
            "method ModuleConfig::configure @mod/ModuleConfig.cfc",
            "calls",
            "method Helpers::first",
        )
        .edge_before(
            "class mod/Child.cfc::Child",
            "extends",
            "class mod/ModuleConfig.cfc::ModuleConfig",
        )
        .edge_after(
            "class mod/Child.cfc::Child",
            "extends",
            "class other/ModuleConfig.cfc::ModuleConfig",
        )
}

/// The same for a `<cfquery>` body whose `#hash#` expression nests past the
/// limit inside a tag function.
pub fn cfml_query_body_nested_past_the_depth_limit_drops_the_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cfml])
        .file(
            "mod/ModuleConfig.cfc",
            "<cfcomponent>\n<cffunction name=\"configure\">\n<cfquery name=\"q\">SELECT #(1)# FROM t</cfquery>\n<cfreturn first()>\n</cffunction>\n<cffunction name=\"first\"><cfreturn 1></cffunction>\n</cfcomponent>\n",
        )
        .file(
            "other/ModuleConfig.cfc",
            "component {\n  function configure() {\n    return 0;\n  }\n}\n",
        )
        .file("mod/Child.cfc", "component extends=\"ModuleConfig\" {\n}\n")
        .write("mod/ModuleConfig.cfc", DEEP_QUERY_MODULE_CONFIG.as_str())
        .edge_before(
            "method ModuleConfig::configure @mod/ModuleConfig.cfc",
            "calls",
            "method ModuleConfig::first @mod/ModuleConfig.cfc",
        )
        .edge_before(
            "class mod/Child.cfc::Child",
            "extends",
            "class mod/ModuleConfig.cfc::ModuleConfig",
        )
        .edge_after(
            "class mod/Child.cfc::Child",
            "extends",
            "class other/ModuleConfig.cfc::ModuleConfig",
        )
}

/// Upstream #2296: `<cfinterface>` is an interface, so a component's
/// `implements` reaches it once the file appears.
pub fn cfml_implements_an_added_interface_tag() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Cfml])
        .file(
            "model/Catalog.cfc",
            "<cfcomponent implements=\"ISearchable\">\n<cffunction name=\"search\" access=\"public\"></cffunction>\n</cfcomponent>\n",
        )
        .write(
            "model/ISearchable.cfc",
            "<cfinterface>\n<cffunction name=\"search\" access=\"public\"></cffunction>\n</cfinterface>\n",
        )
        .edge_after(
            "class model/Catalog.cfc::Catalog",
            "implements",
            "interface model/ISearchable.cfc::ISearchable",
        )
}

pub fn cfml_change_a_base_component() -> Case {
    Case::new(Change::EditHeritage, &[Language::Cfml])
        .file(
            "models/Base.cfc",
            "component {\n    function describe() {\n        return \"base\";\n    }\n}\n",
        )
        .file(
            "models/Person.cfc",
            "component {\n    function describe() {\n        return \"person\";\n    }\n}\n",
        )
        .file(
            "models/User.cfc",
            "component extends=\"Base\" {\n    function greet() {\n        return describe();\n    }\n}\n",
        )
        .write(
            "models/User.cfc",
            "component extends=\"Person\" {\n    function greet() {\n        return describe();\n    }\n}\n",
        )
        .edge_before(
            "class models/User.cfc::User",
            "extends",
            "class models/Base.cfc::Base",
        )
        .edge_after(
            "class models/User.cfc::User",
            "extends",
            "class models/Person.cfc::Person",
        )
}

// ----- a second declaration of a referenced name appears -------------------

pub fn python_add_a_same_named_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Python])
        .file("tools/alpha.py", "def helper():\n    return 1\n")
        .file("main.py", "def main():\n    return helper()\n")
        .write("tools/beta.py", "def helper():\n    return 2\n")
        .edge_before("function main", "calls", "function helper @tools/alpha.py")
}

pub fn arkts_add_a_same_named_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::ArkTs])
        .file(
            "entry/Util.ets",
            "export function greet(name: string): string {\n  return 'hi ' + name;\n}\n",
        )
        .file(
            "entry/Index.ets",
            "import { greet } from './Util';\n\n@Entry\n@Component\nstruct Index {\n  build() {\n    Text(greet('there'))\n  }\n}\n",
        )
        .write(
            "entry/Other.ets",
            "export function greet(name: string): string {\n  return 'hello ' + name;\n}\n",
        )
        .edge_before("method Index::build", "calls", "function greet @entry/Util.ets")
}

pub fn kotlin_add_a_same_named_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Kotlin])
        .file(
            "src/Util.kt",
            "package shop\n\nfun formatPrice(cents: Int): String {\n    return \"$\" + cents / 100\n}\n",
        )
        .file(
            "src/Cart.kt",
            "package shop\n\nclass Cart {\n    fun label(cents: Int): String {\n        return formatPrice(cents)\n    }\n}\n",
        )
        .write(
            "src/Other.kt",
            "package other\n\nfun formatPrice(cents: Int): String {\n    return cents.toString()\n}\n",
        )
        .edge_before("method shop::Cart::label", "calls", "function shop::formatPrice")
}

pub fn ruby_add_a_same_named_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Ruby])
        .file(
            "lib/greeter.rb",
            "class Greeter\n  def greet(name)\n    \"hi #{name}\"\n  end\nend\n",
        )
        .file(
            "lib/app.rb",
            "require_relative 'greeter'\n\nclass App\n  def run\n    Greeter.new.greet('there')\n  end\nend\n",
        )
        .write(
            "lib/robot.rb",
            "class Robot\n  def greet(name)\n    \"beep #{name}\"\n  end\nend\n",
        )
        .edge_before("method App::run", "calls", "method Greeter::greet")
}

/// Upstream #2147: a bare Ruby call in a class reaches the modules its
/// ancestors mix in, read from their source, so a superclass gaining an
/// `include` in another file links an unchanged subclass's call.
pub fn ruby_bare_call_follows_a_mixin_added_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Ruby])
        .file(
            "lib/helpers.rb",
            "module Helpers\n  def helper\n    1\n  end\nend\n",
        )
        .file("lib/base.rb", "class Base\nend\n")
        .file(
            "lib/foo.rb",
            "class Foo < Base\n  def run\n    helper()\n  end\nend\n",
        )
        .write("lib/base.rb", "class Base\n  include Helpers\nend\n")
        .edge_after("method Foo::run", "calls", "method Helpers::helper")
}

/// Upstream #2147: a bare Ruby call follows the superclass chain, so a
/// middle class changing its superclass in another file moves an unchanged
/// leaf's call.
pub fn ruby_bare_call_follows_a_superclass_change_in_another_file() -> Case {
    Case::new(Change::EditHeritage, &[Language::Ruby])
        .file(
            "lib/first.rb",
            "class First\n  def setup\n    1\n  end\nend\n",
        )
        .file(
            "lib/second.rb",
            "class Second\n  def setup\n    2\n  end\nend\n",
        )
        .file("lib/middle.rb", "class Middle < First\nend\n")
        .file(
            "lib/leaf.rb",
            "class Leaf < Middle\n  def run\n    setup()\n  end\nend\n",
        )
        .write("lib/middle.rb", "class Middle < Second\nend\n")
        .edge_before("method Leaf::run", "calls", "method First::setup")
        .edge_after("method Leaf::run", "calls", "method Second::setup")
}

/// Upstream #2147: a mixed-in constant is resolved against the lexical
/// nesting, so a nearer module of that name appearing in another file takes
/// the `include` over, and an unchanged class's call to the farther module's
/// method is gone, as on a fresh index.
pub fn ruby_mixin_resolves_to_a_nearer_module_that_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Ruby])
        .file(
            "lib/app/range_help.rb",
            "module App\n  module RangeHelp\n    def range_between(a, b)\n      [a, b]\n    end\n  end\nend\n",
        )
        .file(
            "lib/app/style/foo.rb",
            "module App\n  module Style\n    class Foo\n      include RangeHelp\n\n      def run\n        range_between(1, 2)\n      end\n    end\n  end\nend\n",
        )
        .write(
            "lib/app/style/range_help.rb",
            "module App\n  module Style\n    module RangeHelp\n    end\n  end\nend\n",
        )
        .edge_before(
            "method App::Style::Foo::run",
            "calls",
            "method App::RangeHelp::range_between",
        )
        .no_edge_after(
            "method App::Style::Foo::run",
            "calls",
            "method App::RangeHelp::range_between",
        )
}

/// Upstream #2147: a bare Ruby call in a class never reaches a method outside
/// its ancestry, so an unrelated class's `format` added in another file
/// leaves rubocop's Kernel `format(…)` unresolved.
pub fn ruby_bare_call_ignores_an_added_unrelated_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Ruby])
        .file(
            "lib/base.rb",
            "class Base\n  def add_offense(node)\n    node\n  end\nend\n",
        )
        .file(
            "lib/foo.rb",
            "class Foo < Base\n  def run(node)\n    add_offense(node)\n    format(\"x\")\n  end\nend\n",
        )
        .write(
            "lib/runtime.rb",
            "class Runtime\n  def format(text)\n    text\n  end\nend\n",
        )
        .edge_throughout("method Foo::run", "calls", "method Base::add_offense")
        .no_edge_after("method Foo::run", "calls", "method Runtime::format")
}

/// Upstream #2147: a Ruby call through an untyped receiver takes the one
/// method of its name only when the receiver is named after its owner, so a
/// lone `loc` added in another file is not `node.loc`.
pub fn ruby_untyped_receiver_ignores_a_lone_added_method() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Ruby])
        .file(
            "lib/web_push_request.rb",
            "class WebPushRequest\n  def legacy_encrypt(payload)\n    payload\n  end\nend\n",
        )
        .file(
            "lib/sender.rb",
            "class Sender\n  def run(web_push_request, node)\n    web_push_request.legacy_encrypt(1)\n    node.loc\n  end\nend\n",
        )
        .write(
            "lib/expression.rb",
            "module Expression\n  def loc\n    @loc\n  end\nend\n",
        )
        .edge_throughout(
            "method Sender::run",
            "calls",
            "method WebPushRequest::legacy_encrypt",
        )
        .no_edge_after("method Sender::run", "calls", "method Expression::loc")
}

/// Upstream #2147: an ancestor's method body changing leaves the unchanged
/// subclass's bare call where it was.
pub fn ruby_ancestor_method_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Ruby])
        .file(
            "lib/base.rb",
            "class Base\n  def add_offense(node)\n    node\n  end\nend\n",
        )
        .file(
            "lib/foo.rb",
            "class Foo < Base\n  def run(node)\n    add_offense(node)\n  end\nend\n",
        )
        .write(
            "lib/base.rb",
            "class Base\n  def add_offense(node)\n    [node]\n  end\nend\n",
        )
        .edge_throughout("method Foo::run", "calls", "method Base::add_offense")
}

pub fn php_add_a_same_named_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "src/Mailer.php",
            "<?php\nnamespace App;\n\nclass Mailer\n{\n    public function send(string $to): bool\n    {\n        return true;\n    }\n}\n",
        )
        .file(
            "src/Signup.php",
            "<?php\nnamespace App;\n\nuse App\\Mailer;\n\nclass Signup\n{\n    public function register(string $email): bool\n    {\n        $mailer = new Mailer();\n        return $mailer->send($email);\n    }\n}\n",
        )
        .write(
            "src/Queue.php",
            "<?php\nnamespace App;\n\nclass Queue\n{\n    public function send(string $to): bool\n    {\n        return false;\n    }\n}\n",
        )
        // The typed receiver keeps its target; the competitor only adds a name.
        .edge_throughout(
            "method App::Signup::register",
            "calls",
            "method App::Mailer::send",
        )
}

// ----- a project control file changes --------------------------------------

/// Upstream #965: an import statement is never a name-match candidate, so a
/// PHP `use` binds the namespace it names, ahead of its own statement, and
/// another file's identical `use` is no competitor either.
pub fn php_use_of_a_namespace_gains_a_same_named_import_elsewhere() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Php])
        .file(
            "src/Field/Base.php",
            "<?php\n\nnamespace App\\Field;\n\nclass Base\n{\n}\n",
        )
        .file(
            "src/Controller.php",
            "<?php\n\nnamespace App;\n\nuse App\\Field;\n\nclass Controller\n{\n}\n",
        )
        .write(
            "src/Other.php",
            "<?php\n\nnamespace App;\n\nuse App\\Field;\n\nclass Other\n{\n}\n",
        )
        .edge_throughout(
            "namespace App @src/Controller.php",
            "imports",
            "namespace App\\Field @src/Field/Base.php",
        )
}

/// Upstream #2437: a catch-all `"*"` path alias does not make a package
/// import local, so a project component that appears with the package's name
/// is no target for it, while the project's own import of it resolves.
pub fn tsx_catch_all_alias_ignores_a_same_named_project_component() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Tsx])
        .file(
            "tsconfig.json",
            "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"*\": [\"./typings/*\"] }\n  }\n}\n",
        )
        .file(
            "src/App.tsx",
            "import { Typography } from '@mui/material';\n\nexport function App() {\n  return <Typography variant=\"h1\" />;\n}\n",
        )
        .file(
            "src/Page.tsx",
            "import { Typography } from './Typography';\n\nexport function Page() {\n  return <Typography text=\"hi\" />;\n}\n",
        )
        .write(
            "src/Typography.tsx",
            "export function Typography(props: { text: string }) {\n  return <span>{props.text}</span>;\n}\n",
        )
        .edge_after(
            "file:src/Page.tsx",
            "imports",
            "function Typography @src/Typography.tsx",
        )
        .no_edge_after("file:src/App.tsx", "imports", "function Typography")
}

/// The index root's `config.toml` is a watcher control file: the watcher
/// reloads its scope and reconciles the whole project.
pub fn typescript_config_toml_excludes_a_competitor() -> Case {
    Case::new(
        Change::ControlFile,
        &[Language::TypeScript, Language::JavaScript],
    )
    .file(
        "src/math.ts",
        "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
    )
    .file(
        "src/app.ts",
        "export function run(): number {\n  return add(1, 2);\n}\n",
    )
    .file(
        "web/bundle.js",
        "function add(a, b) {\n  return a + b;\n}\n",
    )
    .write(
        ".codegraph/config.toml",
        "[app]\nname = \"matrix\"\n\n[indexing]\nexclude = [\"web/\"]\n",
    )
    .edge_throughout("function run", "calls", "function add @src/math.ts")
}

/// The root `.gitignore` is a watcher control file too. The hidden directory
/// is one the default ignores keep (`vendor/` is never indexed at all), so the
/// call moves from the competitor to the remaining definition.
pub fn python_root_gitignore_hides_a_competitor() -> Case {
    Case::new(Change::ControlFile, &[Language::Python])
        .file("tools/alpha.py", "def helper():\n    return 1\n")
        .file("legacy/beta.py", "def helper():\n    return 2\n")
        .file("main.py", "def main():\n    return helper()\n")
        .write(".gitignore", "legacy/\n")
        .edge_before("function main", "calls", "function helper @legacy/beta.py")
        .edge_after("function main", "calls", "function helper @tools/alpha.py")
}

/// The repository's `.git/info/exclude` is read with the root `.gitignore`
/// (upstream #1728), so it is a control file as well.
pub fn python_repository_exclude_hides_a_competitor() -> Case {
    Case::new(Change::ControlFile, &[Language::Python])
        .file("tools/alpha.py", "def helper():\n    return 1\n")
        .file("legacy/beta.py", "def helper():\n    return 2\n")
        .file("main.py", "def main():\n    return helper()\n")
        .file(".git/info/exclude", "# git ls-files --exclude-standard\n")
        .write(".git/info/exclude", "legacy/\n")
        .edge_before("function main", "calls", "function helper @legacy/beta.py")
        .edge_after("function main", "calls", "function helper @tools/alpha.py")
}

pub fn lua_extension_override_appears_in_codegraph_json() -> Case {
    Case::new(Change::ControlFile, &[Language::Lua])
        .file(
            "scripts/util.luax",
            "local function shout(text)\n  return text .. \"!\"\nend\n\nreturn shout\n",
        )
        .file(
            "main.lua",
            "local shout = require('scripts.util')\nprint(shout('hi'))\n",
        )
        .write(
            ".codegraph/codegraph.json",
            "{ \"extensions\": { \".luax\": \"lua\" } }\n",
        )
        .edge_after("file:scripts/util.luax", "contains", "function shout")
}

/// Found by this matrix: only a fresh index resolves the import through the
/// new `paths` alias; the synced one keeps its bare-name guess.
pub fn typescript_tsconfig_paths_alias_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { double } from '@lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .write(
            "tsconfig.json",
            "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@lib/*\": [\"src/lib/*\"] }\n  }\n}\n",
        )
        .edge_after("file:src/main.ts", "imports", "function double")
}

/// A `tsconfig.json` past 64 MiB, its `paths` sending `@lib/*` to `target`,
/// padded with whitespace to a fixed length.
fn large_tsconfig(target: &str) -> String {
    let text = format!(
        "{{\"compilerOptions\":{{\"baseUrl\":\".\",\"paths\":{{\"@lib/*\":[\"{target}\"]}}}}}}"
    );
    format!("{text}{}\n", " ".repeat(64 * 1024 * 1024 + 1))
}

static LARGE_TSCONFIG_ELSEWHERE: LazyLock<String> = LazyLock::new(|| large_tsconfig("src/xyz/*"));
static LARGE_TSCONFIG_LIB: LazyLock<String> = LazyLock::new(|| large_tsconfig("src/lib/*"));

/// W5 goal review: a control file is read whole whatever its size. A
/// `tsconfig.json` past 64 MiB, replaced with its length and modification
/// time kept, moves the alias, as a fresh index of the new text has it (the
/// digest once stood in the size and mtime for the text past 64 MiB).
pub fn typescript_tsconfig_past_64_mib_replaced_with_its_size_and_mtime_kept() -> Case {
    assert_eq!(LARGE_TSCONFIG_ELSEWHERE.len(), LARGE_TSCONFIG_LIB.len());
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("tsconfig.json", LARGE_TSCONFIG_ELSEWHERE.as_str())
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { double } from '@lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .replace("tsconfig.json", LARGE_TSCONFIG_LIB.as_str())
        .edge_after("file:src/main.ts", "imports", "function double")
}

/// Found by this matrix: only a fresh index detected React. React coming into
/// `package.json` makes the data router's table a route that renders its
/// component (the React resolver's extraction runs only once React is
/// detected).
pub fn typescript_react_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\"}}\n",
        )
        .file(
            "src/dashboard.ts",
            "export function Dashboard(): null {\n  return null;\n}\n",
        )
        .file(
            "src/routes.ts",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { Dashboard } from './dashboard';\n\nexport const router = createBrowserRouter([{ path: '/dash', Component: Dashboard }]);\n",
        )
        .write(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\",\"react\":\"18.2.0\"}}\n",
        )
        .edge_before("file:src/routes.ts", "imports", "function Dashboard")
        .edge_after("route src/routes.ts::route:/dash", "references", "function Dashboard")
}

/// W5-06: a `jsconfig.json` gaining `paths` makes the alias import resolve.
pub fn javascript_jsconfig_paths_alias_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::JavaScript])
        .file(
            "src/lib/math.js",
            "export function double(value) {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.js",
            "import { double } from '@lib/math';\n\nexport function run() {\n  return double(2);\n}\n",
        )
        .write(
            "jsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/lib/*\"]}}}",
        )
        .edge_after("file:src/main.js", "imports", "function double")
}

/// W5-06: the tsconfig deleted, the alias import no longer resolves.
pub fn typescript_tsconfig_paths_alias_disappears() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/lib/*\"]}}}",
        )
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { double } from '@lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .edge_before("file:src/main.ts", "imports", "function double")
        .remove("tsconfig.json")
}

/// W5-06: the tsconfig `extends` a file that does not exist yet; once it is
/// created with `paths`, the alias import resolves (the missing target is a
/// recorded input).
pub fn typescript_tsconfig_extends_target_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("tsconfig.json", "{\"extends\":\"./configs/base.json\"}")
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { double } from '@lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .write(
            "configs/base.json",
            "{\"compilerOptions\":{\"baseUrl\":\"..\",\"paths\":{\"@lib/*\":[\"src/lib/*\"]}}}",
        )
        .edge_after("file:src/main.ts", "imports", "function double")
}

/// The two `format` modules an app's `@/lib/format` import may name: the
/// root's alias sends it to `shared/`, the app's own to its `src/`.
fn app_alias_files(case: Case) -> Case {
    case.file(
        "tsconfig.json",
        "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@/*\":[\"./shared/*\"]}}}",
    )
    .file(
        "shared/lib/format.ts",
        "export function format(value: number): string {\n  return String(value);\n}\n",
    )
    .file(
        "apps/web/src/lib/format.ts",
        "export function format(value: number): string {\n  return value.toFixed(2);\n}\n",
    )
    .file(
        "apps/web/src/main.ts",
        "import { format } from '@/lib/format';\n\nexport function render(): string {\n  return format(1);\n}\n",
    )
}

/// W7-09a (upstream #2165): an app's own tsconfig appearing with `paths`
/// takes its imports from the root's alias to its own, as a fresh index
/// reads the nearest config first.
pub fn typescript_app_tsconfig_paths_appear() -> Case {
    app_alias_files(Case::new(Change::ControlFile, &[Language::TypeScript]))
        .edge_before(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @shared/lib/format.ts",
        )
        .write(
            "apps/web/tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@/*\":[\"./src/*\"]}}}",
        )
        .edge_after(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @apps/web/src/lib/format.ts",
        )
}

/// W7-09a: an app's tsconfig losing its `paths` gives its imports back to
/// the root's alias.
pub fn typescript_app_tsconfig_paths_retarget_to_the_root() -> Case {
    app_alias_files(Case::new(Change::ControlFile, &[Language::TypeScript]))
        .file(
            "apps/web/tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@/*\":[\"./src/*\"]}}}",
        )
        .edge_before(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @apps/web/src/lib/format.ts",
        )
        .write(
            "apps/web/tsconfig.json",
            "{\"compilerOptions\":{\"strict\":true}}",
        )
        .edge_after(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @shared/lib/format.ts",
        )
}

/// W7-09a: an app's tsconfig deleted, its imports fall back to the root's
/// alias.
pub fn typescript_app_tsconfig_is_deleted() -> Case {
    app_alias_files(Case::new(Change::ControlFile, &[Language::TypeScript]))
        .file(
            "apps/web/tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@/*\":[\"./src/*\"]}}}",
        )
        .edge_before(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @apps/web/src/lib/format.ts",
        )
        .remove("apps/web/tsconfig.json")
        .edge_after(
            "file:apps/web/src/main.ts",
            "imports",
            "function format @shared/lib/format.ts",
        )
}

/// W7-09a: an app's jsconfig appearing beside its sources gives it `~/*`,
/// which no root config declares (LibreChat's `client/`). The namesake
/// beside the importer is the one a bare-name match would pick.
pub fn javascript_app_jsconfig_paths_appear() -> Case {
    Case::new(Change::ControlFile, &[Language::JavaScript])
        .file(
            "client/src/lib/format.js",
            "export function format(value) {\n  return String(value);\n}\n",
        )
        .file(
            "client/src/format.js",
            "export function format(value) {\n  return `${value}`;\n}\n",
        )
        .file(
            "client/src/main.js",
            "import { format } from '~/lib/format';\n\nexport function render() {\n  return format(1);\n}\n",
        )
        .write(
            "client/jsconfig.json",
            "{\"compilerOptions\":{\"paths\":{\"~/*\":[\"./src/*\"]}}}",
        )
        .edge_after(
            "file:client/src/main.js",
            "imports",
            "function format @client/src/lib/format.js",
        )
}

/// W5-06: `paths` pointed elsewhere moves what a barrel's alias source
/// resolves to, and the import through the barrel follows.
pub fn typescript_paths_retarget_moves_a_barrel_source() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/lib/*\"]}}}",
        )
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/alt/math.ts",
            "export function double(value: number): number {\n  return value + value;\n}\n",
        )
        .file("src/index.ts", "export * from '@lib/math';\n")
        .file(
            "src/main.ts",
            "import { double } from './index';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .write(
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/alt/*\"]}}}",
        )
        .edge_before("function run", "calls", "function double @src/lib/math.ts")
        .edge_after("function run", "calls", "function double @src/alt/math.ts")
}

/// W5-06: React leaving `package.json` takes the framework's route node with
/// it; the import of the component stays.
pub fn typescript_react_dependency_leaves_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\",\"react\":\"18.2.0\"}}\n",
        )
        .file(
            "src/dashboard.ts",
            "export function Dashboard(): null {\n  return null;\n}\n",
        )
        .file(
            "src/routes.ts",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { Dashboard } from './dashboard';\n\nexport const router = createBrowserRouter([{ path: '/dash', Component: Dashboard }]);\n",
        )
        .write(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\"}}\n",
        )
        .edge_before("route src/routes.ts::route:/dash", "references", "function Dashboard")
        .edge_after("file:src/routes.ts", "imports", "function Dashboard")
}

/// Upstream #2395: React coming into an app's manifest rather than the
/// root's detects React as the root's does
/// ([`typescript_react_dependency_appears_in_package_json`]).
pub fn typescript_react_dependency_appears_in_an_app_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"mono\",\"private\":true}\n")
        .file(
            "apps/web/package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\"}}\n",
        )
        .file(
            "apps/web/src/dashboard.ts",
            "export function Dashboard(): null {\n  return null;\n}\n",
        )
        .file(
            "apps/web/src/routes.ts",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { Dashboard } from './dashboard';\n\nexport const router = createBrowserRouter([{ path: '/dash', Component: Dashboard }]);\n",
        )
        .write(
            "apps/web/package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\",\"react\":\"18.2.0\"}}\n",
        )
        .edge_before("file:apps/web/src/routes.ts", "imports", "function Dashboard")
        .edge_after(
            "route apps/web/src/routes.ts::route:/dash",
            "references",
            "function Dashboard",
        )
}

/// Upstream #2395: detection reads a manifest one directory down whatever is
/// indexed below it, so React declared in `frontend/package.json` beside
/// only a Python build script detects React on the next sync too.
pub fn typescript_react_dependency_appears_above_no_script() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"web\",\"private\":true}\n")
        .file("frontend/build.py", "def build():\n    return 1\n")
        .file(
            "src/dashboard.ts",
            "export function Dashboard(): null {\n  return null;\n}\n",
        )
        .file(
            "src/routes.ts",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { Dashboard } from './dashboard';\n\nexport const router = createBrowserRouter([{ path: '/dash', Component: Dashboard }]);\n",
        )
        .write(
            "frontend/package.json",
            "{\"name\":\"assets\",\"devDependencies\":{\"react\":\"18.2.0\"}}\n",
        )
        .edge_before("file:src/routes.ts", "imports", "function Dashboard")
        .edge_after("route src/routes.ts::route:/dash", "references", "function Dashboard")
}

/// Upstream #2395: a file of any language arriving one directory down brings
/// the manifest beside it into detection: React declared in
/// `frontend/package.json` is detected once `frontend/build.py` is indexed,
/// and the sync records that manifest among the JS inputs, as a fresh index
/// does.
pub fn typescript_react_dependency_beside_an_added_script() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"web\",\"private\":true}\n")
        .file(
            "frontend/package.json",
            "{\"name\":\"assets\",\"devDependencies\":{\"react\":\"18.2.0\"}}\n",
        )
        .file(
            "src/dashboard.ts",
            "export function Dashboard(): null {\n  return null;\n}\n",
        )
        .file(
            "src/routes.ts",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { Dashboard } from './dashboard';\n\nexport const router = createBrowserRouter([{ path: '/dash', Component: Dashboard }]);\n",
        )
        .write("frontend/build.py", "def build():\n    return 1\n")
        .edge_before("file:src/routes.ts", "imports", "function Dashboard")
        .edge_after("route src/routes.ts::route:/dash", "references", "function Dashboard")
}

const NEST_PACKAGE: &str = "{\"name\":\"api\",\"dependencies\":{\"@nestjs/core\":\"^11.0.0\",\"@nestjs/common\":\"^11.0.0\"}}\n";

const NEST_USERS_CONTROLLER: &str = "import { Controller, Get } from '@nestjs/common';\n\n@Controller('users')\nexport class UsersController {\n  @Get(':id')\n  findOne() {\n    return 1;\n  }\n}\n";

/// W7-02: the bootstrap comes to set a global prefix (upstream #2117): every
/// route of its app is renamed though no controller file changed.
pub fn typescript_nestjs_bootstrap_sets_a_global_prefix() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("package.json", NEST_PACKAGE)
        .file(
            "src/main.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from './app.module';\n\nasync function bootstrap() {\n  const app = await NestFactory.create(AppModule);\n  await app.listen(3000);\n}\nbootstrap();\n",
        )
        .file(
            "src/app.module.ts",
            "import { Module } from '@nestjs/common';\nimport { UsersController } from './users/users.controller';\n\n@Module({ controllers: [UsersController] })\nexport class AppModule {}\n",
        )
        .file("src/users/users.controller.ts", NEST_USERS_CONTROLLER)
        .write(
            "src/main.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from './app.module';\n\nasync function bootstrap() {\n  const app = await NestFactory.create(AppModule);\n  app.setGlobalPrefix('api');\n  app.enableVersioning({ defaultVersion: '1' });\n  await app.listen(3000);\n}\nbootstrap();\n",
        )
        .named_before("route", "GET /users/:id")
        .named_after("route", "GET /api/v1/users/:id")
        .edge_throughout(
            "route src/users/users.controller.ts::GET:/users/:id",
            "references",
            "method UsersController::findOne",
        )
}

/// W7-02: the bootstrap stops setting a global prefix: a route an earlier
/// sync renamed gets its in-file name back, as a fresh index names it.
pub fn typescript_nestjs_bootstrap_drops_its_global_prefix() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("package.json", NEST_PACKAGE)
        .file(
            "src/main.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from './app.module';\n\nasync function bootstrap() {\n  const app = await NestFactory.create(AppModule);\n  app.setGlobalPrefix('api');\n  app.enableVersioning({ defaultVersion: '1' });\n  await app.listen(3000);\n}\nbootstrap();\n",
        )
        .file(
            "src/app.module.ts",
            "import { Module } from '@nestjs/common';\nimport { UsersController } from './users/users.controller';\n\n@Module({ controllers: [UsersController] })\nexport class AppModule {}\n",
        )
        .file("src/users/users.controller.ts", NEST_USERS_CONTROLLER)
        .write(
            "src/main.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from './app.module';\n\nasync function bootstrap() {\n  const app = await NestFactory.create(AppModule);\n  await app.listen(3000);\n}\nbootstrap();\n",
        )
        .named_before("route", "GET /api/v1/users/:id")
        .named_after("route", "GET /users/:id")
}

/// W7-02b (KEEP-RUST): one feature's e2e spec comes to set a global prefix:
/// only that feature's routes are renamed; the feature beside it, whose own
/// spec configures nothing, keeps its routes as written.
pub fn typescript_nestjs_e2e_spec_sets_a_global_prefix() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("package.json", NEST_PACKAGE)
        .file(
            "integration/security/e2e/csrf.spec.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from '../src/app.module';\n\nit('protects', async () => {\n  const app = await NestFactory.create(AppModule);\n  await app.init();\n});\n",
        )
        .file(
            "integration/security/src/csrf.controller.ts",
            "import { Controller, Get } from '@nestjs/common';\n\n@Controller('csrf')\nexport class CsrfController {\n  @Get()\n  token() {\n    return 1;\n  }\n}\n",
        )
        .file(
            "integration/cookies/e2e/cookies.spec.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from '../src/app.module';\n\nit('sets', async () => {\n  const app = await NestFactory.create(AppModule);\n  await app.init();\n});\n",
        )
        .file(
            "integration/cookies/src/cookies.controller.ts",
            "import { Controller, Get } from '@nestjs/common';\n\n@Controller('cookies')\nexport class CookiesController {\n  @Get('set')\n  set() {\n    return 1;\n  }\n}\n",
        )
        .write(
            "integration/security/e2e/csrf.spec.ts",
            "import { NestFactory } from '@nestjs/core';\nimport { AppModule } from '../src/app.module';\n\nit('protects', async () => {\n  const app = await NestFactory.create(AppModule);\n  app.setGlobalPrefix('api');\n  await app.init();\n});\n",
        )
        .named_before("route", "GET /csrf")
        .named_after("route", "GET /api/csrf")
        .named_after("route", "GET /cookies/set")
}

/// W7-02: a RouterModule registration goes away: the routes it prefixed get
/// their in-file names back (the RouterModule pass renamed them; nothing
/// renamed them back before).
pub fn typescript_nestjs_router_module_registration_is_removed() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("package.json", NEST_PACKAGE)
        .file(
            "src/app.module.ts",
            "import { Module } from '@nestjs/common';\nimport { RouterModule } from '@nestjs/core';\nimport { AdminModule } from './admin.module';\n\n@Module({ imports: [AdminModule, RouterModule.register([{ path: 'admin', module: AdminModule }])] })\nexport class AppModule {}\n",
        )
        .file(
            "src/admin.module.ts",
            "import { Module } from '@nestjs/common';\nimport { UsersController } from './users/users.controller';\n\n@Module({ controllers: [UsersController] })\nexport class AdminModule {}\n",
        )
        .file("src/users/users.controller.ts", NEST_USERS_CONTROLLER)
        .write(
            "src/app.module.ts",
            "import { Module } from '@nestjs/common';\nimport { RouterModule } from '@nestjs/core';\nimport { AdminModule } from './admin.module';\n\n@Module({ imports: [AdminModule] })\nexport class AppModule {}\n",
        )
        .named_before("route", "GET /admin/users/:id")
        .named_after("route", "GET /users/:id")
}

/// W7-02: the convention file beside a controller appears (upstream #2246):
/// the provider it injects is that file's class, not another feature's.
pub fn typescript_nestjs_provider_beside_its_controller_appears() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file("package.json", NEST_PACKAGE)
        .file("src/admin/users.service.ts", "export class UsersService {}\n")
        .file(
            "src/users/users.controller.ts",
            "export class UsersController {\n  constructor(private readonly users: UsersService) {}\n}\n",
        )
        .write("src/users/users.service.ts", "export class UsersService {}\n")
        .edge_after(
            "method UsersController::constructor",
            "references",
            "class UsersService @src/users/users.service.ts",
        )
        .no_edge_after(
            "method UsersController::constructor",
            "references",
            "class UsersService @src/admin/users.service.ts",
        )
}

/// W7-03: a `nuxt.config.ts` appears in a Vue app (upstream #2130): Nuxt is
/// detected, and its `pages/` become routes served by their components.
pub fn vue_nuxt_config_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::Vue, Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"shop\",\"dependencies\":{\"vue\":\"3\"}}\n",
        )
        .file("pages/admin.vue", "<template><div>admin</div></template>\n")
        .file("src/main.ts", "export const ready = true;\n")
        .write("nuxt.config.ts", "export default {};\n")
        .edge_after(
            "route pages/admin.vue::route:/admin",
            "calls",
            "component pages/admin.vue::admin",
        )
}

/// W7-03: an app's manifest comes to declare Nuxt (upstream #2129 and
/// #2130): the files of that app now make Nuxt routes, though no page
/// changed, while the other app's `pages/` stay components.
pub fn vue_nuxt_app_manifest_gains_nuxt() -> Case {
    Case::new(Change::ControlFile, &[Language::Vue])
        .file("package.json", "{\"name\":\"mono\",\"private\":true}\n")
        .file(
            "apps/site/package.json",
            "{\"name\":\"site\",\"dependencies\":{\"nuxt\":\"3\"}}\n",
        )
        .file("apps/site/pages/about.vue", "<template><div/></template>\n")
        .file(
            "apps/admin/package.json",
            "{\"name\":\"admin\",\"dependencies\":{\"vue\":\"3\"}}\n",
        )
        .file(
            "apps/admin/pages/users.vue",
            "<template><div/></template>\n",
        )
        .file(
            "apps/blog/package.json",
            "{\"name\":\"blog\",\"dependencies\":{\"vue\":\"3\"}}\n",
        )
        .file("apps/blog/pages/post.vue", "<template><div/></template>\n")
        .write(
            "apps/admin/package.json",
            "{\"name\":\"admin\",\"dependencies\":{\"vue\":\"3\",\"nuxt\":\"3\"}}\n",
        )
        .edge_throughout(
            "route apps/site/pages/about.vue::route:/about",
            "calls",
            "component apps/site/pages/about.vue::about",
        )
        .edge_after(
            "route apps/admin/pages/users.vue::route:/users",
            "calls",
            "component apps/admin/pages/users.vue::users",
        )
}

/// W7-03: a Nuxt page's template is edited (upstream #2267): its route
/// still references its own component, not the same-named one elsewhere.
pub fn vue_nuxt_page_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue])
        .file(
            "package.json",
            "{\"name\":\"shop\",\"dependencies\":{\"nuxt\":\"3\"}}\n",
        )
        .file("pages/index.vue", "<template><div>home</div></template>\n")
        .file(
            "components/index.vue",
            "<template><div>not a page</div></template>\n",
        )
        .write(
            "pages/index.vue",
            "<template><section>welcome</section></template>\n",
        )
        .edge_throughout(
            "route pages/index.vue::route:/",
            "calls",
            "component pages/index.vue::index",
        )
        .no_edge_after(
            "route pages/index.vue::route:/",
            "calls",
            "component components/index.vue::index",
        )
}

/// W5-06: a package the manifest comes to declare is outside the project,
/// so the import of its binding no longer reaches a project class by name.
pub fn typescript_declared_dependency_makes_a_bare_import_external() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"web\",\"dependencies\":{}}\n")
        .file("src/button.ts", "export class Button {}\n")
        .file(
            "src/app.ts",
            "import { Button } from 'ui-kit';\n\nexport const b = new Button();\n",
        )
        .write(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"ui-kit\":\"1.0.0\"}}\n",
        )
        .edge_before("constant b", "instantiates", "class Button")
        .no_edge_after("constant b", "instantiates", "class Button")
}

/// W5-06: a workspace member with only its manifest renames its package:
/// the import of the old name stops being the workspace's.
pub fn typescript_workspace_member_manifest_renames_its_package() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"root\",\"workspaces\":[\"packages/*\"]}\n",
        )
        .file("packages/ui/package.json", "{\"name\":\"@acme/ui\"}\n")
        .file("src/button.ts", "export class Button {}\n")
        .file(
            "src/app.ts",
            "import { Button } from '@acme/ui';\n\nexport const b = new Button();\n",
        )
        .write("packages/ui/package.json", "{\"name\":\"@acme/kit\"}\n")
        .edge_before("file:src/app.ts", "imports", "class Button")
}

/// W5-06: a workspace member's manifest deleted: the import of its package
/// stops resolving into the member.
pub fn typescript_workspace_member_manifest_is_deleted() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"root\",\"workspaces\":[\"packages/*\"]}\n",
        )
        .file("packages/ui/package.json", "{\"name\":\"@acme/ui\"}\n")
        .file("packages/ui/index.ts", "export class Button {}\n")
        .file(
            "src/app.ts",
            "import { Button } from '@acme/ui';\n\nexport const b = new Button();\n",
        )
        .edge_before("file:src/app.ts", "imports", "class Button")
        .remove("packages/ui/package.json")
}

/// W5-06: the workspace glob comes to match a new member once its manifest
/// appears: the import of its package resolves into it.
pub fn typescript_workspace_glob_gains_a_member() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"root\",\"workspaces\":[\"packages/*\"]}\n",
        )
        .file("packages/kit/index.ts", "export class Button {}\n")
        .file(
            "src/app.ts",
            "import { Button } from '@acme/kit';\n\nexport const b = new Button();\n",
        )
        .write("packages/kit/package.json", "{\"name\":\"@acme/kit\"}\n")
        .edge_after("file:src/app.ts", "imports", "class Button")
}

/// W5-06: a Tauri config appearing makes the project a Tauri one: the
/// command reference an `invoke` call site gains at extraction, parked here
/// with no command to bind, and the detected frameworks match a fresh index.
/// A marker is a JS control input, and the JS files re-resolved whole gather
/// their framework references again (Tauri makes no node), so this case
/// converges without the framework lane; the command-attribute case needs it.
pub fn tauri_config_marker_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Rust])
        .file(
            "src/main.ts",
            "import { invoke } from '@tauri-apps/api/core';\n\nexport function run(): Promise<unknown> {\n  return invoke('greet');\n}\n",
        )
        .file("src-tauri/src/main.rs", "fn main() {}\n")
        .write("src-tauri/tauri.conf.json", "{}\n")
        .edge_throughout("file:src/main.ts", "contains", "function run")
}

/// W5-06: a `#[tauri::command]` appearing in a Rust file makes the project
/// a Tauri one, so every file is re-extracted: the command reference the
/// `invoke` call site gains at extraction binds the command, though no
/// JavaScript file and no control file changed.
pub fn tauri_command_attribute_makes_an_invoke_call_its_edge() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Rust])
        .file(
            "src/main.ts",
            "import { invoke } from '@tauri-apps/api/core';\n\nexport function run(): Promise<unknown> {\n  return invoke('greet');\n}\n",
        )
        .file(
            "src-tauri/src/main.rs",
            "pub fn greet() -> String {\n    String::new()\n}\n\nfn main() {}\n",
        )
        .write(
            "src-tauri/src/main.rs",
            "#[tauri::command] pub fn greet() -> String {\n    String::new()\n}\n\nfn main() {}\n",
        )
        .edge_after("file:src/main.ts", "calls", "function greet")
}

/// W7-04b (upstream #2159): the invoke edge the Tauri resolver makes names it
/// in `metadata.framework`; an edit to the command's body re-resolves the
/// call at its site, and the edge keeps the name, as a fresh index has it.
pub fn tauri_command_body_edit_keeps_the_invoke_edges_resolver_name() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Rust])
        .file(
            "src/main.ts",
            "import { invoke } from '@tauri-apps/api/core';\n\nexport function run(): Promise<unknown> {\n  return invoke('greet');\n}\n",
        )
        .file(
            "src-tauri/src/main.rs",
            "#[tauri::command] pub fn greet() -> String {\n    String::new()\n}\n\nfn main() {}\n",
        )
        .write(
            "src-tauri/src/main.rs",
            "#[tauri::command] pub fn greet() -> String {\n    String::from(\"hi\")\n}\n\nfn main() {}\n",
        )
        .edge_throughout("file:src/main.ts", "calls", "function greet")
        .edge_metadata_after(
            ("file:src/main.ts", "calls", "function greet"),
            "framework",
            "tauri",
        )
}

/// W7-07: a navigation call's file is edited to name another page
/// (upstream F3): the `navigates` edge follows the href.
pub fn typescript_next_navigation_call_names_another_page() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"next\":\"15\"}}\n",
        )
        .file(
            "app/users/page.tsx",
            "export default function UsersPage() {\n  return null\n}\n",
        )
        .file(
            "app/login/page.tsx",
            "export default function LoginPage() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "import { redirect } from 'next/navigation'\nexport function leave() {\n  redirect('/users')\n}\n",
        )
        .write(
            "lib/nav.ts",
            "import { redirect } from 'next/navigation'\nexport function leave() {\n  redirect('/login')\n}\n",
        )
        .edge_before(
            "function leave",
            "navigates",
            "route app/users/page.tsx::route:/users",
        )
        .edge_after(
            "function leave",
            "navigates",
            "route app/login/page.tsx::route:/login",
        )
        .no_edge_after(
            "function leave",
            "navigates",
            "route app/users/page.tsx::route:/users",
        )
        .edge_metadata_after(
            (
                "function leave",
                "navigates",
                "route app/login/page.tsx::route:/login",
            ),
            "href",
            "/login",
        )
}

/// W7-07: a page's body is edited (upstream F3): its route still calls its
/// component, and the call in another file still navigates to it.
pub fn tsx_next_page_body_edit_keeps_its_navigations() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"next\":\"15\"}}\n",
        )
        .file(
            "app/users/[id]/page.tsx",
            "export default function UserPage() {\n  return null\n}\n",
        )
        .file(
            "components/form.ts",
            "import { useRouter } from 'next/navigation'\nexport function submit(id: string) {\n  const router = useRouter()\n  router.push(`/users/${id}`)\n}\n",
        )
        .write(
            "app/users/[id]/page.tsx",
            "export default function UserPage() {\n  return <p>user</p>\n}\n",
        )
        .edge_throughout(
            "route app/users/[id]/page.tsx::route:/users/:id",
            "calls",
            "function UserPage",
        )
        .edge_throughout(
            "function submit",
            "navigates",
            "route app/users/[id]/page.tsx::route:/users/:id",
        )
}

/// W7-07: the static page a call names is deleted (upstream F3): the call
/// navigates to the dynamic route that now serves its path, as a fresh index
/// does.
pub fn tsx_next_static_page_delete_falls_back_to_the_dynamic_route() -> Case {
    Case::new(
        Change::DeleteImportedModule,
        &[Language::TypeScript, Language::Tsx],
    )
    .file(
        "package.json",
        "{\"name\":\"site\",\"dependencies\":{\"next\":\"15\"}}\n",
    )
    .file(
        "app/users/new/page.tsx",
        "export default function NewUser() {\n  return null\n}\n",
    )
    .file(
        "app/users/[id]/page.tsx",
        "export default function UserPage() {\n  return null\n}\n",
    )
    .file(
        "lib/nav.ts",
        "import { redirect } from 'next/navigation'\nexport function create() {\n  redirect('/users/new')\n}\n",
    )
    .remove("app/users/new/page.tsx")
    .edge_before(
        "function create",
        "navigates",
        "route app/users/new/page.tsx::route:/users/new",
    )
    .edge_after(
        "function create",
        "navigates",
        "route app/users/[id]/page.tsx::route:/users/:id",
    )
}

/// W7-07: the manifest comes to declare Next (upstream F3): Next is
/// detected, its pages become routes, and a call names one, though no source
/// changed.
pub fn typescript_next_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"react\":\"19\"}}\n",
        )
        .file(
            "app/login/page.tsx",
            "export default function LoginPage() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "import { redirect } from 'next/navigation'\nexport function leave() {\n  redirect('/login')\n}\n",
        )
        .write(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"react\":\"19\",\"next\":\"15\"}}\n",
        )
        .edge_after(
            "route app/login/page.tsx::route:/login",
            "calls",
            "function LoginPage",
        )
        .edge_after(
            "function leave",
            "navigates",
            "route app/login/page.tsx::route:/login",
        )
}

/// W7-07: a `next.config.mjs` appears beside the pages of an app that
/// declares no Next dependency (upstream F3): Next is detected by its config.
pub fn typescript_next_config_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"react\":\"19\"}}\n",
        )
        .file(
            "pages/about.tsx",
            "export default function About() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "export function go(router: { push(path: string): void }) {\n  router.push('/about')\n}\n",
        )
        .write("next.config.mjs", "export default {}\n")
        .edge_after(
            "route pages/about.tsx::route:/about",
            "calls",
            "function About",
        )
        .edge_after(
            "function go",
            "navigates",
            "route pages/about.tsx::route:/about",
        )
}

/// W7-07: one app's manifest comes to declare Next while another app of the
/// monorepo already does (upstream #2129): that app's pages become routes
/// and its own call navigates to its own page; the other app is unchanged.
pub fn typescript_next_app_manifest_gains_next() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file("package.json", "{\"name\":\"mono\",\"private\":true}\n")
        .file(
            "apps/web/package.json",
            "{\"name\":\"web\",\"dependencies\":{\"next\":\"15\"}}\n",
        )
        .file(
            "apps/web/app/login/page.tsx",
            "export default function WebLogin() {\n  return null\n}\n",
        )
        .file(
            "apps/admin/package.json",
            "{\"name\":\"admin\",\"dependencies\":{\"react\":\"19\"}}\n",
        )
        .file(
            "apps/admin/app/login/page.tsx",
            "export default function AdminLogin() {\n  return null\n}\n",
        )
        .file(
            "apps/admin/lib/nav.ts",
            "import { redirect } from 'next/navigation'\nexport function leave() {\n  redirect('/login')\n}\n",
        )
        .write(
            "apps/admin/package.json",
            "{\"name\":\"admin\",\"dependencies\":{\"react\":\"19\",\"next\":\"15\"}}\n",
        )
        .edge_throughout(
            "route apps/web/app/login/page.tsx::route:/login",
            "calls",
            "function WebLogin",
        )
        .edge_after(
            "route apps/admin/app/login/page.tsx::route:/login",
            "calls",
            "function AdminLogin",
        )
        .edge_after(
            "function leave",
            "navigates",
            "route apps/admin/app/login/page.tsx::route:/login",
        )
        .no_edge_after(
            "function leave",
            "navigates",
            "route apps/web/app/login/page.tsx::route:/login",
        )
}

/// W7-07: a route handler's body is edited (upstream F3): each exported
/// method stays its own endpoint, referencing its function.
pub fn typescript_next_route_handler_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"next\":\"15\"}}\n",
        )
        .file(
            "app/api/users/route.ts",
            "export async function GET() {\n  return Response.json([])\n}\nexport async function POST(req: Request) {\n  return Response.json({})\n}\n",
        )
        .write(
            "app/api/users/route.ts",
            "export async function GET() {\n  return Response.json([1])\n}\nexport async function POST(req: Request) {\n  return Response.json({ ok: true })\n}\n",
        )
        .edge_throughout(
            "route app/api/users/route.ts::GET:/api/users",
            "references",
            "function GET",
        )
        .edge_throughout(
            "route app/api/users/route.ts::POST:/api/users",
            "references",
            "function POST",
        )
}

/// W7-08: a navigation call's file is edited to name another screen
/// (upstream F2): the `navigates` edge follows the href.
pub fn typescript_expo_navigation_call_names_another_screen() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", "{\"name\":\"app\",\"dependencies\":{\"expo-router\":\"4\"}}\n")
        .file(
            "app/settings.tsx",
            "export default function Settings() {\n  return null\n}\n",
        )
        .file(
            "app/profile.tsx",
            "export default function Profile() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "import { router } from 'expo-router'\nexport function open() {\n  router.push('/settings')\n}\n",
        )
        .write(
            "lib/nav.ts",
            "import { router } from 'expo-router'\nexport function open() {\n  router.push('/profile')\n}\n",
        )
        .edge_before(
            "function open",
            "navigates",
            "route app/settings.tsx::route:/settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route app/profile.tsx::route:/profile",
        )
        .no_edge_after(
            "function open",
            "navigates",
            "route app/settings.tsx::route:/settings",
        )
        .edge_metadata_after(
            (
                "function open",
                "navigates",
                "route app/profile.tsx::route:/profile",
            ),
            "framework",
            "expo-router",
        )
}

/// W7-08: a screen's body is edited (upstream F2): its route still calls
/// its component, and the call in another file still navigates to it.
pub fn tsx_expo_screen_body_edit_keeps_its_navigations() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", "{\"name\":\"app\",\"dependencies\":{\"expo-router\":\"4\"}}\n")
        .file(
            "app/item/[id].tsx",
            "export default function Item() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "import { router } from 'expo-router'\nexport function openItem(id: string) {\n  router.push(`/item/${id}`)\n}\n",
        )
        .write(
            "app/item/[id].tsx",
            "export default function Item() {\n  return <p>item</p>\n}\n",
        )
        .edge_throughout(
            "route app/item/[id].tsx::route:/item/[id]",
            "calls",
            "function Item",
        )
        .edge_throughout(
            "function openItem",
            "navigates",
            "route app/item/[id].tsx::route:/item/[id]",
        )
}

/// W7-08: the static screen a call names is deleted (upstream F2): the call
/// navigates to the dynamic screen that now serves its path.
pub fn tsx_expo_static_screen_delete_falls_back_to_the_dynamic_route() -> Case {
    Case::new(
        Change::DeleteImportedModule,
        &[Language::TypeScript, Language::Tsx],
    )
    .file("package.json", "{\"name\":\"app\",\"dependencies\":{\"expo-router\":\"4\"}}\n")
    .file(
        "app/item/new.tsx",
        "export default function NewItem() {\n  return null\n}\n",
    )
    .file(
        "app/item/[id].tsx",
        "export default function Item() {\n  return null\n}\n",
    )
    .file(
        "lib/nav.ts",
        "import { router } from 'expo-router'\nexport function create() {\n  router.push('/item/new')\n}\n",
    )
    .remove("app/item/new.tsx")
    .edge_before(
        "function create",
        "navigates",
        "route app/item/new.tsx::route:/item/new",
    )
    .edge_after(
        "function create",
        "navigates",
        "route app/item/[id].tsx::route:/item/[id]",
    )
}

/// W7-08: the manifest comes to declare Expo Router (upstream F2): its
/// screens become routes and a call names one, though no source changed.
pub fn typescript_expo_router_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\"}}\n",
        )
        .file(
            "app/settings.tsx",
            "export default function Settings() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "import { router } from 'expo-router'\nexport function open() {\n  router.push('/settings')\n}\n",
        )
        .write(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\"}}\n",
        )
        .edge_after(
            "route app/settings.tsx::route:/settings",
            "calls",
            "function Settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route app/settings.tsx::route:/settings",
        )
}

/// W7-08: an `app.config.ts` appears beside an app's `_layout` in a project
/// whose manifest names no router (upstream F2): Expo Router is detected by
/// them.
pub fn typescript_expo_config_appears_beside_a_layout() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"react\":\"19\"}}\n",
        )
        .file(
            "app/_layout.tsx",
            "export default function Layout() {\n  return null\n}\n",
        )
        .file(
            "app/settings.tsx",
            "export default function Settings() {\n  return null\n}\n",
        )
        .file(
            "lib/nav.ts",
            "export function open(router: { push(path: string): void }) {\n  router.push('/settings')\n}\n",
        )
        .write("app.config.ts", "export default {}\n")
        .edge_after(
            "route app/settings.tsx::route:/settings",
            "calls",
            "function Settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route app/settings.tsx::route:/settings",
        )
}

/// W7-08: an Expo API route's body is edited (upstream #2135): each
/// exported method stays its own endpoint, referencing its function.
pub fn typescript_expo_api_route_body_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"expo-router\":\"4\"}}\n",
        )
        .file(
            "app/users+api.ts",
            "export async function GET() {\n  return Response.json([])\n}\n",
        )
        .write(
            "app/users+api.ts",
            "export async function GET() {\n  return Response.json([1])\n}\n",
        )
        .edge_throughout(
            "route app/users+api.ts::GET:/users",
            "references",
            "function GET",
        )
}

// ----- TanStack Router (W7-10) ----------------------------------------------

/// The package of the W7-10 cases.
const TANSTACK_PACKAGE: &str =
    "{\"name\":\"app\",\"dependencies\":{\"react\":\"19\",\"@tanstack/react-router\":\"1\"}}\n";

/// A file route at `/settings` rendering `Settings`.
const TANSTACK_SETTINGS: &str = "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/settings')({ component: Settings })\nfunction Settings() {\n  return null\n}\n";
/// A file route at `/profile` rendering `Profile`.
const TANSTACK_PROFILE: &str = "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/profile')({ component: Profile })\nfunction Profile() {\n  return null\n}\n";
/// A file route at `/posts/$postId` rendering `Post`.
const TANSTACK_POST: &str = "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts/$postId')({ component: Post })\nfunction Post() {\n  return null\n}\n";
/// A file route at `/posts/new` rendering `NewPost`.
const TANSTACK_NEW_POST: &str = "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts/new')({ component: NewPost })\nfunction NewPost() {\n  return null\n}\n";

/// W7-10 (upstream F5): a navigation call's file is edited to name another
/// route: the `navigates` edge follows the pattern.
pub fn typescript_tanstack_navigation_call_names_another_route() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", TANSTACK_PACKAGE)
        .file("src/routes/settings.tsx", TANSTACK_SETTINGS)
        .file("src/routes/profile.tsx", TANSTACK_PROFILE)
        .file(
            "src/lib/nav.ts",
            "export function open(navigate) {\n  navigate({ to: '/settings' })\n}\n",
        )
        .write(
            "src/lib/nav.ts",
            "export function open(navigate) {\n  navigate({ to: '/profile' })\n}\n",
        )
        .edge_before(
            "function open",
            "navigates",
            "route src/routes/settings.tsx::route:/settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route src/routes/profile.tsx::route:/profile",
        )
        .no_edge_after(
            "function open",
            "navigates",
            "route src/routes/settings.tsx::route:/settings",
        )
        .edge_metadata_after(
            (
                "function open",
                "navigates",
                "route src/routes/profile.tsx::route:/profile",
            ),
            "framework",
            "tanstack-router",
        )
}

/// W7-10 (upstream F5): a route's body is edited: it still calls its
/// component, and the call in another file still navigates to it by its
/// pattern.
pub fn tsx_tanstack_route_body_edit_keeps_its_navigations() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", TANSTACK_PACKAGE)
        .file(
            "src/routes/posts.$postId.tsx",
            TANSTACK_POST,
        )
        .file(
            "src/lib/nav.ts",
            "export function openPost(navigate, postId) {\n  navigate({ to: '/posts/$postId', params: { postId } })\n}\n",
        )
        .write(
            "src/routes/posts.$postId.tsx",
            "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts/$postId')({ component: Post })\nfunction Post() {\n  return <article />\n}\n",
        )
        .edge_throughout(
            "route src/routes/posts.$postId.tsx::route:/posts/:postId",
            "calls",
            "function Post",
        )
        .edge_throughout(
            "function openPost",
            "navigates",
            "route src/routes/posts.$postId.tsx::route:/posts/:postId",
        )
}

/// W7-10 (upstream F5): the static route a call names is deleted: the call
/// navigates to the dynamic route that now serves its path.
pub fn tsx_tanstack_static_route_delete_falls_back_to_the_dynamic_route() -> Case {
    Case::new(
        Change::DeleteImportedModule,
        &[Language::TypeScript, Language::Tsx],
    )
    .file("package.json", TANSTACK_PACKAGE)
    .file("src/routes/posts.new.tsx", TANSTACK_NEW_POST)
    .file("src/routes/posts.$postId.tsx", TANSTACK_POST)
    .file(
        "src/lib/nav.ts",
        "export function create(navigate) {\n  navigate({ to: '/posts/new' })\n}\n",
    )
    .remove("src/routes/posts.new.tsx")
    .edge_before(
        "function create",
        "navigates",
        "route src/routes/posts.new.tsx::route:/posts/new",
    )
    .edge_after(
        "function create",
        "navigates",
        "route src/routes/posts.$postId.tsx::route:/posts/:postId",
    )
}

/// W7-10 (upstream F5): the manifest comes to declare TanStack Router: its
/// files' routes appear and a call names one, though no source changed.
pub fn typescript_tanstack_router_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"react\":\"19\"}}\n",
        )
        .file("src/routes/settings.tsx", TANSTACK_SETTINGS)
        .file(
            "src/lib/nav.ts",
            "export function open(navigate) {\n  navigate({ to: '/settings' })\n}\n",
        )
        .write("package.json", TANSTACK_PACKAGE)
        .edge_after(
            "route src/routes/settings.tsx::route:/settings",
            "calls",
            "function Settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route src/routes/settings.tsx::route:/settings",
        )
}

/// The manifest of a Vue Router app.
const VUE_ROUTER_PACKAGE: &str =
    "{\"name\":\"app\",\"dependencies\":{\"vue\":\"3\",\"vue-router\":\"4\"}}\n";
/// A Vue Router table of lazy views, by name: `home` at `/`, `login`,
/// `settings`, and `/posts/new` beside `/posts/:id`.
const VUE_ROUTER_TABLE: &str = "import { createRouter, createWebHistory } from 'vue-router'\nexport default createRouter({\n  history: createWebHistory(),\n  routes: [\n    { name: 'home', path: '/', component: () => import('../views/Home.vue') },\n    { name: 'login', path: '/login', component: () => import('../views/Login.vue') },\n    { name: 'settings', path: '/settings', component: () => import('../views/Settings.vue') },\n    { name: 'new-post', path: '/posts/new', component: () => import('../views/NewPost.vue') },\n    { name: 'post', path: '/posts/:id', component: () => import('../views/Post.vue') },\n  ],\n})\n";
/// [`VUE_ROUTER_TABLE`] with `login` renamed `sign-in` at the same path.
const VUE_ROUTER_TABLE_RENAMED: &str = "import { createRouter, createWebHistory } from 'vue-router'\nexport default createRouter({\n  history: createWebHistory(),\n  routes: [\n    { name: 'home', path: '/', component: () => import('../views/Home.vue') },\n    { name: 'sign-in', path: '/login', component: () => import('../views/Login.vue') },\n    { name: 'settings', path: '/settings', component: () => import('../views/Settings.vue') },\n    { name: 'new-post', path: '/posts/new', component: () => import('../views/NewPost.vue') },\n    { name: 'post', path: '/posts/:id', component: () => import('../views/Post.vue') },\n  ],\n})\n";
/// [`VUE_ROUTER_TABLE`] without its `/posts/new` route.
const VUE_ROUTER_TABLE_WITHOUT_NEW_POST: &str = "import { createRouter, createWebHistory } from 'vue-router'\nexport default createRouter({\n  history: createWebHistory(),\n  routes: [\n    { name: 'home', path: '/', component: () => import('../views/Home.vue') },\n    { name: 'login', path: '/login', component: () => import('../views/Login.vue') },\n    { name: 'settings', path: '/settings', component: () => import('../views/Settings.vue') },\n    { name: 'post', path: '/posts/:id', component: () => import('../views/Post.vue') },\n  ],\n})\n";
/// A `.vue` view rendering `name`.
const VUE_VIEW: &str =
    "<template><div>view</div></template>\n<script setup>\nconst view = 1\n</script>\n";

/// A Vue Router app: its manifest, its table and the views it names.
fn vue_router_app(case: Case) -> Case {
    case.file("package.json", VUE_ROUTER_PACKAGE)
        .file("src/router/index.js", VUE_ROUTER_TABLE)
        .file("src/views/Home.vue", VUE_VIEW)
        .file("src/views/Login.vue", VUE_VIEW)
        .file("src/views/Settings.vue", VUE_VIEW)
        .file("src/views/NewPost.vue", VUE_VIEW)
        .file("src/views/Post.vue", VUE_VIEW)
}

/// W7-11 (upstream F6): a navigation call's file is edited to name another
/// route by name: the `navigates` edge follows the name.
pub fn javascript_vue_router_navigation_call_names_another_route() -> Case {
    vue_router_app(Case::new(
        Change::BodyEdit,
        &[Language::JavaScript, Language::Vue],
    ))
    .file(
        "src/lib/nav.js",
        "export function open(router) {\n  router.push({ name: 'login' })\n}\n",
    )
    .write(
        "src/lib/nav.js",
        "export function open(router) {\n  router.push({ name: 'settings' })\n}\n",
    )
    .edge_before(
        "function open",
        "navigates",
        "route src/router/index.js::route:/login",
    )
    .edge_after(
        "function open",
        "navigates",
        "route src/router/index.js::route:/settings",
    )
    .no_edge_after(
        "function open",
        "navigates",
        "route src/router/index.js::route:/login",
    )
    .edge_metadata_after(
        (
            "function open",
            "navigates",
            "route src/router/index.js::route:/settings",
        ),
        "framework",
        "vue-router",
    )
}

/// W7-11 (upstream F6): the table renames a route and keeps its path, so
/// its node keeps its id: the call in another file that named the old name
/// navigates nowhere.
pub fn javascript_vue_router_route_rename_drops_its_named_navigation() -> Case {
    vue_router_app(Case::new(
        Change::BodyEdit,
        &[Language::JavaScript, Language::Vue],
    ))
    .file(
        "src/lib/nav.js",
        "export function open(router) {\n  router.push({ name: 'login' })\n}\n",
    )
    .write("src/router/index.js", VUE_ROUTER_TABLE_RENAMED)
    .edge_before(
        "function open",
        "navigates",
        "route src/router/index.js::route:/login",
    )
    .no_edge_after(
        "function open",
        "navigates",
        "route src/router/index.js::route:/login",
    )
    .edge_throughout(
        "route src/router/index.js::route:/login",
        "calls",
        "component src/views/Login.vue::Login",
    )
}

/// W7-11 (upstream F6): the static route a call names by path leaves the
/// table: the call navigates to the dynamic route that now serves it.
pub fn javascript_vue_router_static_route_delete_falls_back_to_the_dynamic_route() -> Case {
    vue_router_app(Case::new(
        Change::BodyEdit,
        &[Language::JavaScript, Language::Vue],
    ))
    .file(
        "src/lib/nav.js",
        "export function create(router) {\n  router.push('/posts/new')\n}\n",
    )
    .write("src/router/index.js", VUE_ROUTER_TABLE_WITHOUT_NEW_POST)
    .edge_before(
        "function create",
        "navigates",
        "route src/router/index.js::route:/posts/new",
    )
    .edge_after(
        "function create",
        "navigates",
        "route src/router/index.js::route:/posts/:id",
    )
}

/// W7-11 (upstream F6): the manifest comes to declare Vue Router: the
/// table's routes appear, bound to their views, and a call names one,
/// though no source changed.
pub fn javascript_vue_router_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::JavaScript, Language::Vue])
        .file(
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"vue\":\"3\"}}\n",
        )
        .file("src/router/index.js", VUE_ROUTER_TABLE)
        .file("src/views/Settings.vue", VUE_VIEW)
        .file(
            "src/lib/nav.js",
            "export function open(router) {\n  router.push({ name: 'settings' })\n}\n",
        )
        .write("package.json", VUE_ROUTER_PACKAGE)
        .edge_after(
            "route src/router/index.js::route:/settings",
            "calls",
            "component src/views/Settings.vue::Settings",
        )
        .edge_after(
            "function open",
            "navigates",
            "route src/router/index.js::route:/settings",
        )
}

/// W7-11 (upstream F6): the layout a child route renders inside is edited:
/// the child still references it as its layout.
pub fn javascript_vue_router_layout_body_edit_keeps_the_layout() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Vue])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file(
            "src/router/index.js",
            "import { createRouter } from 'vue-router'\nimport Layout from '../layout/index.vue'\nexport default createRouter({\n  routes: [\n    {\n      path: '/admin',\n      component: Layout,\n      redirect: '/admin/users',\n      children: [{ path: 'users', component: () => import('../views/Users.vue') }],\n    },\n  ],\n})\n",
        )
        .file("src/layout/index.vue", VUE_VIEW)
        .file("src/views/Users.vue", VUE_VIEW)
        .write(
            "src/layout/index.vue",
            "<template><main><router-view /></main></template>\n<script setup>\nconst frame = 1\n</script>\n",
        )
        .edge_throughout(
            "route src/router/index.js::route:/admin/users",
            "references",
            "component src/layout/index.vue::index",
        )
        .edge_throughout(
            "route src/router/index.js::route:/admin/users",
            "calls",
            "component src/views/Users.vue::Users",
        )
}

/// W7-11 (upstream F6): a Nuxt app's `router.push` is edited to name another
/// page: the `navigates` edge follows the path to the Nuxt page. (A bare
/// `navigateTo` is the Vue resolver's Nuxt auto-import at `e8ddc4e2`, so it
/// navigates nowhere; upstream #2826 changes that after the audited range.)
pub fn typescript_nuxt_router_push_names_another_page() -> Case {
    Case::new(Change::BodyEdit, &[Language::Vue, Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"site\",\"dependencies\":{\"nuxt\":\"3\",\"vue\":\"3\"}}\n",
        )
        .file("pages/index.vue", VUE_VIEW)
        .file("pages/about.vue", VUE_VIEW)
        .file(
            "composables/useGo.ts",
            "export function go(router) {\n  return router.push('/about')\n}\n",
        )
        .write(
            "composables/useGo.ts",
            "export function go(router) {\n  return router.push('/')\n}\n",
        )
        .edge_before(
            "function go",
            "navigates",
            "route pages/about.vue::route:/about",
        )
        .edge_after("function go", "navigates", "route pages/index.vue::route:/")
        .no_edge_after(
            "function go",
            "navigates",
            "route pages/about.vue::route:/about",
        )
}

/// W7-11 (upstream F6): the view a lazy route names appears after the
/// table: the route calls its component, as in a fresh index.
pub fn javascript_vue_router_lazy_view_appears() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::JavaScript, Language::Vue],
    )
    .file("package.json", VUE_ROUTER_PACKAGE)
    .file("src/router/index.js", VUE_ROUTER_TABLE)
    .write("src/views/Settings.vue", VUE_VIEW)
    .edge_after(
        "route src/router/index.js::route:/settings",
        "calls",
        "component src/views/Settings.vue::Settings",
    )
}

/// W7-11: a lazy route's view module that is no SFC renders the function
/// its `export default` names. The default moves to another function of
/// the module, both kept: the route calls the new default, as in a fresh
/// index, though the routes file is untouched.
pub fn javascript_vue_router_lazy_module_default_export_moves() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file(
            "src/router/index.js",
            "import { createRouter } from 'vue-router'\nexport default createRouter({\n  routes: [{ path: '/home', component: () => import('../views/home.js') }],\n})\n",
        )
        .file(
            "src/views/home.js",
            "export function Banner() {\n  return null\n}\nexport default function HomeView() {\n  return null\n}\n",
        )
        .write(
            "src/views/home.js",
            "export default function Banner() {\n  return null\n}\nexport function HomeView() {\n  return null\n}\n",
        )
        .edge_before(
            "route src/router/index.js::route:/home",
            "calls",
            "function HomeView",
        )
        .edge_after(
            "route src/router/index.js::route:/home",
            "calls",
            "function Banner",
        )
        .no_edge_after(
            "route src/router/index.js::route:/home",
            "calls",
            "function HomeView",
        )
}

/// W7-11: the same move written as a separate `export default` statement:
/// neither function changes, only the statement naming one.
pub fn javascript_vue_router_lazy_module_default_statement_names_another() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file(
            "src/router/index.js",
            "import { createRouter } from 'vue-router'\nexport default createRouter({\n  routes: [{ path: '/home', component: () => import('../views/home.js') }],\n})\n",
        )
        .file(
            "src/views/home.js",
            "function Banner() {\n  return null\n}\nfunction HomeView() {\n  return null\n}\nexport default HomeView\n",
        )
        .write(
            "src/views/home.js",
            "function Banner() {\n  return null\n}\nfunction HomeView() {\n  return null\n}\nexport default Banner\n",
        )
        .edge_before("route src/router/index.js::route:/home", "calls", "function HomeView")
        .edge_after("route src/router/index.js::route:/home", "calls", "function Banner")
        .no_edge_after("route src/router/index.js::route:/home", "calls", "function HomeView")
}

/// The routes file the W7-11 import cases share: a route renders the
/// layout its named import takes.
const VUE_ROUTER_IMPORTED_LAYOUT: &str = "import { createRouter } from 'vue-router'\nimport { FrameLayout } from '../layouts'\nexport default createRouter({\n  routes: [{ path: '/frame', component: FrameLayout }],\n})\n";

/// W7-11: the module a route's named import names appears after the routes
/// file: the route calls the export, as in a fresh index.
pub fn typescript_vue_router_imported_layout_appears() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::TypeScript, Language::Vue],
    )
    .file("package.json", VUE_ROUTER_PACKAGE)
    .file("src/router/index.ts", VUE_ROUTER_IMPORTED_LAYOUT)
    .file("src/layouts/frame.vue", VUE_VIEW)
    .write(
        "src/layouts/index.ts",
        "export const FrameLayout = () => import('./frame.vue')\n",
    )
    .edge_after(
        "route src/router/index.ts::route:/frame",
        "calls",
        "function FrameLayout",
    )
}

/// W7-11: the barrel a route's named import goes through re-exports
/// another layout under the name: the route calls the new one, as in a
/// fresh index, though the routes file is untouched.
pub fn typescript_vue_router_barrel_reexports_another_layout() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Vue])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file("src/router/index.ts", VUE_ROUTER_IMPORTED_LAYOUT)
        .file(
            "src/layouts/index.ts",
            "export { default as FrameLayout } from './frame.vue'\n",
        )
        .file("src/layouts/frame.vue", VUE_VIEW)
        .file("src/layouts/wide.vue", VUE_VIEW)
        .write(
            "src/layouts/index.ts",
            "export { default as FrameLayout } from './wide.vue'\n",
        )
        .edge_before(
            "route src/router/index.ts::route:/frame",
            "calls",
            "component src/layouts/frame.vue::frame",
        )
        .edge_after(
            "route src/router/index.ts::route:/frame",
            "calls",
            "component src/layouts/wide.vue::wide",
        )
        .no_edge_after(
            "route src/router/index.ts::route:/frame",
            "calls",
            "component src/layouts/frame.vue::frame",
        )
}

/// The layouts module of the W7-11 declarator cases: the layout is a
/// statement's second declarator, unexported and exported.
const VUE_ROUTER_SECOND_DECLARATOR: &str =
    "const Other = 1, FrameLayout = () => import('./frame.vue')\n";
const VUE_ROUTER_EXPORTED_SECOND_DECLARATOR: &str =
    "export const Other = 1, FrameLayout = () => import('./frame.vue')\n";

/// W7-11: the statement declaring a route's layout as its second declarator
/// gains `export`: the route calls the layout, as in a fresh index, though
/// the routes file is untouched.
pub fn typescript_vue_router_second_declarator_gains_its_export() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Vue])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file("src/router/index.ts", VUE_ROUTER_IMPORTED_LAYOUT)
        .file("src/layouts/index.ts", VUE_ROUTER_SECOND_DECLARATOR)
        .file("src/layouts/frame.vue", VUE_VIEW)
        .write(
            "src/layouts/index.ts",
            VUE_ROUTER_EXPORTED_SECOND_DECLARATOR,
        )
        .edge_after(
            "route src/router/index.ts::route:/frame",
            "calls",
            "function FrameLayout",
        )
}

/// W7-11: the statement declaring a route's layout as its second declarator
/// loses `export`: the route calls nothing, as in a fresh index.
pub fn typescript_vue_router_second_declarator_loses_its_export() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Vue])
        .file("package.json", VUE_ROUTER_PACKAGE)
        .file("src/router/index.ts", VUE_ROUTER_IMPORTED_LAYOUT)
        .file(
            "src/layouts/index.ts",
            VUE_ROUTER_EXPORTED_SECOND_DECLARATOR,
        )
        .file("src/layouts/frame.vue", VUE_VIEW)
        .write("src/layouts/index.ts", VUE_ROUTER_SECOND_DECLARATOR)
        .edge_before(
            "route src/router/index.ts::route:/frame",
            "calls",
            "function FrameLayout",
        )
        .no_edge_after(
            "route src/router/index.ts::route:/frame",
            "calls",
            "function FrameLayout",
        )
}

/// The manifest of a SvelteKit app.
const SVELTEKIT_PACKAGE: &str =
    "{\"name\":\"site\",\"devDependencies\":{\"@sveltejs/kit\":\"2\",\"svelte\":\"5\"}}\n";
/// A SvelteKit page with nothing to navigate.
const SVELTEKIT_PAGE: &str = "<h1>page</h1>\n";

/// A SvelteKit app: `/`, `/login`, `/settings`, `/article/new` beside
/// `/article/[slug]`, and `/about` in a `(marketing)` group.
fn sveltekit_app(case: Case) -> Case {
    case.file("package.json", SVELTEKIT_PACKAGE)
        .file("src/routes/+page.svelte", SVELTEKIT_PAGE)
        .file("src/routes/login/+page.svelte", SVELTEKIT_PAGE)
        .file("src/routes/settings/+page.svelte", SVELTEKIT_PAGE)
        .file("src/routes/article/new/+page.svelte", SVELTEKIT_PAGE)
        .file("src/routes/article/[slug]/+page.svelte", SVELTEKIT_PAGE)
        .file("src/routes/(marketing)/about/+page.svelte", SVELTEKIT_PAGE)
}

/// W7-12 (upstream F6): a `goto` is edited to name another page: the
/// `navigates` edge follows it.
pub fn javascript_sveltekit_goto_names_another_page() -> Case {
    sveltekit_app(Case::new(
        Change::BodyEdit,
        &[Language::JavaScript, Language::Svelte],
    ))
    .file(
        "src/lib/nav.js",
        "import { goto } from '$app/navigation'\nexport function open() {\n  goto('/login')\n}\n",
    )
    .write(
        "src/lib/nav.js",
        "import { goto } from '$app/navigation'\nexport function open() {\n  goto('/settings')\n}\n",
    )
    .edge_before(
        "function open",
        "navigates",
        "route src/routes/login/+page.svelte::route:/login",
    )
    .edge_after(
        "function open",
        "navigates",
        "route src/routes/settings/+page.svelte::route:/settings",
    )
    .no_edge_after(
        "function open",
        "navigates",
        "route src/routes/login/+page.svelte::route:/login",
    )
    .edge_metadata_after(
        (
            "function open",
            "navigates",
            "route src/routes/settings/+page.svelte::route:/settings",
        ),
        "framework",
        "sveltekit-router",
    )
}

/// W7-12 (upstream F6): a loader's `redirect` is edited to name another
/// page, after the status as before.
pub fn javascript_sveltekit_server_redirect_names_another_page() -> Case {
    sveltekit_app(Case::new(
        Change::BodyEdit,
        &[Language::JavaScript, Language::Svelte],
    ))
    .file(
        "src/routes/settings/+page.server.js",
        "import { redirect } from '@sveltejs/kit'\nexport function load({ locals }) {\n  if (!locals.user) redirect(302, '/login')\n}\n",
    )
    .write(
        "src/routes/settings/+page.server.js",
        "import { redirect } from '@sveltejs/kit'\nexport function load({ locals }) {\n  if (!locals.user) redirect(302, '/')\n}\n",
    )
    .edge_before(
        "function load",
        "navigates",
        "route src/routes/login/+page.svelte::route:/login",
    )
    .edge_after(
        "function load",
        "navigates",
        "route src/routes/+page.svelte::route:/",
    )
    .no_edge_after(
        "function load",
        "navigates",
        "route src/routes/login/+page.svelte::route:/login",
    )
}

/// W7-12 (upstream F6): the static page a call names is deleted: the call
/// navigates to the `[slug]` page that now serves its path.
pub fn javascript_sveltekit_static_page_delete_falls_back_to_the_dynamic_page() -> Case {
    sveltekit_app(Case::new(
        Change::DeleteImportedModule,
        &[Language::JavaScript, Language::Svelte],
    ))
    .file(
        "src/lib/nav.js",
        "export function create() {\n  goto('/article/new')\n}\n",
    )
    .remove("src/routes/article/new/+page.svelte")
    .edge_before(
        "function create",
        "navigates",
        "route src/routes/article/new/+page.svelte::route:/article/new",
    )
    .edge_after(
        "function create",
        "navigates",
        "route src/routes/article/[slug]/+page.svelte::route:/article/:slug",
    )
}

/// W7-12 (upstream #2131): a page moves to another `(group)` folder: its
/// address stays, and the call that names it navigates to the moved page.
pub fn javascript_sveltekit_group_folder_rename_keeps_the_address() -> Case {
    sveltekit_app(Case::new(
        Change::DeleteImportedModule,
        &[Language::JavaScript, Language::Svelte],
    ))
    .file(
        "src/lib/nav.js",
        "export function about() {\n  goto('/about')\n}\n",
    )
    .rename(
        "src/routes/(marketing)/about/+page.svelte",
        "src/routes/(site)/about/+page.svelte",
    )
    .edge_before(
        "function about",
        "navigates",
        "route src/routes/(marketing)/about/+page.svelte::route:/about",
    )
    .edge_after(
        "function about",
        "navigates",
        "route src/routes/(site)/about/+page.svelte::route:/about",
    )
}

/// W7-12 (upstream F6): the manifest comes to declare SvelteKit: the
/// `goto` of a Svelte app names a page, though no source changed.
pub fn javascript_sveltekit_dependency_appears_in_package_json() -> Case {
    Case::new(
        Change::ControlFile,
        &[Language::JavaScript, Language::Svelte],
    )
    .file(
        "package.json",
        "{\"name\":\"site\",\"devDependencies\":{\"svelte\":\"5\"}}\n",
    )
    .file("src/routes/login/+page.svelte", SVELTEKIT_PAGE)
    .file(
        "src/lib/nav.js",
        "export function open() {\n  goto('/login')\n}\n",
    )
    .write("package.json", SVELTEKIT_PACKAGE)
    .edge_after(
        "function open",
        "navigates",
        "route src/routes/login/+page.svelte::route:/login",
    )
}

/// W7-12 (upstream #2425's shape): a page appears at the path a call in
/// another file already names: the call navigates to it, as in a fresh
/// index.
pub fn javascript_sveltekit_page_appears_where_a_goto_points() -> Case {
    sveltekit_app(Case::new(
        Change::AddImportTarget,
        &[Language::JavaScript, Language::Svelte],
    ))
    .file(
        "src/lib/nav.js",
        "export function open() {\n  goto('/pricing')\n}\n",
    )
    .write("src/routes/pricing/+page.svelte", SVELTEKIT_PAGE)
    .edge_after(
        "function open",
        "navigates",
        "route src/routes/pricing/+page.svelte::route:/pricing",
    )
}

/// W7-12: a static page appears at the path a call reached through the
/// `[slug]` page: the call navigates to the static page.
pub fn javascript_sveltekit_static_page_appears_beside_the_dynamic_page() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::JavaScript, Language::Svelte],
    )
    .file("package.json", SVELTEKIT_PACKAGE)
    .file("src/routes/article/[slug]/+page.svelte", SVELTEKIT_PAGE)
    .file(
        "src/lib/nav.js",
        "export function create() {\n  goto('/article/new')\n}\n",
    )
    .write("src/routes/article/new/+page.svelte", SVELTEKIT_PAGE)
    .edge_before(
        "function create",
        "navigates",
        "route src/routes/article/[slug]/+page.svelte::route:/article/:slug",
    )
    .edge_after(
        "function create",
        "navigates",
        "route src/routes/article/new/+page.svelte::route:/article/new",
    )
    .no_edge_after(
        "function create",
        "navigates",
        "route src/routes/article/[slug]/+page.svelte::route:/article/:slug",
    )
}

/// W7-12: a second page that shortens to the path a call reached through
/// an `[[optional]]` page appears in another group: two pages of the app
/// serve that path now, so the call reaches neither, as in a fresh index
/// that lists the pages in another order.
pub fn javascript_sveltekit_second_optional_page_makes_the_short_path_ambiguous() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::JavaScript, Language::Svelte],
    )
    .file("package.json", SVELTEKIT_PACKAGE)
    .file(
        "src/routes/(shop)/menu/[[lang]]/+page.svelte",
        SVELTEKIT_PAGE,
    )
    .file(
        "src/lib/nav.js",
        "export function open() {\n  goto('/menu')\n}\n",
    )
    .write("src/routes/(app)/menu/[[tab]]/+page.svelte", SVELTEKIT_PAGE)
    .edge_before(
        "function open",
        "navigates",
        "route src/routes/(shop)/menu/[[lang]]/+page.svelte::route:/menu/:lang?",
    )
    .no_edge_after(
        "function open",
        "navigates",
        "route src/routes/(shop)/menu/[[lang]]/+page.svelte::route:/menu/:lang?",
    )
    .no_edge_after(
        "function open",
        "navigates",
        "route src/routes/(app)/menu/[[tab]]/+page.svelte::route:/menu/:tab?",
    )
}

/// W7-12: the module a hook imports through `$lib` appears: the import
/// links the file, as in a fresh index.
pub fn typescript_sveltekit_lib_module_appears() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::TypeScript, Language::Svelte],
    )
    .file("package.json", SVELTEKIT_PACKAGE)
    .file("src/routes/+page.svelte", SVELTEKIT_PAGE)
    .file(
        "src/hooks.server.ts",
        "import { db } from '$lib/server/db'\nexport async function handle({ event, resolve }) {\n  return resolve(event)\n}\n",
    )
    .write("src/lib/server/db.ts", "export const db = {}\n")
    .edge_after(
        "file src/hooks.server.ts",
        "imports",
        "file src/lib/server/db.ts",
    )
}

/// The manifest of an Angular app.
const ANGULAR_PACKAGE: &str =
    "{\"name\":\"app\",\"dependencies\":{\"@angular/core\":\"19\",\"@angular/router\":\"19\"}}\n";
/// An Angular app's sign-in screen.
const ANGULAR_AUTH: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'app-auth', template: '' })\nexport class AuthComponent {}\n";
/// An Angular app's profile screen.
const ANGULAR_PROFILE: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'app-profile', template: '' })\nexport class ProfileComponent {}\n";
/// An Angular app's account screen.
const ANGULAR_ACCOUNT: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'app-account', template: '' })\nexport class AccountComponent {}\n";
/// An Angular app's users screen.
const ANGULAR_USERS: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'app-users', template: '' })\nexport class UsersComponent {}\n";
/// The app's routes: `/login`, and the profile routes mounted at `profile`.
const ANGULAR_APP_ROUTES: &str = "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth.component';\nexport const routes: Routes = [\n  { path: 'login', component: AuthComponent },\n  { path: 'profile', loadChildren: () => import('./profile/profile.routes') },\n];\n";
/// The profile routes file, which knows only its own `:username`.
const ANGULAR_PROFILE_ROUTES: &str = "import { Routes } from '@angular/router';\nimport { ProfileComponent } from './profile.component';\nexport default [{ path: ':username', component: ProfileComponent }] satisfies Routes;\n";
/// A routing module: `users`, in no NgModule yet.
const ANGULAR_ADMIN_ROUTING: &str = "import { NgModule } from '@angular/core';\nimport { RouterModule, Routes } from '@angular/router';\nimport { UsersComponent } from './users.component';\nconst routes: Routes = [{ path: 'users', component: UsersComponent }];\n@NgModule({ imports: [RouterModule.forChild(routes)], exports: [RouterModule] })\nexport class AdminRoutingModule {}\n";
/// An Nx shell that lazy-loads the `@ws/home` library at `home`.
const ANGULAR_NX_SHELL: &str = "import { Route } from '@angular/router';\nexport const shellRoutes: Route[] = [\n  { path: 'home', loadChildren: async () => (await import('@ws/home')).HomeModule },\n];\n";
/// An Nx library module whose `''` route renders its home.
const ANGULAR_NX_HOME_MODULE: &str = "import { NgModule } from '@angular/core';\nimport { RouterModule } from '@angular/router';\nimport { HomeComponent } from './home.component';\n@NgModule({ imports: [RouterModule.forChild([{ path: '', component: HomeComponent }])] })\nexport class HomeModule {}\n";
/// Another library module: `welcome`.
const ANGULAR_NX_LANDING_MODULE: &str = "import { NgModule } from '@angular/core';\nimport { RouterModule } from '@angular/router';\nimport { LandingComponent } from './landing.component';\n@NgModule({ imports: [RouterModule.forChild([{ path: 'welcome', component: LandingComponent }])] })\nexport class LandingModule {}\n";
/// The library components the modules render.
const ANGULAR_NX_HOME: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'ws-home', template: '' })\nexport class HomeComponent {}\n";
const ANGULAR_NX_LANDING: &str = "import { Component } from '@angular/core';\n@Component({ selector: 'ws-landing', template: '' })\nexport class LandingComponent {}\n";

/// An Angular app: the app routes, the profile routes they mount, and the
/// screens both render.
fn angular_app(case: Case) -> Case {
    case.file("package.json", ANGULAR_PACKAGE)
        .file("src/app/auth.component.ts", ANGULAR_AUTH)
        .file("src/app/profile/profile.component.ts", ANGULAR_PROFILE)
}

/// An Nx workspace: the shell lazy-loads `@ws/home`, whose barrel names
/// one of two library modules.
fn angular_nx(case: Case) -> Case {
    case.file("package.json", ANGULAR_PACKAGE)
        .file(
            "tsconfig.base.json",
            "{\"compilerOptions\":{\"paths\":{\"@ws/home\":[\"libs/home/src/index.ts\"]}}}\n",
        )
        .file("libs/shell/src/lib/shell.routes.ts", ANGULAR_NX_SHELL)
        .file("libs/home/src/lib/home.module.ts", ANGULAR_NX_HOME_MODULE)
        .file("libs/home/src/lib/home.component.ts", ANGULAR_NX_HOME)
        .file(
            "libs/home/src/lib/landing.module.ts",
            ANGULAR_NX_LANDING_MODULE,
        )
        .file("libs/home/src/lib/landing.component.ts", ANGULAR_NX_LANDING)
}

/// W7-13 (upstream #2113): the mount that lazy-loads a routes file is
/// edited: the loaded file's routes take the new prefix, though the file is
/// untouched.
pub fn typescript_angular_mount_path_edit_moves_the_loaded_routes() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file("src/app/app.routes.ts", ANGULAR_APP_ROUTES)
        .file("src/app/profile/profile.routes.ts", ANGULAR_PROFILE_ROUTES)
        .write(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth.component';\nexport const routes: Routes = [\n  { path: 'login', component: AuthComponent },\n  { path: 'user', loadChildren: () => import('./profile/profile.routes') },\n];\n",
        )
        .named_before("route", "/profile/:username")
        .named_after("route", "/user/:username")
        .edge_throughout(
            "route src/app/profile/profile.routes.ts::route:/:username",
            "references",
            "class ProfileComponent",
        )
}

/// W7-13 (upstream #2113): the constant a route's path is written with
/// changes in another file: the route is renamed, though its routes file
/// is untouched.
pub fn typescript_angular_constant_path_edit_renames_the_route() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file("src/app/account.component.ts", ANGULAR_ACCOUNT)
        .file(
            "src/app/routes.constants.ts",
            "export const internalRoutes = {\n  account: { path: 'account' },\n};\n",
        )
        .file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AccountComponent } from './account.component';\nimport { internalRoutes } from './routes.constants';\nexport const routes: Routes = [\n  { path: internalRoutes.account.path, component: AccountComponent },\n];\n",
        )
        .write(
            "src/app/routes.constants.ts",
            "export const internalRoutes = {\n  account: { path: 'my-account' },\n};\n",
        )
        .named_before("route", "/account")
        .named_after("route", "/my-account")
        .edge_throughout(
            "route src/app/app.routes.ts::route:/{internalRoutes.account.path}",
            "references",
            "class AccountComponent",
        )
}

/// W7-13 (upstream #2133): the barrel a constant is imported through comes
/// to re-export it from another file: the route takes that file's value.
pub fn typescript_angular_constant_barrel_reexports_another_file() -> Case {
    angular_app(Case::new(Change::ReexportEdit, &[Language::TypeScript]))
        .file("src/app/account.component.ts", ANGULAR_ACCOUNT)
        .file(
            "src/app/paths/en.ts",
            "export const internalRoutes = { account: { path: 'account' } };\n",
        )
        .file(
            "src/app/paths/de.ts",
            "export const internalRoutes = { account: { path: 'konto' } };\n",
        )
        .file("src/app/paths/index.ts", "export * from './en';\n")
        .file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AccountComponent } from './account.component';\nimport { internalRoutes } from './paths';\nexport const routes: Routes = [\n  { path: internalRoutes.account.path, component: AccountComponent },\n];\n",
        )
        .write("src/app/paths/index.ts", "export * from './de';\n")
        .named_before("route", "/account")
        .named_after("route", "/konto")
}

/// W7-13 (upstream #2113): an NgModule a mount lazy-loads comes to import
/// its routing module: the routing module's routes sit under the mount.
pub fn typescript_angular_ng_module_imports_its_routing_module() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file("src/app/admin/users.component.ts", ANGULAR_USERS)
        .file("src/app/admin/admin-routing.module.ts", ANGULAR_ADMIN_ROUTING)
        .file(
            "src/app/admin/admin.module.ts",
            "import { NgModule } from '@angular/core';\nimport { CommonModule } from '@angular/common';\n@NgModule({ imports: [CommonModule] })\nexport class AdminModule {}\n",
        )
        .file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nexport const routes: Routes = [\n  { path: 'admin', loadChildren: () => import('./admin/admin.module').then((m) => m.AdminModule) },\n];\n",
        )
        .write(
            "src/app/admin/admin.module.ts",
            "import { NgModule } from '@angular/core';\nimport { CommonModule } from '@angular/common';\nimport { AdminRoutingModule } from './admin-routing.module';\n@NgModule({ imports: [CommonModule, AdminRoutingModule] })\nexport class AdminModule {}\n",
        )
        .named_before("route", "/users")
        .named_after("route", "/admin/users")
}

/// W7-13 (upstream #2133): an Nx library's barrel comes to forward another
/// module: that module's routes sit under the shell's mount, and the old
/// one's no longer do.
pub fn typescript_angular_nx_barrel_forwards_another_module() -> Case {
    angular_nx(Case::new(Change::ReexportEdit, &[Language::TypeScript]))
        .file(
            "libs/home/src/index.ts",
            "export * from './lib/home.module';\n",
        )
        .write(
            "libs/home/src/index.ts",
            "export * from './lib/landing.module';\n",
        )
        .named_before("route", "/home")
        .named_before("route", "/welcome")
        .named_after("route", "/")
        .named_after("route", "/home/welcome")
}

/// W7-13 (upstream #2133): the path alias an Nx shell lazy-loads by comes
/// to name another library: that library's routes sit under the mount.
pub fn typescript_angular_nx_alias_retargets_the_mount() -> Case {
    angular_nx(Case::new(Change::ControlFile, &[Language::TypeScript]))
        .file(
            "libs/home/src/index.ts",
            "export * from './lib/home.module';\n",
        )
        .file(
            "libs/landing/src/index.ts",
            "export * from '../../home/src/lib/landing.module';\n",
        )
        .write(
            "tsconfig.base.json",
            "{\"compilerOptions\":{\"paths\":{\"@ws/home\":[\"libs/landing/src/index.ts\"]}}}\n",
        )
        .named_before("route", "/home")
        .named_after("route", "/home/welcome")
}

/// W7-13 (upstream #2113): the component file a route lazy-loads moves its
/// default export to another of its components: the route references that
/// one, though the routes file is untouched.
pub fn typescript_angular_lazy_component_default_export_moves() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nexport const routes: Routes = [\n  { path: 'home', loadComponent: () => import('./home/home.component') },\n];\n",
        )
        .file(
            "src/app/home/home.component.ts",
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-home', template: '' })\nexport default class HomeComponent {}\n@Component({ selector: 'app-legacy', template: '' })\nexport class LegacyHomeComponent {}\n",
        )
        .write(
            "src/app/home/home.component.ts",
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-home', template: '' })\nexport class HomeComponent {}\n@Component({ selector: 'app-legacy', template: '' })\nexport default class LegacyHomeComponent {}\n",
        )
        .edge_before(
            "route src/app/app.routes.ts::route:/home",
            "references",
            "class HomeComponent",
        )
        .edge_after(
            "route src/app/app.routes.ts::route:/home",
            "references",
            "class LegacyHomeComponent",
        )
        .no_edge_after(
            "route src/app/app.routes.ts::route:/home",
            "references",
            "class HomeComponent",
        )
}

/// W7-13 (upstream #2113): the component file a route lazy-loads appears:
/// the route references its component, as in a fresh index.
pub fn typescript_angular_lazy_component_appears() -> Case {
    angular_app(Case::new(Change::AddImportTarget, &[Language::TypeScript]))
        .file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nexport const routes: Routes = [\n  { path: 'about', loadComponent: () => import('./about/about.component').then((m) => m.AboutComponent) },\n];\n",
        )
        .write(
            "src/app/about/about.component.ts",
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-about', template: '' })\nexport class AboutComponent {}\n",
        )
        .edge_after(
            "route src/app/app.routes.ts::route:/about",
            "references",
            "class AboutComponent",
        )
}

/// W7-13 (upstream #2113): the routes file a mount lazy-loads appears: its
/// routes sit under the mount, as in a fresh index.
pub fn typescript_angular_mounted_routes_file_appears() -> Case {
    angular_app(Case::new(Change::AddImportTarget, &[Language::TypeScript]))
        .file("src/app/app.routes.ts", ANGULAR_APP_ROUTES)
        .write("src/app/profile/profile.routes.ts", ANGULAR_PROFILE_ROUTES)
        .named_after("route", "/profile/:username")
        .edge_after(
            "route src/app/profile/profile.routes.ts::route:/:username",
            "references",
            "class ProfileComponent",
        )
}

/// W7-13: a file earlier in path order comes to mount the same routes file:
/// a routes file mounted from two places sits under the first in path
/// order, so the loaded routes move there, as in a fresh index.
pub fn typescript_angular_earlier_file_mounts_the_routes_too() -> Case {
    angular_app(Case::new(Change::AddCompetitor, &[Language::TypeScript]))
        .file("src/app/app.routes.ts", ANGULAR_APP_ROUTES)
        .file("src/app/profile/profile.routes.ts", ANGULAR_PROFILE_ROUTES)
        .write(
            "src/app/admin.routes.ts",
            "import { Routes } from '@angular/router';\nexport const adminRoutes: Routes = [\n  { path: 'people', loadChildren: () => import('./profile/profile.routes') },\n];\n",
        )
        .named_before("route", "/profile/:username")
        .named_after("route", "/people/:username")
}

/// W7-13 (upstream #2113): the manifest comes to declare Angular: the app's
/// routes appear, though no source changed.
pub fn typescript_angular_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"app\"}\n")
        .file("src/app/auth.component.ts", ANGULAR_AUTH)
        .file("src/app/profile/profile.component.ts", ANGULAR_PROFILE)
        .file("src/app/app.routes.ts", ANGULAR_APP_ROUTES)
        .file("src/app/profile/profile.routes.ts", ANGULAR_PROFILE_ROUTES)
        .write("package.json", ANGULAR_PACKAGE)
        .named_after("route", "/login")
        .named_after("route", "/profile/:username")
        .edge_after(
            "route src/app/app.routes.ts::route:/login",
            "references",
            "class AuthComponent",
        )
}

/// A component whose `go` navigates to `/login`.
const ANGULAR_NAV_LOGIN: &str = "import { Router } from '@angular/router';\nexport class NavComponent {\n  constructor(private router: Router) {}\n  go() { this.router.navigate(['/login']); }\n}\n";
/// Routes `/x1` and `/x2`, and `go` redirecting to `x1`.
const ANGULAR_X_ROUTES: &str = "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth.component';\nimport { ProfileComponent } from './profile/profile.component';\nexport const routes: Routes = [\n  { path: 'x1', component: AuthComponent },\n  { path: 'x2', component: ProfileComponent },\n  { path: 'go', redirectTo: 'x1' },\n];\n";

/// W7-13 (upstream #2113): a `navigate` is edited to name another route:
/// the `navigates` edge follows it.
pub fn typescript_angular_navigation_call_names_another_route() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file("src/app/app.routes.ts", ANGULAR_APP_ROUTES)
        .file("src/app/profile/profile.routes.ts", ANGULAR_PROFILE_ROUTES)
        .file("src/app/nav.component.ts", ANGULAR_NAV_LOGIN)
        .write(
            "src/app/nav.component.ts",
            "import { Router } from '@angular/router';\nexport class NavComponent {\n  constructor(private router: Router) {}\n  go(name) { this.router.navigate(['/profile', name]); }\n}\n",
        )
        .edge_before(
            "method NavComponent::go",
            "navigates",
            "route src/app/app.routes.ts::route:/login",
        )
        .edge_after(
            "method NavComponent::go",
            "navigates",
            "route src/app/profile/profile.routes.ts::route:/:username",
        )
        .no_edge_after(
            "method NavComponent::go",
            "navigates",
            "route src/app/app.routes.ts::route:/login",
        )
        .edge_metadata_after(
            (
                "method NavComponent::go",
                "navigates",
                "route src/app/profile/profile.routes.ts::route:/:username",
            ),
            "framework",
            "angular-router",
        )
}

/// W7-13: the redirect a call's redirect leads through is edited in
/// another file to send the user elsewhere: the call follows the chain to
/// the new route, though neither its file nor the first redirect's
/// changed.
pub fn typescript_angular_redirect_chain_link_moves() -> Case {
    angular_app(Case::new(Change::BodyEdit, &[Language::TypeScript]))
        .file("src/app/x.routes.ts", ANGULAR_X_ROUTES)
        .file(
            "src/app/chain.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth.component';\nexport const chainRoutes: Routes = [\n  { path: 'chain', component: AuthComponent },\n  { path: 'start', redirectTo: 'go' },\n];\n",
        )
        .file(
            "src/app/nav.component.ts",
            "import { Router } from '@angular/router';\nexport class NavComponent {\n  constructor(private router: Router) {}\n  go() { this.router.navigate(['/start']); }\n}\n",
        )
        .write(
            "src/app/x.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth.component';\nimport { ProfileComponent } from './profile/profile.component';\nexport const routes: Routes = [\n  { path: 'x1', component: AuthComponent },\n  { path: 'x2', component: ProfileComponent },\n  { path: 'go', redirectTo: 'x2' },\n];\n",
        )
        .edge_before(
            "method NavComponent::go",
            "navigates",
            "route src/app/x.routes.ts::route:/x1",
        )
        .edge_after(
            "method NavComponent::go",
            "navigates",
            "route src/app/x.routes.ts::route:/x2",
        )
        .no_edge_after(
            "method NavComponent::go",
            "navigates",
            "route src/app/x.routes.ts::route:/x1",
        )
}

/// The code-based route tree the W7-10 reparenting case edits. It exports
/// the tree: in a module that exports nothing, a route is no other file's to
/// reach (KEEP-RUST, W7-10).
const TANSTACK_CODE_TREE: &str = "import { createRootRoute, createRoute } from '@tanstack/react-router'\nconst rootRoute = createRootRoute({ component: Root })\nconst postsRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  path: 'posts',\n  component: Posts,\n})\nconst postRoute = createRoute({\n  getParentRoute: () => postsRoute,\n  path: '$postId',\n  component: Post,\n})\nfunction Root() {\n  return null\n}\nfunction Posts() {\n  return null\n}\nfunction Post() {\n  return null\n}\nexport const routeTree = rootRoute.addChildren([postsRoute, postRoute])\n";

/// The same tree with `postRoute` moved under the root.
const TANSTACK_CODE_TREE_REPARENTED: &str = "import { createRootRoute, createRoute } from '@tanstack/react-router'\nconst rootRoute = createRootRoute({ component: Root })\nconst postsRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  path: 'posts',\n  component: Posts,\n})\nconst postRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  path: '$postId',\n  component: Post,\n})\nfunction Root() {\n  return null\n}\nfunction Posts() {\n  return null\n}\nfunction Post() {\n  return null\n}\nexport const routeTree = rootRoute.addChildren([postsRoute, postRoute])\n";

/// W7-10 (upstream F5): a code route tree moves a route under another
/// parent: its address changes, and the call that named the old address
/// names no route, not even the moved one.
pub fn tsx_tanstack_code_route_moves_under_another_parent() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", TANSTACK_PACKAGE)
        .file("src/router.tsx", TANSTACK_CODE_TREE)
        .file(
            "src/lib/nav.ts",
            "export function openPost(navigate, postId) {\n  navigate({ to: '/posts/$postId', params: { postId } })\n}\n",
        )
        .write(
            "src/router.tsx",
            TANSTACK_CODE_TREE_REPARENTED,
        )
        .edge_before(
            "function openPost",
            "navigates",
            "route src/router.tsx::route:/posts/:postId",
        )
        .no_edge_after(
            "function openPost",
            "navigates",
            "route src/router.tsx::route:/:postId",
        )
        .edge_after(
            "route src/router.tsx::route:/:postId",
            "calls",
            "function Post",
        )
}

/// The React Router app the W7-09b cases edit: two routes in `src/App.tsx`,
/// their pages, and a navigation call in `src/nav.ts`.
const REACT_ROUTER_PACKAGE: &str =
    "{\"name\":\"web\",\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"6\"}}\n";
const REACT_ROUTER_APP: &str = "import { Routes, Route } from 'react-router-dom'\nimport { Login } from './pages/Login'\nimport { SignIn } from './pages/SignIn'\nimport { Help } from './pages/Help'\nexport function App() {\n  return (\n    <Routes>\n      <Route path=\"/login\" element={<Login />} />\n      <Route path=\"/help\" element={<Help />} />\n    </Routes>\n  )\n}\n";
const REACT_ROUTER_LOGIN: &str = "export function Login() {\n  return null\n}\n";
const REACT_ROUTER_SIGN_IN: &str = "export function SignIn() {\n  return null\n}\n";
const REACT_ROUTER_HELP: &str = "export function Help() {\n  return null\n}\n";
const REACT_ROUTER_NAV: &str = "import { useNavigate } from 'react-router-dom'\nexport function goLogin() {\n  const navigate = useNavigate()\n  navigate('/login')\n}\n";

/// The React Router app before an edit.
fn react_router_app(change: Change) -> Case {
    Case::new(change, &[Language::TypeScript, Language::Tsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/App.tsx", REACT_ROUTER_APP)
        .file("src/pages/Login.tsx", REACT_ROUTER_LOGIN)
        .file("src/pages/SignIn.tsx", REACT_ROUTER_SIGN_IN)
        .file("src/pages/Help.tsx", REACT_ROUTER_HELP)
        .file("src/nav.ts", REACT_ROUTER_NAV)
}

/// W7-09b: the route file gives a route another element (upstream F4): the
/// route renders the new page, and the navigation to it stays.
pub fn tsx_react_router_route_renders_another_page() -> Case {
    react_router_app(Change::BodyEdit)
        .write(
            "src/App.tsx",
            "import { Routes, Route } from 'react-router-dom'\nimport { Login } from './pages/Login'\nimport { SignIn } from './pages/SignIn'\nimport { Help } from './pages/Help'\nexport function App() {\n  return (\n    <Routes>\n      <Route path=\"/login\" element={<SignIn />} />\n      <Route path=\"/help\" element={<Help />} />\n    </Routes>\n  )\n}\n",
        )
        .edge_before(
            "route src/App.tsx::route:/login",
            "references",
            "function Login",
        )
        .edge_after(
            "route src/App.tsx::route:/login",
            "references",
            "function SignIn",
        )
        .no_edge_after(
            "route src/App.tsx::route:/login",
            "references",
            "function Login",
        )
        .edge_throughout(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
        .edge_metadata_after(
            (
                "function goLogin",
                "navigates",
                "route src/App.tsx::route:/login",
            ),
            "framework",
            "react-router",
        )
}

/// W7-09b: a route's page is edited (upstream F4): the route still renders
/// it, and the navigation to the route stays.
pub fn tsx_react_router_page_body_edit_keeps_its_route() -> Case {
    react_router_app(Change::BodyEdit)
        .write(
            "src/pages/Login.tsx",
            "export function Login() {\n  return <form />\n}\n",
        )
        .edge_throughout(
            "route src/App.tsx::route:/login",
            "references",
            "function Login",
        )
        .edge_throughout(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
}

/// W7-09b: a route's page is deleted (upstream F4): the route stays an
/// address, rendering nothing, and the navigation to it stays.
pub fn tsx_react_router_page_delete_leaves_its_route() -> Case {
    react_router_app(Change::DeleteImportedModule)
        .remove("src/pages/Login.tsx")
        .edge_before(
            "route src/App.tsx::route:/login",
            "references",
            "function Login",
        )
        .named_after("route", "/login")
        .edge_throughout(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
}

/// W7-09b: a navigation call is edited to name another route (upstream F4):
/// the `navigates` edge follows the href.
pub fn typescript_react_router_navigation_call_names_another_route() -> Case {
    react_router_app(Change::BodyEdit)
        .write(
            "src/nav.ts",
            "import { useNavigate } from 'react-router-dom'\nexport function goLogin() {\n  const navigate = useNavigate()\n  navigate('/help')\n}\n",
        )
        .edge_before(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
        .edge_after(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/help",
        )
        .no_edge_after(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
}

/// W7-09b: `react-router-dom` appears in the manifest of a React app whose
/// routes React already reads (upstream F4): React Router is detected, and
/// the navigation call binds.
pub fn typescript_react_router_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"react\":\"18\"}}\n",
        )
        .file("src/App.tsx", REACT_ROUTER_APP)
        .file("src/pages/Login.tsx", REACT_ROUTER_LOGIN)
        .file("src/pages/SignIn.tsx", REACT_ROUTER_SIGN_IN)
        .file("src/pages/Help.tsx", REACT_ROUTER_HELP)
        .file("src/nav.ts", REACT_ROUTER_NAV)
        .write("package.json", REACT_ROUTER_PACKAGE)
        .edge_throughout(
            "route src/App.tsx::route:/login",
            "references",
            "function Login",
        )
        .edge_after(
            "function goLogin",
            "navigates",
            "route src/App.tsx::route:/login",
        )
}

/// W7-09b: the constant a route's path is built from changes (upstream
/// #2165 and `postExtract`): the route, whose own file did not change, is
/// renamed in place before anything resolves. (A navigation call in another
/// file bound to the old name is retried by the sync navigation pass, W7-23.)
pub fn typescript_react_router_route_constant_renames_the_route() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"react\":\"18\",\"react-router\":\"7\"}}\n",
        )
        .file(
            "src/config/paths.ts",
            "export const paths = {\n  app: { root: { path: '/app' }, team: { path: 'team' } },\n} as const\n",
        )
        .file(
            "src/router.tsx",
            "import { createBrowserRouter } from 'react-router'\nimport { paths } from './config/paths'\nimport { AppRoot } from './AppRoot'\nimport { Team } from './Team'\nexport const router = createBrowserRouter([\n  {\n    path: paths.app.root.path,\n    element: <AppRoot />,\n    children: [{ path: paths.app.team.path, element: <Team /> }],\n  },\n])\n",
        )
        .file(
            "src/AppRoot.tsx",
            "export function AppRoot() {\n  return null\n}\n",
        )
        .file("src/Team.tsx", "export function Team() {\n  return null\n}\n")
        .write(
            "src/config/paths.ts",
            "export const paths = {\n  app: { root: { path: '/dashboard' }, team: { path: 'team' } },\n} as const\n",
        )
        .named_before("route", "/app")
        .named_before("route", "/app/team")
        .named_after("route", "/dashboard")
        .named_after("route", "/dashboard/team")
        .edge_throughout(
            "route src/router.tsx::route:/{paths.app.root.path}",
            "references",
            "function AppRoot",
        )
}

/// W7-09b: the routes file whose route a constant names is edited, and a
/// navigation in it names the route's path (upstream `postExtract` before
/// resolution): the sync renames the re-extracted route before it resolves
/// the call, as a full index does, so the call still navigates to it.
pub fn tsx_react_router_routes_file_edit_names_its_route_before_resolving() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"react\":\"18\",\"react-router\":\"7\"}}\n",
        )
        .file(
            "src/config/paths.ts",
            "export const paths = {\n  app: { root: { path: '/app' } },\n} as const\n",
        )
        .file(
            "src/router.tsx",
            "import { createBrowserRouter, redirect } from 'react-router'\nimport { paths } from './config/paths'\nimport { AppRoot } from './AppRoot'\nexport const router = createBrowserRouter([\n  { path: paths.app.root.path, element: <AppRoot /> },\n])\nexport function goApp() {\n  return redirect('/app')\n}\n",
        )
        .file(
            "src/AppRoot.tsx",
            "export function AppRoot() {\n  return null\n}\n",
        )
        .write(
            "src/router.tsx",
            "import { createBrowserRouter, redirect } from 'react-router'\nimport { paths } from './config/paths'\nimport { AppRoot } from './AppRoot'\nexport const router = createBrowserRouter([\n  { path: paths.app.root.path, element: <AppRoot /> },\n])\nexport function goApp() {\n  console.log('leaving')\n  return redirect('/app')\n}\n",
        )
        .named_before("route", "/app")
        .named_after("route", "/app")
        .edge_throughout(
            "function goApp",
            "navigates",
            "route src/router.tsx::route:/{paths.app.root.path}",
        )
}

/// W7-09b: the page a lazy route's barrel imports appears (upstream
/// `react-router-barrel-imports.test.ts`, sync): the parked `lazy-import:`
/// reference waits under its path's last segment, which the new page's
/// folder is, so the sync retries it. (A page appearing deeper under the
/// barrel is #2560's, W7-23.)
pub fn tsx_react_router_lazy_barrel_page_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"shop\",\"private\":true,\"dependencies\":{\"react\":\"^18\",\"react-router\":\"^7\"}}\n",
        )
        .file(
            "src/router.tsx",
            "import { createBrowserRouter } from 'react-router';\n\nexport const router = createBrowserRouter([\n  { path: '/login', lazy: () => import('./auth/login') },\n]);\n",
        )
        .file(
            "src/auth/login/index.ts",
            "import Login from './Login';\n\nexport default Login;\n",
        )
        .write(
            "src/auth/login/Login.tsx",
            "export default function Login() {\n  return null;\n}\n",
        )
        .edge_after(
            "route src/router.tsx::route:/login",
            "references",
            "function Login",
        )
}

/// W7-09b: the page a lazy route's barrel imports moves its default export
/// to another component, the old one staying (upstream
/// `react-router-barrel-imports.test.ts`, sync): the route renders the new
/// default.
pub fn tsx_react_router_lazy_page_default_export_moves() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "package.json",
            "{\"name\":\"shop\",\"private\":true,\"dependencies\":{\"react\":\"^18\",\"react-router\":\"^7\"}}\n",
        )
        .file(
            "src/router.tsx",
            "import { createBrowserRouter } from 'react-router';\n\nexport const router = createBrowserRouter([\n  { path: '/login', lazy: () => import('./auth/login') },\n]);\n",
        )
        .file(
            "src/auth/login/index.ts",
            "import Login from './Login';\n\nexport default Login;\n",
        )
        .file(
            "src/auth/login/Login.tsx",
            "export default function Login() {\n  return null;\n}\n",
        )
        .write(
            "src/auth/login/Login.tsx",
            "export function Login() {\n  return null;\n}\n\nexport default function SignIn() {\n  return null;\n}\n",
        )
        .edge_before(
            "route src/router.tsx::route:/login",
            "references",
            "function Login",
        )
        .edge_after(
            "route src/router.tsx::route:/login",
            "references",
            "function SignIn",
        )
}

/// The package of the W7-09c cases: React Router 7.
const REACT_ROUTER_7_PACKAGE: &str = "{\"name\":\"ui\",\"private\":true,\"dependencies\":{\"react\":\"^18\",\"react-router\":\"^7\"}}\n";

/// The `/reports` route of the W7-09c cases, its loader picking
/// `ReportList` from the `reports` barrel.
const REPORTS_ROUTER_LIST: &str = "import { createBrowserRouter } from 'react-router';\n\nexport const router = createBrowserRouter([\n  {\n    path: '/reports',\n    lazy: async () => {\n      const { ReportList } = await import('./reports');\n      return { Component: ReportList };\n    },\n  },\n]);\n";

/// The same route, its loader picking `ReportPage`.
const REPORTS_ROUTER_PAGE: &str = "import { createBrowserRouter } from 'react-router';\n\nexport const router = createBrowserRouter([\n  {\n    path: '/reports',\n    lazy: async () => {\n      const { ReportPage } = await import('./reports');\n      return { Component: ReportPage };\n    },\n  },\n]);\n";

/// W7-09c (upstream #2480): a route's loader is edited to pick another
/// export of the module it loads: the route renders the new pick.
pub fn tsx_react_router_loader_picks_another_export() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file("package.json", REACT_ROUTER_7_PACKAGE)
        .file("src/router.tsx", REPORTS_ROUTER_LIST)
        .file(
            "src/reports/index.ts",
            "export * from './report_list';\nexport * from './report_page';\n",
        )
        .file(
            "src/reports/report_list.tsx",
            "export function ReportList() {\n  return null;\n}\n",
        )
        .file(
            "src/reports/report_page.tsx",
            "export function ReportPage() {\n  return null;\n}\n",
        )
        .write("src/router.tsx", REPORTS_ROUTER_PAGE)
        .edge_before(
            "route src/router.tsx::route:/reports",
            "references",
            "function ReportList",
        )
        .edge_after(
            "route src/router.tsx::route:/reports",
            "references",
            "function ReportPage",
        )
}

/// W7-09c (upstream `react-router-lazy-member.test.ts`, sync): the module a
/// loader picks the default export of moves its default to another
/// component, the old one staying: the route renders the new default.
pub fn tsx_react_router_picked_default_export_moves() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("package.json", REACT_ROUTER_7_PACKAGE)
        .file(
            "src/router.tsx",
            "import { createBrowserRouter } from 'react-router';\n\nexport const router = createBrowserRouter([\n  {\n    path: '/login',\n    lazy: async () => {\n      const { default: Component } = await import('./pages/login');\n      return { Component };\n    },\n  },\n]);\n",
        )
        .file(
            "src/pages/login.tsx",
            "export default function LoginPage() {\n  return null;\n}\n",
        )
        .write(
            "src/pages/login.tsx",
            "export function LoginPage() {\n  return null;\n}\n\nexport default function SignIn() {\n  return null;\n}\n",
        )
        .edge_before(
            "route src/router.tsx::route:/login",
            "references",
            "function LoginPage",
        )
        .edge_after(
            "route src/router.tsx::route:/login",
            "references",
            "function SignIn",
        )
}

/// W7-09c (upstream `react-router-lazy-member.test.ts`, sync): the module a
/// loader picks an export from appears, in a folder of its own. The parked
/// `import:./reports#ReportList` waits under no key the new files carry;
/// the sync retries it because a file in a new folder moves the JS control
/// inputs (the manifests framework detection reads), which re-resolves every
/// JavaScript-family file whole. The same module appearing beside the route
/// file, and an edit that gives the module the export, stay W7-23's.
pub fn tsx_react_router_picked_export_module_appears() -> Case {
    Case::new(
        Change::AddImportTarget,
        &[Language::TypeScript, Language::Tsx],
    )
    .file("package.json", REACT_ROUTER_7_PACKAGE)
    .file("src/router.tsx", REPORTS_ROUTER_LIST)
    .write("src/reports/index.ts", "export * from './report_list';\n")
    .write(
        "src/reports/report_list.tsx",
        "export function ReportList() {\n  return null;\n}\n",
    )
    .edge_after(
        "route src/router.tsx::route:/reports",
        "references",
        "function ReportList",
    )
}

/// W7-09b: a lazily loaded route module's body is edited (upstream F4): the
/// route still renders its default export, and still inside its layout.
pub fn tsx_react_router_lazy_page_body_edit_keeps_its_route() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file(
            "src/router.tsx",
            "import { createBrowserRouter } from 'react-router-dom'\nimport { Layout } from './Layout'\nexport const router = createBrowserRouter([\n  {\n    path: '/',\n    element: <Layout />,\n    children: [{ path: 'team', lazy: () => import('./pages/Team') }],\n  },\n])\n",
        )
        .file(
            "src/Layout.tsx",
            "export function Layout() {\n  return null\n}\n",
        )
        .file(
            "src/pages/Team.tsx",
            "export default function Team() {\n  return null\n}\n",
        )
        .write(
            "src/pages/Team.tsx",
            "export default function Team() {\n  return <ul />\n}\n",
        )
        .edge_throughout(
            "route src/router.tsx::route:/team",
            "references",
            "function Team",
        )
        .edge_throughout(
            "route src/router.tsx::route:/team",
            "references",
            "function Layout",
        )
}

// ----- Route tables another file hands React Router (W7-09d) ----------

/// The route table of the W7-09d cases (upstream `react-router.test.ts`, "a
/// route table as the files around it change"): the index route and
/// `/counter`.
const APP_ROUTES_TABLE: &str = "import { Counter } from './Counter';\nimport { Home } from './Home';\nimport { FetchData } from './FetchData';\n\nconst AppRoutes = [\n  { index: true, element: <Home /> },\n  { path: '/counter', element: <Counter /> },\n];\n\nexport default AppRoutes;\n";

/// The same table with `/fetch-data` in front.
const APP_ROUTES_TABLE_WITH_FETCH: &str = "import { Counter } from './Counter';\nimport { Home } from './Home';\nimport { FetchData } from './FetchData';\n\nconst AppRoutes = [\n  { path: '/fetch-data', element: <FetchData /> },\n  { index: true, element: <Home /> },\n  { path: '/counter', element: <Counter /> },\n];\n\nexport default AppRoutes;\n";

/// The file that maps the table into `<Route>`s.
const APP_MAPS_THE_TABLE: &str = "import { Route, Routes } from 'react-router-dom';\nimport AppRoutes from './AppRoutes';\nimport { Counter } from './Counter';\n\nexport default function App() {\n  return (\n    <Routes>\n      {AppRoutes.map(({ element, ...rest }, index) => <Route key={index} {...rest} element={element} />)}\n    </Routes>\n  );\n}\n";

/// The same file with a route of its own instead.
const APP_WRITES_A_ROUTE: &str = "import { Route, Routes } from 'react-router-dom';\nimport AppRoutes from './AppRoutes';\nimport { Counter } from './Counter';\n\nexport default function App() {\n  return (\n    <Routes>\n      <Route path=\"/static\" element={<Counter />} />\n    </Routes>\n  );\n}\n";

/// A navigation call to the table's index route.
const NAV_MENU_HOME: &str = "import { useNavigate } from 'react-router-dom';\nexport function NavMenu() {\n  const navigate = useNavigate();\n  const goHome = () => navigate('/');\n  return <button onClick={goHome}>Home</button>;\n}\n";

const COUNTER_PAGE: &str = "export function Counter() {\n  return <div>Counter</div>;\n}\n";
const HOME_PAGE: &str = "export function Home() {\n  return <div>Home</div>;\n}\n";
const FETCH_DATA_PAGE: &str = "export function FetchData() {\n  return <div>FetchData</div>;\n}\n";

/// W7-09d (upstream #2400): an edit to a table another file hands the
/// router moves its routes; the navigation that named one still does.
pub fn jsx_react_router_table_edit_moves_its_routes() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/AppRoutes.js", APP_ROUTES_TABLE)
        .file("src/App.js", APP_MAPS_THE_TABLE)
        .file("src/Home.js", HOME_PAGE)
        .file("src/Counter.js", COUNTER_PAGE)
        .file("src/FetchData.js", FETCH_DATA_PAGE)
        .file("src/NavMenu.js", NAV_MENU_HOME)
        .write("src/AppRoutes.js", APP_ROUTES_TABLE_WITH_FETCH)
        .edge_after(
            "route src/AppRoutes.js::route:/fetch-data",
            "references",
            "function FetchData",
        )
        .edge_throughout(
            "function NavMenu::goHome",
            "navigates",
            "route src/AppRoutes.js::route:/",
        )
}

/// W7-09d (upstream #2400): the file that handed the router a table writes
/// a route of its own instead: the table's routes go, and the navigation
/// that named one names nothing.
pub fn jsx_react_router_consumer_stops_handing_its_table() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/AppRoutes.js", APP_ROUTES_TABLE)
        .file("src/App.js", APP_MAPS_THE_TABLE)
        .file("src/Home.js", HOME_PAGE)
        .file("src/Counter.js", COUNTER_PAGE)
        .file("src/FetchData.js", FETCH_DATA_PAGE)
        .file("src/NavMenu.js", NAV_MENU_HOME)
        .write("src/App.js", APP_WRITES_A_ROUTE)
        .edge_before(
            "function NavMenu::goHome",
            "navigates",
            "route src/AppRoutes.js::route:/",
        )
        .edge_after(
            "route src/App.js::route:/static",
            "references",
            "function Counter",
        )
}

/// W7-09d (upstream #2400): a file starts handing the router a table. No
/// navigation names its routes before they exist: a call parked then is
/// W7-23's (#2425).
pub fn jsx_react_router_consumer_starts_handing_a_table() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/AppRoutes.js", APP_ROUTES_TABLE)
        .file("src/App.js", APP_WRITES_A_ROUTE)
        .file("src/Home.js", HOME_PAGE)
        .file("src/Counter.js", COUNTER_PAGE)
        .file("src/FetchData.js", FETCH_DATA_PAGE)
        .write("src/App.js", APP_MAPS_THE_TABLE)
        .edge_after(
            "route src/AppRoutes.js::route:/counter",
            "references",
            "function Counter",
        )
        .edge_after(
            "route src/AppRoutes.js::route:/",
            "references",
            "function Home",
        )
}

/// W7-09d: the file that hands the router a table is deleted, the module
/// that imported it with it: the table's routes go.
pub fn jsx_react_router_table_consumer_deleted() -> Case {
    Case::new(
        Change::DeleteImportedModule,
        &[Language::JavaScript, Language::Jsx],
    )
    .file("package.json", REACT_ROUTER_PACKAGE)
    .file("src/AppRoutes.js", APP_ROUTES_TABLE)
    .file("src/App.js", APP_MAPS_THE_TABLE)
    .file(
        "src/index.js",
        "import App from './App';\n\nexport default App;\n",
    )
    .file("src/Home.js", HOME_PAGE)
    .file("src/Counter.js", COUNTER_PAGE)
    .file("src/FetchData.js", FETCH_DATA_PAGE)
    .file("src/NavMenu.js", NAV_MENU_HOME)
    .remove("src/App.js")
    .edge_before(
        "route src/AppRoutes.js::route:/counter",
        "references",
        "function Counter",
    )
    .edge_before(
        "function NavMenu::goHome",
        "navigates",
        "route src/AppRoutes.js::route:/",
    )
}

/// W7-09d: a page a table's route renders moves within its file: the route
/// renders it where it is now.
pub fn jsx_react_router_table_page_moves() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/AppRoutes.js", APP_ROUTES_TABLE)
        .file("src/App.js", APP_MAPS_THE_TABLE)
        .file("src/Home.js", HOME_PAGE)
        .file("src/Counter.js", COUNTER_PAGE)
        .file("src/FetchData.js", FETCH_DATA_PAGE)
        .write(
            "src/Counter.js",
            "\nexport function Counter() {\n  return <div>Counter</div>;\n}\n",
        )
        .edge_throughout(
            "route src/AppRoutes.js::route:/counter",
            "references",
            "function Counter",
        )
}

/// The routes of a W7-09d case that maps another file's table inside
/// `<Route path="/admin">`.
const ADMIN_MAPS_THE_TABLE: &str = "import { Routes, Route } from 'react-router-dom';\nimport AdminLayout from './layouts/admin';\nimport { adminRoutes } from './adminRoutes';\n\nexport default function Router() {\n  return (\n    <Routes>\n      <Route path=\"/admin\" element={<AdminLayout/>}>\n        {adminRoutes.map((route) => <Route key={route.path} {...route}/>)}\n      </Route>\n    </Routes>\n  );\n}\n";

/// The same routes under `/manage`.
const MANAGE_MAPS_THE_TABLE: &str = "import { Routes, Route } from 'react-router-dom';\nimport AdminLayout from './layouts/admin';\nimport { adminRoutes } from './adminRoutes';\n\nexport default function Router() {\n  return (\n    <Routes>\n      <Route path=\"/manage\" element={<AdminLayout/>}>\n        {adminRoutes.map((route) => <Route key={route.path} {...route}/>)}\n      </Route>\n    </Routes>\n  );\n}\n";

/// W7-09d (upstream `react-router.test.ts`, "a table mapped inside <Route
/// path>"): the file that mounts another file's table moves it to another
/// path. The table's file is unchanged; its route is replaced under the new
/// path, and the navigation to the old path names nothing.
pub fn jsx_react_router_consumer_moves_another_files_table() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/routes.js", ADMIN_MAPS_THE_TABLE)
        .file(
            "src/adminRoutes.js",
            "import Reports from './pages/Reports';\n\nexport const adminRoutes = [{ path: 'reports', element: <Reports/> }];\n",
        )
        .file(
            "src/layouts/admin.js",
            "export default function AdminLayout() {\n  return <div />;\n}\n",
        )
        .file(
            "src/pages/Reports.js",
            "export default function Reports() {\n  return <div />;\n}\n",
        )
        .file(
            "src/nav.js",
            "import { useNavigate } from 'react-router-dom';\nexport function Menu() {\n  const navigate = useNavigate();\n  const open = () => navigate('/admin/reports');\n  return <button onClick={open} />;\n}\n",
        )
        .write("src/routes.js", MANAGE_MAPS_THE_TABLE)
        .edge_before(
            "function Menu::open",
            "navigates",
            "route src/adminRoutes.js::route:/admin/reports",
        )
        .edge_after(
            "route src/adminRoutes.js::route:/manage/reports",
            "references",
            "function Reports",
        )
}

/// A table mapped inside the `<Route>` of the file it is written in, which
/// renders `DashboardLayout`.
const DASHBOARD_TABLE_IN_LAYOUT: &str = "import { Routes, Route } from 'react-router-dom';\nimport DashboardLayout from './layouts/dashboard';\nimport OtherLayout from './layouts/other';\nimport DashboardApp from './pages/DashboardApp';\n\nconst DashboardRoutes = [\n  { path: 'app', element: <DashboardApp/> },\n];\n\nexport default function Router() {\n  return (\n    <Routes>\n      <Route path=\"/dashboard\" element={<DashboardLayout/>}>\n        {DashboardRoutes.map((route, index) => {\n          const { element, ...rest } = route;\n          return <Route key={index} {...rest} element={element}/>;\n        })}\n      </Route>\n    </Routes>\n  );\n}\n";

/// The same file, the `<Route>` rendering `OtherLayout`.
const DASHBOARD_TABLE_IN_OTHER_LAYOUT: &str = "import { Routes, Route } from 'react-router-dom';\nimport DashboardLayout from './layouts/dashboard';\nimport OtherLayout from './layouts/other';\nimport DashboardApp from './pages/DashboardApp';\n\nconst DashboardRoutes = [\n  { path: 'app', element: <DashboardApp/> },\n];\n\nexport default function Router() {\n  return (\n    <Routes>\n      <Route path=\"/dashboard\" element={<OtherLayout/>}>\n        {DashboardRoutes.map((route, index) => {\n          const { element, ...rest } = route;\n          return <Route key={index} {...rest} element={element}/>;\n        })}\n      </Route>\n    </Routes>\n  );\n}\n";

/// W7-09d (upstream #2423): the layout around a table mapped in its own file
/// changes: the table's route renders inside the new one only.
pub fn jsx_react_router_layout_around_a_same_file_table_changes() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/routes.js", DASHBOARD_TABLE_IN_LAYOUT)
        .file(
            "src/layouts/dashboard.js",
            "export default function DashboardLayout() {\n  return <div />;\n}\n",
        )
        .file(
            "src/layouts/other.js",
            "export default function OtherLayout() {\n  return <div />;\n}\n",
        )
        .file(
            "src/pages/DashboardApp.js",
            "export default function DashboardApp() {\n  return <div />;\n}\n",
        )
        .write("src/routes.js", DASHBOARD_TABLE_IN_OTHER_LAYOUT)
        .edge_before(
            "route src/routes.js::route:/dashboard/app",
            "references",
            "function DashboardLayout",
        )
        .edge_after(
            "route src/routes.js::route:/dashboard/app",
            "references",
            "function OtherLayout",
        )
        .no_edge_after(
            "route src/routes.js::route:/dashboard/app",
            "references",
            "function DashboardLayout",
        )
}

/// A table mapped inside a layout of its own file, and handed over again by
/// an earlier file.
const SELF_MAPPED_TABLE: &str = "import { Routes, Route } from 'react-router-dom';\nimport Layout from './Layout';\nimport Reports from './Reports';\n\nexport const tableRoutes = [{ path: 'reports', element: <Reports/> }];\n\nexport function Shell() {\n  return (\n    <Routes>\n      <Route path=\"/\" element={<Layout/>}>\n        {tableRoutes.map((route) => <Route key={route.path} {...route}/>)}\n      </Route>\n    </Routes>\n  );\n}\n";

/// A file read before the table's own that hands it to `useRoutes`.
const EARLIER_CONSUMER: &str = "import { useRoutes } from 'react-router-dom';\nimport { tableRoutes } from './table';\n\nexport default function App() {\n  return useRoutes(tableRoutes);\n}\n";

/// The same file handing over nothing.
const EARLIER_FILE_QUIET: &str = "import { useNavigate } from 'react-router-dom';\n\nexport default function App() {\n  return null;\n}\n";

/// A navigation call to the `/reports` route of the earlier-consumer cases.
const NAV_TO_REPORTS: &str = "import { useNavigate } from 'react-router-dom';\nexport function Menu() {\n  const navigate = useNavigate();\n  const open = () => navigate('/reports');\n  return <button onClick={open} />;\n}\n";

/// W7-09d: of two files handing over one table at the same address, the
/// first read gives its routes, here outside the layout the table's own file
/// maps it inside. When it stops, the route keeps its id and name and gains
/// that layout: a route whose references changed is replaced, where
/// upstream's rename in place keeps the old ones, and the navigation into
/// it finds it again.
pub fn jsx_react_router_earlier_consumer_stops_handing_a_table() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/nav.js", NAV_TO_REPORTS)
        .file("src/a.js", EARLIER_CONSUMER)
        .file("src/table.js", SELF_MAPPED_TABLE)
        .file(
            "src/Layout.js",
            "export default function Layout() {\n  return null;\n}\n",
        )
        .file(
            "src/Reports.js",
            "export default function Reports() {\n  return null;\n}\n",
        )
        .write("src/a.js", EARLIER_FILE_QUIET)
        .edge_throughout(
            "route src/table.js::route:/reports",
            "references",
            "function Reports",
        )
        .edge_throughout(
            "function Menu::open",
            "navigates",
            "route src/table.js::route:/reports",
        )
        .edge_after(
            "route src/table.js::route:/reports",
            "references",
            "function Layout",
        )
}

/// W7-09d: the reverse: an earlier file starts handing over the table, and
/// the route renders outside the layout from then on.
pub fn jsx_react_router_earlier_consumer_starts_handing_a_table() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript, Language::Jsx])
        .file("package.json", REACT_ROUTER_PACKAGE)
        .file("src/nav.js", NAV_TO_REPORTS)
        .file("src/a.js", EARLIER_FILE_QUIET)
        .file("src/table.js", SELF_MAPPED_TABLE)
        .file(
            "src/Layout.js",
            "export default function Layout() {\n  return null;\n}\n",
        )
        .file(
            "src/Reports.js",
            "export default function Reports() {\n  return null;\n}\n",
        )
        .write("src/a.js", EARLIER_CONSUMER)
        .edge_before(
            "route src/table.js::route:/reports",
            "references",
            "function Layout",
        )
        .edge_throughout(
            "function Menu::open",
            "navigates",
            "route src/table.js::route:/reports",
        )
        .no_edge_after(
            "route src/table.js::route:/reports",
            "references",
            "function Layout",
        )
}

// ----- A JSX tag imported from the project renders what its module exports (W7-09e) --

/// The package of the W7-09e cases.
const MOBX_REACT_PACKAGE: &str =
    "{\"name\":\"app\",\"dependencies\":{\"react\":\"^18\",\"mobx-react\":\"^9\"}}\n";

/// outline's `Sidebar/App.tsx`: the default export wraps the component.
const APP_SIDEBAR: &str = "import { observer } from 'mobx-react';\n\nfunction AppSidebar() {\n  return <nav />;\n}\n\nexport default observer(AppSidebar);\n";

/// The same file wrapping another component.
const APP_SIDEBAR_WRAPS_COMPACT: &str = "import { observer } from 'mobx-react';\n\nfunction AppSidebar() {\n  return <nav />;\n}\n\nfunction CompactSidebar() {\n  return <nav />;\n}\n\nexport default observer(CompactSidebar);\n";

/// Another module with a wrapped default export.
const SHARED_SIDEBAR: &str = "import { observer } from 'mobx-react';\n\nfunction SharedSidebar() {\n  return <nav />;\n}\n\nexport default observer(SharedSidebar);\n";

/// A same-named component nothing imports.
const ASIDE_SIDEBAR: &str = "export function Sidebar() {\n  return <aside />;\n}\n";

/// outline's `Sidebar/index.ts`: it forwards `App`'s default export.
const INDEX_FORWARDS_APP: &str = "import Sidebar from './App';\n\nexport default Sidebar;\n";

/// The page that renders `<Sidebar>` through the index.
const HOME_RENDERS_SIDEBAR: &str = "import Sidebar from '../components/Sidebar';\n\nexport function Home() {\n  return <main><Sidebar /></main>;\n}\n";

/// W7-09e (upstream `sync-jsx-import-forwarding`, #2563): only the index
/// changes which module it forwards; the page that renders the tag is
/// untouched and draws it to the other module's component.
pub fn tsx_jsx_tag_forwarding_index_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file("src/components/Sidebar/index.ts", INDEX_FORWARDS_APP)
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import Sidebar from './Shared';\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e (upstream `sync-jsx-import-forwarding`): the index starts holding
/// the other module's component in a value (outline's `const Avatar =
/// AvatarWithHoverCard`).
pub fn tsx_jsx_tag_index_value_holds_another_component() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file("src/components/Sidebar/index.ts", INDEX_FORWARDS_APP)
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import Shared from './Shared';\n\nconst Sidebar = Shared;\n\nexport default Sidebar;\n",
        )
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e (upstream `sync-jsx-import-forwarding`): the module behind the
/// index wraps another component in its default export.
pub fn tsx_jsx_tag_module_behind_the_index_wraps_another() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/index.ts", INDEX_FORWARDS_APP)
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write("src/components/Sidebar/App.tsx", APP_SIDEBAR_WRAPS_COMPACT)
        .edge_after("function Home", "references", "function CompactSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e: two modules forward the component, and the middle one changes
/// which module it forwards; neither the page nor the index changes.
pub fn tsx_jsx_tag_middle_module_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file("src/components/Sidebar/mid.ts", INDEX_FORWARDS_APP)
        .file(
            "src/components/Sidebar/index.ts",
            "import Sidebar from './mid';\n\nexport default Sidebar;\n",
        )
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/mid.ts",
            "import Sidebar from './Shared';\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
}

/// W7-09e: the index comes to declare the component its default export
/// names, so the tag renders the index's own.
pub fn tsx_jsx_tag_index_declares_its_export() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/index.ts", INDEX_FORWARDS_APP)
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import Forwarded from './App';\n\nfunction Sidebar() {\n  return null;\n}\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e: the module the index forwards is deleted.
pub fn tsx_jsx_tag_forwarded_module_deleted() -> Case {
    Case::new(
        Change::DeleteImportedModule,
        &[Language::Tsx, Language::TypeScript],
    )
    .file("package.json", MOBX_REACT_PACKAGE)
    .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
    .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
    .file("src/components/Sidebar/mid.ts", INDEX_FORWARDS_APP)
    .file(
        "src/components/Sidebar/index.ts",
        "import Sidebar from './mid';\n\nexport default Sidebar;\n",
    )
    .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
    .remove("src/components/Sidebar/mid.ts")
    .edge_before("function Home", "references", "function AppSidebar")
    .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e (upstream `jsx-child-disambiguation`, mastodon's `blurhash.tsx`):
/// the component a memo wrapper hands over is renamed; the tag that imports
/// the wrapper under the old name renders the renamed one.
pub fn tsx_jsx_tag_memo_wrapped_component_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Tsx])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file(
            "app/components/blurhash.tsx",
            "import { memo } from 'react';\n\nfunction Blurhash({ hash }: { hash: string }) {\n  return <canvas data-hash={hash} />;\n}\n\nconst MemoizedBlurhash = memo(Blurhash);\n\nexport { MemoizedBlurhash as Blurhash };\n",
        )
        .file(
            "app/components/media_item.tsx",
            "import { Blurhash } from './blurhash';\n\nexport function MediaItem() {\n  return <div><Blurhash hash=\"x\" /></div>;\n}\n",
        )
        .write(
            "app/components/blurhash.tsx",
            "import { memo } from 'react';\n\nfunction BlurhashCanvas({ hash }: { hash: string }) {\n  return <canvas data-hash={hash} />;\n}\n\nconst MemoizedBlurhash = memo(BlurhashCanvas);\n\nexport { MemoizedBlurhash as Blurhash };\n",
        )
        .edge_before("function MediaItem", "references", "function Blurhash")
        .edge_after("function MediaItem", "references", "function BlurhashCanvas")
}

/// W7-09e: the value the index exports is a later declarator of its
/// statement (`const unused = 1, Sidebar = App`), and only the index changes
/// which module that value's import names.
pub fn tsx_jsx_tag_index_later_declarator_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file(
            "src/components/Sidebar/index.ts",
            "import App from './App';\n\nconst unused = 1, Sidebar = App;\n\nexport default Sidebar;\n",
        )
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import App from './Shared';\n\nconst unused = 1, Sidebar = App;\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e: as [`tsx_jsx_tag_index_later_declarator_re_points`], through an
/// export list and a named import. Import resolution binds no node to the
/// page's `{ Sidebar }`, so the #2450 lane re-resolves the page whatever the
/// surface says; the case holds the surface to it should that change.
pub fn tsx_jsx_tag_index_listed_later_declarator_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file(
            "src/components/Sidebar/index.ts",
            "import App from './App';\n\nconst unused = 1, Sidebar = App;\n\nexport { Sidebar };\n",
        )
        .file(
            "src/scenes/Home.tsx",
            "import { Sidebar } from '../components/Sidebar';\n\nexport function Home() {\n  return <main><Sidebar /></main>;\n}\n",
        )
        .write(
            "src/components/Sidebar/index.ts",
            "import App from './Shared';\n\nconst unused = 1, Sidebar = App;\n\nexport { Sidebar };\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e: the value the index exports carries a function type, whose `=>`
/// comes before the initializer's `=`, and only the index changes which
/// module that value's import names.
pub fn tsx_jsx_tag_index_annotated_value_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file(
            "src/components/Sidebar/index.ts",
            "import App from './App';\n\nconst Sidebar: (props: object) => JSX.Element = App;\n\nexport default Sidebar;\n",
        )
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import App from './Shared';\n\nconst Sidebar: (props: object) => JSX.Element = App;\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W7-09e: a function in the index declares a `const` of the exported
/// value's name before the module's own, and only the index changes which
/// module the module's own value's import names.
pub fn tsx_jsx_tag_index_shadowed_value_re_points() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Tsx, Language::TypeScript])
        .file("package.json", MOBX_REACT_PACKAGE)
        .file("src/components/Sidebar/Aside.tsx", ASIDE_SIDEBAR)
        .file("src/components/Sidebar/App.tsx", APP_SIDEBAR)
        .file("src/components/Sidebar/Shared.tsx", SHARED_SIDEBAR)
        .file(
            "src/components/Sidebar/index.ts",
            "import App from './App';\nimport Other from './Aside';\n\nexport function pick() {\n  const Sidebar = Other;\n  return Sidebar;\n}\n\nconst Sidebar = App;\n\nexport default Sidebar;\n",
        )
        .file("src/scenes/Home.tsx", HOME_RENDERS_SIDEBAR)
        .write(
            "src/components/Sidebar/index.ts",
            "import App from './Shared';\nimport Other from './Aside';\n\nexport function pick() {\n  const Sidebar = Other;\n  return Sidebar;\n}\n\nconst Sidebar = App;\n\nexport default Sidebar;\n",
        )
        .edge_before("function Home", "references", "function AppSidebar")
        .edge_after("function Home", "references", "function SharedSidebar")
        .no_edge_after("function Home", "references", "function AppSidebar")
}

/// W5-06, fail closed: only the manifest's `version` changes; every JS file
/// re-resolves and every edge stays.
pub fn typescript_package_version_bump_keeps_every_edge() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file("package.json", "{\"name\":\"web\",\"version\":\"1.0.0\"}\n")
        .file(
            "src/lib/math.ts",
            "export function double(value: number): number {\n  return value * 2;\n}\n",
        )
        .file(
            "src/main.ts",
            "import { double } from './lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
        )
        .write("package.json", "{\"name\":\"web\",\"version\":\"1.0.1\"}\n")
        .edge_throughout("function run", "calls", "function double")
}

// ----- Shopify theme markers (W4-LIQ-01: upstream #2393, #2396) ---------

/// A JSON template naming the section `main-product`.
const PRODUCT_TEMPLATE: &str =
    "{\"sections\":{\"main\":{\"type\":\"main-product\"}},\"order\":[\"main\"]}";
/// A JSON template naming the section `404`.
const NOT_FOUND_TEMPLATE: &str =
    "{\"sections\":{\"main\":{\"type\":\"404\"}},\"order\":[\"main\"]}";

/// Upstream #2393: an unchanged JSON template becomes Liquid once its folder
/// gains a theme's settings schema, which is no source file itself.
pub fn liquid_json_template_joins_a_theme_once_its_settings_schema_appears() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file(
            "sections/main-product.liquid",
            "<div>{{ product.title }}</div>\n",
        )
        .file("templates/product.json", PRODUCT_TEMPLATE)
        .write("config/settings_schema.json", "[]")
        .edge_after(
            "file:templates/product.json",
            "references",
            "file:sections/main-product.liquid",
        )
        .file_language_after("templates/product.json", "liquid")
}

/// Upstream #2393: unchanged JSON templates leave the index once their
/// folder loses the theme's layout.
pub fn liquid_json_templates_leave_once_the_theme_layout_goes() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file("layout/theme.liquid", "{{ content_for_layout }}\n")
        .file(
            "sections/main-product.liquid",
            "<div>{{ product.title }}</div>\n",
        )
        .file("templates/product.json", PRODUCT_TEMPLATE)
        .file("templates/page.json", PRODUCT_TEMPLATE)
        .remove("layout/theme.liquid")
        .edge_before(
            "file:templates/product.json",
            "references",
            "file:sections/main-product.liquid",
        )
        .edge_before(
            "file:templates/page.json",
            "references",
            "file:sections/main-product.liquid",
        )
}

/// Upstream #2393: theme folders are named in any case; an unchanged JSON
/// template in a theme's `Templates/` joins once the theme is marked.
pub fn liquid_json_template_in_a_mixed_case_folder_joins_a_marked_theme() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file(
            "Shop/sections/main-product.liquid",
            "<div>{{ product.title }}</div>\n",
        )
        .file("Shop/Templates/product.json", PRODUCT_TEMPLATE)
        .write("Shop/config/settings_schema.json", "[]")
        .edge_after(
            "file:Shop/Templates/product.json",
            "references",
            "file:Shop/sections/main-product.liquid",
        )
        .file_language_after("Shop/Templates/product.json", "liquid")
}

/// Upstream #2393: a marker counts though ignored, wherever it is; the
/// unchanged JSON template joins once an ignored settings schema appears.
pub fn liquid_json_template_joins_a_theme_through_an_ignored_settings_schema() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file(".gitignore", "config/\n")
        .file(
            "sections/main-product.liquid",
            "<div>{{ product.title }}</div>\n",
        )
        .file("templates/product.json", PRODUCT_TEMPLATE)
        .write("config/settings_schema.json", "[]")
        .edge_after(
            "file:templates/product.json",
            "references",
            "file:sections/main-product.liquid",
        )
        .file_language_after("templates/product.json", "liquid")
}

/// Upstream #2393: the unchanged JSON template leaves once an ignored
/// settings schema, its theme's only marker, is removed.
pub fn liquid_json_template_leaves_once_an_ignored_settings_schema_goes() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file(".gitignore", "config/\n")
        .file("config/settings_schema.json", "[]")
        .file(
            "sections/main-product.liquid",
            "<div>{{ product.title }}</div>\n",
        )
        .file("templates/product.json", PRODUCT_TEMPLATE)
        .remove("config/settings_schema.json")
        .edge_before(
            "file:templates/product.json",
            "references",
            "file:sections/main-product.liquid",
        )
}

/// Upstream #2396: an unchanged section's `render` stops reaching the root
/// theme's snippet once its folder is a theme of its own, which lacks it.
pub fn liquid_render_stays_in_a_folder_once_it_is_marked_a_theme() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file("layout/theme.liquid", "{{ content_for_layout }}\n")
        .file("snippets/price.liquid", "{{ product.price | money }}\n")
        .file("shop/sections/header.liquid", "{% render 'price' %}\n")
        .write("shop/config/settings_schema.json", "[]")
        .edge_before(
            "file:shop/sections/header.liquid",
            "references",
            "file:snippets/price.liquid",
        )
        .no_edge_after(
            "file:shop/sections/header.liquid",
            "references",
            "file:snippets/price.liquid",
        )
}

/// Upstream #2396: an unchanged section's `render` reaches the root theme's
/// snippet by its path once its folder is no theme any more.
pub fn liquid_render_leaves_a_folder_theme_once_its_marker_goes() -> Case {
    Case::new(Change::ControlFile, &[Language::Liquid])
        .file("layout/theme.liquid", "{{ content_for_layout }}\n")
        .file("snippets/price.liquid", "{{ product.price | money }}\n")
        .file("shop/config/settings_schema.json", "[]")
        .file("shop/sections/header.liquid", "{% render 'price' %}\n")
        .remove("shop/config/settings_schema.json")
        .edge_after(
            "file:shop/sections/header.liquid",
            "references",
            "file:snippets/price.liquid",
        )
}

/// Upstream #2396: a theme's JSON template whose own section is deleted
/// links no other theme's copy.
pub fn liquid_json_template_loses_its_own_themes_section() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Liquid])
        .file(
            "examples/tailwind/layout/theme.liquid",
            "{{ content_for_layout }}\n",
        )
        .file("examples/tailwind/sections/404.liquid", "<h1>404</h1>\n")
        .file("examples/tailwind/templates/404.json", NOT_FOUND_TEMPLATE)
        .file("straps/dusk/config/settings_schema.json", "[]")
        .file("straps/dusk/sections/404.liquid", "<h1>Not found</h1>\n")
        .remove("examples/tailwind/sections/404.liquid")
        .edge_before(
            "file:examples/tailwind/templates/404.json",
            "references",
            "file:examples/tailwind/sections/404.liquid",
        )
        .no_edge_after(
            "file:examples/tailwind/templates/404.json",
            "references",
            "file:straps/dusk/sections/404.liquid",
        )
}

/// Upstream #2396: a theme's JSON template links the section its own theme
/// gains, and no other theme's copy.
pub fn liquid_json_template_gains_its_own_themes_section() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Liquid])
        .file(
            "examples/tailwind/layout/theme.liquid",
            "{{ content_for_layout }}\n",
        )
        .file("examples/tailwind/templates/404.json", NOT_FOUND_TEMPLATE)
        .file("straps/dusk/config/settings_schema.json", "[]")
        .file("straps/dusk/sections/404.liquid", "<h1>Not found</h1>\n")
        .write("examples/tailwind/sections/404.liquid", "<h1>404</h1>\n")
        .edge_after(
            "file:examples/tailwind/templates/404.json",
            "references",
            "file:examples/tailwind/sections/404.liquid",
        )
        .no_edge_after(
            "file:examples/tailwind/templates/404.json",
            "references",
            "file:straps/dusk/sections/404.liquid",
        )
}

// ----- a `.h` header changes from C to C++ ----------------------------------

/// A unit calling `Helper` (W4-PAS-01).
const PASCAL_VIEW: &str = "unit View;\ninterface\nfunction Render: Integer;\nimplementation\nfunction Render: Integer;\nbegin\n  Result := Helper(10);\nend;\nend.\n";
/// An include declaring `Helper`, of Pascal shapes.
const PASCAL_HELPER_INCLUDE: &str =
    "function Helper(Amount: Integer): Integer;\nbegin\n  Result := Amount;\nend;\n";

/// Upstream #2289: an `.inc` include whose content turns from PHP to Pascal
/// is read as Pascal, and the routine it now declares is called.
pub fn pascal_include_flips_from_php_and_its_routine_is_called() -> Case {
    Case::new(Change::HeaderFlip, &[Language::Pascal, Language::Php])
        .file("View.pas", PASCAL_VIEW)
        .file("routines.inc", "<?php\necho 1;\n")
        .write("routines.inc", PASCAL_HELPER_INCLUDE)
        .edge_after("file:View.pas", "calls", "function Helper")
        .file_language_after("routines.inc", "pascal")
}

/// Upstream #2289: an `.inc` include that opens a PHP tag is PHP again, and
/// the routine it declared leaves.
pub fn pascal_include_flips_to_php_and_its_routine_leaves() -> Case {
    Case::new(Change::HeaderFlip, &[Language::Pascal, Language::Php])
        .file("View.pas", PASCAL_VIEW)
        .file("routines.inc", PASCAL_HELPER_INCLUDE)
        .write("routines.inc", "<?php\necho 1;\n")
        .edge_before("file:View.pas", "calls", "function Helper")
        .file_language_after("routines.inc", "php")
}

pub fn c_header_flips_to_cpp() -> Case {
    Case::new(Change::HeaderFlip, &[Language::C, Language::Cpp])
        .file(
            "shape.h",
            "struct shape {\n    int sides;\n};\n\nint shape_sides(const struct shape *s);\n",
        )
        .file(
            "shape.c",
            "#include \"shape.h\"\n\nint shape_sides(const struct shape *s) {\n    return s->sides;\n}\n",
        )
        .file(
            "main.c",
            "#include \"shape.h\"\n\nint main(void) {\n    struct shape square = {4};\n    return shape_sides(&square);\n}\n",
        )
        .write(
            "shape.h",
            "class Shape {\npublic:\n    int sides() const { return 4; }\n};\n\nint shape_sides(const Shape *s);\n",
        )
        .edge_throughout("file:main.c", "imports", "file:shape.h")
        .edge_throughout("function main", "calls", "function shape_sides @shape.c")
        .edge_after("file:shape.h", "contains", "class Shape")
        .file_language_after("shape.h", "cpp")
}

// ----- a `.js` file gains Flow's `@flow` pragma ------------------------------

/// Upstream #2161: a `.js` file that gains Flow's `@flow` pragma is read as
/// TSX, so the class its annotations cut short (the JavaScript grammar ends
/// it at `React.Component<Props>`) has its methods, as a fresh index of the
/// edit finds.
pub fn javascript_file_gains_the_flow_pragma() -> Case {
    Case::new(Change::PragmaFlip, &[Language::JavaScript, Language::Tsx])
        .file(
            "js/SegmentedControl.js",
            "class SegmentedControl extends React.Component<Props> {\n  render(): React.Node {\n    return this._renderItems();\n  }\n\n  _renderItems(): React.Node {\n    return null;\n  }\n}\n\nmodule.exports = SegmentedControl;\n",
        )
        .file(
            "js/index.js",
            "const SegmentedControl = require('./SegmentedControl');\nmodule.exports = SegmentedControl;\n",
        )
        .write(
            "js/SegmentedControl.js",
            "// @flow\nclass SegmentedControl extends React.Component<Props> {\n  render(): React.Node {\n    return this._renderItems();\n  }\n\n  _renderItems(): React.Node {\n    return null;\n  }\n}\n\nmodule.exports = SegmentedControl;\n",
        )
        .edge_throughout("file:js/index.js", "imports", "file:js/SegmentedControl.js")
        .edge_after(
            "method SegmentedControl::render",
            "calls",
            "method SegmentedControl::_renderItems",
        )
        .file_language_after("js/SegmentedControl.js", "tsx")
}

// ----- Dart libraries (W4-DART-02: upstream #2386, #2388, #2391) ----------

/// A barrel's `show` gains a name: an unchanged importer's bare call of it
/// reaches the declaration, though no node of that name changed.
pub fn dart_export_show_gains_a_name_an_unchanged_importer_calls() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/src/impl.dart",
            "int shown() => 1;\nint later() => 2;\n",
        )
        .file("lib/kit.dart", "export 'src/impl.dart' show shown;\n")
        .file(
            "lib/main.dart",
            "import 'kit.dart';\n\nint run() => later();\n",
        )
        .write(
            "lib/kit.dart",
            "export 'src/impl.dart' show shown, later;\n",
        )
        .edge_after("function run", "calls", "function later")
}

/// The reverse: a barrel comes to `hide` a name, so the unchanged
/// importer's call leaves it.
pub fn dart_export_hide_drops_a_name_an_unchanged_importer_calls() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/src/impl.dart",
            "int shown() => 1;\nint later() => 2;\n",
        )
        .file("lib/kit.dart", "export 'src/impl.dart';\n")
        .file(
            "lib/main.dart",
            "import 'kit.dart';\n\nint run() => later();\n",
        )
        .write("lib/kit.dart", "export 'src/impl.dart' hide later;\n")
        .edge_before("function run", "calls", "function later")
        .no_edge_after("function run", "calls", "function later")
}

/// A library comes to list a part that declares a name: an unchanged part's
/// bare call moves from the imported namesake to the library's own.
pub fn dart_part_joins_its_library_and_shadows_an_imported_name() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/a_util.dart", "int helper() => 1;\n")
        .file(
            "lib/feature.dart",
            "import 'a_util.dart';\n\npart 'other.dart';\n",
        )
        .file(
            "lib/other.dart",
            "part of 'feature.dart';\n\nint run() => helper();\n",
        )
        .file(
            "lib/extra.dart",
            "part of 'feature.dart';\n\nint helper() => 2;\n",
        )
        .write(
            "lib/feature.dart",
            "import 'a_util.dart';\n\npart 'other.dart';\npart 'extra.dart';\n",
        )
        .edge_before("function run", "calls", "function helper @lib/a_util.dart")
        .edge_after("function run", "calls", "function helper @lib/extra.dart")
}

/// A library gains an import: an unchanged part's bare call reaches what it
/// brings in, though no node of that name changed.
pub fn dart_library_import_reaches_its_unchanged_part() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/util.dart", "int helper() => 1;\n")
        .file("lib/lib.dart", "part 'piece.dart';\n")
        .file(
            "lib/piece.dart",
            "part of 'lib.dart';\n\nint run() => helper();\n",
        )
        .write(
            "lib/lib.dart",
            "import 'util.dart';\n\npart 'piece.dart';\n",
        )
        .edge_after("function run", "calls", "function helper")
}

/// The reverse: the library drops the import, so the part's call leaves it.
pub fn dart_library_import_leaves_its_unchanged_part() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/util.dart", "int helper() => 1;\n")
        .file(
            "lib/lib.dart",
            "import 'util.dart';\n\npart 'piece.dart';\n",
        )
        .file(
            "lib/piece.dart",
            "part of 'lib.dart';\n\nint run() => helper();\n",
        )
        .write("lib/lib.dart", "part 'piece.dart';\n")
        .edge_before("function run", "calls", "function helper")
        .no_edge_after("function run", "calls", "function helper")
}

/// A library's two import prefixes swap: an unchanged part's `p.Report`
/// moves to the library the prefix now names.
pub fn dart_import_prefix_swap_moves_an_unchanged_parts_type() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/a.dart", "class Report {}\n")
        .file("lib/b.dart", "class Report {}\n")
        .file(
            "lib/lib.dart",
            "import 'a.dart' as p;\nimport 'b.dart' as q;\n\npart 'piece.dart';\n",
        )
        .file(
            "lib/piece.dart",
            "part of 'lib.dart';\n\nvoid run(p.Report r) {}\n",
        )
        .write(
            "lib/lib.dart",
            "import 'a.dart' as q;\nimport 'b.dart' as p;\n\npart 'piece.dart';\n",
        )
        .edge_before("function run", "references", "class Report @lib/a.dart")
        .edge_after("function run", "references", "class Report @lib/b.dart")
}

/// A library takes the name an unchanged part's `part of` gives: the part
/// joins it, so its bare call, which saw every file while its library was
/// unknown, reaches only what the library imports.
pub fn dart_library_name_brings_in_an_unchanged_part() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/util.dart", "int helper() => 1;\n")
        .file("lib/aaa.dart", "int helper() => 2;\n")
        .file(
            "lib/models.dart",
            "library app.old;\n\nimport 'util.dart';\n\npart 'user.dart';\n",
        )
        .file(
            "lib/user.dart",
            "part of app.models;\n\nint run() => helper();\n",
        )
        .write(
            "lib/models.dart",
            "library app.models;\n\nimport 'util.dart';\n\npart 'user.dart';\n",
        )
        .edge_before("function run", "calls", "function helper @lib/aaa.dart")
        .edge_after("function run", "calls", "function helper @lib/util.dart")
}

/// The library an unchanged part names by URI appears: the part takes its
/// imports, so its bare call reaches the one name they bring in.
pub fn dart_library_of_an_unchanged_part_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/util.dart", "int helper() => 1;\n")
        .file("lib/other.dart", "int helper() => 2;\n")
        .file(
            "lib/user.dart",
            "part of 'models.dart';\n\nint run() => helper();\n",
        )
        .write(
            "lib/models.dart",
            "import 'util.dart';\n\npart 'user.dart';\n",
        )
        .edge_after("function run", "calls", "function helper @lib/util.dart")
}

/// A second package of a name gains its first Dart file: an unchanged
/// file outside both can no longer tell which `package:example/x.dart` it
/// imports.
pub fn dart_same_named_package_gains_its_first_dart_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("packages/a/pubspec.yaml", "name: example\n")
        .file("packages/a/lib/x.dart", "int fromA() => 1;\n")
        .file("packages/b/pubspec.yaml", "name: example\n")
        .file("tool/pubspec.yaml", "name: tool\n")
        .file(
            "tool/bin/run.dart",
            "import 'package:example/x.dart';\n\nint run() => fromA();\n",
        )
        .write("packages/b/lib/y.dart", "int fromB() => 2;\n")
        .edge_before(
            "file:tool/bin/run.dart",
            "imports",
            "file:packages/a/lib/x.dart",
        )
        .no_edge_after(
            "file:tool/bin/run.dart",
            "imports",
            "file:packages/a/lib/x.dart",
        )
}

/// A part that declares a name is deleted: an unchanged part's bare call
/// falls back to the imported namesake.
pub fn dart_delete_a_part_that_shadowed_an_imported_name() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/a_util.dart", "int helper() => 1;\n")
        .file(
            "lib/feature.dart",
            "import 'a_util.dart';\n\npart 'other.dart';\npart 'extra.dart';\n",
        )
        .file(
            "lib/other.dart",
            "part of 'feature.dart';\n\nint run() => helper();\n",
        )
        .file(
            "lib/extra.dart",
            "part of 'feature.dart';\n\nint helper() => 2;\n",
        )
        .remove("lib/extra.dart")
        .edge_before("function run", "calls", "function helper @lib/extra.dart")
        .edge_after("function run", "calls", "function helper @lib/a_util.dart")
}

/// A library comes to list the part an unnamed extension sits in: an
/// unchanged part's bare call reaches the extension's member, which applies
/// in every file of its library and in no other.
pub fn dart_unnamed_extension_reaches_a_part_once_its_file_joins_the_library() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shapes.dart", "part 'use.dart';\n\nclass Shape {}\n")
        .file(
            "lib/area_ext.dart",
            "part of 'shapes.dart';\n\nextension on Shape {\n  int area() => 1;\n}\n",
        )
        .file(
            "lib/use.dart",
            "part of 'shapes.dart';\n\nclass Square extends Shape {\n  int twice() => area();\n}\n",
        )
        .write(
            "lib/shapes.dart",
            "part 'use.dart';\npart 'area_ext.dart';\n\nclass Shape {}\n",
        )
        .edge_after("method Square::twice", "calls", "method <anonymous>::area")
}

/// A package's pubspec.yaml renames it: an unchanged file's
/// `package:kit/…` import no longer names its file.
pub fn dart_pubspec_rename_strands_a_package_import() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file("packages/kit/lib/kit.dart", "int shown() => 1;\n")
        .file(
            "lib/main.dart",
            "import 'package:kit/kit.dart';\n\nint run() => shown();\n",
        )
        .write("packages/kit/pubspec.yaml", "name: kit2\n")
        .edge_before(
            "file:lib/main.dart",
            "imports",
            "file:packages/kit/lib/kit.dart",
        )
        .no_edge_after(
            "file:lib/main.dart",
            "imports",
            "file:packages/kit/lib/kit.dart",
        )
}

/// An app's pubspec.yaml comes to depend on one of two same-named packages
/// by `path:`: its unchanged import now names that package's file.
pub fn dart_pubspec_path_dependency_picks_a_same_named_package() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file("examples/login/pubspec.yaml", "name: login\n")
        .file("examples/login/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/login/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file("examples/other/pubspec.yaml", "name: other\n")
        .file("examples/other/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/other/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file(
            "examples/login/lib/app.dart",
            "import 'package:auth/auth.dart';\n\nclass App {}\n",
        )
        .write(
            "examples/login/pubspec.yaml",
            "name: login\ndependencies:\n  auth:\n    path: packages/auth\n",
        )
        .edge_after(
            "file:examples/login/lib/app.dart",
            "imports",
            "file:examples/login/packages/auth/lib/auth.dart",
        )
}

/// W5-06: an app's `pubspec_overrides.yaml` comes to name one of two
/// same-named packages by `path:`: its unchanged import now names that
/// package's file.
pub fn dart_pubspec_overrides_path_dependency_picks_a_same_named_package() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file("examples/login/pubspec.yaml", "name: login\n")
        .file("examples/login/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/login/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file("examples/other/pubspec.yaml", "name: other\n")
        .file("examples/other/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/other/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file(
            "examples/login/lib/app.dart",
            "import 'package:auth/auth.dart';\n\nclass App {}\n",
        )
        .write(
            "examples/login/pubspec_overrides.yaml",
            "dependency_overrides:\n  auth:\n    path: packages/auth\n",
        )
        .edge_after(
            "file:examples/login/lib/app.dart",
            "imports",
            "file:examples/login/packages/auth/lib/auth.dart",
        )
}

/// W5-06: the same through a `pubspec_overrides.yaml` the repository
/// ignores, as melos writes it: the manifest is no indexed file, so only the
/// pubspec lane hears of it, and its `path:` still picks the package.
pub fn dart_ignored_pubspec_overrides_path_dependency_picks_a_same_named_package() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file(".gitignore", "pubspec_overrides.yaml\n")
        .file("examples/login/pubspec.yaml", "name: login\n")
        .file("examples/login/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/login/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file("examples/other/pubspec.yaml", "name: other\n")
        .file("examples/other/packages/auth/pubspec.yaml", "name: auth\n")
        .file(
            "examples/other/packages/auth/lib/auth.dart",
            "class Auth {}\n",
        )
        .file(
            "examples/login/lib/app.dart",
            "import 'package:auth/auth.dart';\n\nclass App {}\n",
        )
        .write(
            "examples/login/pubspec_overrides.yaml",
            "dependency_overrides:\n  auth:\n    path: packages/auth\n",
        )
        .edge_after(
            "file:examples/login/lib/app.dart",
            "imports",
            "file:examples/login/packages/auth/lib/auth.dart",
        )
}

/// W5-06: a `pubspec.yaml` appearing makes its folder the package it names:
/// an unchanged file's `package:kit/…` import comes to name its file. The
/// manifest is an indexed YAML file in the folder `kit`, so the parked
/// import is also retried under that key (W5-01) and this case converges
/// without the per-state manifests; the rename and deletion cases need them.
pub fn dart_pubspec_appears_and_its_folder_becomes_a_package() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/lib/kit.dart", "int shown() => 1;\n")
        .file(
            "lib/main.dart",
            "import 'package:kit/kit.dart';\n\nint run() => shown();\n",
        )
        .write("packages/kit/pubspec.yaml", "name: kit\n")
        .edge_after(
            "file:lib/main.dart",
            "imports",
            "file:packages/kit/lib/kit.dart",
        )
}

/// W5-06: a package's `pubspec.yaml` deleted: its folder is no package, so
/// an unchanged file's `package:kit/…` import no longer names its file.
pub fn dart_pubspec_deleted_strands_a_package_import() -> Case {
    Case::new(Change::ControlFile, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file("packages/kit/lib/kit.dart", "int shown() => 1;\n")
        .file(
            "lib/main.dart",
            "import 'package:kit/kit.dart';\n\nint run() => shown();\n",
        )
        .remove("packages/kit/pubspec.yaml")
        .edge_before(
            "file:lib/main.dart",
            "imports",
            "file:packages/kit/lib/kit.dart",
        )
}

/// 501 parked imports of `main.dart` files the project lacks: more than the
/// shared import-key lookup wakes for one key.
static MAIN_IMPORTS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::new();
    for index in 0..501 {
        text.push_str(&format!("import 'gen{index}/main.dart';\n"));
    }
    text.push_str("\nvoid noise() {}\n");
    text
});

/// A Dart import links the file it names once it appears, however many
/// parked imports share the file's name. No pubspec.yaml: the files' scopes
/// are unknown, so the library models of the sync's two states see the same
/// for them, and only the uncapped retry of the parked imports links them.
pub fn dart_import_of_an_added_library_past_the_import_key_ceiling() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("lib/noise.dart", MAIN_IMPORTS.as_str())
        .file(
            "lib/app.dart",
            "import 'gen7/main.dart';\n\nvoid run() {}\n",
        )
        .write("lib/gen7/main.dart", "class Main {}\n")
        .edge_after("file:lib/app.dart", "imports", "file:lib/gen7/main.dart")
        .edge_after("file:lib/noise.dart", "imports", "file:lib/gen7/main.dart")
}

/// Two barrels trade a name in one edit: one stops exporting it as the
/// other starts, so its importers' calls move both ways at once (each
/// library's namespace is compared on its own).
pub fn dart_two_barrels_trade_an_exported_name() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/src/x.dart", "int foo() => 1;\nint bar() => 2;\n")
        .file("lib/a.dart", "export 'src/x.dart' show foo;\n")
        .file("lib/b.dart", "export 'src/x.dart' hide foo;\n")
        .file(
            "lib/use_a.dart",
            "import 'a.dart';\n\nint runA() => foo();\n",
        )
        .file(
            "lib/use_b.dart",
            "import 'b.dart';\n\nint runB() => foo();\n",
        )
        .write("lib/a.dart", "export 'src/x.dart' hide foo;\n")
        .write("lib/b.dart", "export 'src/x.dart' show foo;\n")
        .edge_before("function runA", "calls", "function foo")
        .no_edge_after("function runA", "calls", "function foo")
        .edge_after("function runB", "calls", "function foo")
}

/// A library's directives move down a line: their import nodes move, and
/// nothing another file resolves changes (no resolution reads a Dart import
/// node, so a moved one touches no name).
pub fn dart_library_directives_move_down_a_line() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/util.dart", "int helper() => 1;\n")
        .file(
            "lib/lib.dart",
            "import 'util.dart';\n\npart 'piece.dart';\n",
        )
        .file(
            "lib/piece.dart",
            "part of 'lib.dart';\n\nint run() => helper();\n",
        )
        .file(
            "lib/main.dart",
            "import 'lib.dart';\nimport 'util.dart';\n\nint go() => helper();\n",
        )
        .write(
            "lib/lib.dart",
            "// The library.\nimport 'util.dart';\n\npart 'piece.dart';\n",
        )
        .edge_throughout("function run", "calls", "function helper")
        .edge_throughout("file:lib/main.dart", "imports", "file:lib/lib.dart")
}

/// A barrel comes to hide a base type: a subclass in another library loses
/// it, so a call on the subclass in a third file no longer reaches the
/// base's method through it (bloc's `export 'src/cubit.dart' hide Cubit`
/// moved `cubit.close()` in its tests).
pub fn dart_hidden_base_type_leaves_a_call_on_its_subclass() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/src/base.dart", "class Base {\n  void close() {}\n}\n")
        .file(
            "lib/src/cubit.dart",
            "import 'base.dart';\n\nabstract class Cubit extends Base {}\n",
        )
        .file("lib/kit.dart", "export 'src/base.dart';\nexport 'src/cubit.dart';\n")
        .file(
            "test/seeded.dart",
            "import 'package:app/kit.dart';\n\nclass Seeded extends Cubit {}\n",
        )
        .file("test/other.dart", "class Other {\n  void close() {}\n}\n")
        .file(
            "test/use.dart",
            "import 'seeded.dart';\n\nvoid run() {\n  final cubit = Seeded();\n  cubit.close();\n}\n",
        )
        .write(
            "lib/kit.dart",
            "export 'src/base.dart';\nexport 'src/cubit.dart' hide Cubit;\n",
        )
        .edge_before("function run", "calls", "method Base::close")
}

/// 501 parked `x.close()` calls: more than the shared tail retry wakes for
/// one tail.
static CLOSE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("void noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("  missing{index}.close();\n"));
    }
    text.push_str("}\n");
    text
});

/// The reverse, past the tail ceiling: the barrel stops hiding the base
/// type, so the call on the subclass reaches the base's method through it,
/// however many parked calls share the method's name.
pub fn dart_shown_base_type_brings_its_method_to_a_call_on_its_subclass_past_the_tail_ceiling()
-> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/src/base.dart", "class Base {\n  void close() {}\n}\n")
        .file(
            "lib/src/cubit.dart",
            "import 'base.dart';\n\nabstract class Cubit extends Base {}\n",
        )
        .file(
            "lib/kit.dart",
            "export 'src/base.dart';\nexport 'src/cubit.dart' hide Cubit;\n",
        )
        .file(
            "test/seeded.dart",
            "import 'package:app/kit.dart';\n\nclass Seeded extends Cubit {}\n",
        )
        .file(
            "test/use.dart",
            "import 'seeded.dart';\n\nvoid run() {\n  final cubit = Seeded();\n  cubit.close();\n}\n",
        )
        .file("test/noise.dart", CLOSE_CALLS.as_str())
        .write("lib/kit.dart", "export 'src/base.dart';\nexport 'src/cubit.dart';\n")
        .edge_after("function run", "calls", "method Base::close")
}

/// The library a subclass sits in comes to import its base type: the
/// subclass gains the base's members, so a call on it in another file
/// reaches the base's method (the part re-resolves whole; the call's file is
/// unchanged).
pub fn dart_library_import_brings_a_base_method_to_a_call_on_its_parts_subclass() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {\n  void close() {}\n}\n")
        .file("lib/models.dart", "part 'seeded.dart';\n")
        .file(
            "lib/seeded.dart",
            "part of 'models.dart';\n\nclass Seeded extends Base {}\n",
        )
        .file(
            "lib/use.dart",
            "import 'models.dart';\n\nvoid run() {\n  final cubit = Seeded();\n  cubit.close();\n}\n",
        )
        .write("lib/models.dart", "import 'base.dart';\n\npart 'seeded.dart';\n")
        .edge_after("function run", "calls", "method Base::close")
}

// ----- W4-DART-03: getter reads, extension members, static constants -------

/// 501 parked reads of `area` through receivers nothing types, so the shared
/// tail retry skips that tail.
static AREA_READS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("int noise() {\n  var total = 0;\n");
    for index in 0..501 {
        text.push_str(&format!("  total += missing{index}.area;\n"));
    }
    text.push_str("  return total;\n}\n");
    text
});

/// Upstream #2338 (W4-DART-03): a getter read links the getter its
/// receiver's type gains in another file; a field read before, it linked
/// nothing.
pub fn dart_getter_read_reaches_a_getter_its_type_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/box.dart", "class Box {\n  final int size = 1;\n}\n")
        .file(
            "lib/use.dart",
            "import 'box.dart';\n\nint read(Box x) => x.area;\n",
        )
        .write(
            "lib/box.dart",
            "class Box {\n  final int size = 1;\n  int get area => size * size;\n}\n",
        )
        .edge_after("function read", "calls", "method Box::area")
}

/// The same past the tail ceiling: 501 parked reads share the tail `area`.
pub fn dart_getter_read_reaches_a_getter_its_type_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/box.dart", "class Box {\n  final int size = 1;\n}\n")
        .file(
            "lib/use.dart",
            "import 'box.dart';\n\nint read(Box x) => x.area;\n",
        )
        .file("noise/noise.dart", AREA_READS.as_str())
        .write(
            "lib/box.dart",
            "class Box {\n  final int size = 1;\n  int get area => size * size;\n}\n",
        )
        .edge_after("function read", "calls", "method Box::area")
}

/// The reverse: the getter moves to another type, and the read, through a
/// receiver of the first, links nothing: never a getter found by its name.
pub fn dart_getter_read_leaves_a_getter_its_type_drops() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/box.dart",
            "class Box {\n  final int size = 1;\n  int get area => size * size;\n}\n\nclass Other {}\n",
        )
        .file("lib/use.dart", "import 'box.dart';\n\nint read(Box x) => x.area;\n")
        .write(
            "lib/box.dart",
            "class Box {\n  final int size = 1;\n}\n\nclass Other {\n  int get area => 1;\n}\n",
        )
        .edge_before("function read", "calls", "method Box::area")
        .no_edge_after("function read", "calls", "method Other::area")
}

/// A field of the base class, declared in another file, types the read in
/// the subclass (upstream #2171's walk through the hierarchy): retyped there,
/// the read moves, though nothing in the subclass's file changed.
pub fn dart_getter_read_through_an_inherited_field_follows_its_retyping() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/shapes.dart",
            "class Box {\n  int get area => 1;\n}\n\nclass Panel {\n  int get area => 2;\n}\n",
        )
        .file(
            "lib/base.dart",
            "import 'shapes.dart';\n\nclass Base {\n  Base(this.shape);\n  final Box shape;\n}\n",
        )
        .file(
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Sub(super.shape);\n\n  int measure() => shape.area;\n}\n",
        )
        .write(
            "lib/base.dart",
            "import 'shapes.dart';\n\nclass Base {\n  Base(this.shape);\n  final Panel shape;\n}\n",
        )
        .edge_before("method Sub::measure", "calls", "method Box::area")
        .edge_after("method Sub::measure", "calls", "method Panel::area")
}

/// The receiver's type, declared in another file, comes to extend another
/// base: the read moves to that base's getter.
pub fn dart_getter_read_follows_its_receiver_types_new_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/shapes.dart",
            "class Box {\n  int get area => 1;\n}\n\nclass Panel {\n  int get area => 2;\n}\n",
        )
        .file(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Box {}\n",
        )
        .file(
            "lib/use.dart",
            "import 'square.dart';\n\nint read(Square s) => s.area;\n",
        )
        .write(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Panel {}\n",
        )
        .edge_before("function read", "calls", "method Box::area")
        .edge_after("function read", "calls", "method Panel::area")
}

/// 501 parked calls of `shout` through receivers nothing types.
static SHOUT_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("void noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("  missing{index}.shout();\n"));
    }
    text.push_str("}\n");
    text
});

/// Upstream #2338: an extension on an enum, in a file added later, brings
/// the member a call on a receiver of the enum's type makes.
pub fn dart_call_on_an_enum_reaches_a_member_an_added_extension_brings() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shape.dart", "enum Shape { circle }\n")
        .file(
            "lib/use.dart",
            "import 'shape.dart';\n\nString loud(Shape s) => s.shout();\n",
        )
        .write(
            "lib/info.dart",
            "import 'shape.dart';\n\nextension ShapeInfo on Shape {\n  String shout() => name;\n}\n",
        )
        .edge_after("function loud", "calls", "method ShapeInfo::shout")
}

/// A class declares a named constructor `Foo.value`; an extension in a file
/// added later brings an instance `value()`. A call on a parameter of the
/// type and the last link of a chain typed to it reach the extension's member,
/// never the constructor (KEEP-RUST, W4d end).
pub fn dart_value_call_named_like_a_constructor_reaches_an_added_extension_member() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/foo.dart",
            "class Foo {\n  const Foo.value(this.v);\n\n  final int v;\n}\n\nFoo build() => const Foo.value(1);\n",
        )
        .file(
            "lib/use.dart",
            "import 'foo.dart';\n\nint viaParameter(Foo x) => x.value();\n\nint viaChain() => build().value();\n",
        )
        .write(
            "lib/ext.dart",
            "import 'foo.dart';\n\nextension FooX on Foo {\n  int value() => v;\n}\n",
        )
        .edge_after("function viaParameter", "calls", "method FooX::value")
        .edge_after("function viaChain", "calls", "method FooX::value")
}

/// The extension that brought `value()` to a class with a named constructor
/// `Foo.value` goes away: calls through values of the type, receivers named
/// `foo` and `fooValue` included, link nothing rather than fall back to the
/// constructor (KEEP-RUST, W4d end).
pub fn dart_value_call_named_like_a_constructor_leaves_a_removed_extension_member() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/foo.dart",
            "class Foo {\n  const Foo.value(this.v);\n\n  final int v;\n}\n",
        )
        .file(
            "lib/ext.dart",
            "import 'foo.dart';\n\nextension FooX on Foo {\n  int value() => v;\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'foo.dart';\nimport 'ext.dart';\n\nint viaNamed(Foo foo) => foo.value();\n\nint viaWord(Foo fooValue) => fooValue.value();\n",
        )
        .remove("lib/ext.dart")
        .edge_before("function viaNamed", "calls", "method FooX::value")
        .edge_before("function viaWord", "calls", "method FooX::value")
        .no_edge_after("function viaNamed", "calls", "method Foo::value")
        .no_edge_after("function viaWord", "calls", "method Foo::value")
}

/// The same past the tail ceiling: 501 parked calls share the tail `shout`.
pub fn dart_call_on_an_enum_reaches_a_member_an_added_extension_brings_past_the_tail_ceiling()
-> Case {
    Case::new(Change::AddCompetitor, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shape.dart", "enum Shape { circle }\n")
        .file(
            "lib/use.dart",
            "import 'shape.dart';\n\nString loud(Shape s) => s.shout();\n",
        )
        .file("noise/noise.dart", SHOUT_CALLS.as_str())
        .write(
            "lib/info.dart",
            "import 'shape.dart';\n\nextension ShapeInfo on Shape {\n  String shout() => name;\n}\n",
        )
        .edge_after("function loud", "calls", "method ShapeInfo::shout")
}

/// A getter an extension in a file added later brings to the receiver's
/// type, past the tail ceiling.
pub fn dart_getter_read_reaches_a_getter_an_added_extension_brings_past_the_tail_ceiling() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/box.dart", "class Box {\n  final int size = 1;\n}\n")
        .file(
            "lib/use.dart",
            "import 'box.dart';\n\nint read(Box x) => x.area;\n",
        )
        .file("noise/noise.dart", AREA_READS.as_str())
        .write(
            "lib/box_x.dart",
            "import 'box.dart';\n\nextension BoxX on Box {\n  int get area => size * size;\n}\n",
        )
        .edge_after("function read", "calls", "method BoxX::area")
}

/// Upstream #2367: a call through a type's name links the static constant the
/// type gains, not another type's same-named method.
pub fn dart_call_through_a_type_reaches_the_static_constant_it_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/builders.dart",
            "class Builder {\n  const Builder();\n  Object call(Object create) => create;\n}\n\nclass Other {\n  Object autoDispose(Object create) => create;\n}\n",
        )
        .file("lib/provider.dart", "import 'builders.dart';\n\nclass Provider {}\n")
        .file(
            "lib/use.dart",
            "import 'provider.dart';\n\nfinal p = Provider.autoDispose(1);\n",
        )
        .write(
            "lib/provider.dart",
            "import 'builders.dart';\n\nclass Provider {\n  static const autoDispose = Builder();\n}\n",
        )
        .edge_after("constant p", "calls", "constant Provider::autoDispose")
}

/// A namesake appears for a call a parameter covers (upstream #2379): the
/// parked call stays unlinked, while the same call outside the parameter's
/// function reaches it.
pub fn dart_parameter_call_links_nothing_when_a_namesake_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/run.dart",
            "import 'create.dart';\n\nvoid run(void Function() create) {\n  create();\n}\n\nvoid later() {\n  create();\n}\n",
        )
        .write("lib/create.dart", "void create() {}\n")
        .edge_after("function later", "calls", "function create")
        .no_edge_after("function run", "calls", "function create")
}

/// The same for a bloc handler's `emit` parameter, the method it would
/// otherwise reach arriving in its base class's file.
pub fn dart_handler_parameter_call_links_nothing_when_its_base_gains_the_method() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/bloc.dart", "class Bloc<S> {\n  void on(void Function(Object event, Object emit) handler) {}\n}\n")
        .file(
            "lib/counter.dart",
            "import 'bloc.dart';\n\nclass CounterBloc extends Bloc<int> {\n  void onReset(String event, Object emit) {\n    emit(0);\n  }\n\n  void bump() => emit(2);\n}\n",
        )
        .write(
            "lib/bloc.dart",
            "class Bloc<S> {\n  void on(void Function(Object event, Object emit) handler) {}\n\n  void emit(S state) {}\n}\n",
        )
        .edge_after("method CounterBloc::bump", "calls", "method Bloc::emit")
        .no_edge_after("method CounterBloc::onReset", "calls", "method Bloc::emit")
}

/// A local function is nearer than a namesake an imported file comes to
/// declare, and the parameter it shadows outside its block covers the call
/// there (upstream #2379; W4-DART-03c, a W4-DART-01 regression).
pub fn dart_local_function_call_keeps_its_local_when_a_namesake_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/wire.dart",
            "import 'make.dart';\n\nint outer(int Function() make) {\n  int helper() {\n    int make() => 2;\n    return make();\n  }\n\n  return helper() + make();\n}\n",
        )
        .write("lib/make.dart", "int make() => 1;\n")
        .edge_throughout("function outer::helper", "calls", "function outer::make")
        .edge_throughout("function outer", "calls", "function outer::helper")
        .no_edge_after("function outer", "calls", "function make")
}

/// A type gains the named `const` constructor a call through it names
/// (upstream #2380): the call reaches it, never another type's member of
/// that name.
pub fn dart_call_through_a_type_reaches_a_const_constructor_it_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/provider.dart", "class BlocProvider {\n  const BlocProvider();\n}\n")
        .file("lib/other.dart", "class RepositoryProvider {\n  RepositoryProvider.value();\n}\n")
        .file(
            "lib/use.dart",
            "import 'provider.dart';\nimport 'other.dart';\n\nfinal p = BlocProvider.value();\nfinal q = BlocProvider<int>.value();\n",
        )
        .write(
            "lib/provider.dart",
            "class BlocProvider {\n  const BlocProvider();\n\n  const BlocProvider.value();\n}\n",
        )
        .edge_after("constant p", "calls", "method BlocProvider::value")
        .edge_after("constant q", "calls", "method BlocProvider::value")
        .no_edge_after("constant p", "calls", "method RepositoryProvider::value")
}

/// A static method turns into a named constructor on the same line, so its
/// node keeps its id: a bare call in a subclass leaves it, as a constructor
/// is reached only through its class (upstream #2380).
pub fn dart_bare_call_leaves_a_method_that_becomes_a_constructor() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/base.dart",
            "class Base {\n  static Base value() => Base();\n}\n",
        )
        .file(
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Object m() => value();\n}\n",
        )
        .write("lib/base.dart", "class Base {\n  const Base.value();\n}\n")
        .edge_before("method Sub::m", "calls", "method Base::value")
        .no_edge_after("method Sub::m", "calls", "method Base::value")
}

/// The reverse: a named constructor turns into a static method on its line,
/// and the bare call in the subclass reaches it.
pub fn dart_bare_call_reaches_a_constructor_that_becomes_a_method() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/base.dart", "class Base {\n  const Base.value();\n}\n")
        .file(
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Object m() => value();\n}\n",
        )
        .write(
            "lib/base.dart",
            "class Base {\n  static Base value() => Base();\n}\n",
        )
        .edge_after("method Sub::m", "calls", "method Base::value")
}

/// A factory comes to sit below an instance getter of its name: a call
/// through the type reaches the factory, not the getter (upstream #2380).
pub fn dart_call_through_a_type_reaches_a_factory_added_below_a_getter_of_its_name() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/async_value.dart",
            "class AsyncValue {\n  const AsyncValue._();\n\n  Object? get error => null;\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'async_value.dart';\n\nObject fail(Object e) => AsyncValue.error(e);\n",
        )
        .write(
            "lib/async_value.dart",
            "class AsyncValue {\n  const AsyncValue._();\n\n  Object? get error => null;\n\n  const factory AsyncValue.error(Object error) = _Error;\n}\n\nclass _Error extends AsyncValue {\n  const _Error(this.error) : super._();\n\n  @override\n  final Object error;\n}\n",
        )
        .edge_after(
            "function fail",
            "calls",
            "method AsyncValue::error @lib/async_value.dart:6",
        )
        .no_edge_after(
            "function fail",
            "calls",
            "method AsyncValue::error @lib/async_value.dart:4",
        )
}

/// A call through an import prefix reaches a name the prefixed package's
/// barrel starts to show (upstream #2389): the caller's file is unchanged.
pub fn dart_prefixed_call_reaches_a_name_its_barrel_starts_to_show() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown;\n",
        )
        .file(
            "packages/kit/lib/src/impl.dart",
            "int shown() => 1;\nint notShown() => 2;\n",
        )
        .file(
            "lib/app.dart",
            "import 'package:kit/kit.dart' as kit;\n\nint run() => kit.notShown() + kit.shown();\n",
        )
        .write(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown, notShown;\n",
        )
        .edge_throughout("function run", "calls", "function shown")
        .edge_after("function run", "calls", "function notShown")
}

/// The reverse: the barrel stops showing the name, and the call links
/// nothing.
pub fn dart_prefixed_call_leaves_a_name_its_barrel_stops_showing() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown, notShown;\n",
        )
        .file(
            "packages/kit/lib/src/impl.dart",
            "int shown() => 1;\nint notShown() => 2;\n",
        )
        .file(
            "lib/app.dart",
            "import 'package:kit/kit.dart' as kit;\n\nint run() => kit.notShown() + kit.shown();\n",
        )
        .write(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown;\n",
        )
        .edge_before("function run", "calls", "function notShown")
        .no_edge_after("function run", "calls", "function notShown")
}

/// A part takes its library's import prefixes: the library gains the import,
/// and the unchanged part's prefixed call reaches what it brings in.
pub fn dart_prefixed_call_in_a_part_follows_a_prefix_its_library_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/format.dart", "String describe(Object value) => '';\n")
        .file("lib/app.dart", "part 'app_part.dart';\n")
        .file(
            "lib/app_part.dart",
            "part of 'app.dart';\n\nString again(Object value) => fmt.describe(value);\n",
        )
        .write(
            "lib/app.dart",
            "import 'format.dart' as fmt;\n\npart 'app_part.dart';\n",
        )
        .edge_after("function again", "calls", "function describe")
}

/// A chain through an import prefix is on the type the prefix brings in: it
/// reaches the static member that type gains, never a namesake's (upstream
/// #2389).
pub fn dart_chain_through_a_prefix_reaches_the_member_its_type_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file("packages/kit/lib/kit.dart", "export 'src/impl.dart';\n")
        .file(
            "packages/kit/lib/src/impl.dart",
            "class Box {\n  const Box();\n}\n",
        )
        .file(
            "lib/legacy.dart",
            "class Box {\n  static Box create() => Box();\n}\n",
        )
        .file(
            "lib/app.dart",
            "import 'package:kit/kit.dart' as kit;\nimport 'legacy.dart';\n\nObject run() => kit.Box.create();\n",
        )
        .write(
            "packages/kit/lib/src/impl.dart",
            "class Box {\n  const Box();\n\n  static Box create() => const Box();\n}\n",
        )
        .edge_after(
            "function run",
            "calls",
            "method Box::create @packages/kit/lib/src/impl.dart",
        )
        .no_edge_after("function run", "calls", "method Box::create @lib/legacy.dart")
}

/// An annotation is a constant: it reaches the one its imported library
/// gains, never a getter of the name (upstream #2390).
pub fn dart_annotation_reaches_a_constant_its_imported_library_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/annotation.dart",
            "class Riverpod {\n  const Riverpod();\n}\n",
        )
        .file(
            "lib/nodes.dart",
            "extension Annotated on Object {\n  Object? get riverpod => null;\n}\n",
        )
        .file(
            "lib/app.dart",
            "import 'annotation.dart';\nimport 'nodes.dart';\n\n@riverpod\nint counter() => 0;\n",
        )
        .write(
            "lib/annotation.dart",
            "class Riverpod {\n  const Riverpod();\n}\n\nconst riverpod = Riverpod();\n",
        )
        .edge_after("function counter", "decorates", "constant riverpod")
        .no_edge_after(
            "function counter",
            "decorates",
            "method Annotated::riverpod",
        )
}

/// 501 parked calls of `describe` through types the project lacks.
static DESCRIBE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::new();
    for index in 0..501 {
        text.push_str(&format!(
            "final noise{index} = Missing{index}.describe(1);\n"
        ));
    }
    text
});

/// A call through an import prefix reaches the function its prefixed library
/// gains, past the tail ceiling: 501 parked calls share the tail `describe`
/// (upstream #2389).
pub fn dart_prefixed_call_reaches_a_function_its_library_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/format.dart", "int unrelated() => 0;\n")
        .file(
            "lib/app.dart",
            "import 'format.dart' as fmt;\n\nString run(Object value) => fmt.describe(value);\n",
        )
        .file("noise/noise.dart", DESCRIBE_CALLS.as_str())
        .write(
            "lib/format.dart",
            "int unrelated() => 0;\n\nString describe(Object value) => '';\n",
        )
        .edge_after("function run", "calls", "function describe")
}

/// 501 parked calls of `notShown` through types the project lacks.
static NOT_SHOWN_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::new();
    for index in 0..501 {
        text.push_str(&format!(
            "final noise{index} = Missing{index}.notShown();\n"
        ));
    }
    text
});

/// The same for a name the prefixed package's barrel starts to show, past
/// the tail ceiling: no declaration of the name comes or goes.
pub fn dart_prefixed_call_reaches_a_name_its_barrel_starts_to_show_past_the_tail_ceiling() -> Case {
    Case::new(Change::ReexportEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("packages/kit/pubspec.yaml", "name: kit\n")
        .file(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown;\n",
        )
        .file(
            "packages/kit/lib/src/impl.dart",
            "int shown() => 1;\nint notShown() => 2;\n",
        )
        .file(
            "lib/app.dart",
            "import 'package:kit/kit.dart' as kit;\n\nint run() => kit.notShown();\n",
        )
        .file("noise/noise.dart", NOT_SHOWN_CALLS.as_str())
        .write(
            "packages/kit/lib/kit.dart",
            "export 'src/impl.dart' show shown, notShown;\n",
        )
        .edge_after("function run", "calls", "function notShown")
}

/// 501 parked calls of `autoDispose` through types the project lacks.
static AUTO_DISPOSE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::new();
    for index in 0..501 {
        text.push_str(&format!(
            "final noise{index} = Missing{index}.autoDispose(1);\n"
        ));
    }
    text
});

/// The same past the tail ceiling, no method of the name anywhere: 501
/// parked calls share the tail `autoDispose`.
pub fn dart_call_through_a_type_reaches_the_static_constant_it_gains_past_the_tail_ceiling() -> Case
{
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/builders.dart",
            "class Builder {\n  const Builder();\n  Object call(Object create) => create;\n}\n",
        )
        .file("lib/provider.dart", "import 'builders.dart';\n\nclass Provider {}\n")
        .file(
            "lib/use.dart",
            "import 'provider.dart';\n\nfinal p = Provider.autoDispose(1);\n",
        )
        .file("noise/noise.dart", AUTO_DISPOSE_CALLS.as_str())
        .write(
            "lib/provider.dart",
            "import 'builders.dart';\n\nclass Provider {\n  static const autoDispose = Builder();\n}\n",
        )
        .edge_after("constant p", "calls", "constant Provider::autoDispose")
}

/// Upstream #2367: an initializer's calls are its constant's, a field's its
/// class's; the callee, added in another file later, links.
pub fn dart_initializer_calls_reach_a_function_added_in_another_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/use.dart",
            "import 'data.dart';\n\nfinal dioProvider = Provider((ref) => createDio());\n\nclass Service {\n  final dio = createDio();\n}\n\nclass Provider {\n  Provider(Object create);\n}\n",
        )
        .write("lib/data.dart", "Object createDio() => Object();\n")
        .edge_after("constant dioProvider", "calls", "function createDio")
        .edge_after("class Service", "calls", "function createDio")
}

/// Upstream #2327: a field's type is its class's reference, and a type an
/// extension is `on` the extension's; the type, added later, links.
pub fn dart_field_and_extension_types_reach_a_type_added_in_another_file() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/holder.dart",
            "import 'report.dart';\n\nclass Holder {\n  const Holder(this.report);\n  final Report report;\n}\n\nextension ReportX on Report {\n  bool get high => true;\n}\n",
        )
        .write("lib/report.dart", "class Report {}\n")
        .edge_after("class Holder", "references", "class Report")
        .edge_after("class ReportX", "references", "class Report")
}

/// The receiver's type, declared in another file, goes: the read through it
/// links nothing, though no name of the read's file changed.
pub fn dart_getter_read_through_a_deleted_type_links_nothing() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shapes.dart", "class Box {\n  int get area => 1;\n}\n")
        .file(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Box {}\n",
        )
        .file(
            "lib/use.dart",
            "import 'square.dart';\n\nint read(Square s) => s.area;\n",
        )
        .remove("lib/square.dart")
        .edge_before("function read", "calls", "method Box::area")
        .no_edge_after("function read", "calls", "method Box::area")
}

/// A type the read's file already names comes to be declared, extending a
/// type with the getter: the read links it. The read's file imports nothing
/// (a receiver's type is read from its declaration, whatever the file
/// imports), so no directive of it moves.
pub fn dart_getter_read_through_a_type_that_appears_reaches_its_base() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shapes.dart", "class Box {\n  int get area => 1;\n}\n")
        .file("lib/use.dart", "int read(Square s) => s.area;\n")
        .write(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Box {}\n",
        )
        .edge_after("function read", "calls", "method Box::area")
}

/// 501 parked calls of `make` through types the project lacks.
static MAKE_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::new();
    for index in 0..501 {
        text.push_str(&format!("final noise{index} = Missing{index}.make();\n"));
    }
    text
});

/// A type that appears with a static member a constant calls through it,
/// past the tail ceiling: the call names the type only as its receiver (the
/// calling file imports nothing, so no directive of it moves).
pub fn dart_call_through_a_type_that_appears_reaches_its_member_past_the_tail_ceiling() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/use.dart", "final made = Square.make();\n")
        .file("noise/noise.dart", MAKE_CALLS.as_str())
        .write(
            "lib/square.dart",
            "class Square {\n  static Square make() => Square();\n}\n",
        )
        .edge_after("constant made", "calls", "method Square::make")
}

/// A field of a base class types a read in a subclass by a type that comes
/// to extend another base: the subclass's file names neither type, yet its
/// read moves.
pub fn dart_getter_read_through_an_inherited_field_follows_its_types_new_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/shapes.dart",
            "class Box {\n  int get area => 1;\n}\n\nclass Panel {\n  int get area => 2;\n}\n",
        )
        .file(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Box {}\n",
        )
        .file(
            "lib/base.dart",
            "import 'square.dart';\n\nclass Base {\n  Base(this.shape);\n  final Square shape;\n}\n",
        )
        .file(
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Sub(super.shape);\n\n  int measure() => shape.area;\n}\n",
        )
        .write(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Panel {}\n",
        )
        .edge_before("method Sub::measure", "calls", "method Box::area")
        .edge_after("method Sub::measure", "calls", "method Panel::area")
}

/// The same, the read's file declaring the base it leaves: a same-file edge
/// moves too.
pub fn dart_getter_read_leaves_a_base_of_its_own_file_its_receivers_type_stops_extending() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/shapes.dart", "class Box {\n  int get area => 1;\n}\n")
        .file(
            "lib/square.dart",
            "import 'use.dart';\n\nclass Square extends Panel {}\n",
        )
        .file(
            "lib/use.dart",
            "import 'square.dart';\n\nclass Panel {\n  int get area => 2;\n}\n\nint read(Square s) => s.area;\n",
        )
        .write(
            "lib/square.dart",
            "import 'shapes.dart';\n\nclass Square extends Box {}\n",
        )
        .edge_before("function read", "calls", "method Panel::area")
        .edge_after("function read", "calls", "method Box::area")
}

/// A class comes to extend another base, which declares the field a
/// subclass reads through with another type: the read moves, though no type
/// the subclass's file names changed (the member lane, the head rule).
pub fn dart_getter_read_through_a_field_its_class_inherits_from_a_new_base() -> Case {
    Case::new(Change::EditHeritage, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/shapes.dart",
            "class Box {\n  int get area => 1;\n}\n\nclass Panel {\n  int get area => 2;\n}\n",
        )
        .file(
            "lib/holders.dart",
            "import 'shapes.dart';\n\nclass BoxHolder {\n  BoxHolder(this.shape);\n  final Box shape;\n}\n\nclass PanelHolder {\n  PanelHolder(this.shape);\n  final Panel shape;\n}\n",
        )
        .file(
            "lib/base.dart",
            "import 'holders.dart';\n\nclass Base extends BoxHolder {\n  Base(super.shape);\n}\n",
        )
        .file(
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Sub(super.shape);\n\n  int measure() => shape.area;\n}\n",
        )
        .write(
            "lib/base.dart",
            "import 'holders.dart';\n\nclass Base extends PanelHolder {\n  Base(super.shape);\n}\n",
        )
        .edge_before("method Sub::measure", "calls", "method Box::area")
        .edge_after("method Sub::measure", "calls", "method Panel::area")
}

/// An extension that already sits `on` the receiver's type gains the getter,
/// past the tail ceiling: no head moved, so only the new member's name says
/// which parked reads to retry.
pub fn dart_getter_read_reaches_a_getter_an_existing_extension_gains_past_the_tail_ceiling() -> Case
{
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/box.dart", "class Box {\n  final int size = 1;\n}\n")
        .file(
            "lib/box_x.dart",
            "import 'box.dart';\n\nextension BoxX on Box {\n  int twice() => size * 2;\n}\n",
        )
        .file("lib/use.dart", "import 'box.dart';\n\nint read(Box x) => x.area;\n")
        .file("noise/noise.dart", AREA_READS.as_str())
        .write(
            "lib/box_x.dart",
            "import 'box.dart';\n\nextension BoxX on Box {\n  int twice() => size * 2;\n  int get area => size * size;\n}\n",
        )
        .edge_after("function read", "calls", "method BoxX::area")
}

static WRAP_CALLS: LazyLock<String> = LazyLock::new(|| {
    let mut text = String::from("void noise() {\n");
    for index in 0..501 {
        text.push_str(&format!("  missing{index}.wrap();\n"));
    }
    text.push_str("}\n");
    text
});

/// Upstream #2376 (W4-DART-03b): a later link of a chain calls a member of
/// what the chain before it evaluates to, typed by each link's declared
/// return type: retyped in another file, the link moves, though its own
/// name's declarations did not change.
pub fn dart_chain_link_follows_a_return_type_retyped_in_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Item load() => Item();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'repo.dart';\n\nvoid run(Repo r) {\n  r.load().save();\n}\n",
        )
        .write(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Other load() => Other();\n}\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A chain's head call types it by the function's declared return type.
pub fn dart_chain_link_follows_a_top_level_functions_return_type() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/make.dart",
            "import 'items.dart';\n\nItem make() => Item();\n",
        )
        .file(
            "lib/use.dart",
            "import 'make.dart';\n\nvoid run() {\n  make().touch().save();\n}\n",
        )
        .write(
            "lib/make.dart",
            "import 'items.dart';\n\nOther make() => Other();\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// `FutureProvider.autoDispose.family()` is the `family` of the builder the
/// type's static constant holds: another builder there, and the link and the
/// one after it move.
pub fn dart_chain_link_follows_a_static_constants_initializer() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/builders.dart",
            "import 'items.dart';\n\nclass FBuilder {\n  const FBuilder();\n  Item family() => Item();\n}\n\nclass SBuilder {\n  const SBuilder();\n  Other family() => Other();\n}\n",
        )
        .file(
            "lib/providers.dart",
            "import 'builders.dart';\n\nclass FutureProvider {\n  static const autoDispose = FBuilder();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'providers.dart';\n\nvoid run() {\n  FutureProvider.autoDispose.family().save();\n}\n",
        )
        .write(
            "lib/providers.dart",
            "import 'builders.dart';\n\nclass FutureProvider {\n  static const autoDispose = SBuilder();\n}\n",
        )
        .edge_before("function run", "calls", "method FBuilder::family")
        .edge_after("function run", "calls", "method SBuilder::family")
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A top-level constant heading a chain is typed by its initializer, read
/// back from where it ends: an edit past what its node records of it (the
/// first hundred characters) still moves the chain (W4-DART-03b's top-level
/// lines).
pub fn dart_chain_link_follows_the_end_of_a_long_top_level_initializer() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/maker.dart",
            "import 'items.dart';\n\nclass Maker {\n  static Maker make(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p, int q, int r, int s, int t, int u, int v, int w, int x, int y, int z, int aa, int bb, int cc, int dd, int ee, int ff, int gg) => Maker();\n  Item item() => Item();\n  Other other() => Other();\n}\n",
        )
        .file(
            "lib/consts.dart",
            "import 'maker.dart';\n\nfinal thing = Maker.make(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33).item();\n",
        )
        .file(
            "lib/use.dart",
            "import 'consts.dart';\n\nvoid run() {\n  thing.touch().save();\n}\n",
        )
        .write(
            "lib/consts.dart",
            "import 'maker.dart';\n\nfinal thing = Maker.make(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33).other();\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// An extension on a type parameter of its own (`extension X<T> on T`)
/// reaches every project type: added in another file, its member and the
/// link after it come into reach.
pub fn dart_chain_link_after_a_universal_extension_it_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/ext.dart",
            "import 'items.dart';\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'ext.dart';\n\nvoid run(Item i) {\n  i.touch().wrap().save();\n}\n",
        )
        .write(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap<T> on T {\n  Other wrap() => Other();\n}\n",
        )
        .edge_after("function run", "calls", "method Wrap::wrap")
        .edge_after("function run", "calls", "method Other::save")
}

/// No call runs a setter: a method turned into one, its signature unchanged,
/// leaves the chain's link (W4-DART-03b's member statements).
pub fn dart_chain_link_leaves_a_method_that_becomes_a_setter() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/holder.dart",
            "class Holder {\n  void value(int v) {}\n}\n\nclass Box {\n  Holder wrap() => Holder();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'holder.dart';\n\nvoid run(Box b) {\n  b.wrap().value(1);\n}\n",
        )
        .write(
            "lib/holder.dart",
            "class Holder {\n  set value(int v) {}\n}\n\nclass Box {\n  Holder wrap() => Holder();\n}\n",
        )
        .edge_before("function run", "calls", "method Holder::value")
}

/// `T read<T>()` called as `read<Repo>()` is a Repo: the method's own type
/// parameters, renamed, no longer give it the call's type argument.
pub fn dart_chain_link_follows_a_methods_own_type_parameters() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/ref.dart",
            "class Ref {\n  T read<T>(Object p) => throw 0;\n}\n",
        )
        .file(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Item load() => Item();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'ref.dart';\nimport 'repo.dart';\n\nvoid run(Ref ref) {\n  ref.read<Repo>(1).load().save();\n}\n",
        )
        .write(
            "lib/ref.dart",
            "class Ref {\n  T read<U>(Object p) => throw 0;\n}\n",
        )
        .edge_before("function run", "calls", "method Repo::load")
        .edge_before("function run", "calls", "method Item::save")
}

/// A chain through fields of what a call returns (`make().box.item.save()`)
/// reads each field's declared type from its class: retyped there, the
/// links after it move, though the calling file writes no class.
pub fn dart_chain_link_through_fields_of_a_call_result_follows_a_retyping() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/boxes.dart",
            "import 'items.dart';\n\nclass Box {\n  final Item item = Item();\n}\n\nclass Shelf {\n  final Other item = Other();\n}\n",
        )
        .file(
            "lib/crate.dart",
            "import 'boxes.dart';\n\nclass Crate {\n  final Box box = Box();\n}\n\nCrate make() => Crate();\n",
        )
        .file(
            "lib/use.dart",
            "import 'crate.dart';\n\nvoid run() {\n  make().box.item.save();\n}\n",
        )
        .write(
            "lib/crate.dart",
            "import 'boxes.dart';\n\nclass Crate {\n  final Shelf box = Shelf();\n}\n\nCrate make() => Crate();\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A local heading a chain is typed by its initializer (`final item =
/// r.load();`): the initializer's call retyped in another file moves the
/// chain written after it.
pub fn dart_chain_link_through_a_local_follows_its_initializers_return_type() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Item load() => Item();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'repo.dart';\n\nvoid run(Repo r) {\n  final item = r.load();\n  item.touch().save();\n}\n",
        )
        .write(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Other load() => Other();\n}\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A getter's value called (`b.family().save()`) is what its type's `call`
/// returns: retyped, the link after it moves, though the chain never writes
/// `call`.
pub fn dart_chain_link_follows_a_retyped_call_method() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/family.dart",
            "import 'items.dart';\n\nclass Family {\n  Item call() => Item();\n}\n",
        )
        .file(
            "lib/builder.dart",
            "import 'family.dart';\n\nclass Builder {\n  Family get family => Family();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'builder.dart';\n\nvoid run(Builder b) {\n  b.family().save();\n}\n",
        )
        .write(
            "lib/family.dart",
            "import 'items.dart';\n\nclass Family {\n  Other call() => Other();\n}\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A top-level constant heading a chain is typed by what its initializer's
/// function returns: retyped in a third file, the chain moves, though its
/// file names neither the function nor its type.
pub fn dart_chain_link_through_a_constant_follows_the_function_its_initializer_calls() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/repos.dart",
            "import 'items.dart';\n\nclass Repo {\n  Item load() => Item();\n}\n\nclass OtherRepo {\n  Other load() => Other();\n}\n",
        )
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/makers.dart",
            "import 'repos.dart';\n\nRepo makeRepo() => Repo();\n",
        )
        .file(
            "lib/consts.dart",
            "import 'makers.dart';\n\nfinal repo = makeRepo();\n",
        )
        .file(
            "lib/use.dart",
            "import 'consts.dart';\n\nvoid run() {\n  repo.load().touch().save();\n}\n",
        )
        .write(
            "lib/makers.dart",
            "import 'repos.dart';\n\nOtherRepo makeRepo() => OtherRepo();\n",
        )
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A field declared without a type is typed by its initializer: the function
/// it calls retyped in another file moves a chain through the field.
pub fn dart_chain_link_through_a_field_initializer_follows_the_function_it_calls() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/caches.dart",
            "import 'items.dart';\n\nclass Cache {\n  Item get() => Item();\n}\n\nclass OtherCache {\n  Other get() => Other();\n}\n",
        )
        .file(
            "lib/makers.dart",
            "import 'caches.dart';\n\nCache makeCache() => Cache();\n",
        )
        .file(
            "lib/box.dart",
            "import 'makers.dart';\n\nclass Box {\n  final cache = makeCache();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'box.dart';\n\nvoid run(Box b) {\n  b.cache.get().save();\n}\n",
        )
        .write(
            "lib/makers.dart",
            "import 'caches.dart';\n\nOtherCache makeCache() => OtherCache();\n",
        )
        .edge_before("function run", "calls", "method Cache::get")
        .edge_after("function run", "calls", "method OtherCache::get")
        .edge_before("function run", "calls", "method Item::save")
        .edge_after("function run", "calls", "method Other::save")
}

/// A getter an extension adds is a link of the chain (`c.manager.read()`):
/// added in another file, the call through it comes into reach.
pub fn dart_chain_link_after_an_extension_getter_its_receiver_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/manager.dart",
            "class Manager {\n  void read() {}\n}\n\nclass OtherManager {\n  void read() {}\n}\n\nclass Container {\n  final Manager inner = Manager();\n}\n",
        )
        .file(
            "lib/ext.dart",
            "import 'manager.dart';\n",
        )
        .file(
            "lib/use.dart",
            "import 'manager.dart';\nimport 'ext.dart';\n\nvoid run(Container c) {\n  c.manager.read();\n}\n",
        )
        .write(
            "lib/ext.dart",
            "import 'manager.dart';\n\nextension ContainerX on Container {\n  Manager get manager => inner;\n}\n",
        )
        .edge_after("function run", "calls", "method ContainerX::manager")
        .edge_after("function run", "calls", "method Manager::read")
}

/// A link reaches a member through the lineage of the type the link before
/// it returns: that type stops extending the base declaring it, and the
/// link and the one after it link nothing.
pub fn dart_chain_link_leaves_a_member_a_base_of_an_inner_type_drops() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base {\n  Other wrap() => Other();\n}\n",
        )
        .file(
            "lib/inner.dart",
            "import 'base.dart';\n\nclass Inner extends Base {}\n",
        )
        .file(
            "lib/repo.dart",
            "import 'inner.dart';\n\nclass Repo {\n  Inner load() => Inner();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'repo.dart';\n\nvoid run(Repo r) {\n  r.load().wrap().save();\n}\n",
        )
        .write(
            "lib/inner.dart",
            "import 'base.dart';\n\nclass Inner {}\n",
        )
        .edge_before("function run", "calls", "method Base::wrap")
        .edge_before("function run", "calls", "method Other::save")
}

/// The same past the tail ceiling: 501 parked calls share the tail `wrap`.
pub fn dart_chain_link_after_a_universal_extension_it_gains_past_the_tail_ceiling() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/items.dart",
            "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n",
        )
        .file(
            "lib/ext.dart",
            "import 'items.dart';\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'ext.dart';\n\nvoid run(Item i) {\n  i.touch().wrap().save();\n}\n",
        )
        .file("noise/noise.dart", WRAP_CALLS.as_str())
        .write(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap<T> on T {\n  Other wrap() => Other();\n}\n",
        )
        .edge_after("function run", "calls", "method Wrap::wrap")
        .edge_after("function run", "calls", "method Other::save")
}

/// A getter typed by a type parameter of its class (`Item get held` in
/// `Holder<Item>`) says nothing; once the class stops declaring that
/// parameter, `Item` is the type, though no member of the class changed
/// (W4-DART-03b: a class head that changed moves every member's name).
pub fn dart_chain_link_follows_a_class_head_that_stops_declaring_a_type_parameter() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/holder.dart",
            "import 'items.dart';\n\nclass Holder<Item> {\n  Item get held => throw 0;\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'holder.dart';\n\nvoid run(Holder h) {\n  h.held.save();\n}\n",
        )
        .write(
            "lib/holder.dart",
            "import 'items.dart';\n\nclass Holder<X> {\n  Item get held => throw 0;\n}\n",
        )
        .edge_after("function run", "calls", "method Item::save")
}

/// The type a chain's local is initialized through goes with its file: the
/// links after the local link nothing, though their own targets stayed.
pub fn dart_chain_link_through_a_local_of_a_deleted_type_links_nothing() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/repo.dart",
            "import 'items.dart';\n\nclass Repo {\n  Item load() => Item();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'repo.dart';\n\nvoid run(Repo r) {\n  final item = r.load();\n  item.touch().save();\n}\n",
        )
        .remove("lib/repo.dart")
        .edge_before("function run", "calls", "method Item::save")
}

/// An extension's member answers a lookup on the type it is `on`, whatever
/// the chain names: the extension moves to another type, and the link to it
/// and the link after it link nothing, though no member changed
/// (W4-DART-03b).
pub fn dart_chain_link_leaves_an_extension_that_moves_to_another_type() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap on Item {\n  Other wrap() => Other();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'ext.dart';\n\nvoid run(Item i) {\n  i.touch().wrap().save();\n}\n",
        )
        .write(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap on Other {\n  Other wrap() => Other();\n}\n",
        )
        .edge_before("function run", "calls", "method Wrap::wrap")
        .edge_before("function run", "calls", "method Other::save")
}

/// A constant an initializer types through an extension's member is what
/// the member returns: the extension moves to another type, and a chain
/// headed by the constant in another file links nothing, though neither
/// file names the extension (W4-DART-03b).
pub fn dart_chain_link_through_a_constant_an_extension_types_leaves_with_it() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap on Item {\n  Other wrap() => Other();\n}\n",
        )
        .file(
            "lib/w.dart",
            "import 'items.dart';\nimport 'ext.dart';\n\nfinal w = Item().wrap();\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'w.dart';\n\nvoid run() {\n  w.touch().save();\n}\n",
        )
        .write(
            "lib/ext.dart",
            "import 'items.dart';\n\nextension Wrap on Other {\n  Other wrap() => Other();\n}\n",
        )
        .edge_before("function run", "calls", "method Other::save")
}

/// A class head that changes moves the lookups on the class, reached
/// through a function typed by it in another file: once the head stops
/// declaring the type parameter its getter returns, the getter's type is
/// the class it names (W4-DART-03b).
pub fn dart_chain_link_through_a_function_typed_by_a_class_whose_head_changes() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/holder.dart",
            "import 'items.dart';\n\nclass Holder<Item> {\n  Item get held => throw 0;\n}\n",
        )
        .file(
            "lib/make.dart",
            "import 'holder.dart';\n\nHolder makeHolder() => Holder();\n",
        )
        .file(
            "lib/use.dart",
            "import 'make.dart';\n\nvoid run() {\n  makeHolder().held.save();\n}\n",
        )
        .write(
            "lib/holder.dart",
            "import 'items.dart';\n\nclass Holder<X> {\n  Item get held => throw 0;\n}\n",
        )
        .edge_after("function run", "calls", "method Item::save")
}

/// A static constant's initializer calls the function its own library
/// holds: a file declaring `pick` becomes a part of that library, and a
/// chain through the constant in a file that imports neither moves with it,
/// though no declaration changed (W4-DART-03b; the names a library's
/// visibility moved).
pub fn dart_chain_link_through_a_static_constant_follows_a_part_joining_its_library() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file("lib/b.dart", "import 'items.dart';\n\nOther pick() => Other();\n")
        .file("lib/pa.dart", "import 'items.dart';\n\nItem pick() => Item();\n")
        .file("lib/holder.dart", "class Holder {\n  static final p = pick();\n}\n")
        .file(
            "lib/use.dart",
            "void run() {\n  Holder.p.touch().save();\n}\n",
        )
        .write(
            "lib/holder.dart",
            "import 'items.dart';\n\npart 'pa.dart';\n\nclass Holder {\n  static final p = pick();\n}\n",
        )
        .write("lib/pa.dart", "part of 'holder.dart';\n\nItem pick() => Item();\n")
        .edge_after("function run", "calls", "method Item::save")
}

/// A chain's head naming a local is the local's declaration, unless a type
/// of its name exists: once one comes, the head is that type, and a link it
/// declares nothing for links nothing, though the chain names nothing the
/// new type declares (W4-DART-03b; a local's name is read only as a
/// type's).
pub fn dart_chain_link_headed_by_a_local_follows_a_type_of_its_name() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file("lib/box.dart", "import 'items.dart';\n")
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'box.dart';\n\nvoid run() {\n  final box = Item();\n  box.touch().save();\n}\n",
        )
        .write("lib/box.dart", "import 'items.dart';\n\nclass box {}\n")
        .edge_before("function run", "calls", "method Item::save")
}

/// A field a type gains through the base its base comes to extend is read
/// through a chain on the type, typed in another file: the field has no node,
/// so the names a lookup on the type reaches are read from the declarations
/// of its ancestry (W4-DART-03b).
pub fn dart_chain_link_reads_a_field_its_type_gains_through_a_base() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base {}\n\nclass Holder {\n  final Item item = Item();\n}\n",
        )
        .file(
            "lib/make.dart",
            "import 'base.dart';\n\nclass Child extends Base {}\n\nChild makeChild() => Child();\n",
        )
        .file(
            "lib/use.dart",
            "import 'make.dart';\n\nvoid run() {\n  makeChild().item.save();\n}\n",
        )
        .write(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base extends Holder {}\n\nclass Holder {\n  final Item item = Item();\n}\n",
        )
        .edge_after("function run", "calls", "method Item::save")
}

/// An extension `on` the base a type's base comes to extend answers a link
/// on the type: the names a lookup reaches include the extensions on its
/// ancestry (W4-DART-03b).
pub fn dart_chain_link_reaches_an_extension_on_a_base_its_type_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base {}\n\nclass Holder {}\n\nextension HolderX on Holder {\n  Other wrap() => Other();\n}\n",
        )
        .file(
            "lib/make.dart",
            "import 'base.dart';\n\nclass Child extends Base {}\n\nChild makeChild() => Child();\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'base.dart';\nimport 'make.dart';\n\nvoid run() {\n  makeChild().wrap().save();\n}\n",
        )
        .write(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base extends Holder {}\n\nclass Holder {}\n\nextension HolderX on Holder {\n  Other wrap() => Other();\n}\n",
        )
        .edge_after("function run", "calls", "method Other::save")
}

/// An extension `on` any type applies to a project type only: once the type
/// a function returns comes, a chain on the function's result reaches the
/// extension's member, though no name the chain writes changed
/// (W4-DART-03b).
pub fn dart_chain_link_reaches_an_extension_on_any_type_once_its_type_comes() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file(
            "lib/also.dart",
            "extension Also<T> on T {\n  T also() => this;\n}\n",
        )
        .file(
            "lib/make.dart",
            "import 'fresh.dart';\n\nFresh makeFresh() => Fresh();\n",
        )
        .file(
            "lib/use.dart",
            "import 'also.dart';\nimport 'make.dart';\n\nvoid run() {\n  makeFresh().also();\n}\n",
        )
        .write("lib/fresh.dart", "class Fresh {\n  void go() {}\n}\n")
        .edge_after("function run", "calls", "method Also::also")
}

/// A chain written in a type reads the type: `this` and a receiver-less
/// head are typed by it, so its links move with the type's lineage, though
/// the chain names nothing that changed (W4-DART-03b).
pub fn dart_chain_link_in_a_type_follows_a_base_its_base_gains() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n  Item touch() => this;\n}\n\nclass Other {\n  void save() {}\n  Other touch() => this;\n}\n")
        .file(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base {}\n\nclass Mixer {\n  Other wrap() => Other();\n}\n",
        )
        .file(
            "lib/child.dart",
            "import 'items.dart';\nimport 'base.dart';\n\nclass Child extends Base {\n  void run() {\n    this.wrap().save();\n    wrap().touch();\n  }\n}\n",
        )
        .write(
            "lib/base.dart",
            "import 'items.dart';\n\nclass Base extends Mixer {}\n\nclass Mixer {\n  Other wrap() => Other();\n}\n",
        )
        .edge_after("method Child::run", "calls", "method Other::save")
        .edge_after("method Child::run", "calls", "method Other::touch")
}

/// An unnamed extension applies only in its own library: once its file
/// becomes a part of the chain's library, its member and the link after it
/// come into reach, though neither file of the chain changed.
pub fn dart_chain_link_reaches_an_unnamed_extension_once_its_file_joins_the_library() -> Case {
    Case::new(Change::BodyEdit, &[Language::Dart])
        .file("pubspec.yaml", "name: app\n")
        .file("lib/items.dart", "class Item {\n  void save() {}\n}\n")
        .file(
            "lib/shapes.dart",
            "class Shape {}\n\nclass Wrap {\n  Shape get shape => Shape();\n}\n",
        )
        .file(
            "lib/use.dart",
            "import 'items.dart';\nimport 'shapes.dart';\n\npart 'ext.dart';\n\nvoid run(Wrap w) {\n  w.shape.area().save();\n}\n",
        )
        .file(
            "lib/ext.dart",
            "import 'items.dart';\nimport 'shapes.dart';\n\nextension on Shape {\n  Item area() => Item();\n}\n",
        )
        .write(
            "lib/ext.dart",
            "part of 'use.dart';\n\nextension on Shape {\n  Item area() => Item();\n}\n",
        )
        .edge_after("function run", "calls", "method <anonymous>::area")
        .edge_after("function run", "calls", "method Item::save")
}

const SYNTH_SCENE: &str = "type Callback = () => void;\nexport class Scene {\n  private callbacks = new Set<Callback>();\n  onUpdate(cb: Callback) {\n    this.callbacks.add(cb);\n  }\n  triggerUpdate() {\n    for (const cb of this.callbacks) cb();\n  }\n}\n";
const SYNTH_APP_RENDER: &str = "import { Scene } from './scene';\nexport class App {\n  scene = new Scene();\n  constructor() {\n    this.scene.onUpdate(this.triggerRender);\n  }\n  triggerRender() {\n    return 1;\n  }\n  triggerPaint() {\n    return 2;\n  }\n}\n";
const SYNTH_APP_PAINT: &str = "import { Scene } from './scene';\nexport class App {\n  scene = new Scene();\n  constructor() {\n    this.scene.onUpdate(this.triggerPaint);\n  }\n  triggerRender() {\n    return 1;\n  }\n  triggerPaint() {\n    return 2;\n  }\n}\n";
const SYNTH_BUS: &str = "export function save(bus) {\n  bus.emit('save');\n}\nexport function wire(bus) {\n  bus.on('save', handleSave);\n}\n";

/// W7-14: a field registration moves to another method; the synthesized
/// dispatch edge follows it.
pub fn typescript_synthesis_registration_moves() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/scene.ts", SYNTH_SCENE)
        .file("src/app.ts", SYNTH_APP_RENDER)
        .write("src/app.ts", SYNTH_APP_PAINT)
        .edge_before(
            "method Scene::triggerUpdate",
            "calls",
            "method App::triggerRender",
        )
        .no_edge_after(
            "method Scene::triggerUpdate",
            "calls",
            "method App::triggerRender",
        )
        .edge_metadata_after(
            (
                "method Scene::triggerUpdate",
                "calls",
                "method App::triggerPaint",
            ),
            "synthesizedBy",
            "callback",
        )
}

/// W7-14: the file holding a channel's registrar and dispatcher goes, and
/// with it every edge the channel synthesized.
pub fn typescript_synthesis_channel_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::TypeScript])
        .file("src/scene.ts", SYNTH_SCENE)
        .file("src/app.ts", SYNTH_APP_RENDER)
        .remove("src/scene.ts")
        .edge_before(
            "method Scene::triggerUpdate",
            "calls",
            "method App::triggerRender",
        )
}

/// W7-14: an edit to a file no pass reads recomputes the stage to the same
/// synthesized edges.
pub fn typescript_synthesis_survives_an_unrelated_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/scene.ts", SYNTH_SCENE)
        .file("src/app.ts", SYNTH_APP_RENDER)
        .file(
            "src/util.ts",
            "export function twice(n: number) {\n  return n * 2;\n}\n",
        )
        .write(
            "src/util.ts",
            "export function twice(n: number) {\n  return n + n;\n}\n",
        )
        .edge_throughout(
            "method Scene::triggerUpdate",
            "calls",
            "method App::triggerRender",
        )
        .edge_metadata_after(
            (
                "method Scene::triggerUpdate",
                "calls",
                "method App::triggerRender",
            ),
            "synthesizedBy",
            "callback",
        )
}

/// W7-14: a definition of a synthesized edge's target's name appears in
/// another file. The registration binds `this.triggerRender`, so the edge
/// stays, and the sync's site machinery leaves it to the stage.
pub fn typescript_synthesis_target_gains_a_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file("src/scene.ts", SYNTH_SCENE)
        .file("src/app.ts", SYNTH_APP_RENDER)
        .write(
            "src/other.ts",
            "export function triggerRender() {\n  return 3;\n}\n",
        )
        .edge_throughout(
            "method Scene::triggerUpdate",
            "calls",
            "method App::triggerRender",
        )
        .edge_metadata_after(
            (
                "method Scene::triggerUpdate",
                "calls",
                "method App::triggerRender",
            ),
            "synthesizedBy",
            "callback",
        )
}

/// W7-14: a second definition of an emitter handler's name appears in a
/// file the registration does not name: the name means no one function
/// any more (KEEP-RUST), so the edge goes, as a fresh index leaves it out.
pub fn javascript_emitter_handler_gains_a_competitor() -> Case {
    Case::new(Change::AddCompetitor, &[Language::JavaScript])
        .file("src/app.js", SYNTH_BUS)
        .file(
            "src/handlers.js",
            "export function handleSave() {\n  return 1;\n}\n",
        )
        .write(
            "src/other.js",
            "export function handleSave() {\n  return 2;\n}\n",
        )
        .edge_before(
            "function save",
            "calls",
            "function handleSave @src/handlers.js",
        )
        .no_edge_after(
            "function save",
            "calls",
            "function handleSave @src/handlers.js",
        )
}

/// W7-14: the competing definition is renamed, the handler's name means
/// one function again, and the edge appears.
pub fn javascript_emitter_handler_loses_a_competitor() -> Case {
    Case::new(Change::RenameSymbol, &[Language::JavaScript])
        .file("src/app.js", SYNTH_BUS)
        .file(
            "src/handlers.js",
            "export function handleSave() {\n  return 1;\n}\n",
        )
        .file(
            "src/other.js",
            "export function handleSave() {\n  return 2;\n}\n",
        )
        .write(
            "src/other.js",
            "export function handleOther() {\n  return 2;\n}\n",
        )
        .edge_metadata_after(
            (
                "function save",
                "calls",
                "function handleSave @src/handlers.js",
            ),
            "synthesizedBy",
            "event-emitter",
        )
}

/// W7-14: `complete` iterates both fields `feed` appends to, so the first
/// field the pass walks names its edge. The pass walks callables in
/// upstream's order (the methods by file, line and id); a sync rewrites the
/// edited file's node rows after the others', and the edge keeps a fresh
/// index's field.
pub fn swift_closure_collection_keeps_its_field_after_an_edit() -> Case {
    Case::new(Change::BodyEdit, &[Language::Swift])
        .file(
            "Sources/A.swift",
            "class Early {\n    var handlers: [() -> Void] = []\n\n    func runHandlers() {\n        handlers.forEach { $0() }\n    }\n}\n",
        )
        .file(
            "Sources/B.swift",
            "class Late {\n    var validators: [() -> Void] = []\n    var handlers: [() -> Void] = []\n\n    func complete() {\n        validators.forEach { $0() }\n        handlers.forEach { $0() }\n    }\n}\n",
        )
        .file(
            "Sources/C.swift",
            "class Feeder {\n    func feed(_ job: @escaping () -> Void) {\n        validators.append(job)\n        handlers.append(job)\n    }\n}\n",
        )
        .write(
            "Sources/A.swift",
            "class Early {\n    var handlers: [() -> Void] = []\n\n    func runHandlers() {\n        let count = handlers.count\n        handlers.forEach { $0() }\n    }\n}\n",
        )
        .edge_metadata_after(
            ("method Late::complete", "calls", "method Feeder::feed"),
            "field",
            "handlers",
        )
}

const SYNTH_COUNTER: &str = "export class Counter extends Component {\n  increment() {\n    this.setState({ count: 1 });\n  }\n  render() {\n    return null;\n  }\n}\n";
const SYNTH_EXPECT: &str = "package app\n\nexpect fun platformName(): String\n";
const SYNTH_MAPPER: &str =
    "package com.example;\n\npublic interface UserMapper {\n    User findById(long id);\n}\n";
const SYNTH_MAPPER_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE mapper PUBLIC \"-//mybatis.org//DTD Mapper 3.0//EN\" \"http://mybatis.org/dtd/mybatis-3-mapper.dtd\">\n<mapper namespace=\"com.example.UserMapper\">\n  <select id=\"findById\" resultType=\"User\">\n    SELECT * FROM users WHERE id = #{id}\n  </select>\n</mapper>\n";

/// W7-14b: a React class method stops setting state; its re-render edge to
/// `render` goes.
pub fn typescript_react_state_update_goes() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/counter.ts", SYNTH_COUNTER)
        .write(
            "src/counter.ts",
            "export class Counter extends Component {\n  increment() {\n    return 1;\n  }\n  render() {\n    return null;\n  }\n}\n",
        )
        .edge_before("method Counter::increment", "calls", "method Counter::render")
        .no_edge_after("method Counter::increment", "calls", "method Counter::render")
}

/// W7-14b: one platform's `actual` file goes; the `expect` declaration keeps
/// its edge to the other platform's.
pub fn kotlin_actual_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Kotlin])
        .file("src/commonMain/kotlin/Platform.kt", SYNTH_EXPECT)
        .file(
            "src/jvmMain/kotlin/Platform.jvm.kt",
            "package app\n\nactual fun platformName(): String = \"JVM\"\n",
        )
        .file(
            "src/iosMain/kotlin/Platform.ios.kt",
            "package app\n\nactual fun platformName(): String = \"iOS\"\n",
        )
        .remove("src/jvmMain/kotlin/Platform.jvm.kt")
        .edge_before(
            "function app::platformName @src/commonMain/kotlin/Platform.kt",
            "calls",
            "function app::platformName @src/jvmMain/kotlin/Platform.jvm.kt",
        )
        .edge_metadata_after(
            (
                "function app::platformName @src/commonMain/kotlin/Platform.kt",
                "calls",
                "function app::platformName @src/iosMain/kotlin/Platform.ios.kt",
            ),
            "synthesizedBy",
            "kotlin-expect-actual",
        )
}

/// W7-14b: a C++ subclass renames its override; the base method no longer
/// reaches it.
pub fn cpp_override_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Cpp])
        .file(
            "include/iterator.h",
            "class Iterator {\n public:\n  virtual void Next() = 0;\n};\n",
        )
        .file(
            "src/db_iter.cc",
            "#include \"iterator.h\"\nclass DBIter : public Iterator {\n public:\n  void Next() override {}\n};\n",
        )
        .write(
            "src/db_iter.cc",
            "#include \"iterator.h\"\nclass DBIter : public Iterator {\n public:\n  void Advance() {}\n};\n",
        )
        .edge_before("method Iterator::Next", "calls", "method DBIter::Next")
        .no_edge_after("method Iterator::Next", "calls", "method DBIter::Advance")
}

/// W7-14b: a Delphi unit's form appears beside it; the unit references it.
pub fn pascal_form_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Pascal])
        .file(
            "src/UFRMAbout.pas",
            "unit UFRMAbout;\n\ninterface\n\nimplementation\n\n{$R *.dfm}\n\nend.\n",
        )
        .write(
            "src/UFRMAbout.dfm",
            "object FRMAbout: TFRMAbout\n  Caption = 'About'\nend\n",
        )
        .edge_metadata_after(
            (
                "file:src/UFRMAbout.pas",
                "references",
                "file:src/UFRMAbout.dfm",
            ),
            "synthesizedBy",
            "pascal-form",
        )
}

/// W7-14b: a second Java class of a mapper's simple name appears; the
/// statement's mapper method is ambiguous and the edge goes.
pub fn java_mybatis_mapper_gains_a_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Java, Language::Xml])
        .file("src/main/java/com/example/UserMapper.java", SYNTH_MAPPER)
        .file("src/main/resources/mapper/UserMapper.xml", SYNTH_MAPPER_XML)
        .write(
            "src/main/java/com/other/UserMapper.java",
            "package com.other;\n\npublic class UserMapper {\n    public User findById(long id) {\n        return null;\n    }\n}\n",
        )
        .edge_before(
            "method com.example::UserMapper::findById",
            "calls",
            "method com.example.UserMapper::findById",
        )
        .no_edge_after(
            "method com.example::UserMapper::findById",
            "calls",
            "method com.example.UserMapper::findById",
        )
}

/// W7-14c: a TypeScript class stops implementing an interface; the
/// interface method no longer reaches its method.
pub fn typescript_class_drops_its_interface() -> Case {
    Case::new(Change::EditHeritage, &[Language::TypeScript])
        .file("src/shape.ts", "export interface Shape {\n  area(): number;\n}\n")
        .file(
            "src/circle.ts",
            "import { Shape } from './shape';\nexport class Circle implements Shape {\n  area() {\n    return 1;\n  }\n}\n",
        )
        .write(
            "src/circle.ts",
            "import { Shape } from './shape';\nexport class Circle {\n  area() {\n    return 1;\n  }\n}\n",
        )
        .edge_before("method Shape::area", "calls", "method Circle::area")
        .no_edge_after("method Shape::area", "calls", "method Circle::area")
}

/// W7-14c: an extension in another file gives a Swift protocol a default
/// implementation; it reaches the conformer's override.
pub fn swift_protocol_extension_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Swift])
        .file(
            "Sources/EventMonitor.swift",
            "protocol EventMonitor {\n    func request()\n}\n",
        )
        .file(
            "Sources/Logger.swift",
            "final class Logger: EventMonitor {\n    func request() {}\n}\n",
        )
        .write(
            "Sources/EventMonitor+Defaults.swift",
            "extension EventMonitor {\n    func request() {}\n}\n",
        )
        .edge_metadata_after(
            (
                "method EventMonitor::request",
                "calls",
                "method Logger::request",
            ),
            "synthesizedBy",
            "interface-impl",
        )
}

/// W7-14c: one of a Java class's two interfaces loses its file; the other
/// interface's method keeps its edge to the class's method.
pub fn java_interface_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Java])
        .file(
            "src/main/java/app/Repo.java",
            "package app;\n\npublic interface Repo {\n    void save();\n}\n",
        )
        .file(
            "src/main/java/app/Store.java",
            "package app;\n\npublic interface Store {\n    void save();\n}\n",
        )
        .file(
            "src/main/java/app/JpaRepo.java",
            "package app;\n\npublic class JpaRepo implements Repo, Store {\n    public void save() {}\n}\n",
        )
        .remove("src/main/java/app/Repo.java")
        .edge_before(
            "method app::Repo::save",
            "calls",
            "method app::JpaRepo::save",
        )
        .edge_metadata_after(
            (
                "method app::Store::save",
                "calls",
                "method app::JpaRepo::save",
            ),
            "synthesizedBy",
            "interface-impl",
        )
}

const REGISTRY_SAVE: &str = "export function saveDoc(x: string) { return x; }\n";
const REGISTRY_LOAD: &str = "export function loadDoc(x: string) { return x; }\n";
const REGISTRY_ROUTE: &str = "import { saveDoc } from './save';\nimport { loadDoc } from './load';\nconst handlers = { save: saveDoc, load: loadDoc };\nexport function route(action: string, x: string) {\n  return handlers[action](x);\n}\n";

/// W7-14d: an object registry gains a handler defined in another file; the
/// dispatching function reaches it.
pub fn typescript_registry_gains_a_handler() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/save.ts", REGISTRY_SAVE)
        .file("src/load.ts", REGISTRY_LOAD)
        .file(
            "src/open.ts",
            "export function openDoc(x: string) { return x; }\n",
        )
        .file("src/route.ts", REGISTRY_ROUTE)
        .write(
            "src/route.ts",
            "import { saveDoc } from './save';\nimport { loadDoc } from './load';\nimport { openDoc } from './open';\nconst handlers = { save: saveDoc, load: loadDoc, open: openDoc };\nexport function route(action: string, x: string) {\n  return handlers[action](x);\n}\n",
        )
        .edge_metadata_after(
            ("function route", "calls", "function openDoc"),
            "synthesizedBy",
            "object-registry",
        )
}

/// W7-14d: a registered handler's file is deleted; the dispatch keeps its
/// edge to the other handler only.
pub fn typescript_registry_handler_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::TypeScript])
        .file("src/save.ts", REGISTRY_SAVE)
        .file("src/load.ts", REGISTRY_LOAD)
        .file("src/route.ts", REGISTRY_ROUTE)
        .remove("src/load.ts")
        .edge_before("function route", "calls", "function loadDoc")
        .edge_metadata_after(
            ("function route", "calls", "function saveDoc"),
            "synthesizedBy",
            "object-registry",
        )
}

/// W7-14d: the dispatch is edited away; the registry's edges go with it.
pub fn typescript_registry_dispatch_removed() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file("src/save.ts", REGISTRY_SAVE)
        .file("src/load.ts", REGISTRY_LOAD)
        .file("src/route.ts", REGISTRY_ROUTE)
        .write(
            "src/route.ts",
            "import { saveDoc } from './save';\nimport { loadDoc } from './load';\nconst handlers = { save: saveDoc, load: loadDoc };\nexport function route(action: string, x: string) {\n  return x;\n}\n",
        )
        .edge_before("function route", "calls", "function saveDoc")
        .no_edge_after("function route", "calls", "function saveDoc")
}

const JSX_APP: &str =
    "import { Card } from './card';\nexport function App() {\n  return <Card />;\n}\n";
const JSX_CARD: &str = "export function Card() {\n  return <div />;\n}\n";

/// W7-14e (#2452's JSX half): the component a tag imports appears in a new
/// file; the unchanged parent renders it.
pub fn tsx_rendered_component_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Tsx])
        .file("src/app.tsx", JSX_APP)
        .write("src/card.tsx", JSX_CARD)
        .edge_metadata_after(
            ("function App", "calls", "function Card"),
            "synthesizedBy",
            "jsx-render",
        )
}

/// W7-14e: one rendered component's file is deleted; the parent keeps
/// rendering the other.
pub fn tsx_rendered_component_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Tsx])
        .file(
            "src/app.tsx",
            "import { Card } from './card';\nimport { Badge } from './badge';\nexport function App() {\n  return (\n    <div>\n      <Card />\n      <Badge />\n    </div>\n  );\n}\n",
        )
        .file("src/card.tsx", JSX_CARD)
        .file(
            "src/badge.tsx",
            "export function Badge() {\n  return <span />;\n}\n",
        )
        .remove("src/card.tsx")
        .edge_before("function App", "calls", "function Card")
        .edge_metadata_after(
            ("function App", "calls", "function Badge"),
            "synthesizedBy",
            "jsx-render",
        )
}

/// W7-14e: the tag is edited out of the parent; its edge goes with it.
pub fn tsx_tag_removed() -> Case {
    Case::new(Change::BodyEdit, &[Language::Tsx])
        .file("src/app.tsx", JSX_APP)
        .file("src/card.tsx", JSX_CARD)
        .write(
            "src/app.tsx",
            "import { Card } from './card';\nexport function App() {\n  return null;\n}\n",
        )
        .edge_before("function App", "calls", "function Card")
        .no_edge_after("function App", "calls", "function Card")
}

const GO_SAVER: &str =
    "package svc\n\ntype Store struct{}\n\ntype Saver interface {\n\tSave(key string) error\n}\n";
const GO_SAVE: &str = "package svc\n\nfunc (s *Store) Save(key string) error { return nil }\n";

/// W7-15b: a file of the package declares a method of a type in an
/// unchanged file; the type owns it (`go-method-contains`) and satisfies
/// the interface asking for it (`go-implements`).
pub fn go_method_in_an_added_file_gains_its_owner() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("svc/types.go", GO_SAVER)
        .write("svc/methods.go", GO_SAVE)
        .edge_metadata_after(
            ("struct Store", "contains", "method Store::Save"),
            "synthesizedBy",
            "go-method-contains",
        )
        .edge_metadata_after(
            ("struct Store", "implements", "interface Saver"),
            "synthesizedBy",
            "go-implements",
        )
}

/// W7-15b: the interface's method is renamed; the struct declaring the old
/// name no longer satisfies it.
pub fn go_interface_method_renamed_off_its_implementer() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("svc/types.go", GO_SAVER)
        .file("svc/methods.go", GO_SAVE)
        .write(
            "svc/types.go",
            "package svc\n\ntype Store struct{}\n\ntype Saver interface {\n\tKeep(key string) error\n}\n",
        )
        .edge_before("struct Store", "implements", "interface Saver")
        .no_edge_after("struct Store", "implements", "interface Saver")
}

const GO_BASE: &str =
    "package base\n\ntype Base struct{}\n\nfunc (Base) Close() error { return nil }\n";
const GO_NAMED_CLOSER: &str =
    "package api\n\ntype NamedCloser interface {\n\tClose() error\n\tName() string\n}\n";
const GO_WRAPPER: &str = "package wrap\n\nimport \"example.com/app/base\"\n\ntype Wrapper struct{ base.Base }\n\nfunc (Wrapper) Name() string { return \"\" }\n";

/// W7-15b: the struct stops embedding the type its `Close` came from; it no
/// longer satisfies the interface.
pub fn go_embedding_removed_drops_the_interface() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("api/api.go", GO_NAMED_CLOSER)
        .file("base/base.go", GO_BASE)
        .file("wrap/wrap.go", GO_WRAPPER)
        .write(
            "wrap/wrap.go",
            "package wrap\n\ntype Wrapper struct{}\n\nfunc (Wrapper) Name() string { return \"\" }\n",
        )
        .edge_before("struct Wrapper", "implements", "interface NamedCloser")
        .no_edge_after("struct Wrapper", "implements", "interface NamedCloser")
}

/// W7-15b (upstream's sync case): only the embedded type changes, in a file
/// neither the struct's nor the interface's.
pub fn go_embedded_type_renames_its_method() -> Case {
    Case::new(Change::RenameSymbol, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("api/api.go", GO_NAMED_CLOSER)
        .file("base/base.go", GO_BASE)
        .file("wrap/wrap.go", GO_WRAPPER)
        .write(
            "base/base.go",
            "package base\n\ntype Base struct{}\n\nfunc (Base) Shutdown() error { return nil }\n",
        )
        .edge_before("struct Wrapper", "implements", "interface NamedCloser")
        .no_edge_after("struct Wrapper", "implements", "interface NamedCloser")
}

/// W7-15b (upstream's sync case): the interface embeds another
/// standard-library type, read back from resolution's failed `extends` row;
/// an `io.Reader` has to `Read`, which the struct can't.
pub fn go_interface_embeds_another_std_type() -> Case {
    Case::new(Change::EditHeritage, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "api/api.go",
            "package api\n\nimport stdio \"io\"\n\ntype SyncWriter interface {\n\tstdio.Writer\n\tSync() error\n}\n",
        )
        .file(
            "log/file.go",
            "package log\n\ntype file struct{}\n\nfunc (f *file) Write(p []byte) (int, error) { return len(p), nil }\nfunc (f *file) Sync() error { return nil }\n",
        )
        .write(
            "api/api.go",
            "package api\n\nimport stdio \"io\"\n\ntype SyncWriter interface {\n\tstdio.Reader\n\tSync() error\n}\n",
        )
        .edge_before("struct file", "implements", "interface SyncWriter")
        .no_edge_after("struct file", "implements", "interface SyncWriter")
}

const GO_GREETER_STUB: &str = "package svc\n\ntype UnimplementedGreeterServer struct{}\n\nfunc (UnimplementedGreeterServer) SayHello() error { return nil }\nfunc (UnimplementedGreeterServer) SayBye() error   { return nil }\nfunc (UnimplementedGreeterServer) mustEmbedUnimplementedGreeterServer() {}\n";

/// W7-15c: the hand-written server gains the RPC it lacked, in a file of its
/// own; the generated stub's methods now link to its (`go-grpc-stub-impl`).
pub fn go_grpc_server_gains_its_missing_rpc() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("svc/greeter_grpc.pb.go", GO_GREETER_STUB)
        .file(
            "svc/server.go",
            "package svc\n\ntype server struct{}\n\nfunc (s *server) SayHello() error { return nil }\n",
        )
        .write(
            "svc/bye.go",
            "package svc\n\nfunc (s *server) SayBye() error { return nil }\n",
        )
        .edge_metadata_after(
            (
                "method UnimplementedGreeterServer::SayHello",
                "calls",
                "method server::SayHello",
            ),
            "synthesizedBy",
            "go-grpc-stub-impl",
        )
        .edge_metadata_after(
            (
                "method UnimplementedGreeterServer::SayBye",
                "calls",
                "method server::SayBye",
            ),
            "synthesizedBy",
            "go-grpc-stub-impl",
        )
}

const GO_GIN_CONTEXT: &str = "package gin\n\ntype HandlerFunc func(*Context)\n\ntype Context struct {\n\thandlers []HandlerFunc\n\tindex    int\n}\n\nfunc (c *Context) Next() {\n\tfor c.index < len(c.handlers) {\n\t\tc.handlers[c.index](c)\n\t\tc.index++\n\t}\n}\n\ntype Engine struct{}\n\nfunc (e *Engine) Use(h ...HandlerFunc)          {}\nfunc (e *Engine) GET(p string, h ...HandlerFunc) {}\n";

/// W7-15c: a route registration moves from one function body to another
/// file's; the chain dispatcher's edge to the handler names the new site.
pub fn go_gin_registration_moves_to_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("gin.go", GO_GIN_CONTEXT)
        .file(
            "app.go",
            "package gin\n\nfunc getUser(c *Context) {}\n\nfunc setup(e *Engine) {\n\te.GET(\"/users\", getUser)\n}\n",
        )
        .file("routes.go", "package gin\n\nfunc routes(e *Engine) {\n}\n")
        .write(
            "app.go",
            "package gin\n\nfunc getUser(c *Context) {}\n\nfunc setup(e *Engine) {\n}\n",
        )
        .write(
            "routes.go",
            "package gin\n\nfunc routes(e *Engine) {\n\te.GET(\"/users\", getUser)\n}\n",
        )
        .edge_before("method Context::Next", "calls", "function getUser")
        .edge_metadata_after(
            ("method Context::Next", "calls", "function getUser"),
            "registeredAt",
            "routes.go:4",
        )
}

/// W7-15c: the dispatcher stops calling into its handler slice; it links
/// to no registered handler.
pub fn go_gin_dispatch_call_removed() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("gin.go", GO_GIN_CONTEXT)
        .file(
            "app.go",
            "package gin\n\nfunc Logger(c *Context) {}\n\nfunc setup(e *Engine) {\n\te.Use(Logger)\n}\n",
        )
        .write(
            "gin.go",
            "package gin\n\ntype HandlerFunc func(*Context)\n\ntype Context struct {\n\thandlers []HandlerFunc\n\tindex    int\n}\n\nfunc (c *Context) Next() {\n\tfor c.index < len(c.handlers) {\n\t\t_ = c.handlers[c.index]\n\t\tc.index++\n\t}\n}\n\ntype Engine struct{}\n\nfunc (e *Engine) Use(h ...HandlerFunc)          {}\nfunc (e *Engine) GET(p string, h ...HandlerFunc) {}\n",
        )
        .edge_before("method Context::Next", "calls", "function Logger")
        .no_edge_after("method Context::Next", "calls", "function Logger")
}

/// W7-15d0 (upstream #2682, #693): a member a `var ( … )` group gains is a
/// variable of its own, and the call its initializer makes is its own.
pub fn go_var_group_gains_a_member_whose_initializer_calls_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", "module example.com/shapes\n\ngo 1.22\n")
        .file(
            "errors.go",
            "package shapes\n\nfunc newError(msg string) error { return nil }\n",
        )
        .file(
            "shapes.go",
            "package shapes\n\nvar (\n\tErrNoSides = newError(\"no sides\")\n)\n",
        )
        .write(
            "shapes.go",
            "package shapes\n\nvar (\n\tErrNoSides = newError(\"no sides\")\n\tErrTooMany = newError(\"too many\")\n)\n",
        )
        .edge_before("variable ErrNoSides", "calls", "function newError")
        .edge_after("variable ErrTooMany", "calls", "function newError")
}

/// W7-15d0 (upstream #693): a call in a package-level closure, a cobra
/// `RunE`, is the variable's; it links once the function appears.
pub fn go_package_closure_call_reaches_a_function_that_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "cmd.go",
            "package app\n\ntype Command struct {\n\tRunE func(args []string) error\n}\n\nvar rootCmd = &Command{\n\tRunE: func(args []string) error {\n\t\treturn run(args)\n\t},\n}\n",
        )
        .write("run.go", "package app\n\nfunc run(args []string) error { return nil }\n")
        .edge_after("variable rootCmd", "calls", "function run @run.go")
}

/// W7-15d0 (upstream #693): a package-level composite literal instantiates
/// its struct from the variable; the struct's package file deleted, the
/// qualified literal links nothing, not the importer's own namesake.
pub fn go_package_literal_struct_file_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file(
            "refresh/refresh.go",
            "package refresh\n\ntype Options struct {\n\tName string\n}\n",
        )
        .file("app/options.go", "package app\n\ntype Options struct {\n\tName string\n}\n")
        .file(
            "app/config.go",
            "package app\n\nimport \"example.com/app/refresh\"\n\nvar defaults = refresh.Options{Name: \"x\"}\n",
        )
        .edge_before("variable defaults", "instantiates", "struct Options @refresh/refresh.go")
        .remove("refresh/refresh.go")
        .no_edge_after("variable defaults", "instantiates", "struct Options @app/options.go")
}

/// W7-15d0 (upstream #2680): every member of a `const ( … )` group is a
/// constant, so a function's read of a later member reaches it.
pub fn go_const_group_member_read_by_a_function() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", "module example.com/limits\n\ngo 1.22\n")
        .file(
            "limits.go",
            "package limits\n\nconst (\n\tBaseLimit = 10\n\tWideLimit = 50\n)\n\nfunc scale() int { return BaseLimit * 2 }\n",
        )
        .write(
            "limits.go",
            "package limits\n\nconst (\n\tBaseLimit = 10\n\tWideLimit = 50\n)\n\nfunc scale() int { return WideLimit * 2 }\n",
        )
        .edge_before("function scale", "references", "constant BaseLimit")
        .edge_after("function scale", "references", "constant WideLimit")
}

const GO_SHOP_MAIN: &str =
    "package main\n\nimport \"example.com/shop/util\"\n\nfunc main() { util.Go() }\n";
const GO_UTIL_MAIN: &str =
    "package main\n\nimport \"example.com/util\"\n\nfunc main() { util.Go() }\n";
const GO_UTIL: &str = "package util\n\nfunc Go() {}\n";

/// W7-15d1: `go.mod` is a control file. Its module path renamed, the
/// import it named is outside the project, and the call through it links
/// nothing, though no `.go` file changed.
pub fn go_module_path_renamed_unlinks_a_cross_package_call() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file("main.go", GO_SHOP_MAIN)
        .file("util/util.go", GO_UTIL)
        .edge_before("function main", "calls", "function Go @util/util.go")
        .write("go.mod", "module example.com/store\n\ngo 1.22\n")
        .no_edge_after("function main", "calls", "function Go @util/util.go")
}

/// W7-15d1: a nested module's `go.mod` appearing makes its directory the
/// package an unchanged import names.
pub fn go_nested_module_file_appears_and_links_its_importer() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file("main.go", GO_UTIL_MAIN)
        .file("util/util.go", GO_UTIL)
        .write("util/go.mod", "module example.com/util\n\ngo 1.22\n")
        .edge_after("function main", "calls", "function Go @util/util.go")
}

/// W7-15d1: the nested module's `go.mod` deleted, the import names no
/// project package again.
pub fn go_nested_module_file_deleted_unlinks_its_importer() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file("util/go.mod", "module example.com/util\n\ngo 1.22\n")
        .file("main.go", GO_UTIL_MAIN)
        .file("util/util.go", GO_UTIL)
        .edge_before("function main", "calls", "function Go @util/util.go")
        .remove("util/go.mod")
        .no_edge_after("function main", "calls", "function Go @util/util.go")
}

/// W7-15d1: the root `go.mod` deleted, the project has no module, so the
/// import is outside it.
pub fn go_root_module_file_deleted_unlinks_a_cross_package_call() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", "module example.com/shop\n\ngo 1.22\n")
        .file("main.go", GO_SHOP_MAIN)
        .file("util/util.go", GO_UTIL)
        .edge_before("function main", "calls", "function Go @util/util.go")
        .remove("go.mod")
        .no_edge_after("function main", "calls", "function Go @util/util.go")
}

/// `discovery/refresh/refresh.go` of upstream's `go-func-field-dispatch.test.ts`:
/// `Discovery.refresh` calls through `refreshf`, which `NewDiscovery` copies
/// from `Options.RefreshF`.
const GO_REFRESH: &str = "package refresh\n\nimport \"context\"\n\ntype Group struct{ Source string }\n\ntype Options struct {\n\tMech     string\n\tInterval int\n\tRefreshF func(ctx context.Context) ([]*Group, error)\n}\n\ntype Discovery struct {\n\tinterval int\n\trefreshf func(ctx context.Context) ([]*Group, error)\n}\n\nfunc NewDiscovery(opts Options) *Discovery {\n\treturn &Discovery{\n\t\tinterval: opts.Interval,\n\t\trefreshf: opts.RefreshF,\n\t}\n}\n\nfunc (d *Discovery) Run(ctx context.Context) {\n\td.refresh(ctx)\n}\n\nfunc (d *Discovery) refresh(ctx context.Context) ([]*Group, error) {\n\ttgs, err := d.refreshf(ctx)\n\treturn tgs, err\n}\n";

/// The gce provider of upstream's sync case, before it wires its function.
const GO_GCE_UNWIRED: &str = "package gce\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/discovery/refresh\"\n)\n\nfunc refreshInstances(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }\n\nfunc NewDiscovery() *refresh.Discovery {\n\treturn refresh.NewDiscovery(refresh.Options{Mech: \"gce\"})\n}\n";

/// The same provider wiring `refreshInstances` in through `Options`.
const GO_GCE_WIRED: &str = "package gce\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/discovery/refresh\"\n)\n\nfunc refreshInstances(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }\n\nfunc NewDiscovery() *refresh.Discovery {\n\treturn refresh.NewDiscovery(refresh.Options{Mech: \"gce\", RefreshF: refreshInstances})\n}\n";

const GO_APP_MOD: &str = "module example.com/app\n\ngo 1.22\n";

/// W7-15d (upstream `go-func-field-dispatch.test.ts`'s sync case): a file
/// that only wires puts its function in the field, and the call through the
/// copied field links to it.
pub fn go_func_field_wiring_appears_in_a_file_that_only_wires() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file("discovery/refresh/refresh.go", GO_REFRESH)
        .file("discovery/gce/gce.go", GO_GCE_UNWIRED)
        .write("discovery/gce/gce.go", GO_GCE_WIRED)
        .edge_after(
            "method Discovery::refresh",
            "calls",
            "function refreshInstances",
        )
}

/// W7-15d (the same upstream case's second step): the wiring goes, and so
/// does the link.
pub fn go_func_field_wiring_removed() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file("discovery/refresh/refresh.go", GO_REFRESH)
        .file("discovery/gce/gce.go", GO_GCE_WIRED)
        .edge_before(
            "method Discovery::refresh",
            "calls",
            "function refreshInstances",
        )
        .write("discovery/gce/gce.go", GO_GCE_UNWIRED)
        .no_edge_after(
            "method Discovery::refresh",
            "calls",
            "function refreshInstances",
        )
}

/// W7-15d: the field stops holding a function, so a call through it is
/// no call through a func field.
pub fn go_func_field_type_stops_being_a_func() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file(
            "jobs/jobs.go",
            "package jobs\n\ntype Job struct {\n\tfn func() error\n}\n\nfunc (j *Job) Run() error { return j.fn() }\n",
        )
        .file(
            "jobs/wire.go",
            "package jobs\n\nfunc cleanup() error { return nil }\n\nfunc NewJob() *Job { return &Job{fn: cleanup} }\n",
        )
        .edge_before("method Job::Run", "calls", "function cleanup")
        .write(
            "jobs/jobs.go",
            "package jobs\n\ntype Job struct {\n\tfn int\n}\n\nfunc (j *Job) Run() error { return j.fn() }\n",
        )
        .no_edge_after("method Job::Run", "calls", "function cleanup")
}

/// `discovery/aws/ec2.go` wiring its `refresh` method in.
const GO_EC2_REFRESH: &str = "package aws\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/discovery/refresh\"\n)\n\ntype EC2Discovery struct {\n\t*refresh.Discovery\n}\n\nfunc NewEC2Discovery() *EC2Discovery {\n\td := &EC2Discovery{}\n\td.Discovery = refresh.NewDiscovery(refresh.Options{RefreshF: d.refresh})\n\treturn d\n}\n\nfunc (d *EC2Discovery) refresh(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }\n\nfunc (d *EC2Discovery) list(ctx context.Context) ([]*refresh.Group, error)    { return nil, nil }\n";

/// The same provider wiring its `list` method instead.
const GO_EC2_LIST: &str = "package aws\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/discovery/refresh\"\n)\n\ntype EC2Discovery struct {\n\t*refresh.Discovery\n}\n\nfunc NewEC2Discovery() *EC2Discovery {\n\td := &EC2Discovery{}\n\td.Discovery = refresh.NewDiscovery(refresh.Options{RefreshF: d.list})\n\treturn d\n}\n\nfunc (d *EC2Discovery) refresh(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }\n\nfunc (d *EC2Discovery) list(ctx context.Context) ([]*refresh.Group, error)    { return nil, nil }\n";

/// W7-15d: the value another file copies into the field changes, and the
/// call through the copy follows it.
pub fn go_func_field_copied_value_changes_in_another_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file("discovery/refresh/refresh.go", GO_REFRESH)
        .file("discovery/aws/ec2.go", GO_EC2_REFRESH)
        .edge_before(
            "method Discovery::refresh",
            "calls",
            "method EC2Discovery::refresh",
        )
        .write("discovery/aws/ec2.go", GO_EC2_LIST)
        .edge_after(
            "method Discovery::refresh",
            "calls",
            "method EC2Discovery::list",
        )
        .no_edge_after(
            "method Discovery::refresh",
            "calls",
            "method EC2Discovery::refresh",
        )
}

/// W7-15d (after W7-15d0): a package-level variable's literal wires the
/// field from a file that appears.
pub fn go_func_field_package_level_wiring_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file("discovery/refresh/refresh.go", GO_REFRESH)
        .write(
            "discovery/static/static.go",
            "package static\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/discovery/refresh\"\n)\n\nfunc list(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }\n\nvar defaults = refresh.Options{Mech: \"static\", RefreshF: list}\n",
        )
        .edge_after("method Discovery::refresh", "calls", "function list")
}

/// W7-15d (after W7-15d1): the root `go.mod` renames the module, so the
/// import a qualified literal is written through names no project package
/// and the literal wires nothing.
pub fn go_func_field_module_renamed_unlinks_a_qualified_literal() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", GO_APP_MOD)
        .file("discovery/refresh/refresh.go", GO_REFRESH)
        .file("discovery/aws/ec2.go", GO_EC2_REFRESH)
        .edge_before(
            "method Discovery::refresh",
            "calls",
            "method EC2Discovery::refresh",
        )
        .write("go.mod", "module example.com/other\n\ngo 1.22\n")
        .no_edge_after(
            "method Discovery::refresh",
            "calls",
            "method EC2Discovery::refresh",
        )
}

/// W7-16a: a redux-thunk constant's body starts dispatching another thunk;
/// the constant links to it (`redux-thunk`).
pub fn ts_thunk_starts_dispatching_another_thunk() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "thunks.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\n\nexport const load = createAsyncThunk('a/load', async (_: void, { dispatch }) => {\n  return 1;\n});\n\nexport const save = createAsyncThunk('a/save', async () => 2);\n",
        )
        .write(
            "thunks.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\n\nexport const load = createAsyncThunk('a/load', async (_: void, { dispatch }) => {\n  dispatch(save());\n});\n\nexport const save = createAsyncThunk('a/save', async () => 2);\n",
        )
        .edge_metadata_after(
            ("constant load", "calls", "constant save"),
            "synthesizedBy",
            "redux-thunk",
        )
}

/// W7-16d: a second thunk of the dispatched name appears in another file;
/// the name has no one target, so the dispatch reaches neither (OD-B14).
/// The call `inner()` the dispatch makes also resolves as a base edge, which
/// stays, so the case reads the synthesized edge's metadata.
pub fn ts_thunk_dispatch_target_gains_a_namesake_thunk() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file(
            "a.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\n\nexport const inner = createAsyncThunk('a/inner', async () => 1);\n",
        )
        .file(
            "c.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\n\nexport const outer = createAsyncThunk('c/outer', async (_: void, { dispatch }) => {\n  await dispatch(inner());\n});\n",
        )
        .write(
            "b.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\n\nexport const inner = createAsyncThunk('b/inner', async () => 1);\n",
        )
        .edge_metadata_before(
            ("constant outer", "calls", "constant inner @a.ts"),
            "synthesizedBy",
            "redux-thunk",
        )
        .no_edge_metadata_after(
            ("constant outer", "calls", "constant inner @a.ts"),
            "synthesizedBy",
            "redux-thunk",
        )
        .no_edge_metadata_after(
            ("constant outer", "calls", "constant inner @b.ts"),
            "synthesizedBy",
            "redux-thunk",
        )
}

/// W7-16a: a Celery task loses its decorator, in its own file; the
/// dispatching function of another file reaches it no more.
pub fn python_celery_task_loses_its_decorator() -> Case {
    Case::new(Change::BodyEdit, &[Language::Python])
        .file(
            "tasks.py",
            "from celery import shared_task\n\n@shared_task\ndef send(to):\n    return to\n",
        )
        .file(
            "views.py",
            "from tasks import send\n\n\ndef signup(to):\n    send.delay(to)\n",
        )
        .write(
            "tasks.py",
            "from celery import shared_task\n\n# not a task\ndef send(to):\n    return to\n",
        )
        .edge_before("function signup", "calls", "function send")
        .no_edge_after("function signup", "calls", "function send")
}

/// W7-16a: a Spring listener's parameter becomes another event; the
/// publisher of the first one, in another file, reaches it no more.
pub fn java_spring_listener_hears_another_event() -> Case {
    Case::new(Change::BodyEdit, &[Language::Java])
        .file("app/Events.java", "package app;\nclass Shipped { }\nclass Cancelled { }\n")
        .file(
            "app/Service.java",
            "package app;\nclass Service {\n    void ship() {\n        publisher.publishEvent(new Shipped());\n    }\n}\n",
        )
        .file(
            "app/Listener.java",
            "package app;\nclass Listener {\n    @EventListener\n    public void on(Shipped event) { }\n}\n",
        )
        .write(
            "app/Listener.java",
            "package app;\nclass Listener {\n    @EventListener\n    public void on(Cancelled event) { }\n}\n",
        )
        .edge_before("method app::Service::ship", "calls", "method app::Listener::on")
        .no_edge_after("method app::Service::ship", "calls", "method app::Listener::on")
}

/// W7-16a: a MediatR handler's base list takes another request; the sender
/// of the first, in another file, reaches its `Handle` no more.
pub fn csharp_mediatr_handler_takes_another_request() -> Case {
    Case::new(Change::EditHeritage, &[Language::CSharp])
        .file(
            "Requests.cs",
            "namespace Shop;\npublic class Ping : IRequest<int> { }\npublic class Pong : IRequest<int> { }\n",
        )
        .file(
            "PingHandler.cs",
            "namespace Shop;\npublic class PingHandler : IRequestHandler<Ping, int> {\n    public Task<int> Handle(Ping request, CancellationToken ct) => Task.FromResult(1);\n}\n",
        )
        .file(
            "Controller.cs",
            "namespace Shop;\npublic class Controller {\n    public async Task Run() {\n        await _mediator.Send(new Ping());\n    }\n}\n",
        )
        .write(
            "PingHandler.cs",
            "namespace Shop;\npublic class PingHandler : IRequestHandler<Pong, int> {\n    public Task<int> Handle(Pong request, CancellationToken ct) => Task.FromResult(1);\n}\n",
        )
        .edge_before("method Shop::Controller::Run", "calls", "method Shop::PingHandler::Handle")
        .no_edge_after(
            "method Shop::Controller::Run",
            "calls",
            "method Shop::PingHandler::Handle",
        )
}

/// W7-16a: a Sidekiq worker drops the mixin, in its own file; the enqueue
/// of another file reaches its `perform` no more.
pub fn ruby_sidekiq_worker_drops_its_mixin() -> Case {
    Case::new(Change::BodyEdit, &[Language::Ruby])
        .file(
            "app/workers/cleanup_worker.rb",
            "class CleanupWorker\n  include Sidekiq::Worker\n  def perform(id)\n  end\nend\n",
        )
        .file(
            "app/services/service.rb",
            "class Service\n  def run\n    CleanupWorker.perform_async(1)\n  end\nend\n",
        )
        .write(
            "app/workers/cleanup_worker.rb",
            "class CleanupWorker\n  include Helpers\n  def perform(id)\n  end\nend\n",
        )
        .edge_before(
            "method Service::run",
            "calls",
            "method CleanupWorker::perform",
        )
        .no_edge_after(
            "method Service::run",
            "calls",
            "method CleanupWorker::perform",
        )
}

/// W7-16d: another file writes `class Comments::NotifyWorker` with the
/// mixin and a `perform`; the qualified name has no one worker, so the
/// enqueue reaches neither (OD-B14).
pub fn ruby_sidekiq_qualified_worker_is_reopened_with_a_perform() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Ruby])
        .file(
            "app/workers/comments/notify_worker.rb",
            "module Comments\n  class NotifyWorker\n    include Sidekiq::Job\n    def perform(id)\n    end\n  end\nend\n",
        )
        .file(
            "app/services/notifier.rb",
            "class Notifier\n  def run\n    Comments::NotifyWorker.perform_async(1)\n  end\nend\n",
        )
        .write(
            "app/workers/legacy/notify_worker.rb",
            "class Comments::NotifyWorker\n  include Sidekiq::Job\n  def perform(id)\n  end\nend\n",
        )
        .edge_before(
            "method Notifier::run",
            "calls",
            "method Comments::NotifyWorker::perform @app/workers/comments/notify_worker.rb",
        )
        .no_edge_after(
            "method Notifier::run",
            "calls",
            "method Comments::NotifyWorker::perform @app/workers/comments/notify_worker.rb",
        )
        .no_edge_after(
            "method Notifier::run",
            "calls",
            "method Comments::NotifyWorker::perform @app/workers/legacy/notify_worker.rb",
        )
}

/// W7-16a: an event service provider's `$listen` map gains a listener, in a
/// file of its own; the dispatcher of the event reaches its `handle`.
pub fn php_laravel_listen_map_gains_a_listener() -> Case {
    Case::new(Change::BodyEdit, &[Language::Php])
        .file(
            "app/Providers/EventServiceProvider.php",
            "<?php\nnamespace App\\Providers;\nclass EventServiceProvider {\n    protected $listen = [\n        Saved::class => [],\n    ];\n}\n",
        )
        .file(
            "app/Listeners/Notify.php",
            "<?php\nnamespace App\\Listeners;\nclass Notify {\n    public function handle($event) {}\n}\n",
        )
        .file(
            "app/Services/Service.php",
            "<?php\nnamespace App\\Services;\nclass Service {\n    public function save() { event(new Saved()); }\n}\n",
        )
        .write(
            "app/Providers/EventServiceProvider.php",
            "<?php\nnamespace App\\Providers;\nclass EventServiceProvider {\n    protected $listen = [\n        Saved::class => [Notify::class],\n    ];\n}\n",
        )
        .edge_metadata_after(
            (
                "method App\\Services::Service::save",
                "calls",
                "method App\\Listeners::Notify::handle",
            ),
            "synthesizedBy",
            "laravel-event",
        )
}

/// W7-16b: a Pinia options store's `actions` gains a member, which calls a
/// function of another file; the member is a node of its own that owns the
/// call, and an unchanged file's call of its name reaches it.
pub fn ts_pinia_options_store_gains_an_action() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "lib.ts",
            "export function track(item: string) { return item; }\nexport function clearAll() { return []; }\n",
        )
        .file("other.ts", "export function wipe() { return reset(); }\n")
        .file(
            "store.ts",
            "import { defineStore } from 'pinia';\nimport { track } from './lib';\nexport const useCartStore = defineStore('cart', {\n  actions: {\n    add(item: string) { track(item); },\n  },\n});\n",
        )
        .write(
            "store.ts",
            "import { defineStore } from 'pinia';\nimport { track, clearAll } from './lib';\nexport const useCartStore = defineStore('cart', {\n  actions: {\n    add(item: string) { track(item); },\n    reset() { clearAll(); },\n  },\n});\n",
        )
        .edge_before("function useCartStore::add", "calls", "function track")
        .edge_after("function useCartStore::reset", "calls", "function clearAll")
        .edge_after("function wipe", "calls", "function useCartStore::reset")
}

/// W7-16b: the api file's destructuring gains the RTK Query hook an
/// unchanged component already imports and calls; the call reaches the hook.
pub fn ts_rtk_api_gains_a_hook_binding() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript, Language::Tsx])
        .file(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nexport const api = createApi({\n  endpoints: (build) => ({\n    getPosts: build.query({ query: () => '/posts' }),\n    getUser: build.query({ query: () => '/user' }),\n  }),\n});\nexport const { useGetPostsQuery } = api;\n",
        )
        .file(
            "Profile.tsx",
            "import { useGetUserQuery } from './api';\nexport function Profile() {\n  return useGetUserQuery();\n}\n",
        )
        .write(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nexport const api = createApi({\n  endpoints: (build) => ({\n    getPosts: build.query({ query: () => '/posts' }),\n    getUser: build.query({ query: () => '/user' }),\n  }),\n});\nexport const { useGetPostsQuery, useGetUserQuery } = api;\n",
        )
        .named_before("function", "useGetPostsQuery")
        .edge_after("function Profile", "calls", "function useGetUserQuery @api.ts")
}

/// W7-16b: a module's default export gains a second store word, so the file
/// reads like a Vuex store; its `actions` member becomes a node that owns the
/// call the file made before.
pub fn js_vuex_module_becomes_a_store_file() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file("api.js", "export function apiLogin(info) { return info; }\n")
        .file(
            "store/user.js",
            "import { apiLogin } from '../api';\nexport default {\n  actions: {\n    login(context, info) { apiLogin(info); },\n  },\n};\n",
        )
        .write(
            "store/user.js",
            "import { apiLogin } from '../api';\nexport default {\n  namespaced: true,\n  actions: {\n    login(context, info) { apiLogin(info); },\n  },\n};\n",
        )
        .edge_before("file:store/user.js", "calls", "function apiLogin")
        .edge_after("function login @store/user.js", "calls", "function apiLogin")
}

/// W7-16b: an RTK Query endpoint's key is renamed; the endpoint node follows
/// with the call its handler makes.
pub fn ts_rtk_endpoint_key_is_renamed() -> Case {
    Case::new(Change::RenameSymbol, &[Language::TypeScript])
        .file("fetchers.ts", "export function fetchPosts() { return []; }\n")
        .file(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nimport { fetchPosts } from './fetchers';\nexport const api = createApi({\n  endpoints: (build) => ({\n    getPosts: build.query({ queryFn: () => fetchPosts() }),\n  }),\n});\n",
        )
        .write(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nimport { fetchPosts } from './fetchers';\nexport const api = createApi({\n  endpoints: (build) => ({\n    listPosts: build.query({ queryFn: () => fetchPosts() }),\n  }),\n});\n",
        )
        .edge_before("function api::getPosts", "calls", "function fetchPosts")
        .edge_after("function api::listPosts", "calls", "function fetchPosts")
}

/// W7-16b: the RTK api file gains a function named like an endpoint; the
/// hook's endpoint has no one function of its name in the file, so the hook
/// reaches neither (hard invariant 6).
pub fn ts_rtk_api_gains_a_namesake_of_an_endpoint() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nexport const api = createApi({\n  endpoints: (build) => ({ getPosts: build.query({ query: () => '/posts' }) }),\n});\nexport const { useGetPostsQuery } = api;\n",
        )
        .write(
            "api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nexport const api = createApi({\n  endpoints: (build) => ({ getPosts: build.query({ query: () => '/posts' }) }),\n});\nexport const { useGetPostsQuery } = api;\nexport function getPosts() { return []; }\n",
        )
        .edge_metadata_before(
            ("function useGetPostsQuery", "calls", "function api::getPosts"),
            "synthesizedBy",
            "rtk-query",
        )
        .no_edge_metadata_after(
            ("function useGetPostsQuery", "calls", "function api::getPosts"),
            "synthesizedBy",
            "rtk-query",
        )
        .no_edge_metadata_after(
            ("function useGetPostsQuery", "calls", "function getPosts"),
            "synthesizedBy",
            "rtk-query",
        )
}

/// W7-16b: another file defines the Pinia factory a consumer binds; the
/// name is no one store's, so the consumer's call reaches neither action.
pub fn ts_pinia_factory_gains_a_namesake_in_another_file() -> Case {
    Case::new(Change::AddCompetitor, &[Language::TypeScript])
        .file(
            "cart.ts",
            "import { defineStore } from 'pinia';\nexport const useCartStore = defineStore('cart', {\n  actions: { add(item: string) { this.items.push(item); } },\n});\n",
        )
        .file(
            "checkout.ts",
            "import { useCartStore } from './cart';\nexport function checkout() {\n  const cart = useCartStore();\n  cart.add('x');\n}\n",
        )
        .write(
            "legacy/cart.ts",
            "import { defineStore } from 'pinia';\nexport const useCartStore = defineStore('legacy-cart', {\n  actions: { add(item: string) { this.items.push(item); } },\n});\n",
        )
        .edge_metadata_before(
            ("function checkout", "calls", "function useCartStore::add @cart.ts"),
            "synthesizedBy",
            "pinia-store",
        )
        .no_edge_metadata_after(
            ("function checkout", "calls", "function useCartStore::add @cart.ts"),
            "synthesizedBy",
            "pinia-store",
        )
        .no_edge_metadata_after(
            (
                "function checkout",
                "calls",
                "function useCartStore::add @legacy/cart.ts",
            ),
            "synthesizedBy",
            "pinia-store",
        )
}

/// W7-16b: a Pinia options store gains the action an unchanged consumer
/// already calls; the call reaches it.
pub fn ts_pinia_store_gains_the_action_its_consumer_calls() -> Case {
    Case::new(Change::BodyEdit, &[Language::TypeScript])
        .file(
            "cart.ts",
            "import { defineStore } from 'pinia';\nexport const useCartStore = defineStore('cart', {\n  actions: { add(item: string) { this.items.push(item); } },\n});\n",
        )
        .file(
            "checkout.ts",
            "import { useCartStore } from './cart';\nexport function checkout() {\n  const cart = useCartStore();\n  cart.add('x');\n  cart.clear();\n}\n",
        )
        .write(
            "cart.ts",
            "import { defineStore } from 'pinia';\nexport const useCartStore = defineStore('cart', {\n  actions: {\n    add(item: string) { this.items.push(item); },\n    clear() { this.items = []; },\n  },\n});\n",
        )
        .edge_metadata_before(
            ("function checkout", "calls", "function useCartStore::add"),
            "synthesizedBy",
            "pinia-store",
        )
        .edge_metadata_after(
            ("function checkout", "calls", "function useCartStore::clear"),
            "synthesizedBy",
            "pinia-store",
        )
}

/// W7-16b: a second store file of a Vuex namespace gains the action a
/// namespaced dispatch names; the namespace has no one candidate, so the
/// dispatch reaches neither (hard invariant 6).
pub fn js_vuex_namespace_gains_a_second_store_file_of_the_action() -> Case {
    Case::new(Change::AddCompetitor, &[Language::JavaScript])
        .file(
            "store/modules/user.js",
            "export default {\n  namespaced: true,\n  actions: { login() { return 1; } },\n};\n",
        )
        .file(
            "app.js",
            "export function bootstrap(store) {\n  store.dispatch('user/login');\n}\n",
        )
        .write(
            "store/user/actions.js",
            "export default {\n  namespaced: true,\n  actions: { login() { return 2; } },\n};\n",
        )
        .edge_metadata_before(
            (
                "function bootstrap",
                "calls",
                "function login @store/modules/user.js",
            ),
            "synthesizedBy",
            "vuex-dispatch",
        )
        .no_edge_metadata_after(
            (
                "function bootstrap",
                "calls",
                "function login @store/modules/user.js",
            ),
            "synthesizedBy",
            "vuex-dispatch",
        )
        .no_edge_metadata_after(
            (
                "function bootstrap",
                "calls",
                "function login @store/user/actions.js",
            ),
            "synthesizedBy",
            "vuex-dispatch",
        )
}

/// W7-16b: a Vuex module gains the mutation its action already commits; the
/// commit reaches it.
pub fn js_vuex_module_gains_the_mutation_its_action_commits() -> Case {
    Case::new(Change::BodyEdit, &[Language::JavaScript])
        .file(
            "store/user.js",
            "export default {\n  namespaced: true,\n  actions: { save({ commit }) { commit('SAVED'); } },\n};\n",
        )
        .write(
            "store/user.js",
            "export default {\n  namespaced: true,\n  mutations: { SAVED(state) { state.saved = true; } },\n  actions: { save({ commit }) { commit('SAVED'); } },\n};\n",
        )
        .named_before("function", "save")
        .edge_metadata_after(
            ("function save", "calls", "function SAVED"),
            "synthesizedBy",
            "vuex-dispatch",
        )
}

/// W7-17a: a C table gains an entry; the dispatch in another file, unchanged,
/// reaches it.
pub fn c_fnptr_table_gains_an_entry() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file("ops.h", "struct ops { void (*run)(void); };\n")
        .file(
            "tab.c",
            "#include \"ops.h\"\nvoid one(void) {}\nvoid two(void) {}\nstatic struct ops table[] = { { one } };\n",
        )
        .file(
            "go.c",
            "#include \"ops.h\"\nvoid go(struct ops *p) { p->run(); }\n",
        )
        .write(
            "tab.c",
            "#include \"ops.h\"\nvoid one(void) {}\nvoid two(void) {}\nstatic struct ops table[] = { { one }, { two } };\n",
        )
        .edge_before("function go", "calls", "function one")
        .edge_after("function go", "calls", "function two")
        .edge_metadata_after(("function go", "calls", "function two"), "via", "ops.run")
}

/// W7-17a: a header's function-pointer typedef becomes a data typedef; the
/// field it types is no function pointer, so the dispatch reaches nothing.
pub fn c_fnptr_header_typedef_becomes_data() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file(
            "ops.h",
            "typedef void (*handler_t)(void);\nstruct ops { handler_t run; };\n",
        )
        .file(
            "tab.c",
            "#include \"ops.h\"\nvoid one(void) {}\nstatic struct ops table[] = { { one } };\n",
        )
        .file(
            "go.c",
            "#include \"ops.h\"\nvoid go(struct ops *p) { p->run(); }\n",
        )
        .write(
            "ops.h",
            "typedef int handler_t;\nstruct ops { handler_t run; };\n",
        )
        .edge_before("function go", "calls", "function one")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17a: the macro that builds a table, defined in an included header,
/// swaps its arguments; the function lands in the name's position.
pub fn c_fnptr_header_table_macro_changes_its_expansion() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file(
            "cmd.h",
            "struct cmd { const char *name; void (*fn)(void); };\n#define ENTRY(name, fn) { name, fn }\n",
        )
        .file(
            "cmd.c",
            "#include \"cmd.h\"\nvoid one(void) {}\nstatic struct cmd table[] = { ENTRY(\"one\", one) };\nvoid go(struct cmd *p) { p->fn(); }\n",
        )
        .write(
            "cmd.h",
            "struct cmd { const char *name; void (*fn)(void); };\n#define ENTRY(name, fn) { fn, name }\n",
        )
        .edge_before("function go", "calls", "function one")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17a: a dispatch reads another field of the struct.
pub fn c_fnptr_dispatch_reads_another_field() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file(
            "ops.c",
            "struct ops { void (*run)(void); void (*stop)(void); };\nvoid one(void) {}\nvoid two(void) {}\nstatic struct ops table = { one, two };\n",
        )
        .file("go.c", "void go(struct ops *p) { p->run(); }\n")
        .write("go.c", "void go(struct ops *p) { p->stop(); }\n")
        .edge_before("function go", "calls", "function one")
        .edge_after("function go", "calls", "function two")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17a (#1172): a C++ local function pointer is bound to another
/// function; the call through it follows.
pub fn cpp_local_fn_ptr_binds_another_function() -> Case {
    Case::new(Change::BodyEdit, &[Language::Cpp])
        .file(
            "launch.cpp",
            "void f() {}\nvoid g() {}\nvoid run() { auto k = &f; k(); }\n",
        )
        .write(
            "launch.cpp",
            "void f() {}\nvoid g() {}\nvoid run() { auto k = &g; k(); }\n",
        )
        .edge_before("function run", "calls", "function f")
        .edge_after("function run", "calls", "function g")
        .no_edge_after("function run", "calls", "function f")
}

/// W7-17a: of two files defining `struct ops` with their fields in a
/// different order, one is renamed past the other; each dispatch still
/// reaches only its own table.
pub fn c_fnptr_struct_definitions_change_their_order() -> Case {
    Case::new(Change::RenameSymbol, &[Language::C])
        .file(
            "a.c",
            "struct ops { int n; void (*run)(void); };\nvoid a_run(void) {}\nstatic struct ops a_tab[] = { { 1, a_run } };\nvoid a_go(struct ops *p) { p->run(); }\n",
        )
        .file(
            "b.c",
            "struct ops { void (*run)(void); int n; };\nvoid b_run(void) {}\nstatic struct ops b_tab[] = { { b_run, 1 } };\nvoid b_go(struct ops *p) { p->run(); }\n",
        )
        .rename("a.c", "z.c")
        .edge_before("function a_go", "calls", "function a_run")
        .edge_before("function b_go", "calls", "function b_run")
        .edge_after("function a_go", "calls", "function a_run")
        .edge_after("function b_go", "calls", "function b_run")
        .no_edge_after("function a_go", "calls", "function b_run")
        .no_edge_after("function b_go", "calls", "function a_run")
}

/// W7-17a: a table entry named by one function elsewhere gains a namesake
/// in another file and is registered no more.
pub fn c_fnptr_entry_gains_a_namesake() -> Case {
    Case::new(Change::AddCompetitor, &[Language::C])
        .file(
            "t.c",
            "struct ops { void (*run)(void); };\nstatic struct ops tab = { handler };\nvoid go(struct ops *p) { p->run(); }\n",
        )
        .file("x.c", "void handler(void) {}\n")
        .write("y.c", "void handler(void) {}\n")
        .edge_before("function go", "calls", "function handler")
        .no_edge_after("function go", "calls", "function handler")
}

/// W7-17a: two files define the same `struct ops`, each with its table;
/// one table gains an entry, and the other file's dispatch does not reach
/// it.
pub fn c_fnptr_identical_struct_table_gains_an_entry() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file(
            "a.c",
            "struct ops { void (*run)(void); };\nvoid a_run(void) {}\nstatic struct ops a_tab[] = { { a_run } };\nvoid a_go(struct ops *p) { p->run(); }\n",
        )
        .file(
            "b.c",
            "struct ops { void (*run)(void); };\nvoid b_run(void) {}\nvoid b_two(void) {}\nstatic struct ops b_tab[] = { { b_run } };\nvoid b_go(struct ops *p) { p->run(); }\n",
        )
        .write(
            "b.c",
            "struct ops { void (*run)(void); };\nvoid b_run(void) {}\nvoid b_two(void) {}\nstatic struct ops b_tab[] = { { b_run }, { b_two } };\nvoid b_go(struct ops *p) { p->run(); }\n",
        )
        .edge_before("function a_go", "calls", "function a_run")
        .edge_after("function b_go", "calls", "function b_two")
        .no_edge_after("function a_go", "calls", "function b_two")
}

/// W7-17a: two files share a header's `struct ops`, so each dispatch
/// reaches both tables; one file takes its own copy of the definition, and
/// each reaches only its own.
pub fn c_fnptr_shared_definition_splits() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file("ops.h", "struct ops { void (*run)(void); };\n")
        .file(
            "a.c",
            "#include \"ops.h\"\nvoid a_run(void) {}\nstatic struct ops a_tab[] = { { a_run } };\nvoid a_go(struct ops *p) { p->run(); }\n",
        )
        .file(
            "b.c",
            "#include \"ops.h\"\nvoid b_run(void) {}\nstatic struct ops b_tab[] = { { b_run } };\nvoid b_go(struct ops *p) { p->run(); }\n",
        )
        .write(
            "b.c",
            "struct ops { void (*run)(void); };\nvoid b_run(void) {}\nstatic struct ops b_tab[] = { { b_run } };\nvoid b_go(struct ops *p) { p->run(); }\n",
        )
        .edge_before("function a_go", "calls", "function b_run")
        .edge_after("function a_go", "calls", "function a_run")
        .edge_after("function b_go", "calls", "function b_run")
        .no_edge_after("function a_go", "calls", "function b_run")
        .no_edge_after("function b_go", "calls", "function a_run")
}

/// W7-17a: a header's struct gains an `#ifdef` field before its function
/// pointer; the positional entry is registered no more, the designated one
/// still is.
pub fn c_fnptr_header_struct_gains_a_conditional_field() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file("ops.h", "struct ops {\n    void (*run)(void);\n};\n")
        .file(
            "tab.c",
            "#include \"ops.h\"\nvoid one(void) {}\nvoid two(void) {}\nstatic struct ops pos = { one };\nstatic struct ops named = { .run = two };\nvoid go(struct ops *p) { p->run(); }\n",
        )
        .write(
            "ops.h",
            "struct ops {\n#ifdef EXTRA\n    int extra;\n#endif\n    void (*run)(void);\n};\n",
        )
        .edge_before("function go", "calls", "function one")
        .edge_after("function go", "calls", "function two")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17a: the reverse: the `#ifdef` field goes, and the positional entry
/// is registered again.
pub fn c_fnptr_header_struct_drops_its_conditional_field() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file(
            "ops.h",
            "struct ops {\n#ifdef EXTRA\n    int extra;\n#endif\n    void (*run)(void);\n};\n",
        )
        .file(
            "tab.c",
            "#include \"ops.h\"\nvoid one(void) {}\nvoid two(void) {}\nstatic struct ops pos = { one };\nstatic struct ops named = { .run = two };\nvoid go(struct ops *p) { p->run(); }\n",
        )
        .write("ops.h", "struct ops {\n    void (*run)(void);\n};\n")
        .edge_before("function go", "calls", "function two")
        .edge_after("function go", "calls", "function one")
}

/// W7-17b: the C file that dispatches and includes a `.def`, which the scan
/// does not index, and the struct it registers into.
const FNPTR_DEF_OPS_H: &str = "struct ops { void (*run)(void); };\n";
const FNPTR_DEF_GO_C: &str = "#include \"ops.h\"\nvoid one(void) {}\nvoid two(void) {}\n#include \"tab.def\"\nvoid go(struct ops *p) { p->run(); }\n";
const FNPTR_DEF_TAB: &str = "static struct ops table[] = { { one } };\n";

/// W7-17b: only the `.def` the unchanged includer reads changes, and its
/// table gains an entry.
pub fn c_fnptr_non_indexed_include_gains_an_entry() -> Case {
    Case::new(Change::BodyEdit, &[Language::C])
        .file("ops.h", FNPTR_DEF_OPS_H)
        .file("go.c", FNPTR_DEF_GO_C)
        .file("tab.def", FNPTR_DEF_TAB)
        .write(
            "tab.def",
            "static struct ops table[] = { { one }, { two } };\n",
        )
        .edge_before("function go", "calls", "function one")
        .edge_after("function go", "calls", "function two")
        .edge_metadata_after(("function go", "calls", "function two"), "via", "ops.run")
}

/// W7-17b: the `.def` an include names appears, though no file the index
/// holds changes.
pub fn c_fnptr_non_indexed_include_appears() -> Case {
    Case::new(Change::AddImportTarget, &[Language::C])
        .file("ops.h", FNPTR_DEF_OPS_H)
        .file("go.c", FNPTR_DEF_GO_C)
        .write("tab.def", FNPTR_DEF_TAB)
        .edge_after("function go", "calls", "function one")
}

/// W7-17b: the `.def` is deleted.
pub fn c_fnptr_non_indexed_include_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::C])
        .file("ops.h", FNPTR_DEF_OPS_H)
        .file("go.c", FNPTR_DEF_GO_C)
        .file("tab.def", FNPTR_DEF_TAB)
        .remove("tab.def")
        .edge_before("function go", "calls", "function one")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17b: the root `.gitignore` comes to ignore the `.def`, which the
/// stage then no longer reads.
pub fn c_fnptr_non_indexed_include_becomes_gitignored() -> Case {
    Case::new(Change::ControlFile, &[Language::C])
        .file("ops.h", FNPTR_DEF_OPS_H)
        .file("go.c", FNPTR_DEF_GO_C)
        .file("tab.def", FNPTR_DEF_TAB)
        .write(".gitignore", "*.def\n")
        .edge_before("function go", "calls", "function one")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-17b: the configuration comes to exclude the `.def`.
pub fn c_fnptr_non_indexed_include_becomes_excluded() -> Case {
    Case::new(Change::ControlFile, &[Language::C])
        .file("ops.h", FNPTR_DEF_OPS_H)
        .file("go.c", FNPTR_DEF_GO_C)
        .file("tab.def", FNPTR_DEF_TAB)
        .write(
            ".codegraph/config.toml",
            "[app]\nname = \"matrix\"\n\n[indexing]\nexclude = [\"tab.def\"]\n",
        )
        .edge_before("function go", "calls", "function one")
        .no_edge_after("function go", "calls", "function one")
}

/// W7-16c: the root `go.mod` of a GoFrame app.
const GOFRAME_GO_MOD: &str = "module example.com/app\n\nrequire github.com/gogf/gf/v2 v2.7.0\n";

/// W7-16c: two `g.Meta` request types of a GoFrame api.
const GOFRAME_DEPT_API: &str = "package system\n\nimport \"github.com/gogf/gf/v2/frame/g\"\n\ntype DeptSearchReq struct {\n\tg.Meta `path:\"/dept/list\" method:\"get\"`\n}\ntype DeptSearchRes struct{}\n\ntype DeptAddReq struct {\n\tg.Meta `path:\"/dept/add\" method:\"post\"`\n}\ntype DeptAddRes struct{}\n";

/// W7-16c: [`GOFRAME_DEPT_API`] with `POST /dept/add` moved to `/dept/create`.
const GOFRAME_DEPT_API_CREATE: &str = "package system\n\nimport \"github.com/gogf/gf/v2/frame/g\"\n\ntype DeptSearchReq struct {\n\tg.Meta `path:\"/dept/list\" method:\"get\"`\n}\ntype DeptSearchRes struct{}\n\ntype DeptAddReq struct {\n\tg.Meta `path:\"/dept/create\" method:\"post\"`\n}\ntype DeptAddRes struct{}\n";

/// W7-16c: the controller serving [`GOFRAME_DEPT_API`]'s routes.
const GOFRAME_DEPT_CONTROLLER: &str = "package controller\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/api/system\"\n)\n\ntype ctl struct{}\n\nfunc (c *ctl) List(ctx context.Context, req *system.DeptSearchReq) (res *system.DeptSearchRes, err error) {\n\treturn\n}\n\nfunc (c *ctl) Add(ctx context.Context, req *system.DeptAddReq) (res *system.DeptAddRes, err error) {\n\treturn\n}\n";

/// W7-16c: a controller of `Add` only.
const GOFRAME_ADD_CONTROLLER: &str = "package controller\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/api/system\"\n)\n\ntype ctl struct{}\n\nfunc (c *ctl) Add(ctx context.Context, req *system.DeptAddReq) (res *system.DeptAddRes, err error) {\n\treturn\n}\n";

/// W7-16c: [`GOFRAME_ADD_CONTROLLER`] with `Add` taking the search request.
const GOFRAME_ADD_TAKES_SEARCH: &str = "package controller\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/api/system\"\n)\n\ntype ctl struct{}\n\nfunc (c *ctl) Add(ctx context.Context, req *system.DeptSearchReq) (res *system.DeptAddRes, err error) {\n\treturn\n}\n";

/// W7-16c: `POST /dept/add`'s route by its request type.
const GOFRAME_ADD_ROUTE: &str = "route api/system/dept.go::goframe-route:system.DeptAddReq";
/// W7-16c: `GET /dept/list`'s route by its request type.
const GOFRAME_SEARCH_ROUTE: &str = "route api/system/dept.go::goframe-route:system.DeptSearchReq";

/// W7-16c: the root `go.mod` comes to require GoFrame: the request types
/// become routes, joined to their controller methods.
pub fn go_goframe_go_mod_comes_to_require_goframe() -> Case {
    Case::new(Change::ControlFile, &[Language::Go])
        .file("go.mod", "module example.com/app\n\ngo 1.22\n")
        .file("api/system/dept.go", GOFRAME_DEPT_API)
        .file("internal/controller/dept.go", GOFRAME_DEPT_CONTROLLER)
        .write("go.mod", GOFRAME_GO_MOD)
        .named_after("route", "POST /dept/add")
        .edge_after(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .edge_after(GOFRAME_SEARCH_ROUTE, "calls", "method ctl::List")
}

/// W7-16c: the handler comes to take another request type; its route
/// follows the parameter.
pub fn go_goframe_handler_takes_another_request_type() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GOFRAME_GO_MOD)
        .file("api/system/dept.go", GOFRAME_DEPT_API)
        .file("internal/controller/dept.go", GOFRAME_ADD_CONTROLLER)
        .write("internal/controller/dept.go", GOFRAME_ADD_TAKES_SEARCH)
        .edge_before(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .no_edge_after(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .edge_after(GOFRAME_SEARCH_ROUTE, "calls", "method ctl::Add")
}

/// W7-16c: a `g.Meta` tag's path changes: the route is renamed, with a new
/// id, and its edge follows.
pub fn go_goframe_route_path_is_changed() -> Case {
    Case::new(Change::BodyEdit, &[Language::Go])
        .file("go.mod", GOFRAME_GO_MOD)
        .file("api/system/dept.go", GOFRAME_DEPT_API)
        .file("internal/controller/dept.go", GOFRAME_DEPT_CONTROLLER)
        .write("api/system/dept.go", GOFRAME_DEPT_API_CREATE)
        .named_before("route", "POST /dept/add")
        .named_after("route", "POST /dept/create")
        .edge_before(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .edge_after(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
}

/// W7-16c: another controller directory gains a method taking the same
/// request type, in the same addon root: the route has two handlers and
/// links neither.
pub fn go_goframe_another_controller_takes_the_request_type() -> Case {
    Case::new(Change::AddCompetitor, &[Language::Go])
        .file("go.mod", GOFRAME_GO_MOD)
        .file("api/system/dept.go", GOFRAME_DEPT_API)
        .file("internal/controller/dept.go", GOFRAME_DEPT_CONTROLLER)
        .write(
            "internal/controller/v2/dept.go",
            "package v2\n\nimport (\n\t\"context\"\n\n\t\"example.com/app/api/system\"\n)\n\ntype ctl2 struct{}\n\nfunc (c *ctl2) Add(ctx context.Context, req *system.DeptAddReq) (res *system.DeptAddRes, err error) {\n\treturn\n}\n",
        )
        .edge_before(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .no_edge_after(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .edge_after(GOFRAME_SEARCH_ROUTE, "calls", "method ctl::List")
}

/// W7-16c: the controller file is deleted: its routes stay, unlinked.
pub fn go_goframe_controller_file_is_deleted() -> Case {
    Case::new(Change::DeleteImportedModule, &[Language::Go])
        .file("go.mod", GOFRAME_GO_MOD)
        .file("api/system/dept.go", GOFRAME_DEPT_API)
        .file("internal/controller/dept.go", GOFRAME_DEPT_CONTROLLER)
        .remove("internal/controller/dept.go")
        .edge_before(GOFRAME_ADD_ROUTE, "calls", "method ctl::Add")
        .named_after("route", "POST /dept/add")
        .named_after("route", "GET /dept/list")
}
