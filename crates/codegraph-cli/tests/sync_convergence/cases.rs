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
        .edge_after(
            "namespace App @src/Svc.cs",
            "imports",
            "namespace App.Models",
        )
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
