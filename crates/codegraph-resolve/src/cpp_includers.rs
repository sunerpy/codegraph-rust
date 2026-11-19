//! Which files include a C or C++ file: the files whose `#include` names it,
//! then the files that include those, out to the translation units (upstream
//! #2421 `cppIncluders`).
//!
//! Read from the `import` node extraction mints for every `#include`, each
//! resolved by the import resolver as the graph's include edge is, so
//! "includes" here is the include edge a reader sees. The test-suite rule asks
//! it whether a C or C++ file named like a test is part of a reference's
//! translation unit after all: protobuf's conformance framework lives in
//! `conformance/conformance_test.h`, which the suites and runners include.
//!
//! The walk starts from the included file, not from the reference's: a
//! handful of such files are asked about, by references from many files, and
//! a file nothing includes costs one map lookup. Every answer is a pure
//! function of the indexed files, so evaluation order cannot reach the output.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::import_resolver::resolve_via_import;
use crate::types::{RefView, ResolutionContext};

/// The files whose C or C++ `#include` spells the name or the stem of one of
/// `paths`, with every file including those, directly or through other
/// includes (W7-27): a file that comes or goes at such a path, or gains or
/// loses its nodes across `max_file_size`, can change which file that
/// include takes or what it reads, and with it whether a call in any of
/// those translation units is a macro expansion (`c_macro_visibility`).
/// Empty when the index holds no function-like macro, since the walk then
/// decides no call. Sorted.
pub(crate) fn include_walk_dependents(
    paths: &[String],
    context: &dyn ResolutionContext,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if paths.is_empty()
        || !context
            .get_nodes_by_kind_shared(NodeKind::Constant)
            .iter()
            .any(|node| crate::c_macro_visibility::is_define_constant(node))
    {
        return out;
    }
    // The walk enters a file only through an include spelling its name or
    // its stem (the includer's own directory, the import resolver's
    // extension candidates, or the one indexed file of that name), so the
    // files whose include spells a member's name or stem, followed upward,
    // hold every translation unit that can reach one of `paths`. No
    // include's resolution is checked: one the walk finds only by its name
    // resolves no include edge (production lists no include directories).
    let mut graph = CppIncluders::default();
    let mut queue: Vec<String> = paths.to_vec();
    while let Some(member) = queue.pop() {
        let spellings: BTreeSet<String> = [file_name(&member), file_stem(&member)]
            .into_iter()
            .map(str::to_string)
            .collect();
        for spelled in &spellings {
            for include in graph.includes_named(spelled, context) {
                if out.insert(include.file.clone()) {
                    queue.push(include.file);
                }
            }
        }
    }
    out
}

/// One `#include` directive, as its import node records it.
#[derive(Clone)]
struct Include {
    id: String,
    /// The path as written: `conformance/conformance_test.h`.
    spec: String,
    file: String,
    language: Language,
    line: i64,
    column: i64,
}

impl Include {
    fn of(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            spec: node.name.clone(),
            file: node.file_path.clone(),
            language: node.language,
            line: node.start_line,
            column: node.start_column,
        }
    }
}

fn is_c_family(language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
}

/// The last `/` or `\` segment of a path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A file name without its last extension (`conformance_test` of
/// `conformance/conformance_test.h`).
pub(crate) fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The per-pass include graph, filled on demand.
#[derive(Default)]
pub(crate) struct CppIncluders {
    /// Every C / C++ `#include`, by the file name it spells (`a/b.h` → `b.h`).
    by_name: Option<HashMap<String, Vec<Include>>>,
    /// Import node id → the indexed file its `#include` names (`None`: none).
    targets: HashMap<String, Option<String>>,
    /// File → the files that include it, directly or through other includes.
    includers: HashMap<String, Arc<BTreeSet<String>>>,
}

impl CppIncluders {
    /// The includes spelling `name`.
    fn includes_named(&mut self, name: &str, context: &dyn ResolutionContext) -> Vec<Include> {
        let by_name = self.by_name.get_or_insert_with(|| {
            let mut by_name: HashMap<String, Vec<Include>> = HashMap::new();
            for node in context.get_nodes_by_kind_shared(NodeKind::Import) {
                if is_c_family(node.language) {
                    by_name
                        .entry(file_name(&node.name).to_string())
                        .or_default()
                        .push(Include::of(&node));
                }
            }
            by_name
        });
        by_name.get(name).cloned().unwrap_or_default()
    }

    /// The indexed file an `#include` names, the way its include edge
    /// resolves it.
    fn included_file(
        &mut self,
        include: &Include,
        context: &dyn ResolutionContext,
    ) -> Option<String> {
        if let Some(hit) = self.targets.get(&include.id) {
            return hit.clone();
        }
        let reference = RefView {
            row_id: None,
            from_node_id: include.id.clone(),
            reference_name: include.spec.clone(),
            reference_kind: EdgeKind::Imports,
            line: include.line,
            column: include.column,
            file_path: include.file.clone(),
            language: include.language,
            is_function_ref: false,
            reference_subkind: None,
        };
        let file = resolve_via_import(&reference, context)
            .and_then(|resolved| context.get_node_by_id_shared(&resolved.target_node_id))
            .filter(|target| target.kind == NodeKind::File)
            .map(|target| target.file_path.clone());
        self.targets.insert(include.id.clone(), file.clone());
        file
    }

    /// The indexed file the `#include` an import node records names.
    pub(crate) fn included_file_of(
        &mut self,
        include: &Node,
        context: &dyn ResolutionContext,
    ) -> Option<String> {
        (include.kind == NodeKind::Import && is_c_family(include.language))
            .then(|| self.included_file(&Include::of(include), context))
            .flatten()
    }

    /// Every file that includes `file_path`, directly or through the files
    /// that include it: the translation units (and headers) it is part of.
    pub(crate) fn includers(
        &mut self,
        file_path: &str,
        context: &dyn ResolutionContext,
    ) -> Arc<BTreeSet<String>> {
        if let Some(hit) = self.includers.get(file_path) {
            return Arc::clone(hit);
        }
        let mut found = BTreeSet::new();
        let mut queue = vec![file_path.to_string()];
        let mut at = 0;
        while at < queue.len() {
            let target = queue[at].clone();
            at += 1;
            let name = file_name(&target).to_string();
            // `#include "foo"` can name `foo.h`: the include resolver tries
            // extensions.
            let mut spellings = vec![name.clone()];
            if let Some(dot) = name.rfind('.').filter(|&dot| dot > 0) {
                spellings.push(name[..dot].to_string());
            }
            for spelled in spellings {
                for include in self.includes_named(&spelled, context) {
                    if include.file == file_path || found.contains(&include.file) {
                        continue;
                    }
                    if self.included_file(&include, context).as_deref() != Some(target.as_str()) {
                        continue;
                    }
                    found.insert(include.file.clone());
                    queue.push(include.file);
                }
            }
        }
        let found = Arc::new(found);
        self.includers
            .insert(file_path.to_string(), Arc::clone(&found));
        found
    }
}
