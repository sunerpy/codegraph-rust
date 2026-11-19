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
//! view's transitions) read one site at a time through a [`WhenReader`]:
//! each file looked up and read once per request, drifted files unread, and
//! the sites it labels counted against a budget.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use codegraph_core::types::Language;
use codegraph_graph::branch_guards::{
    BranchGuard, MAX_PARSE_BYTES, guard_label, guards_at, supports_branch_guards,
};

use super::Ctx;
use super::source::{
    find_indexed_file, has_drifted_on_disk, max_file_size, read_indexed_file_text, to_request_path,
};
use super::wire::WireEdge;
use crate::security::resolve_project_file;

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

/// A per-request reader of the conditions ONE call site sits under, for the
/// endpoints that walk chains rather than annotate rails — the Screens
/// view's transitions (upstream `createWhenReader`, the `when` half of
/// `createSiteReader`, `when.ts:150-200`). Each file is looked up and read
/// once; a file that is not indexed, has no rules, drifted since the index
/// or cannot be read yields no label; and the sites labelled are counted, so
/// a wide walk cannot turn one request into a parse of the repository.
pub struct WhenReader<'c, 'a> {
    ctx: &'c Ctx<'a>,
    max_sites: usize,
    max_bytes: u64,
    sites: usize,
    /// Lookups only, never iterated: a file's answer, by the path asked.
    files: HashMap<String, Option<(SiteFile, Option<String>)>>,
}

impl<'c, 'a> WhenReader<'c, 'a> {
    /// A reader that labels at most `max_sites` sites (upstream's default is
    /// 600).
    pub fn new(ctx: &'c Ctx<'a>, max_sites: usize) -> Self {
        Self {
            ctx,
            max_sites,
            max_bytes: max_file_size(ctx),
            sites: 0,
            files: HashMap::new(),
        }
    }

    /// The conditions a site in `file` runs under, outermost first; empty
    /// when it is unconditional, unreadable or past the budget. A site
    /// without a line is not counted; one whose file resolves is, whether
    /// or not its text can be read.
    pub fn guards(
        &mut self,
        file: &str,
        language: Language,
        line: Option<i64>,
        column: Option<i64>,
    ) -> Vec<BranchGuard> {
        let Some(line) = line
            .filter(|line| *line > 0)
            .and_then(|line| usize::try_from(line).ok())
        else {
            return Vec::new();
        };
        if self.sites >= self.max_sites || !supports_branch_guards(language) {
            return Vec::new();
        }
        let (ctx, max_bytes) = (self.ctx, self.max_bytes);
        let entry = self
            .files
            .entry(to_request_path(file))
            .or_insert_with_key(|posix| {
                site_file(ctx, posix, max_bytes).map(|site| {
                    let text = read_indexed_file_text(ctx, &site.stored, MAX_PARSE_BYTES as u64);
                    (site, text)
                })
            });
        let Some((site, text)) = entry.as_ref() else {
            return Vec::new();
        };
        self.sites += 1;
        let Some(text) = text else {
            return Vec::new();
        };
        let column = column.and_then(|column| usize::try_from(column).ok());
        guards_at(&site.key, text, site.language, line, column)
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
