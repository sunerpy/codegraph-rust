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

/// Upstream #2403: a Liquid `render` gains its snippet only on a fresh index.
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

/// Found by this matrix: the remote call is parked as `util::double/1`, a name
/// no added node carries, so the sync never retries it.
pub fn erlang_remote_call_into_an_added_module() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Erlang])
        .file(
            "src/main.erl",
            "-module(main).\n-export([run/1]).\n\nrun(X) -> util:double(X).\n",
        )
        .write(
            "src/util.erl",
            "-module(util).\n-export([double/1]).\n\ndouble(X) -> X * 2.\n",
        )
        .edge_after("function main::run/1", "calls", "function util::double/1")
}

/// Found by this matrix: the include is parked as
/// `app.CommonMapper::userColumns`, which no added node is named.
pub fn mybatis_include_of_an_added_fragment() -> Case {
    Case::new(Change::AddImportTarget, &[Language::Xml])
        .file(
            "mapper/UserMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.UserMapper\">\n  <select id=\"findAll\" resultType=\"app.User\">\n    SELECT <include refid=\"app.CommonMapper.userColumns\"/> FROM users\n  </select>\n</mapper>\n",
        )
        .write(
            "mapper/CommonMapper.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mapper namespace=\"app.CommonMapper\">\n  <sql id=\"userColumns\">id, name, email</sql>\n</mapper>\n",
        )
        .edge_after(
            "method app.UserMapper::findAll",
            "references",
            "method app.CommonMapper::userColumns",
        )
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

// ----- a call through a class name and the heads it reads (W5-07) ------------
//
// Upstream #2180 (W3-06): a call through a class name reaches the method the
// class inherits, walking the supertypes each class's head names, read from
// source (`class_heads::declared_supertypes`). No sync lane records those
// heads, so a head changed in one file leaves such a call in an unchanged
// file at its old target (W5-07 adds the lane).

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

/// Found by this matrix: only a fresh index detects React, mints the hook node
/// and resolves the call through it.
pub fn typescript_react_dependency_appears_in_package_json() -> Case {
    Case::new(Change::ControlFile, &[Language::TypeScript])
        .file(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\"}}\n",
        )
        .file(
            "src/hooks.ts",
            "export function useCart(): number {\n  return 1;\n}\n",
        )
        .file(
            "src/page.ts",
            "export function page(): number {\n  return useCart();\n}\n",
        )
        .write(
            "package.json",
            "{\"name\":\"web\",\"dependencies\":{\"left-pad\":\"1.3.0\",\"react\":\"18.2.0\"}}\n",
        )
        .edge_before("function page", "calls", "function useCart")
        .edge_after("function page", "calls", "function src/hooks.ts::useCart")
}

// ----- a `.h` header changes from C to C++ ----------------------------------

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
