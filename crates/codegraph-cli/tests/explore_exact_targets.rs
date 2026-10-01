//! EXACT targets in `codegraph explore` (upstream v1.6.1 #2063): a qualified
//! name (`SQLCompiler.as_sql`) or a line anchor (`compiler.py:776`,
//! `compiler.py lines 900-1003`).
//!
//! Upstream's django gap: `SQLCompiler.as_sql pre_sql_setup get_select`
//! returned the two unqualified methods and not `as_sql` — qualified, the one
//! the agent singled out — and the follow-ups that named its lines pinned the
//! file but dropped the numbers, so every run ended in a Read. The fixture is
//! upstream's in miniature (Python, as django), sized to this port's per-file
//! budget: `get_select` and `get_qualify_sql` above a larger `as_sql`, three
//! subclasses overriding `as_sql`, and a second file whose base method is too
//! big for any budget.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-exact-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run_in(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    assert!(
        output.status.success(),
        "{args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `n` filler statements, each a distinct line so nothing dedups or folds.
fn filler(tag: &str, n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "        {tag}_{i} = self.query.alias_refcount.get(\"{tag}_{i}\", 0) + len(self.query.select)"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Unrelated methods, so compiler.py is too big to ship whole: a whole file
/// would answer every question by accident and hide the gap.
fn helpers(n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "\n    def helper_{i}(self, value):\n        \"\"\"Unrelated helper {i}.\"\"\"\n        first = self.query.alias_map.get(value)\n        second = self.query.alias_refcount.get(value, 0)\n        third = self.query.external_aliases.get(value, False)\n        return first, second, third"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compiler_source() -> String {
    format!(
        r#"class SQLCompiler:
    def pre_sql_setup(self, with_col_aliases=False):
        # PRE_SQL_SETUP_BODY
        self.setup_query(with_col_aliases=with_col_aliases)
        order_by = self.get_order_by()
        return order_by

    def setup_query(self, with_col_aliases=False):
        self.select = self.get_select(with_col_aliases=with_col_aliases)
        return self.select

    def get_order_by(self):
        return []

    def get_select(self, with_col_aliases=False):
        # GET_SELECT_BODY
{sel}
        return []

    def get_qualify_sql(self):
        # GET_QUALIFY_BODY
{qual}
        inner = self.get_select()
        return inner

    def as_sql(self, with_limits=True, with_col_aliases=False):
        # AS_SQL_HEAD
        order_by = self.pre_sql_setup(with_col_aliases=with_col_aliases)
{head}
        result = self.get_qualify_sql()
{tail}
        return result  # AS_SQL_TAIL_MARKER

    def execute_sql(self):
        sql = self.as_sql()
        return sql


{helpers}

def render_sql(compiler):
    return SQLCompiler.as_sql(compiler)


class SQLInsertCompiler(SQLCompiler):
    def as_sql(self):
        # INSERT_AS_SQL_BODY
        return super().as_sql()

    def execute_sql(self):
        sql = self.as_sql()
        return sql


class SQLUpdateCompiler(SQLCompiler):
    def as_sql(self):
        # UPDATE_AS_SQL_BODY
        return super().as_sql()

    def pre_sql_setup(self):
        # UPDATE_PRE_SQL_SETUP_BODY
        return super().pre_sql_setup()


class SQLDeleteCompiler(SQLCompiler):
    def as_sql(self):
        # DELETE_AS_SQL_BODY
        return super().as_sql()
"#,
        sel = filler("sel", 20),
        qual = filler("qual", 20),
        head = filler("head", 20),
        tail = filler("tail", 21),
        helpers = helpers(40),
    )
}

/// A base method too big for any budget, whose call into `stage_query` sits
/// 300 lines into the body: far from the head and from every other definition,
/// so only a window on that call reaches it.
fn planner_source() -> String {
    format!(
        r#"class Planner:
    def plan(self, query):
        # PLAN_HEAD
{pre}
        staged = self.stage_query(query)  # PLAN_CALLS_STAGE
{post}
        return staged

    def stage_query(self, query):
        return self.finalize(query)

    def finalize(self, query):
        # FINALIZE_BODY
        return query

    def explain_plan(self, query):
        # EXPLAIN_PLAN_BODY
        return str(query)


class HashPlanner(Planner):
    def plan(self, query):
        return super().plan(query)


class MergePlanner(Planner):
    def plan(self, query):
        return super().plan(query)
"#,
        pre = filler("plan", 300),
        post = filler("post", 120),
    )
}

const LOOKUPS: &str = r#"class Exact:
    def as_sql(self, compiler, connection):
        return "%s = %s"


class IExact:
    def as_sql(self, compiler, connection):
        return "UPPER(%s) = UPPER(%s)"
"#;

struct Fixture {
    _dir: TestDir,
    project: PathBuf,
    compiler: String,
    planner: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TestDir::new(label);
        let project = dir.path.join("orm");
        std::fs::create_dir_all(&project).unwrap();
        let compiler = compiler_source();
        let planner = planner_source();
        std::fs::write(project.join("compiler.py"), &compiler).unwrap();
        std::fs::write(project.join("planner.py"), &planner).unwrap();
        std::fs::write(project.join("lookups.py"), LOOKUPS).unwrap();
        run_in(&dir.path, &["init", project.to_str().unwrap()]);
        Self {
            _dir: dir,
            project,
            compiler,
            planner,
        }
    }

    fn explore(&self, query: &str) -> String {
        run_in(&self.project, &["explore", query])
    }
}

/// 1-based line of the first line containing `needle`.
fn line_of(source: &str, needle: &str) -> usize {
    source
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not in fixture"))
        + 1
}

/// The `#### <file>` section, header through the line before the next one.
fn section_for<'a>(text: &'a str, file: &str) -> &'a str {
    let header = format!("#### {file} ");
    let Some(start) = text.find(&header) else {
        return "";
    };
    let rest = &text[start + header.len()..];
    let end = rest
        .find("\n#### ")
        .map_or(text.len(), |i| start + header.len() + i);
    &text[start..end]
}

/// Line numbers rendered in `file`'s section.
fn rendered_lines(text: &str, file: &str) -> BTreeSet<usize> {
    section_for(text, file)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(number, _)| number.parse().ok())
        .collect()
}

fn assert_rendered(text: &str, file: &str, lines: std::ops::RangeInclusive<usize>) {
    let rendered = rendered_lines(text, file);
    let missing: Vec<usize> = lines.filter(|line| !rendered.contains(line)).collect();
    assert!(
        missing.is_empty(),
        "{file} lines {missing:?} not rendered in:\n{text}"
    );
}

#[test]
fn a_qualified_name_returns_the_methods_whole_body() {
    let fx = Fixture::new("qualified");
    let def = line_of(&fx.compiler, "def as_sql(self, with_limits");
    let tail = line_of(&fx.compiler, "AS_SQL_TAIL_MARKER");
    let text = fx.explore("SQLCompiler.as_sql pre_sql_setup get_select");
    assert_rendered(&text, "compiler.py", def..=tail);
    assert!(
        section_for(&text, "compiler.py").contains("PRE_SQL_SETUP_BODY"),
        "a named method that still fits keeps its body:\n{text}"
    );
}

#[test]
fn the_blast_radius_leads_with_the_qualified_method() {
    let fx = Fixture::new("blast");
    let def = line_of(&fx.compiler, "def as_sql(self, with_limits");
    let text = fx.explore("SQLCompiler.as_sql pre_sql_setup get_select");
    let first = text
        .lines()
        .skip_while(|line| !line.starts_with("### Blast radius"))
        .find(|line| line.starts_with("- `"))
        .unwrap_or_else(|| panic!("no blast radius entry in:\n{text}"));
    assert!(
        first.starts_with(&format!("- `as_sql` (compiler.py:{def})")),
        "{first}"
    );
}

#[test]
fn a_line_anchor_returns_the_method_enclosing_that_line() {
    let fx = Fixture::new("line");
    let def = line_of(&fx.compiler, "def as_sql(self, with_limits");
    let tail = line_of(&fx.compiler, "AS_SQL_TAIL_MARKER");
    // No symbol named: the line alone has to say which method.
    let text = fx.explore(&format!("compiler.py:{} full body", def + 20));
    assert_rendered(&text, "compiler.py", def..=tail);
}

#[test]
fn a_line_range_returns_exactly_that_span() {
    let fx = Fixture::new("range");
    // The tail of `as_sql`: the span a windowed render elides and the agent
    // then asks for by number.
    let start = line_of(&fx.compiler, "result = self.get_qualify_sql()") + 1;
    let end = line_of(&fx.compiler, "AS_SQL_TAIL_MARKER");
    let text = fx.explore(&format!("compiler.py lines {start}-{end} tail"));
    assert_rendered(&text, "compiler.py", start..=end);
}

#[test]
fn an_oversize_exact_body_keeps_its_head_and_its_call_into_a_named_symbol() {
    let fx = Fixture::new("oversize");
    let call = line_of(&fx.planner, "PLAN_CALLS_STAGE");
    let text = fx.explore("Planner.plan stage_query explain_plan");
    let section = section_for(&text, "planner.py");
    assert!(section.contains("PLAN_HEAD"), "{text}");
    let rendered = rendered_lines(&text, "planner.py");
    assert!(
        rendered.contains(&call),
        "the call into stage_query (line {call}) is not rendered:\n{text}"
    );
    // Windowed, not dumped: most of the 425-line body is elided.
    assert!(rendered.len() < 300, "{} lines rendered", rendered.len());
}
