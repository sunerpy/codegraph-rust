//! The convergence cases, one function per case; `main.rs` registers each in
//! its `matrix!` list. Fixtures stay minimal: two or three files that hold one
//! cross-file edge the edit has to keep, move or drop.
//!
//! Selectors use the probe notation (`tests/probes/support.rs`). An
//! `edge_before`/`edge_after` expectation proves that the `before` index or a
//! fresh index of the final tree holds the relationship the case is about.

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
        .edge_throughout("function main", "instantiates", "class Greeter")
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
        .edge_before("constant shown", "calls", "function formatName")
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
        .edge_before("constant today", "calls", "function formatDate")
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
}
