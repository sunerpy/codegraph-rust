//! `--gen-go-std-methods`: regenerate the rows of
//! `codegraph-resolve/src/go_std_methods.rs` from a Go source tree (upstream
//! `scripts/gen-go-stdlib-methods.mjs` at `e8ddc4e2`). For every type the
//! rows list, the generator reads `<go-src>/src/<import path>/*.go`, tests
//! skipped, with this repository's Go grammar, and lists each method as
//! `name:params/results`: an interface's own and those of the interfaces it
//! embeds, a named type's (receiver `T` or `*T`) and those its embedded fields
//! promote, an alias's target's. Exported names are kept, and an unexported
//! one only where a listed interface requires it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use codegraph_core::types::Language;
use codegraph_extract::lang::spec_for_language;
use codegraph_resolve::go_std_methods::go_signature_arity;
use regex::Regex;
use tree_sitter::{Node, Parser};

/// The line before the generated rows.
pub const BEGIN: &str = "    // BEGIN GENERATED (bench --gen-go-std-methods)\n";
/// The line after them.
pub const END: &str = "    // END GENERATED\n";

/// A method receiver's type name (upstream's receiver pattern).
static RECEIVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\(\s*(?:[A-Za-z0-9_]+\s+)?\*?\s*([A-Za-z0-9_]+)").expect("receiver")
});
/// An exported Go name.
static EXPORTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Lu}").expect("exported"));

/// The committed table module.
pub fn table_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../codegraph-resolve/src/go_std_methods.rs")
}

/// The byte range of the generated rows in `module`.
fn section(module: &str) -> Result<(usize, usize)> {
    let start = module.find(BEGIN).context("no BEGIN GENERATED line")? + BEGIN.len();
    let end = start + module[start..].find(END).context("no END GENERATED line")?;
    Ok((start, end))
}

/// The import path and type each row of `module` lists, in order.
pub fn listed(module: &str) -> Result<Vec<(String, String)>> {
    let (start, end) = section(module)?;
    module[start..end]
        .lines()
        .map(|line| {
            let inner = line
                .trim()
                .strip_prefix("(\"")
                .and_then(|rest| rest.strip_suffix("\"),"))
                .with_context(|| format!("not a row: {line}"))?;
            let parts: Vec<&str> = inner.split("\", \"").collect();
            if parts.len() != 3 {
                bail!("not a row: {line}");
            }
            Ok((parts[0].to_string(), parts[1].to_string()))
        })
        .collect()
}

/// One row as the module writes it.
fn row_line(path: &str, name: &str, methods: &str) -> String {
    format!("    (\"{path}\", \"{name}\", \"{methods}\"),\n")
}

/// A type an interface, struct or alias embeds.
#[derive(Clone)]
enum Embed {
    /// `pkg.Name`, with the import path the declaring file gives `pkg`.
    Qualified(Option<String>, String),
    /// A type of the same package.
    Local(String),
    /// The predeclared `error`.
    Error,
}

/// One element of an interface, in source order.
#[derive(Clone)]
enum Element {
    Method(String, Option<String>),
    Embed(Embed),
}

/// What a top-level type declaration is.
#[derive(Clone)]
enum Declared {
    Interface(Vec<Element>),
    Struct(Vec<Embed>),
    Alias(Option<Embed>),
    Other,
}

/// A package's top-level type declarations and the methods declared on each
/// receiver type, first declaration first.
#[derive(Default)]
struct Package {
    types: HashMap<String, Declared>,
    methods: HashMap<String, Vec<(String, Option<String>)>>,
}

/// The method lists of the listed types, each `(name, params/results)`.
type Methods = Vec<(String, Option<String>)>;

struct Generator {
    root: PathBuf,
    parser: Parser,
    packages: HashMap<String, Package>,
}

/// A node's text.
fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

/// A function or method node's signature as extraction stores it: its
/// parameter list's text, then its result's.
fn signature(node: Node<'_>, source: &str) -> Option<String> {
    let params = text(node.child_by_field_name("parameters")?, source);
    Some(match node.child_by_field_name("result") {
        Some(result) => format!("{params} {}", text(result, source)),
        None => params.to_string(),
    })
}

/// The type `node` embeds (upstream `embedded`): a pointer's and a generic
/// instantiation's base type, a qualified or a local type name.
fn embed(node: Option<Node<'_>>, source: &str, imports: &HashMap<String, String>) -> Option<Embed> {
    let mut node = node?;
    if node.kind() == "pointer_type" {
        node = node.named_child(0)?;
    }
    if node.kind() == "generic_type" {
        node = node.child_by_field_name("type")?;
    }
    match node.kind() {
        "qualified_type" => {
            let package = text(node.child_by_field_name("package")?, source);
            let name = text(node.child_by_field_name("name")?, source);
            Some(Embed::Qualified(
                imports.get(package).cloned(),
                name.to_string(),
            ))
        }
        "type_identifier" => Some(match text(node, source) {
            "error" => Embed::Error,
            name => Embed::Local(name.to_string()),
        }),
        _ => None,
    }
}

/// Every descendant of `node` of one of `kinds`, in document order.
fn descendants<'t>(node: Node<'t>, kinds: &[&str], found: &mut Vec<Node<'t>>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            found.push(child);
        }
        descendants(child, kinds, found);
    }
}

impl Generator {
    fn new(root: &Path) -> Result<Self> {
        let spec = spec_for_language(Language::Go).context("no Go grammar")?;
        let mut parser = Parser::new();
        parser
            .set_language(&spec.tree_sitter_language())
            .context("Go grammar")?;
        Ok(Self {
            root: root.to_path_buf(),
            parser,
            packages: HashMap::new(),
        })
    }

    /// Parse the package at `import_path` once.
    fn load(&mut self, import_path: &str) -> Result<()> {
        if self.packages.contains_key(import_path) {
            return Ok(());
        }
        let mut package = Package::default();
        let dir = self.root.join("src").join(import_path);
        let mut files: Vec<String> = match fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| name.ends_with(".go") && !name.ends_with("_test.go"))
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        for file in files {
            let source = fs::read_to_string(dir.join(&file))
                .with_context(|| format!("{import_path}/{file}"))?;
            let tree = self.parser.parse(&source, None).context("parse")?;
            let root = tree.root_node();
            let mut imports = HashMap::new();
            let mut specs = Vec::new();
            descendants(root, &["import_spec"], &mut specs);
            for spec in specs {
                let Some(path) = spec.child_by_field_name("path") else {
                    continue;
                };
                let path = text(path, &source).trim_matches('"').to_string();
                let name = spec
                    .child_by_field_name("name")
                    .map(|name| text(name, &source).to_string())
                    .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).to_string());
                imports.insert(name, path);
            }
            let mut types = Vec::new();
            descendants(root, &["type_spec", "type_alias"], &mut types);
            for spec in types {
                let top = spec.parent().is_some_and(|parent| {
                    parent.kind() == "type_declaration"
                        && parent
                            .parent()
                            .is_some_and(|file| file.kind() == "source_file")
                });
                let Some(name) = spec.child_by_field_name("name") else {
                    continue;
                };
                let name = text(name, &source).to_string();
                if !top || package.types.contains_key(&name) {
                    continue;
                }
                let ty = spec.child_by_field_name("type");
                let declared = if spec.kind() == "type_alias" {
                    Declared::Alias(embed(ty, &source, &imports))
                } else {
                    match ty {
                        Some(ty) if ty.kind() == "interface_type" => {
                            let mut elements = Vec::new();
                            let mut cursor = ty.walk();
                            for element in ty.named_children(&mut cursor) {
                                match element.kind() {
                                    "method_elem" => {
                                        let Some(method) = element.child_by_field_name("name")
                                        else {
                                            continue;
                                        };
                                        elements.push(Element::Method(
                                            text(method, &source).to_string(),
                                            go_signature_arity(
                                                signature(element, &source).as_deref(),
                                            ),
                                        ));
                                    }
                                    "type_elem" => {
                                        let mut inner = element.walk();
                                        let terms: Vec<Node<'_>> = element
                                            .named_children(&mut inner)
                                            .filter(|term| term.kind() != "comment")
                                            .collect();
                                        if let [term] = terms[..]
                                            && let Some(embedded) =
                                                embed(Some(term), &source, &imports)
                                        {
                                            elements.push(Element::Embed(embedded));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Declared::Interface(elements)
                        }
                        Some(ty) if ty.kind() == "struct_type" => {
                            let mut embeds = Vec::new();
                            let mut cursor = ty.walk();
                            if let Some(fields) = ty
                                .named_children(&mut cursor)
                                .find(|child| child.kind() == "field_declaration_list")
                            {
                                let mut inner = fields.walk();
                                for field in fields.named_children(&mut inner) {
                                    if field.kind() != "field_declaration" {
                                        continue;
                                    }
                                    let mut names = field.walk();
                                    if field
                                        .named_children(&mut names)
                                        .any(|c| c.kind() == "field_identifier")
                                    {
                                        continue;
                                    }
                                    if let Some(embedded) =
                                        embed(field.child_by_field_name("type"), &source, &imports)
                                    {
                                        embeds.push(embedded);
                                    }
                                }
                            }
                            Declared::Struct(embeds)
                        }
                        _ => Declared::Other,
                    }
                };
                package.types.insert(name, declared);
            }
            let mut cursor = root.walk();
            for method in root.named_children(&mut cursor) {
                if method.kind() != "method_declaration" {
                    continue;
                }
                let receiver = method
                    .child_by_field_name("receiver")
                    .and_then(|receiver| RECEIVER.captures(text(receiver, &source)))
                    .map(|captures| captures[1].to_string());
                let name = method
                    .child_by_field_name("name")
                    .map(|name| text(name, &source).to_string());
                let (Some(receiver), Some(name)) = (receiver, name) else {
                    continue;
                };
                let methods = package.methods.entry(receiver).or_default();
                if !methods.iter().any(|(known, _)| *known == name) {
                    methods.push((
                        name,
                        go_signature_arity(signature(method, &source).as_deref()),
                    ));
                }
            }
        }
        self.packages.insert(import_path.to_string(), package);
        Ok(())
    }

    /// Whether the package at `import_path` declares the type `name`.
    fn declared(&mut self, import_path: &str, name: &str) -> Result<Option<Declared>> {
        self.load(import_path)?;
        Ok(self.packages[import_path].types.get(name).cloned())
    }

    /// The methods of the type `name` the package at `import_path` declares
    /// (upstream `methodsOf`), its own first, then what it embeds.
    fn methods_of(
        &mut self,
        import_path: &str,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Result<Methods> {
        if !seen.insert(format!("{import_path}.{name}")) {
            return Ok(Vec::new());
        }
        self.load(import_path)?;
        let package = &self.packages[import_path];
        let mut methods: Methods = package.methods.get(name).cloned().unwrap_or_default();
        let Some(declared) = package.types.get(name).cloned() else {
            return Ok(methods);
        };
        let take = |methods: &mut Methods, more: Methods| {
            for (method, arity) in more {
                if !methods.iter().any(|(known, _)| *known == method) {
                    methods.push((method, arity));
                }
            }
        };
        match declared {
            Declared::Interface(elements) => {
                for element in elements {
                    match element {
                        Element::Method(method, arity) => {
                            match methods.iter_mut().find(|(known, _)| *known == method) {
                                Some(entry) => entry.1 = arity,
                                None => methods.push((method, arity)),
                            }
                        }
                        Element::Embed(embedded) => {
                            let more = self.embedded(import_path, &embedded, seen)?;
                            take(&mut methods, more);
                        }
                    }
                }
            }
            Declared::Struct(embeds) => {
                for embedded in embeds {
                    let more = self.embedded(import_path, &embedded, seen)?;
                    take(&mut methods, more);
                }
            }
            Declared::Alias(Some(embedded)) => {
                let more = self.embedded(import_path, &embedded, seen)?;
                take(&mut methods, more);
            }
            Declared::Alias(None) | Declared::Other => {}
        }
        Ok(methods)
    }

    fn embedded(
        &mut self,
        import_path: &str,
        embedded: &Embed,
        seen: &mut HashSet<String>,
    ) -> Result<Methods> {
        match embedded {
            Embed::Error => Ok(vec![("Error".to_string(), Some("0/1".to_string()))]),
            Embed::Local(name) => self.methods_of(import_path, name, seen),
            Embed::Qualified(Some(path), name) => self.methods_of(path, name, seen),
            Embed::Qualified(None, name) => {
                bail!("{import_path}: `{name}` names a package the file does not import")
            }
        }
    }
}

/// The rows for the types `listed` names, generated from the Go source tree
/// at `go_src`. Fails when a listed type is missing or a kept method's
/// signature does not read.
pub fn generate(go_src: &Path, listed: &[(String, String)]) -> Result<String> {
    let mut generator = Generator::new(go_src)?;
    let mut generated = Vec::new();
    let mut required: HashSet<String> = HashSet::new();
    let mut failures = Vec::new();
    for (import_path, name) in listed {
        let Some(declared) = generator.declared(import_path, name)? else {
            failures.push(format!("not found: {import_path}.{name}"));
            continue;
        };
        let methods = generator.methods_of(import_path, name, &mut HashSet::new())?;
        if matches!(declared, Declared::Interface(_)) {
            required.extend(
                methods
                    .iter()
                    .filter(|(method, _)| !EXPORTED.is_match(method))
                    .map(|(method, _)| method.clone()),
            );
        }
        generated.push((import_path, name, methods));
    }
    let mut rows = String::new();
    for (import_path, name, methods) in generated {
        let mut kept: Vec<&(String, Option<String>)> = methods
            .iter()
            .filter(|(method, _)| EXPORTED.is_match(method) || required.contains(method))
            .collect();
        kept.sort_by(|a, b| a.0.cmp(&b.0));
        let mut listed = Vec::new();
        for (method, arity) in kept {
            match arity {
                Some(arity) => listed.push(format!("{method}:{arity}")),
                None => failures.push(format!(
                    "signature doesn't read: {import_path}.{name}.{method}"
                )),
            }
        }
        rows.push_str(&row_line(import_path, name, &listed.join(" ")));
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("\n"));
    }
    Ok(rows)
}

/// What `--gen-go-std-methods` does with the generated rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Print,
    Check,
    Write,
}

/// Run `--gen-go-std-methods` over the module at `module_path`: print the
/// rows, check them against the committed ones (`Ok(false)` when they
/// differ) or write them in.
pub fn run(
    go_src: &Path,
    module_path: &Path,
    mode: Mode,
    out: &mut dyn std::io::Write,
) -> Result<bool> {
    if !go_src.join("src").join("io").is_dir() {
        bail!("{} is not a Go source tree (no src/io)", go_src.display());
    }
    let module =
        fs::read_to_string(module_path).with_context(|| module_path.display().to_string())?;
    let rows = generate(go_src, &listed(&module)?)?;
    let (start, end) = section(&module)?;
    match mode {
        Mode::Print => {
            write!(out, "{rows}")?;
            Ok(true)
        }
        Mode::Check => {
            let same = module[start..end] == rows;
            if !same {
                let committed: Vec<&str> = module[start..end].lines().collect();
                for (at, line) in rows.lines().enumerate() {
                    if committed.get(at) != Some(&line) {
                        writeln!(
                            out,
                            "row {}: committed {:?}, generated {line:?}",
                            at + 1,
                            committed.get(at)
                        )?;
                    }
                }
                if committed.len() != rows.lines().count() {
                    writeln!(
                        out,
                        "committed {} rows, generated {}",
                        committed.len(),
                        rows.lines().count()
                    )?;
                }
            }
            Ok(same)
        }
        Mode::Write => {
            fs::write(
                module_path,
                format!("{}{rows}{}", &module[..start], &module[end..]),
            )?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch Go source tree, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tree(files: &[(&str, &str)]) -> Tree {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "cg-go-std-gen-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&dir).expect("a fresh scratch directory");
        let tree = Tree(dir);
        for (path, content) in files {
            let full = tree.path().join("src").join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, content).expect("write");
        }
        tree
    }

    const IO: &str = "package io\n\ntype Reader interface {\n\tRead(p []byte) (n int, err error)\n}\n\ntype Closer interface {\n\tClose() error\n}\n\ntype ReadCloser interface {\n\tReader\n\tCloser\n}\n";

    fn listed_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|&(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn interfaces_structs_and_aliases_list_what_they_embed() {
        let dir = tree(&[
            ("io/io.go", IO),
            (
                "os/file.go",
                "package os\n\nimport \"io\"\n\ntype File struct {\n\tname string\n\t*fileInner\n}\n\ntype fileInner struct{}\n\nfunc (f *fileInner) Fd() uintptr { return 0 }\nfunc (f *File) Read(b []byte) (n int, err error) { return }\nfunc (f *File) close() error { return nil }\nfunc (f File) Name() string { return f.name }\n\ntype Wrapped = io.ReadCloser\n\ntype Errs interface {\n\terror\n\tCode() int\n}\n",
            ),
            (
                "os/file_test.go",
                "package os\n\nfunc (f *File) Hidden() {}\n",
            ),
            ("os/zz.go", "package os\n\nfunc (f *File) Read() {}\n"),
        ]);
        let rows = generate(
            dir.path(),
            &listed_of(&[
                ("io", "ReadCloser"),
                ("os", "File"),
                ("os", "Wrapped"),
                ("os", "Errs"),
            ]),
        )
        .expect("generate");
        assert_eq!(
            rows,
            "    (\"io\", \"ReadCloser\", \"Close:0/1 Read:1/2\"),\n    (\"os\", \"File\", \"Fd:0/1 Name:0/1 Read:1/2\"),\n    (\"os\", \"Wrapped\", \"Close:0/1 Read:1/2\"),\n    (\"os\", \"Errs\", \"Code:0/1 Error:0/1\"),\n"
        );
    }

    #[test]
    fn an_unexported_method_stays_only_where_an_interface_requires_it() {
        let dir = tree(&[(
            "testing/testing.go",
            "package testing\n\ntype TB interface {\n\tHelper()\n\tprivate()\n}\n\ntype T struct{}\n\nfunc (t *T) Helper() {}\nfunc (t *T) private() {}\nfunc (t *T) other() {}\n",
        )]);
        let rows = generate(
            dir.path(),
            &listed_of(&[("testing", "TB"), ("testing", "T")]),
        )
        .expect("generate");
        assert_eq!(
            rows,
            "    (\"testing\", \"TB\", \"Helper:0/0 private:0/0\"),\n    (\"testing\", \"T\", \"Helper:0/0 private:0/0\"),\n"
        );
    }

    #[test]
    fn the_first_declaration_of_a_type_wins() {
        let dir = tree(&[
            ("pkg/a.go", "package pkg\n\ntype X interface {\n\tA()\n}\n"),
            ("pkg/b.go", "package pkg\n\ntype X interface {\n\tB()\n}\n"),
        ]);
        let rows = generate(dir.path(), &listed_of(&[("pkg", "X")])).expect("generate");
        assert_eq!(rows, "    (\"pkg\", \"X\", \"A:0/0\"),\n");
    }

    #[test]
    fn a_missing_type_fails() {
        let dir = tree(&[("io/io.go", IO)]);
        let error = generate(dir.path(), &listed_of(&[("io", "Writer")])).expect_err("missing");
        assert!(
            error.to_string().contains("not found: io.Writer"),
            "{error}"
        );
    }

    #[test]
    fn check_reports_a_changed_row_and_write_fixes_it() {
        let dir = tree(&[("io/io.go", IO)]);
        let module = dir.path().join("table.rs");
        let committed =
            format!("const ROWS = &[\n{BEGIN}    (\"io\", \"Reader\", \"Read:2/2\"),\n{END}];\n");
        fs::write(&module, &committed).expect("write");
        let mut out = Vec::new();
        assert!(!run(dir.path(), &module, Mode::Check, &mut out).expect("check"));
        assert!(String::from_utf8_lossy(&out).contains("Read:1/2"));
        assert!(run(dir.path(), &module, Mode::Write, &mut Vec::new()).expect("write"));
        assert!(run(dir.path(), &module, Mode::Check, &mut Vec::new()).expect("check"));
        assert_eq!(
            fs::read_to_string(&module).expect("read"),
            committed.replace("Read:2/2", "Read:1/2")
        );
    }
}
