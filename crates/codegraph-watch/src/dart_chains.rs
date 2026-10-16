//! What a later link of a Dart call chain reads of other files
//! (W4-DART-03b, [`codegraph_resolve::dart_chains`]), so that a sync
//! re-resolves an unchanged file as a fresh index would.
//!
//! A chain's typing reads, by name, the declarations its own file's code
//! names: the chain's head and links, the declarations of its locals, the
//! type it is written in, the members the types they reach declare. It reads
//! other files only through such a name: a node of it (a member, a function,
//! a constant, a type), the statement declaring it (a field's or constant's
//! type and initializer, a method's head, a getter's or setter's keyword), a
//! type's head; or through a declaration that names it in turn; or by the
//! `call` a called value runs. And where its typing first reads otherwise
//! after a change, it has read the same up to there, so the name it looks up
//! there is written in its file or in a declaration it read on the way.
//!
//! So a sync collects the names whose declarations a changed Dart file
//! changed: a member or top-level statement declaring one came, went or
//! reads otherwise (the member surface rows keep the statements,
//! [`MemberSurface`]: a node that came, went or changed anything a typing
//! reads changed its statement); it is a member of an extension whose head
//! changed; a library's visibility of it moved. Every constant and field
//! typed by an initializer naming one joins them, until none is added.
//!
//! A class whose head changed, or that came or went, answers a lookup
//! otherwise, and so does every type below it, but only of a name its
//! ancestry declares, before the change or after it, or an extension on a
//! type of it declares (every extension's, for a class that came or went: a
//! new project type is one an extension `on` any type applies to,
//! [`crate::dart_heads::ancestry_owners`]); a typing that looks one up on
//! such a type has read the type's name on the way, and the member's. So
//! such a class's name, and what every declaration whose type names one of
//! them declares (a member typed by one, a subtype, whose head names it, a
//! constant or field an initializer naming one types), until none is added,
//! move a chain only together with one of those member names: a constructor's
//! type is its class's.
//!
//! In each unchanged Dart file whose text writes such names, the chain links
//! (its parked calls, and its calls' edges by the member they reach) whose
//! typing reads them are retried at their sites, read as the typing reads
//! them ([`chain_link_reads`]): a local or a parameter through the
//! declaration the typing's own lookup finds, its name read only as a type's
//! (the typing asks only whether one of its name exists); with `call` among
//! the names, every chain link is.
//!
//! The lane rides on the member surface rows and their marker: a sync that
//! rebuilds them re-resolves every Dart file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{EdgeKind, Language};
use codegraph_resolve::dart_chains::{
    ChainReads, DartCode, can_name_a_declaration, chain_link_names, chain_link_reads,
    dart_declared_names, dart_type_words, dart_words,
};
use codegraph_resolve::member_types::{MemberClass, MemberSurface, SURFACE_ROW_KEY_PREFIX};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// How many words a file's text is searched for one by one before it is
/// read word by word instead.
const FEW_WORDS: usize = 16;

/// The names a sync's changed declarations may move a chain link through
/// (see the module docs).
#[derive(Debug, Default)]
struct ChainWords {
    /// Names whose declarations changed, looked up by them.
    plain: HashSet<String>,
    /// Names a type of which came, went or changed: a local's or
    /// parameter's name the typing asks a type of is read as one.
    types: HashSet<String>,
    /// Names typed through a type whose head changed, came or went.
    typed: HashSet<String>,
    /// The member names a lookup on such a type can reach otherwise.
    ancestry: HashSet<String>,
    /// Every chain link moves: a `call` method changed, or is typed through
    /// such a type.
    all: bool,
}

impl ChainWords {
    /// Whether a chain link whose typing reads `reads` may move.
    fn moves(&self, reads: &ChainReads) -> bool {
        self.all
            || reads.names.iter().any(|name| self.plain.contains(name))
            || reads.types.iter().any(|name| self.types.contains(name))
            || (reads.names.iter().any(|name| self.typed.contains(name))
                && reads.names.iter().any(|name| self.ancestry.contains(name)))
    }

    /// Whether a file whose text is `text` may hold such a link. A few words
    /// are looked for as text first, and only a file holding one is read
    /// word by word.
    fn written_in(&self, text: &str) -> bool {
        if self.all {
            return true;
        }
        let may_write = |words: &HashSet<String>| {
            words.len() > FEW_WORDS || words.iter().any(|word| text.contains(word.as_str()))
        };
        let (plain, typed) = (
            may_write(&self.plain) || may_write(&self.types),
            may_write(&self.typed),
        );
        if !plain && !typed {
            return false;
        }
        let (mut typed, mut ancestry) = (false, false);
        for word in dart_words(text) {
            if plain && (self.plain.contains(word) || self.types.contains(word)) {
                return true;
            }
            typed |= self.typed.contains(word);
            ancestry |= self.ancestry.contains(word);
            if typed && ancestry {
                return true;
            }
        }
        false
    }
}

/// Each declared name's statements in `statements`, sorted.
fn statements_by_name(statements: &[String]) -> BTreeMap<String, Vec<&str>> {
    let mut out: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for statement in statements {
        for name in dart_declared_names(statement) {
            out.entry(name).or_default().push(statement.as_str());
        }
    }
    for statements in out.values_mut() {
        statements.sort_unstable();
    }
    out
}

/// The names whose declaring statements differ between `old` and `new`, and
/// every name either declares when `whole`.
fn changed_names(old: &[String], new: &[String], whole: bool, out: &mut BTreeSet<String>) {
    let (before, after) = (statements_by_name(old), statements_by_name(new));
    for name in before.keys().chain(after.keys()) {
        if whole || before.get(name) != after.get(name) {
            out.insert(name.clone());
        }
    }
}

/// What a changed file's member surface rows move. `plain`: per class (by
/// qualified name) and at the top level, the names whose declaring
/// statements differ, every member name of an extension (`extensions`)
/// whose head changed, came or went; `types`: every top-level name whose
/// declaration differs (a class that came or went among them); `typed`: a
/// class whose head changed, came or went that is no extension, whose
/// lookups the ancestry lane narrows ([`crate::dart_heads::ancestry_owners`]).
fn row_changes(
    old: Option<&MemberSurface>,
    new: Option<&MemberSurface>,
    extensions: &BTreeSet<String>,
    plain: &mut BTreeSet<String>,
    types: &mut BTreeSet<String>,
    typed: &mut BTreeSet<String>,
) {
    let empty = MemberSurface::default();
    let (old, new) = (old.unwrap_or(&empty), new.unwrap_or(&empty));
    let mut top = BTreeSet::new();
    changed_names(&old.top, &new.top, false, &mut top);
    let mut classes: BTreeMap<&str, (Vec<&MemberClass>, Vec<&MemberClass>)> = BTreeMap::new();
    for class in &old.classes {
        classes
            .entry(class.qualified_name.as_str())
            .or_default()
            .0
            .push(class);
    }
    for class in &new.classes {
        classes
            .entry(class.qualified_name.as_str())
            .or_default()
            .1
            .push(class);
    }
    let mut seeds = BTreeSet::new();
    for (_, (before, after)) in classes {
        let flat = |classes: &[&MemberClass]| -> Vec<String> {
            classes
                .iter()
                .flat_map(|class| class.statements.clone())
                .collect()
        };
        let whole = before.len() != after.len()
            || before
                .iter()
                .zip(&after)
                .any(|(a, b)| a.head != b.head || a.supers != b.supers);
        let mut extension = false;
        if whole {
            for class in before.iter().chain(&after) {
                extension |= extensions.contains(&class.name);
            }
            if extension {
                plain.extend(before.iter().chain(&after).map(|class| class.name.clone()));
            } else {
                seeds.extend(before.iter().chain(&after).map(|class| class.name.clone()));
            }
        }
        changed_names(&flat(&before), &flat(&after), extension, plain);
    }
    types.extend(top.iter().cloned());
    plain.extend(top.into_iter().filter(|name| !seeds.contains(name)));
    typed.extend(seeds);
}

/// A declaration of a member surface row, as the closure reads it: what it
/// declares, the words saying its type that can name a declaration
/// ([`dart_type_words`]), and whether they are its initializer's.
struct Declaration<'r> {
    declared: Vec<String>,
    typing: HashSet<&'r str>,
    untyped: bool,
}

/// The declarations of `row` that declare a name.
fn declarations(row: &MemberSurface) -> impl Iterator<Item = Declaration<'_>> {
    row.top
        .iter()
        .map(|statement| (statement, None))
        .chain(row.classes.iter().flat_map(|class| {
            class
                .statements
                .iter()
                .map(|statement| (statement, Some(class.name.as_str())))
        }))
        .filter_map(|(statement, class)| {
            let declared: Vec<String> = dart_declared_names(statement)
                .into_iter()
                .filter(|name| can_name_a_declaration(name))
                .collect();
            let (typing, untyped) = dart_type_words(statement, declared.first()?, class);
            Some(Declaration {
                typing: typing
                    .into_iter()
                    .filter(|word| can_name_a_declaration(word))
                    .collect(),
                declared,
                untyped,
            })
        })
}

/// What the declarations whose type names one of `typed` declare joins
/// `typed`, and what the untyped ones whose initializer names one of `words`
/// declare joins `words`, until none is added.
fn close_over(
    declarations: Vec<Declaration<'_>>,
    typed: &mut HashSet<String>,
    words: &mut HashSet<String>,
) {
    let mut pending: Vec<(Declaration, bool)> = declarations
        .into_iter()
        .map(|declaration| (declaration, false))
        .collect();
    loop {
        let mut grew = false;
        pending.retain_mut(|(declaration, joined)| {
            if declaration.typing.iter().any(|word| typed.contains(*word)) {
                for name in &declaration.declared {
                    grew |= typed.insert(name.clone());
                }
                return false;
            }
            if !*joined
                && declaration.untyped
                && declaration.typing.iter().any(|word| words.contains(*word))
            {
                *joined = true;
                for name in &declaration.declared {
                    grew |= words.insert(name.clone());
                }
            }
            true
        });
        if !grew {
            break;
        }
    }
}

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

/// The sites of the chain links in unchanged Dart files that a sync's changed
/// declarations may move (see the module docs). `names` are the names a
/// library's visibility moved; `dart` what the changed files' Dart heads
/// were and are; `members` the member surface rows of the changed files,
/// before and after, whose statements say every declaration a typing reads
/// (a node that changed changed its statement).
#[allow(clippy::too_many_arguments)]
pub(crate) fn chain_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    members: &crate::member_surfaces::MemberChanges,
    names: &BTreeSet<String>,
    dart: &crate::dart_heads::DartChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<Vec<FileReferenceSite>> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    let extensions = dart.extension_names();
    let (mut plain, mut types, mut seeds) = (names.clone(), BTreeSet::new(), BTreeSet::new());
    for (_, old, new) in members.rows() {
        row_changes(old, new, &extensions, &mut plain, &mut types, &mut seeds);
    }
    if plain.is_empty() && types.is_empty() && seeds.is_empty() {
        return Ok(Vec::new());
    }
    let named = |names: BTreeSet<String>| -> HashSet<String> {
        names
            .into_iter()
            .filter(|name| can_name_a_declaration(name))
            .collect()
    };
    let mut typed = named(seeds.clone());
    let mut words = named(plain);
    // The unchanged Dart files and their text.
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for path in store.file_paths_of_language(Language::Dart)? {
        if unchanged(&path)
            && let Some(text) = read_text(project_root, &path, max_file_size)
        {
            texts.insert(path, text);
        }
    }
    // What the declarations typed through one declare, until none is added:
    // of the unchanged files' rows and the changed files' new ones. A
    // declaration whose type names a type whose head moved types what it
    // declares by it; an untyped one, by whatever its initializer names.
    let stored: HashMap<String, MemberSurface> = store
        .project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            let path = key.strip_prefix(SURFACE_ROW_KEY_PREFIX)?;
            texts
                .contains_key(path)
                .then(|| MemberSurface::decode(&value).map(|row| (path.to_string(), row)))
                .flatten()
        })
        .collect();
    close_over(
        stored
            .values()
            .chain(members.rows().filter_map(|(_, _, new)| new))
            .flat_map(declarations)
            .collect(),
        &mut typed,
        &mut words,
    );
    // The names declared by the types whose members a lookup on such a type
    // reaches, before the change and after it.
    let owners = crate::dart_heads::ancestry_owners(store, dart, &seeds)?;
    let ancestry: HashSet<String> = stored
        .values()
        .chain(
            members
                .rows()
                .flat_map(|(_, old, new)| old.into_iter().chain(new)),
        )
        .flat_map(|row| &row.classes)
        .filter(|class| owners.contains(&class.name))
        .flat_map(|class| &class.statements)
        .flat_map(|statement| dart_declared_names(statement))
        .filter(|name| can_name_a_declaration(name))
        .collect();
    let chain = ChainWords {
        all: words.contains("call") || typed.contains("call"),
        plain: words,
        types: named(types),
        typed,
        ancestry,
    };
    let mut sites: BTreeSet<FileReferenceSite> = BTreeSet::new();
    for (path, text) in &texts {
        if !chain.written_in(text) {
            continue;
        }
        let code = DartCode::of(text);
        let nodes = store.nodes_by_file_path(path)?;
        let moves = |name: &str, line: i64, col: i64| {
            chain_link_reads(&code, &nodes, name, line, col)
                .is_some_and(|reads| chain.moves(&reads))
        };
        let mut site = |from_node_id: String, line: i64, col: i64| {
            sites.insert(FileReferenceSite {
                file_path: path.clone(),
                site: Some(ReferenceSite {
                    from_node_id,
                    line,
                    col,
                }),
            });
        };
        for reference in store.unresolved_refs_by_file_path(path)? {
            if reference.language == Language::Dart
                && reference.reference_kind == EdgeKind::Calls
                && !reference.is_function_ref
                && moves(&reference.reference_name, reference.line, reference.col)
            {
                site(reference.from_node_id, reference.line, reference.col);
            }
        }
        // A resolved call keeps no name: the member it reaches is the
        // link's, recorded bare or past the segment written before it.
        for (source, line, col, member) in store.call_sites_in_file(path)? {
            if chain_link_names(&code, &member, line, col)
                .iter()
                .any(|name| moves(name, line, col))
            {
                site(source, line, col);
            }
        }
    }
    Ok(sites.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_declaration_is_typed_through_joins_the_names() {
        let row = MemberSurface {
            classes: vec![MemberClass {
                name: "Holder".to_string(),
                qualified_name: "Holder".to_string(),
                supers: Vec::new(),
                digest: String::new(),
                head: String::new(),
                lines: Vec::new(),
                statements: vec![
                    "Holder.from() : value = make();".to_string(),
                    "Probe get value;".to_string(),
                ],
            }],
            top: vec![
                "Probe make() => ;".to_string(),
                "final viaMake = make();".to_string(),
                "final Other fixed = make();".to_string(),
                "final viaChanged = changed();".to_string(),
                "class Child extends Probe {}".to_string(),
            ],
        };
        let mut typed: HashSet<String> = HashSet::from(["Probe".to_string()]);
        let mut words: HashSet<String> =
            HashSet::from(["Probe".to_string(), "changed".to_string()]);
        close_over(declarations(&row).collect(), &mut typed, &mut words);
        let sorted = |set: &HashSet<String>| {
            let mut out: Vec<String> = set.iter().cloned().collect();
            out.sort();
            out
        };
        assert_eq!(
            sorted(&typed),
            ["Child", "Probe", "make", "value", "viaMake"]
        );
        // What the typed names reach stays out of the plain words: the
        // ancestry lane narrows it.
        assert_eq!(sorted(&words), ["Probe", "changed", "viaChanged"]);
    }

    #[test]
    fn a_name_moves_when_a_statement_declaring_it_reads_otherwise() {
        let old = vec!["final a = make();".to_string(), "int b() => 1;".to_string()];
        let new = vec![
            "final a = other();".to_string(),
            "int b() => 1;".to_string(),
        ];
        let mut out = BTreeSet::new();
        changed_names(&old, &new, false, &mut out);
        assert_eq!(out, BTreeSet::from(["a".to_string()]));
        let mut out = BTreeSet::new();
        changed_names(&old, &new, true, &mut out);
        assert_eq!(out, BTreeSet::from(["a".to_string(), "b".to_string()]));
    }
}
