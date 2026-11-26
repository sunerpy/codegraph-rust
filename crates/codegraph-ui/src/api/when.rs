//! `when` on a wire edge — the branch conditions its call site sits under,
//! read from the source at request time. Upstream `src/ui-server/api/when.ts`
//! (`e8ddc4e2`); the walk itself is `codegraph_graph::branch_guards`.
//!
//! The viewer groups a symbol's edges into relations; [`annotate_when`]
//! annotates the edges of a set of relations in one pass, parsing each file
//! once. A file that drifted since the index sync is skipped: the recorded
//! line no longer reliably points at the call, and a label at the wrong line
//! is worse than none. The pass is bounded so a hub with hundreds of callers
//! cannot turn one Symbol view into a parse of the repository: at most
//! [`MAX_FILES`] files and [`MAX_SITES`] sites, and once the first file is
//! read, no new file after [`BUDGET`] has passed. The batches are taken in
//! order (the focal file first), so a budget that runs out leaves the rails
//! furthest from the focal symbol unlabelled, never the focal file's own.
//!
//! The endpoints that walk chains rather than annotate rails (the Screens
//! view's transitions, the Steps view) read one site at a time through a
//! [`SiteReader`]: each file looked up and read once per request, drifted
//! files unread, and the sites it reads counted against a budget.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_graph::branch_guards::{
    BranchGuard, MAX_PARSE_BYTES, guard_label, guards_at, supports_branch_guards,
};

use super::Ctx;
use super::source::{
    find_indexed_file, has_drifted_on_disk, max_file_size, read_indexed_file_text, to_request_path,
};
use super::wire::WireEdge;
use crate::security::resolve_project_file;
use codegraph_graph::call_sites;

/// Distinct files parsed per request.
pub const MAX_FILES: usize = 24;
/// Call sites labelled per request.
pub const MAX_SITES: usize = 400;
/// Wall-clock allowance for the whole pass, checked before each file after
/// the first.
pub const BUDGET: Duration = Duration::from_millis(40);

/// The edges of one rail whose call sites are all in `file` (a project
/// relative path, either separator).
pub struct WhenBatch<'e> {
    pub file: String,
    pub edges: Vec<&'e mut WireEdge>,
}

/// A file the pass may read: in the index, of a language with rules, not
/// drifted, and inside the project.
pub(crate) struct SiteFile {
    /// The parse cache's key: the file's absolute path.
    pub key: String,
    /// The path the index stores the file under.
    pub stored: String,
    pub language: Language,
}

/// Label the edges of `batches` with the conditions their call sites run
/// under (upstream `annotateWhen`). An edge without a recorded line, or one
/// whose site is unconditional, keeps `when` unset.
pub fn annotate_when(ctx: &Ctx<'_>, batches: Vec<WhenBatch<'_>>) {
    let max_bytes = max_file_size(ctx);
    annotate_batches(
        batches,
        Some(BUDGET),
        |file| site_file(ctx, file, max_bytes),
        |site| read_indexed_file_text(ctx, &site.stored, MAX_PARSE_BYTES as u64),
    );
}

/// Whether the pass may read `file`, and where.
pub(crate) fn site_file(ctx: &Ctx<'_>, file: &str, max_bytes: u64) -> Option<SiteFile> {
    let (record, stored) = find_indexed_file(ctx, file).ok()??;
    if !supports_branch_guards(record.language) {
        return None;
    }
    if has_drifted_on_disk(ctx, &stored, &record, max_bytes) {
        return None;
    }
    let absolute = resolve_project_file(ctx.project_root(), &stored).ok()?;
    Some(SiteFile {
        key: absolute.to_string_lossy().into_owned(),
        stored,
        language: record.language,
    })
}

/// The pass itself, with the file lookup and the read handed in: batches of
/// one file merge in first-seen order, each file counts once it passes the
/// lookup and has a site with a line, and a file that cannot be read after
/// that still counts (upstream reads it inside `guardsForFile`). `budget`
/// `None` switches the wall clock off, for the tests that pin the caps.
pub(crate) fn annotate_batches<R, T>(
    batches: Vec<WhenBatch<'_>>,
    budget: Option<Duration>,
    mut resolve: R,
    mut read: T,
) where
    R: FnMut(&str) -> Option<SiteFile>,
    T: FnMut(&SiteFile) -> Option<String>,
{
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<&mut WireEdge>> = HashMap::new();
    for batch in batches {
        let entry = by_file.entry(batch.file.clone()).or_insert_with(|| {
            order.push(batch.file.clone());
            Vec::new()
        });
        entry.extend(batch.edges);
    }
    let started = Instant::now();
    let mut files = 0usize;
    let mut sites = 0usize;
    for file in order {
        if files >= MAX_FILES || sites >= MAX_SITES {
            return;
        }
        if files > 0 && budget.is_some_and(|budget| started.elapsed() > budget) {
            return;
        }
        let edges = by_file.remove(&file).unwrap_or_default();
        let Some(site) = resolve(&file) else {
            continue;
        };
        let with_line: Vec<&mut WireEdge> = edges
            .into_iter()
            .filter(|edge| edge.line.is_some_and(|line| line > 0))
            .collect();
        if with_line.is_empty() {
            continue;
        }
        files += 1;
        sites += with_line.len();
        let Some(text) = read(&site) else {
            continue;
        };
        for edge in with_line {
            let Some(line) = edge.line.and_then(|line| usize::try_from(line).ok()) else {
                continue;
            };
            let column = edge.col.and_then(|col| usize::try_from(col).ok());
            let label = guard_label(&guards_at(&site.key, &text, site.language, line, column));
            if !label.is_empty() {
                edge.when = Some(label);
            }
        }
    }
}

/// The `'use server'` / `'use client'` a JS-family file opens with, and
/// whether a definition itself opens with `'use server'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Directive {
    pub file: Option<&'static str>,
    pub own: bool,
}

/// A file read for its directives or declarations, at most.
const MAX_DIRECTIVE_FILE: u64 = 512 * 1024;

/// A file a [`SiteReader`] may read: where it is, its text for the parser,
/// and — read when first asked — its whole text and its line facts.
struct ReadFile {
    site: SiteFile,
    text: Option<String>,
    full: Option<Option<String>>,
    facts: Option<Option<codegraph_resolve::source_facts::SourceFacts>>,
}

/// A per-request reader of what ONE call site, or one definition, says —
/// its conditions, loops, arguments, trigger, the call as written, the
/// decorators, member types, returns and directives — for the endpoints that
/// walk chains rather than annotate rails: the Screens view's transitions and
/// the Steps view (upstream `createSiteReader`, `when.ts:96-281`). Each file
/// is looked up and read once; a file that is not indexed, has no rules,
/// drifted since the index or cannot be read yields nothing; and the sites
/// whose conditions, arguments or call are read are counted against a
/// budget, so a wide walk cannot turn one request into a parse of the
/// repository. The other readings are not counted: they climb trees the
/// counted ones parsed anyway.
pub struct SiteReader<'c, 'a> {
    ctx: &'c Ctx<'a>,
    max_sites: usize,
    max_bytes: u64,
    sites: usize,
    /// Lookups only, never iterated: a file's answer, by the path asked.
    files: HashMap<String, Option<ReadFile>>,
}

/// A site's 1-based line, when it has one (upstream's `!site.line`).
fn site_line(line: Option<i64>) -> Option<usize> {
    line.filter(|line| *line > 0)
        .and_then(|line| usize::try_from(line).ok())
}

fn site_column(column: Option<i64>) -> Option<usize> {
    column.and_then(|column| usize::try_from(column).ok())
}

fn is_js_family(language: Language) -> bool {
    matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Tsx | Language::Jsx
    )
}

/// The directive a file opens with, past whitespace and comments (upstream's
/// `^(?:\s|//[^\n]*\n|/\*[\s\S]*?\*/)*(['"])use (server|client)\1`, whose
/// back-reference is checked by hand).
fn file_directive(text: &str) -> Option<&'static str> {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.len() != rest.len() {
            rest = trimmed;
            continue;
        }
        if let Some(after) = rest.strip_prefix("//") {
            rest = &after[after.find('\n')? + 1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            rest = &after[after.find("*/")? + 2..];
            continue;
        }
        break;
    }
    let quote = rest.chars().next().filter(|ch| matches!(ch, '\'' | '"'))?;
    let body = &rest[1..];
    ["server", "client"].into_iter().find(|word| {
        body.strip_prefix("use ")
            .and_then(|rest| rest.strip_prefix(word))
            .is_some_and(|rest| rest.starts_with(quote))
    })
}

/// A line that is the `'use server'` directive alone (upstream's
/// `^\s*(['"])use server\1\s*;?\s*$`, per line).
fn is_own_directive(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_suffix(';').map_or(line, str::trim_end);
    matches!(line, "'use server'" | "\"use server\"")
}

impl<'c, 'a> SiteReader<'c, 'a> {
    /// A reader that reads at most `max_sites` counted sites (upstream's
    /// default is 600).
    pub fn new(ctx: &'c Ctx<'a>, max_sites: usize) -> Self {
        Self {
            ctx,
            max_sites,
            max_bytes: max_file_size(ctx),
            sites: 0,
            files: HashMap::new(),
        }
    }

    /// The file `file` names, looked up once per request.
    fn resolve(&mut self, file: &str) -> Option<&mut ReadFile> {
        let (ctx, max_bytes) = (self.ctx, self.max_bytes);
        self.files
            .entry(to_request_path(file))
            .or_insert_with_key(|posix| {
                site_file(ctx, posix, max_bytes).map(|site| {
                    let text = read_indexed_file_text(ctx, &site.stored, MAX_PARSE_BYTES as u64);
                    ReadFile {
                        site,
                        text,
                        full: None,
                        facts: None,
                    }
                })
            })
            .as_mut()
    }

    /// Count a site of `file` when the budget allows and the file resolves:
    /// whether it was counted (upstream counts it before reading the tree).
    fn count(&mut self, file: &str, language: Language) -> bool {
        if self.sites >= self.max_sites || !supports_branch_guards(language) {
            return false;
        }
        if self.resolve(file).is_none() {
            return false;
        }
        self.sites += 1;
        true
    }

    /// Whether `file` may be read for a reading that is not counted.
    fn readable(&mut self, file: &str, language: Language) -> bool {
        supports_branch_guards(language) && self.resolve(file).is_some()
    }

    /// The parse key, text and language of a file already resolved.
    fn cached(&self, file: &str) -> Option<(&str, &str, Language)> {
        let read = self.files.get(&to_request_path(file))?.as_ref()?;
        Some((
            read.site.key.as_str(),
            read.text.as_deref()?,
            read.site.language,
        ))
    }

    /// A file's whole text, once, for the directive and declaration reads.
    fn full_text(&mut self, file: &str) -> Option<String> {
        let ctx = self.ctx;
        let read = self.resolve(file)?;
        if read.full.is_none() {
            read.full = Some(read_indexed_file_text(
                ctx,
                &read.site.stored,
                MAX_DIRECTIVE_FILE,
            ));
        }
        read.full.clone().flatten()
    }

    /// The conditions a site in `file` runs under, outermost first; empty
    /// when it is unconditional, unreadable or past the budget.
    pub fn guards(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Vec<BranchGuard> {
        let Some(line) = site_line(line) else {
            return Vec::new();
        };
        if !self.count(file, language) {
            return Vec::new();
        }
        let Some((key, text, language)) = self.cached(file) else {
            return Vec::new();
        };
        guards_at(key, text, language, line, site_column(column))
    }

    /// [`Self::guards`], joined the way a label prints them; empty when
    /// unconditional.
    pub fn when(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> String {
        guard_label(&self.guards(file, language, line, column))
    }

    /// The loops a site is written inside, outermost first. Not counted.
    pub fn loops(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Vec<call_sites::SiteLoop> {
        let Some(line) = site_line(line) else {
            return Vec::new();
        };
        if !self.readable(file, language) {
            return Vec::new();
        }
        let Some((key, text, language)) = self.cached(file) else {
            return Vec::new();
        };
        call_sites::loops_at(key, text, language, line, site_column(column))
    }

    /// What the site passes, abbreviated; `None` when unreadable, `''` for
    /// an empty list.
    pub fn args(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Option<String> {
        let line = site_line(line)?;
        if !self.count(file, language) {
            return None;
        }
        let (key, text, language) = self.cached(file)?;
        call_sites::call_arguments_at(key, text, language, line, site_column(column))
    }

    /// What fires the site. Not counted.
    pub fn trigger(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Option<call_sites::SiteTrigger> {
        let line = site_line(line)?;
        if !self.readable(file, language) {
            return None;
        }
        let (key, text, language) = self.cached(file)?;
        call_sites::trigger_at(key, text, language, line, site_column(column))
    }

    /// The call as written and what it passes; `callee` names the call when
    /// a position is shared.
    pub fn call_site(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
        callee: Option<&str>,
    ) -> Option<call_sites::CallSite> {
        let line = site_line(line)?;
        if !self.count(file, language) {
            return None;
        }
        let (key, text, language) = self.cached(file)?;
        call_sites::call_site_at(key, text, language, line, site_column(column), callee)
    }

    /// The decorators on a definition and on its class. Not counted.
    pub fn decorators(
        &mut self,
        file: &str,
        language: Language,
        start_line: i64,
    ) -> Option<call_sites::DefinitionDecorators> {
        let line = site_line(Some(start_line))?;
        if !self.readable(file, language) {
            return None;
        }
        let (key, text, language) = self.cached(file)?;
        call_sites::decorators_of(key, text, language, line)
    }

    /// The declared types of the members of a definition's class. Not
    /// counted.
    pub fn member_types(
        &mut self,
        file: &str,
        language: Language,
        start_line: i64,
    ) -> BTreeMap<String, String> {
        let Some(line) = site_line(Some(start_line)) else {
            return BTreeMap::new();
        };
        if !self.readable(file, language) {
            return BTreeMap::new();
        }
        let Some((key, text, language)) = self.cached(file) else {
            return BTreeMap::new();
        };
        call_sites::member_types_of(key, text, language, line)
    }

    /// The returns a definition makes itself. Not counted.
    pub fn returns(
        &mut self,
        file: &str,
        language: Language,
        start_line: i64,
    ) -> Vec<call_sites::ReturnSite> {
        let Some(line) = site_line(Some(start_line)) else {
            return Vec::new();
        };
        if !self.readable(file, language) {
            return Vec::new();
        }
        let Some((key, text, language)) = self.cached(file) else {
            return Vec::new();
        };
        call_sites::returns_of(key, text, language, line)
    }

    /// The directive a JS-family file opens with, and whether the definition
    /// at `start_line` opens with `'use server'` itself. Not counted.
    pub fn directive(&mut self, file: &str, language: Language, start_line: i64) -> Directive {
        if !is_js_family(language) {
            return Directive::default();
        }
        let Some(text) = self.full_text(file) else {
            return Directive::default();
        };
        let own = start_line > 0
            && text
                .split('\n')
                .skip(start_line as usize - 1)
                .take(4)
                .any(is_own_directive);
        Directive {
            file: file_directive(&text),
            own,
        }
    }

    /// A Swift `extension X {}` node — the index keeps it as a class, but it
    /// declares no type.
    pub fn swift_extension(&mut self, node: &Node) -> bool {
        if node.language != Language::Swift || node.kind != NodeKind::Class {
            return false;
        }
        let Some(text) = self.full_text(&node.file_path) else {
            return false;
        };
        let Some(read) = self.resolve(&node.file_path) else {
            return false;
        };
        let facts = read.facts.get_or_insert_with(|| {
            Some(codegraph_resolve::source_facts::SourceFacts::new(
                std::sync::Arc::from(text.as_str()),
            ))
        });
        facts.as_ref().is_some_and(|facts| {
            codegraph_resolve::swift_type_visibility::declares_extension(facts, node)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::wire::to_wire_edge;
    use codegraph_core::types::{Edge, EdgeKind};

    /// A call at line 3, column 4, under `if (x)`.
    const GUARDED: &str = "export function f(x: boolean) {\n  if (x) {\n    g()\n  }\n}\n";

    fn edge(line: Option<i64>) -> WireEdge {
        to_wire_edge(&Edge {
            id: None,
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line,
            col: line.map(|_| 4),
            provenance: None,
        })
    }

    fn site(file: &str) -> Option<SiteFile> {
        Some(SiteFile {
            key: format!("/unit/{file}"),
            stored: file.to_string(),
            language: Language::TypeScript,
        })
    }

    fn run(batches: Vec<WhenBatch<'_>>) {
        annotate_batches(batches, None, site, |_| Some(GUARDED.to_string()));
    }

    #[test]
    fn labels_a_guarded_site_and_leaves_an_edge_without_a_line_alone() {
        let mut with_line = edge(Some(3));
        let mut without = edge(None);
        run(vec![WhenBatch {
            file: "src/a.ts".to_string(),
            edges: vec![&mut with_line, &mut without],
        }]);
        assert_eq!(with_line.when.as_deref(), Some("x"));
        assert_eq!(without.when, None);
    }

    #[test]
    fn stops_after_twenty_four_files() {
        let mut edges: Vec<WireEdge> = (0..25).map(|_| edge(Some(3))).collect();
        let batches: Vec<WhenBatch<'_>> = edges
            .iter_mut()
            .enumerate()
            .map(|(i, e)| WhenBatch {
                file: format!("src/f{i:02}.ts"),
                edges: vec![e],
            })
            .collect();
        run(batches);
        let labelled = edges.iter().filter(|e| e.when.is_some()).count();
        assert_eq!(labelled, MAX_FILES);
        assert_eq!(edges[24].when, None);
        assert_eq!(edges[23].when.as_deref(), Some("x"));
    }

    #[test]
    fn stops_once_four_hundred_sites_are_read() {
        let mut first: Vec<WireEdge> = (0..MAX_SITES).map(|_| edge(Some(3))).collect();
        let mut second = edge(Some(3));
        run(vec![
            WhenBatch {
                file: "src/a.ts".to_string(),
                edges: first.iter_mut().collect(),
            },
            WhenBatch {
                file: "src/b.ts".to_string(),
                edges: vec![&mut second],
            },
        ]);
        assert!(first.iter().all(|e| e.when.as_deref() == Some("x")));
        assert_eq!(second.when, None);
    }

    #[test]
    fn batches_of_one_file_are_read_once_in_first_seen_order() {
        let mut a1 = edge(Some(3));
        let mut b = edge(Some(3));
        let mut a2 = edge(Some(3));
        let mut asked: Vec<String> = Vec::new();
        annotate_batches(
            vec![
                WhenBatch {
                    file: "src/a.ts".to_string(),
                    edges: vec![&mut a1],
                },
                WhenBatch {
                    file: "src/b.ts".to_string(),
                    edges: vec![&mut b],
                },
                WhenBatch {
                    file: "src/a.ts".to_string(),
                    edges: vec![&mut a2],
                },
            ],
            None,
            |file| {
                asked.push(file.to_string());
                site(file)
            },
            |_| Some(GUARDED.to_string()),
        );
        assert_eq!(asked, ["src/a.ts", "src/b.ts"]);
        assert_eq!(a2.when.as_deref(), Some("x"));
    }

    #[test]
    fn a_file_the_lookup_refuses_does_not_count_and_an_unreadable_one_does() {
        // Twenty-four refused files cost nothing: the twenty-fifth is read.
        let mut refused: Vec<WireEdge> = (0..24).map(|_| edge(Some(3))).collect();
        let mut read = edge(Some(3));
        let mut batches: Vec<WhenBatch<'_>> = refused
            .iter_mut()
            .enumerate()
            .map(|(i, e)| WhenBatch {
                file: format!("refused/f{i:02}.ts"),
                edges: vec![e],
            })
            .collect();
        batches.push(WhenBatch {
            file: "src/ok.ts".to_string(),
            edges: vec![&mut read],
        });
        annotate_batches(
            batches,
            None,
            |file| {
                if file.starts_with("refused/") {
                    None
                } else {
                    site(file)
                }
            },
            |_| Some(GUARDED.to_string()),
        );
        assert_eq!(read.when.as_deref(), Some("x"));

        // Twenty-four files that pass the lookup but cannot be read use the
        // cap up: the twenty-fifth is not read.
        let mut unreadable: Vec<WireEdge> = (0..24).map(|_| edge(Some(3))).collect();
        let mut late = edge(Some(3));
        let mut batches: Vec<WhenBatch<'_>> = unreadable
            .iter_mut()
            .enumerate()
            .map(|(i, e)| WhenBatch {
                file: format!("gone/f{i:02}.ts"),
                edges: vec![e],
            })
            .collect();
        batches.push(WhenBatch {
            file: "src/late.ts".to_string(),
            edges: vec![&mut late],
        });
        annotate_batches(batches, None, site, |s| {
            (!s.stored.starts_with("gone/")).then(|| GUARDED.to_string())
        });
        assert_eq!(late.when, None);
    }
}
