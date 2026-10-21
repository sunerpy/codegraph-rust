//! Same-file value references (upstream #895, #897, `flushValueRefs`): a
//! `references` edge from a symbol to a constant or variable of its own file
//! that it reads, so impact analysis reaches the readers of a shared value
//! ("change this table, affect its readers"). The edge carries
//! `{"valueRef": true}`.
//!
//! Same-file only, so the target is never a guess. Only a distinctive name is
//! a target: three or more characters, one of them an uppercase letter or
//! `_`. A name the file binds again in an inner scope is no target at all,
//! since its nested readers read that binding. Always on (OD-8): upstream's
//! `CODEGRAPH_VALUE_REFS=0` switch is not ported.

use std::collections::{HashMap, HashSet};

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Edge, EdgeKind, Language, NodeKind};
use tree_sitter::Node as SyntaxNode;

use crate::walker::{child_by_field, node_text};

/// How many syntax nodes one scan visits at most.
const MAX_VALUE_REF_NODES: usize = 20_000;

/// The languages whose symbols get value references.
pub(crate) fn has_value_refs(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript
            | Language::JavaScript
            | Language::Tsx
            | Language::ArkTs
            | Language::Go
            | Language::Python
            | Language::Php
            | Language::Scala
            | Language::Rust
            | Language::Ruby
            | Language::C
            | Language::Pascal
            | Language::Java
            | Language::CSharp
            | Language::Kotlin
            | Language::Swift
            | Language::Dart
    )
}

/// A symbol whose syntax is scanned for the values it reads.
struct Reader<'tree> {
    id: String,
    name: String,
    node: SyntaxNode<'tree>,
}

/// What one file's walk records for its value references.
#[derive(Default)]
pub(crate) struct ValueRefs<'tree> {
    /// Target name -> node id; a later declaration of the name wins.
    targets: HashMap<String, String>,
    /// How many target nodes carry each name.
    target_counts: HashMap<String, usize>,
    readers: Vec<Reader<'tree>>,
}

impl<'tree> ValueRefs<'tree> {
    /// Record a node as it is created: a constant or variable declared at file
    /// scope, or in a class, module, struct or enum, with a distinctive name
    /// is a target; a function, method, constant or variable is a reader.
    /// `parent_id` is the scope the node is declared in, `None` for a value no
    /// other scope names by this name.
    pub(crate) fn capture(
        &mut self,
        language: Language,
        kind: NodeKind,
        name: &str,
        id: &str,
        node: SyntaxNode<'tree>,
        parent_id: Option<&str>,
    ) {
        // Pascal's shared values are its `const`s: a Pascal `variable` is a
        // routine's parameter or a class field, which would be noise.
        let target_kind = if language == Language::Pascal {
            kind == NodeKind::Constant
        } else {
            matches!(kind, NodeKind::Constant | NodeKind::Variable)
        };
        if target_kind && is_distinctive(name) && parent_id.is_some_and(is_target_scope) {
            self.targets.insert(name.to_string(), id.to_string());
            *self.target_counts.entry(name.to_string()).or_default() += 1;
        }
        if matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
        ) {
            self.readers.push(Reader {
                id: id.to_string(),
                name: name.to_string(),
                node,
            });
        }
    }

    /// Scan `node` for the reads of the reader `id` instead of the syntax its
    /// node was created from: a Go value's node is its name, while what it
    /// reads is in its whole `const_spec` / `var_spec`, upstream's reader.
    pub(crate) fn scan_reader_from(&mut self, id: &str, node: SyntaxNode<'tree>) {
        if let Some(reader) = self.readers.iter_mut().rev().find(|reader| reader.id == id) {
            reader.node = node;
        }
    }

    /// The value-reference edges of the file whose tree is `root`.
    pub(crate) fn into_edges(
        mut self,
        language: Language,
        file_path: &str,
        root: SyntaxNode<'tree>,
        source: &str,
    ) -> Vec<Edge> {
        if !has_value_refs(language)
            || self.targets.is_empty()
            || self.readers.is_empty()
            || is_generated_file(file_path)
        {
            return Vec::new();
        }
        self.drop_shadowed_targets(root, source);
        if self.targets.is_empty() {
            return Vec::new();
        }
        let mut edges = Vec::new();
        for reader in &self.readers {
            let mut seen = HashSet::new();
            let mut stack = vec![reader.node];
            // A Pascal routine's body is a sibling of its header, the reader's
            // node (`defProc` holds `declProc` and `block`).
            if language == Language::Pascal
                && let Some(body) = reader.node.next_named_sibling()
                && body.kind() == "block"
            {
                stack.push(body);
            }
            let mut visited = 0;
            // One cursor per scan, reset onto each node: `walk()` is a C
            // allocation, and one per visited node was most of the parse
            // stage's allocations (W10-05).
            let mut cursor = reader.node.walk();
            while let Some(node) = stack.pop() {
                if visited >= MAX_VALUE_REF_NODES {
                    break;
                }
                visited += 1;
                if matches!(
                    node.kind(),
                    "identifier" | "constant" | "name" | "simple_identifier"
                ) {
                    let name = node_text(node, source);
                    if let Some(target) = self.targets.get(&name)
                        && *target != reader.id
                        && name != reader.name
                        && seen.insert(target.clone())
                    {
                        edges.push(value_ref_edge(&reader.id, target));
                    }
                }
                stack.extend(node.named_children(&mut cursor));
            }
        }
        edges
    }

    /// A name the file declares more often than it has target nodes of that
    /// name is bound again in an inner scope; it is no target. A conditional
    /// definition at file scope (`try: X = a` / `except: X = b`) declares it
    /// as often as it has nodes and stays.
    fn drop_shadowed_targets(&mut self, root: SyntaxNode<'tree>, source: &str) {
        let mut declared: HashMap<String, usize> = HashMap::new();
        let mut bump = |name_node: Option<SyntaxNode<'tree>>| {
            if let Some(name_node) = name_node
                && matches!(name_node.kind(), "identifier" | "simple_identifier")
            {
                let name = node_text(name_node, source);
                if self.targets.contains_key(&name) {
                    *declared.entry(name).or_default() += 1;
                }
            }
        };
        let mut stack = vec![root];
        let mut visited = 0;
        let mut cursor = root.walk();
        while let Some(node) = stack.pop() {
            if visited >= MAX_VALUE_REF_NODES {
                break;
            }
            visited += 1;
            match node.kind() {
                // TS/JS `const X = …`; Go `const X = …` / `var X = …`.
                "variable_declarator" | "const_spec" | "var_spec" => bump(node.named_child(0)),
                // Rust `const X: T = …` / `static X: T = …`; Pascal `const X =
                // …` (the target and a routine's own) and `var X: T`.
                "const_item" | "static_item" | "declConst" | "declVar" => {
                    bump(child_by_field(node, "name"));
                }
                // C `T X = …`: the file-scope value and a local that shadows it.
                "init_declarator" => bump(crate::lang::c_declarator_identifier(Some(node))),
                // Kotlin / Swift `val`/`let X = …`: an object's or a type's
                // static value, and a local that shadows it.
                "property_declaration" => bump(property_declarator_name(node)),
                // Dart: a top-level or `static` constant, a field or `var`, and
                // a local.
                "static_final_declaration"
                | "initialized_identifier"
                | "initialized_variable_definition" => bump(
                    node.named_children(&mut node.walk())
                        .find(|child| child.kind() == "identifier"),
                ),
                // Scala `val X = …` / `var X = …`.
                "val_definition" | "var_definition" => {
                    bump(child_by_field(node, "pattern").filter(|p| p.kind() == "identifier"));
                }
                // Rust `let x = …`; Go `x, Y := …`; Python `X = …`, `A, B = …`.
                "let_declaration" | "short_var_declaration" | "assignment" => {
                    let left = child_by_field(node, "left")
                        .or_else(|| child_by_field(node, "pattern"))
                        .or_else(|| node.named_child(0));
                    match left {
                        Some(left) if left.kind() == "identifier" => bump(Some(left)),
                        Some(left) => {
                            for child in left.named_children(&mut left.walk()) {
                                bump(Some(child));
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
            stack.extend(node.named_children(&mut cursor));
        }
        for (name, count) in declared {
            if count > self.target_counts.get(&name).copied().unwrap_or(1) {
                self.targets.remove(&name);
            }
        }
    }
}

/// The name a Kotlin or Swift `property_declaration` binds: kotlin-ng's
/// `variable_declaration` identifier, or the first `simple_identifier` of a
/// Swift pattern.
fn property_declarator_name(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if let Some(declaration) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "variable_declaration")
    {
        return declaration
            .named_children(&mut declaration.walk())
            .find(|child| child.kind() == "identifier");
    }
    let pattern = child_by_field(node, "name").or_else(|| {
        node.named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "value_binding_pattern" | "pattern"))
    })?;
    first_simple_identifier(pattern)
}

/// The first `simple_identifier` at or below `node`, breadth first.
fn first_simple_identifier(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut queue = std::collections::VecDeque::from([node]);
    let mut guard = 0;
    while let Some(current) = queue.pop_front() {
        guard += 1;
        if guard > 40 {
            break;
        }
        if current.kind() == "simple_identifier" {
            return Some(current);
        }
        queue.extend(current.named_children(&mut current.walk()));
    }
    None
}

/// Three or more UTF-16 code units, one of them an ASCII uppercase letter or
/// `_` (upstream `name.length >= 3 && /[A-Z_]/`).
fn is_distinctive(name: &str) -> bool {
    name.encode_utf16().count() >= 3 && name.chars().any(|c| c.is_ascii_uppercase() || c == '_')
}

/// File, class, module, struct and enum scope hold shared values.
fn is_target_scope(parent_id: &str) -> bool {
    ["file:", "class:", "module:", "struct:", "enum:"]
        .iter()
        .any(|prefix| parent_id.starts_with(prefix))
}

fn value_ref_edge(source: &str, target: &str) -> Edge {
    Edge {
        id: None,
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::References,
        metadata: Some(serde_json::json!({ "valueRef": true })),
        line: None,
        col: None,
        provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const CHILD: &str = "CODEGRAPH_TEST_VALUE_REF_ALLOCATIONS";

    /// A tree-sitter allocator over Rust's that counts allocation calls.
    mod counting {
        use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub static CALLS: AtomicUsize = AtomicUsize::new(0);
        const HEADER: usize = 16;

        fn layout(size: usize) -> Layout {
            Layout::from_size_align(size + HEADER, 16).expect("allocation layout")
        }

        unsafe fn finish(base: *mut u8, size: usize) -> *mut c_void {
            assert!(!base.is_null(), "tree-sitter allocation failed");
            unsafe {
                base.cast::<usize>().write(size);
                base.add(HEADER).cast()
            }
        }

        unsafe fn base_of(ptr: *mut c_void) -> (*mut u8, usize) {
            let base = unsafe { ptr.cast::<u8>().sub(HEADER) };
            (base, unsafe { base.cast::<usize>().read() })
        }

        pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
            CALLS.fetch_add(1, Ordering::Relaxed);
            unsafe { finish(alloc(layout(size)), size) }
        }

        pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
            CALLS.fetch_add(1, Ordering::Relaxed);
            let total = count.checked_mul(size).expect("calloc size");
            unsafe { finish(alloc_zeroed(layout(total)), total) }
        }

        pub unsafe extern "C" fn resize(ptr: *mut c_void, size: usize) -> *mut c_void {
            if ptr.is_null() {
                return unsafe { malloc(size) };
            }
            CALLS.fetch_add(1, Ordering::Relaxed);
            unsafe {
                let (base, old) = base_of(ptr);
                finish(realloc(base, layout(old), size + HEADER), size)
            }
        }

        pub unsafe extern "C" fn free(ptr: *mut c_void) {
            if !ptr.is_null() {
                unsafe {
                    let (base, old) = base_of(ptr);
                    dealloc(base, layout(old));
                }
            }
        }
    }

    /// W10-05: a value-reference scan allocates one tree cursor per reader,
    /// not one per syntax node it visits. Each cursor is a C allocation, and
    /// the musl release build serializes those on one lock: a cursor per node
    /// made redis's walk stage three times slower at 32 threads. Runs in a
    /// child process, since the allocator must be installed before any other
    /// tree-sitter call.
    #[test]
    fn value_ref_scan_allocates_a_cursor_per_reader_not_per_node() {
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "value_refs::tests::value_ref_scan_allocation_child",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("run the allocation child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("CASE_PASSED"),
            "allocation child failed: stdout={stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn value_ref_scan_allocation_child() {
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        // SAFETY: the child runs this test alone, before any tree-sitter call.
        unsafe {
            tree_sitter::set_allocator(Some(tree_sitter::Allocator {
                malloc: counting::malloc,
                calloc: counting::calloc,
                realloc: counting::resize,
                free: counting::free,
            }));
        }
        const READERS: usize = 30;
        let mut source = String::from("static const int LIMIT_VALUE = 3;\n");
        for i in 0..READERS {
            source.push_str(&format!(
                "int f{i}(int a, int b) {{ return a + b * LIMIT_VALUE - (a / 2) + (b % 3) + a * b; }}\n"
            ));
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("C grammar");
        let tree = parser.parse(&source, None).expect("parse");
        let root = tree.root_node();
        let mut refs = ValueRefs::default();
        let file = "file:src/limits.c";
        let mut cursor = root.walk();
        for (i, item) in root.named_children(&mut cursor).enumerate() {
            match item.kind() {
                "declaration" => refs.capture(
                    Language::C,
                    NodeKind::Constant,
                    "LIMIT_VALUE",
                    "constant:limit",
                    item,
                    Some(file),
                ),
                "function_definition" => refs.capture(
                    Language::C,
                    NodeKind::Function,
                    &format!("f{}", i - 1),
                    &format!("function:f{}", i - 1),
                    item,
                    Some(file),
                ),
                other => panic!("unexpected top-level node {other}"),
            }
        }
        drop(cursor);
        let before = counting::CALLS.load(std::sync::atomic::Ordering::Relaxed);
        let edges = refs.into_edges(Language::C, "src/limits.c", root, &source);
        let used = counting::CALLS.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert_eq!(edges.len(), READERS, "every function reads LIMIT_VALUE");
        // One cursor for the shadowing scan and one per reader (the constant
        // reads nothing but is a reader too), with room for a second
        // allocation each; a cursor per visited node made 1,935.
        assert!(
            used <= 2 * (READERS + 2),
            "the scan made {used} tree-sitter allocations for {READERS} readers"
        );
        println!("CASE_PASSED");
    }
}
