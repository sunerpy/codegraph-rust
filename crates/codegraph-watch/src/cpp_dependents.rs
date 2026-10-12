//! C and C++ references whose resolution reads another file's text
//! (W4-CPP-04, upstream #2399), so that a sync re-resolves them as a fresh
//! index would.
//!
//! Receiver inference reads a receiver's declaration in the calling file or in
//! its same-stem header (`x.cc` reads `x.h`, `x.hpp`, `x.hxx`), and an alias
//! lookup finds aliases by name in every C/C++ file, reads their declarations
//! and walks class heads for bases. None of that text is a node, so a changed
//! header leaves an unchanged caller's edge where the old text put it. Two
//! lanes send such callers down the sync's whole-file lane:
//!
//! - **Same stem:** a changed header's same-stem sources.
//! - **Names:** what an alias lookup reads of a changed C/C++ file, its *type
//!   surface* (aliases with their declarations, class-like and enum nodes,
//!   each class's bases), before and after the change. An alias that changed,
//!   a type added or removed under an alias's name, and a class whose bases
//!   changed (with every class deriving from it, whose out-of-line methods
//!   name only their own class) seed a set of names, closed over the aliases
//!   whose declarations mention them. Every unchanged C/C++ file whose text
//!   mentions one of them is re-resolved whole, with a header's same-stem
//!   sources.
//!
//! The surfaces before the change come from one snapshot of the store taken
//! before the sync first changes a C/C++ file, so a file changed earlier in
//! the batch, whose nodes took other files' `extends` edges with them, cannot
//! hide what a later file held. A sync withdraws [`VERSION_KEY`] before its
//! first change and writes it back once everything resolved; an index without
//! it re-resolves every C/C++ file once.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;

/// Metadata key present when no sync left a C/C++ dependent unrefreshed.
pub const VERSION_KEY: &str = "cpp_type_surface_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Kinds of the nodes a type surface reads.
const SURFACE_KINDS: [NodeKind; 5] = [
    NodeKind::TypeAlias,
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Enum,
];

/// Extensions of the sources whose receiver inference reads a same-stem
/// header (`infer_cpp_receiver_type`, case-insensitive).
const SOURCE_EXTENSIONS: [&str; 4] = ["c", "cc", "cpp", "cxx"];

pub(crate) fn is_c_family(language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
}

/// A path receiver inference reads as a source's header: `.h`, `.hpp` or
/// `.hxx`, the spellings it substitutes.
pub(crate) fn is_header_path(path: &str) -> bool {
    path.ends_with(".h") || path.ends_with(".hpp") || path.ends_with(".hxx")
}

/// A written base reduced to the name an alias lookup reads: template
/// arguments dropped, the last `::` segment (`ns::Base<T>` → `Base`).
fn base_name(written: &str) -> String {
    let mut bare = String::with_capacity(written.len());
    let mut depth = 0usize;
    for c in written.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => bare.push(c),
            _ => {}
        }
    }
    bare.rsplit("::")
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Each identifier token of `text`, `[A-Za-z_][A-Za-z0-9_]*`.
fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut at = 0usize;
    std::iter::from_fn(move || {
        while at < bytes.len() {
            let start = at;
            if bytes[at] == b'_' || bytes[at].is_ascii_alphabetic() {
                at += 1;
                while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                    at += 1;
                }
                // A token right after a digit run (`0x1F`) is not one.
                if start > 0 && bytes[start - 1].is_ascii_digit() {
                    continue;
                }
                return Some(&text[start..at]);
            }
            at += 1;
        }
        None
    })
}

fn mentions(text: &str, names: &HashSet<String>) -> bool {
    identifiers(text).any(|word| names.contains(word))
}

/// What an alias lookup reads of one file's declarations. Each list is a
/// sorted multiset.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TypeSurface {
    /// `(qualified name, name, declaration)` of each alias.
    aliases: Vec<(String, String, String)>,
    /// `(kind, qualified name, name)` of each class, struct, union and enum.
    types: Vec<(&'static str, String, String)>,
    /// `(class qualified name, class name, base name)` of each base.
    bases: Vec<(String, String, String)>,
}

impl TypeSurface {
    fn note_node(&mut self, node: &Node) {
        if node.kind == NodeKind::TypeAlias {
            self.aliases.push((
                node.qualified_name.clone(),
                node.name.clone(),
                node.signature.clone().unwrap_or_default(),
            ));
        } else {
            self.types.push((
                node.kind.as_str(),
                node.qualified_name.clone(),
                node.name.clone(),
            ));
        }
    }

    fn note_base(&mut self, class: &Node, base: String) {
        self.bases
            .push((class.qualified_name.clone(), class.name.clone(), base));
    }

    fn sort(&mut self) {
        self.aliases.sort();
        self.types.sort();
        self.bases.sort();
    }

    /// `path`'s surface as the store holds it right after the file was
    /// re-extracted: its bases are all still unresolved references.
    fn of_file(store: &Store, path: &str) -> Result<Self> {
        let mut surface = Self::default();
        let mut classes = HashMap::new();
        for node in store.nodes_by_file_path(path)? {
            if is_c_family(node.language) && SURFACE_KINDS.contains(&node.kind) {
                surface.note_node(&node);
                classes.insert(node.id.clone(), node);
            }
        }
        for reference in store.unresolved_refs_by_file_path(path)? {
            if reference.reference_kind == EdgeKind::Extends
                && let Some(class) = classes.get(&reference.from_node_id)
            {
                surface.note_base(class, base_name(&reference.reference_name));
            }
        }
        surface.sort();
        Ok(surface)
    }
}

/// Every C/C++ file's type surface, read in one pass.
#[derive(Debug, Default)]
struct Snapshot {
    files: BTreeMap<String, TypeSurface>,
}

impl Snapshot {
    fn read(store: &Store) -> Result<Self> {
        let mut files: BTreeMap<String, TypeSurface> = BTreeMap::new();
        let mut nodes: HashMap<String, Node> = HashMap::new();
        for kind in SURFACE_KINDS {
            for node in store.nodes_by_kind(kind)? {
                if is_c_family(node.language) {
                    files
                        .entry(node.file_path.clone())
                        .or_default()
                        .note_node(&node);
                    nodes.insert(node.id.clone(), node);
                }
            }
        }
        for edge in store.edges_by_kind(EdgeKind::Extends)? {
            let Some(class) = nodes.get(&edge.source) else {
                continue;
            };
            let base = match nodes.get(&edge.target) {
                Some(target) => target.name.clone(),
                None => match store.node_by_id(&edge.target)? {
                    Some(target) => target.name,
                    None => continue,
                },
            };
            files
                .entry(class.file_path.clone())
                .or_default()
                .note_base(class, base);
        }
        for reference in store.unresolved_refs_of_kind(EdgeKind::Extends)? {
            if let Some(class) = nodes.get(&reference.from_node_id) {
                files
                    .entry(class.file_path.clone())
                    .or_default()
                    .note_base(class, base_name(&reference.reference_name));
            }
        }
        for surface in files.values_mut() {
            surface.sort();
        }
        Ok(Self { files })
    }
}

/// What one sync changed that C/C++ resolution in other files reads.
#[derive(Debug, Default)]
pub(crate) struct CppChanges {
    /// Every C/C++ file's surface before the sync first changed a C/C++
    /// file.
    snapshot: Option<Snapshot>,
    /// Each changed file that is, or was, C or C++, with its surface after
    /// the change (empty once removed or no longer C/C++).
    after: BTreeMap<String, TypeSurface>,
    /// Every changed path a source's receiver inference may read as its
    /// header.
    headers: BTreeSet<String>,
}

impl CppChanges {
    /// Before `path` changes: take the snapshot on the batch's first C/C++
    /// change. `c_family` is whether the file is C or C++ before or after it.
    pub(crate) fn before_change(
        &mut self,
        store: &Store,
        path: &str,
        c_family: bool,
    ) -> Result<()> {
        if is_header_path(path) {
            self.headers.insert(path.to_string());
        }
        if c_family && self.snapshot.is_none() {
            self.snapshot = Some(Snapshot::read(store)?);
        }
        Ok(())
    }

    /// After `path` changed (or left the index): record its surface.
    /// `c_family` is as in [`Self::before_change`]; `present` is whether the
    /// file is still indexed as C or C++.
    pub(crate) fn after_change(
        &mut self,
        store: &Store,
        path: &str,
        c_family: bool,
        present: bool,
    ) -> Result<()> {
        if c_family {
            let surface = if present {
                TypeSurface::of_file(store, path)?
            } else {
                TypeSurface::default()
            };
            self.after.insert(path.to_string(), surface);
        }
        Ok(())
    }

    /// The names a changed surface touches: see the module documentation.
    fn touched_names(&self, store: &Store) -> Result<HashSet<String>> {
        let Some(snapshot) = &self.snapshot else {
            return Ok(HashSet::new());
        };
        let empty = TypeSurface::default();
        let mut aliases_changed = HashSet::new();
        let mut types_changed = HashSet::new();
        let mut classes_changed = HashSet::new();
        for (path, after) in &self.after {
            let before = snapshot.files.get(path).unwrap_or(&empty);
            for (_, name, _) in symmetric_difference(&before.aliases, &after.aliases) {
                aliases_changed.insert(name.clone());
            }
            for (_, _, name) in symmetric_difference(&before.types, &after.types) {
                types_changed.insert(name.clone());
            }
            let bases = |surface: &TypeSurface| {
                let mut by_class: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
                for (qualified, name, base) in &surface.bases {
                    by_class
                        .entry((qualified.clone(), name.clone()))
                        .or_default()
                        .push(base.clone());
                }
                by_class
            };
            let (old, new) = (bases(before), bases(after));
            for (class, old_bases) in &old {
                if new.get(class) != Some(old_bases) {
                    classes_changed.insert(class.1.clone());
                }
            }
            for (class, new_bases) in &new {
                if old.get(class) != Some(new_bases) {
                    classes_changed.insert(class.1.clone());
                }
            }
        }
        // Every alias the lookup may see, before the change and now.
        let mut aliases: Vec<(String, String)> = snapshot
            .files
            .values()
            .flat_map(|surface| surface.aliases.iter())
            .map(|(_, name, declaration)| (name.clone(), declaration.clone()))
            .collect();
        for node in store.nodes_by_kind(NodeKind::TypeAlias)? {
            if is_c_family(node.language) {
                aliases.push((node.name, node.signature.unwrap_or_default()));
            }
        }
        let alias_names: HashSet<&str> = aliases.iter().map(|(name, _)| name.as_str()).collect();
        let mut names: HashSet<String> = aliases_changed;
        names.extend(
            types_changed
                .into_iter()
                .filter(|name| alias_names.contains(name.as_str())),
        );
        // A class whose bases changed, and every class deriving from it.
        let mut derivations: Vec<(String, String)> = snapshot
            .files
            .values()
            .chain(self.after.values())
            .flat_map(|surface| surface.bases.iter())
            .map(|(_, class, base)| (class.clone(), base.clone()))
            .collect();
        derivations.sort();
        derivations.dedup();
        let mut classes = classes_changed;
        loop {
            let before = classes.len();
            for (class, base) in &derivations {
                if classes.contains(base) {
                    classes.insert(class.clone());
                }
            }
            if classes.len() == before {
                break;
            }
        }
        names.extend(classes);
        // Close over the aliases whose declarations mention a touched name.
        loop {
            let before = names.len();
            for (name, declaration) in &aliases {
                if !names.contains(name) && mentions(declaration, &names) {
                    names.insert(name.clone());
                }
            }
            if names.len() == before {
                break;
            }
        }
        Ok(names)
    }

    /// The unchanged files this sync must re-resolve whole: every indexed
    /// C/C++ file on `bootstrap`, otherwise the two lanes of the module
    /// documentation. `reindexed` files resolve anyway and are left out.
    pub(crate) fn dependent_files(
        &self,
        store: &Store,
        project_root: &Path,
        max_file_size: u64,
        reindexed: &HashSet<String>,
        bootstrap: bool,
    ) -> Result<BTreeSet<String>> {
        if !bootstrap && self.headers.is_empty() && self.snapshot.is_none() {
            return Ok(BTreeSet::new());
        }
        let files = store.all_files()?;
        let mut out = BTreeSet::new();
        if bootstrap {
            out.extend(
                files
                    .iter()
                    .filter(|file| is_c_family(file.language))
                    .map(|file| file.path.clone()),
            );
        } else {
            let sources = same_stem_sources(files.iter().map(|file| file.path.as_str()));
            for header in &self.headers {
                out.extend(
                    sources
                        .get(header_stem(header))
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            let names = self.touched_names(store)?;
            #[cfg(test)]
            tests::LAST_NAMES.with(|last| {
                let mut sorted: Vec<String> = names.iter().cloned().collect();
                sorted.sort();
                *last.borrow_mut() = Some(sorted);
            });
            if !names.is_empty() {
                for file in &files {
                    if !(is_c_family(file.language) || is_header_path(&file.path))
                        || reindexed.contains(&file.path)
                    {
                        continue;
                    }
                    let full = project_root.join(&file.path);
                    let Ok((_, SourceText::Text(text))) =
                        read_source_file(&full, &file.path, max_file_size)
                    else {
                        continue;
                    };
                    if mentions(&text, &names) {
                        out.insert(file.path.clone());
                        if is_header_path(&file.path) {
                            out.extend(
                                sources
                                    .get(header_stem(&file.path))
                                    .into_iter()
                                    .flatten()
                                    .cloned(),
                            );
                        }
                    }
                }
            }
        }
        out.retain(|file| !reindexed.contains(file));
        Ok(out)
    }
}

/// The items of `a` and `b` (sorted multisets) that one has more of.
fn symmetric_difference<'s, T: Ord>(a: &'s [T], b: &'s [T]) -> Vec<&'s T> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                out.push(x);
                i += 1;
            }
            (Some(_), Some(y)) => {
                out.push(y);
                j += 1;
            }
            (Some(x), None) => {
                out.push(x);
                i += 1;
            }
            (None, Some(y)) => {
                out.push(y);
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

/// `db/memtable` of `db/memtable.h`.
fn header_stem(header: &str) -> &str {
    header.rsplit_once('.').map_or(header, |(stem, _)| stem)
}

/// The indexed sources by stem, for the extensions receiver inference reads
/// a same-stem header from.
fn same_stem_sources<'p>(paths: impl Iterator<Item = &'p str>) -> HashMap<&'p str, Vec<String>> {
    let mut sources: HashMap<&str, Vec<String>> = HashMap::new();
    for path in paths {
        if let Some((stem, extension)) = path.rsplit_once('.')
            && !stem.ends_with('/')
            && SOURCE_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        {
            sources.entry(stem).or_default().push(path.to_string());
        }
    }
    sources
}

/// Whether the index holds no proof that every C/C++ dependent is current.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every C/C++ dependent is current: a full index, a migration,
/// or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    std::thread_local! {
        /// The names the last non-bootstrap [`CppChanges::dependent_files`]
        /// on this thread touched, sorted.
        pub(crate) static LAST_NAMES: std::cell::RefCell<Option<Vec<String>>> =
            const { std::cell::RefCell::new(None) };
    }

    #[test]
    fn a_base_reads_as_its_last_segment_without_template_arguments() {
        assert_eq!(base_name("ns::Base<T, U<V>>"), "Base");
        assert_eq!(base_name("::Base"), "Base");
        assert_eq!(base_name("Base"), "Base");
    }

    #[test]
    fn identifiers_skip_number_suffixes() {
        assert_eq!(
            identifiers("Table t = 0x1F + Lookup_2;").collect::<Vec<_>>(),
            ["Table", "t", "Lookup_2"]
        );
    }

    #[test]
    fn multiset_differences_count_duplicates() {
        let a = ["a", "a", "b"];
        let b = ["a", "c"];
        assert_eq!(symmetric_difference(&a, &b), [&"a", &"b", &"c"]);
        assert!(symmetric_difference(&a, &a).is_empty());
    }

    #[test]
    fn a_header_reaches_its_sources_of_any_extension_case() {
        let paths = [
            "db/memtable.cc",
            "db/memtable.h",
            "db/MemTable.CPP",
            "db/other.cxx",
        ];
        let sources = same_stem_sources(paths.into_iter());
        assert_eq!(
            sources.get(header_stem("db/memtable.h")),
            Some(&vec!["db/memtable.cc".to_string()])
        );
        assert!(is_header_path("a.hpp") && is_header_path("a.hxx") && !is_header_path("a.H"));
    }
}
