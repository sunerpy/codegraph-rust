use tree_sitter::{Language, Parser};

fn smoke(name: &str, language: Language, source: &str) -> bool {
    let mut parser = Parser::new();
    let result = parser
        .set_language(&language)
        .map_err(|error| format!("set_language: {error}"))
        .and_then(|()| {
            parser
                .parse(source, None)
                .ok_or_else(|| String::from("parse returned None"))
        })
        .and_then(|tree| {
            if tree.root_node().has_error() {
                Err(format!("root has ERROR: {}", tree.root_node().to_sexp()))
            } else {
                Ok(())
            }
        });

    match result {
        Ok(()) => {
            println!("{name}: PASS");
            true
        }
        Err(error) => {
            println!("{name}: FAIL({error})");
            false
        }
    }
}

fn custom(name: &str, reason: &str) {
    println!("{name}: CUSTOM ({reason})");
}

fn main() {
    let mut failures = 0usize;

    let checks = [
        smoke(
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "const x: number = 1;",
        ),
        smoke(
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            "const x = 1;",
        ),
        smoke(
            "tsx",
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "const x = <div>{1}</div>;",
        ),
        smoke(
            "jsx",
            tree_sitter_javascript::LANGUAGE.into(),
            "const x = <div>{1}</div>;",
        ),
        smoke(
            "python",
            tree_sitter_python::LANGUAGE.into(),
            "def f():\n    return 1\n",
        ),
        smoke(
            "go",
            tree_sitter_go::LANGUAGE.into(),
            "package main\nfunc main() {}\n",
        ),
        smoke("rust", tree_sitter_rust::LANGUAGE.into(), "fn main() {}"),
        smoke(
            "java",
            tree_sitter_java::LANGUAGE.into(),
            "class A { void f() {} }",
        ),
        smoke(
            "c",
            tree_sitter_c::LANGUAGE.into(),
            "int main(void) { return 0; }",
        ),
        smoke(
            "cpp",
            tree_sitter_cpp::LANGUAGE.into(),
            "class A { public: void f() {} };",
        ),
        smoke(
            "csharp",
            tree_sitter_c_sharp::LANGUAGE.into(),
            "class A { void F() {} }",
        ),
        smoke(
            "php",
            tree_sitter_php::LANGUAGE_PHP.into(),
            "<?php function f() { return 1; }",
        ),
        smoke(
            "ruby",
            tree_sitter_ruby::LANGUAGE.into(),
            "def f\n  1\nend\n",
        ),
        smoke(
            "swift",
            tree_sitter_swift::LANGUAGE.into(),
            "func f() -> Int { return 1 }",
        ),
        smoke(
            "kotlin",
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            "fun main() { println(1) }",
        ),
        smoke(
            "dart",
            tree_sitter_dart::LANGUAGE.into(),
            "void main() { print(1); }",
        ),
        smoke(
            "pascal",
            tree_sitter_pascal::LANGUAGE.into(),
            "program Hello; begin end.",
        ),
        smoke(
            "scala",
            tree_sitter_scala::LANGUAGE.into(),
            "object A { def f: Int = 1 }",
        ),
        smoke(
            "lua",
            tree_sitter_lua::LANGUAGE.into(),
            "local function f() return 1 end",
        ),
        smoke(
            "luau",
            tree_sitter_luau::LANGUAGE.into(),
            "local function f(): number return 1 end",
        ),
        smoke(
            "objc",
            tree_sitter_objc::LANGUAGE.into(),
            "@interface A\n- (void)f;\n@end",
        ),
        smoke("yaml", tree_sitter_yaml::LANGUAGE.into(), "a:\n  b: 1\n"),
        smoke(
            "xml",
            tree_sitter_xml::LANGUAGE_XML.into(),
            "<mapper namespace=\"A\"><select id=\"x\">select 1</select></mapper>",
        ),
        smoke(
            "properties",
            tree_sitter_properties::LANGUAGE.into(),
            "a.b=1\n",
        ),
        smoke(
            "html",
            tree_sitter_html::LANGUAGE.into(),
            "<main><h1>Hello</h1></main>",
        ),
        smoke(
            "css",
            tree_sitter_css::LANGUAGE.into(),
            "main { color: red; }",
        ),
        smoke(
            "json",
            tree_sitter_json::LANGUAGE.into(),
            "{\"type\":\"hero\"}",
        ),
    ];

    for passed in checks {
        if !passed {
            failures += 1;
        }
    }

    custom(
        "razor",
        "custom Razor extractor; no tree-sitter grammar in the upstream registry",
    );
    custom(
        "svelte",
        "custom extractor delegates script content to TypeScript/JavaScript",
    );
    custom(
        "vue",
        "custom extractor delegates script content to TypeScript/JavaScript",
    );
    custom(
        "liquid",
        "custom regex extractor; Shopify JSON templates delegate here",
    );
    custom(
        "twig",
        "file-level only in the upstream; no selected Rust grammar crate",
    );
    custom(
        "dfm",
        "custom DFM/FMX extractor under pascal language extension handling",
    );

    if failures > 0 {
        std::process::exit(1);
    }
}
