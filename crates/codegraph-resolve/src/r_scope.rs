//! R calls and the bindings they can name (upstream #2144, #2177, #2239).
//!
//! R looks a call's name up among functions: a plain `range(x)` is base R's
//! or a project function, never an object's method (ggplot2's ggproto Coord
//! has a `range` method); a data binding that shadows a base function
//! (`c <- data_frame(b = 3)`) is never what `c(1, 2)` calls; and testthat runs
//! each test file in an environment of its own, so a variable one test file
//! assigns is invisible to the package and to the other tests, except in the
//! `helper-*.R` and `setup-*.R` files every test sources.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// Base R functions whose names data often shadows (`c <- data_frame(…)`,
/// `df <- …`, `t <- 1`; upstream `R_BASE_FUNCTIONS`).
pub const R_BASE_FUNCTIONS: [&str; 67] = [
    "c",
    "t",
    "q",
    "df",
    "dt",
    "data",
    "list",
    "length",
    "names",
    "max",
    "min",
    "sum",
    "mean",
    "range",
    "rev",
    "sort",
    "order",
    "rep",
    "seq",
    "cat",
    "print",
    "paste",
    "paste0",
    "format",
    "levels",
    "factor",
    "matrix",
    "vector",
    "table",
    "scale",
    "sample",
    "exp",
    "log",
    "abs",
    "all",
    "any",
    "which",
    "nchar",
    "summary",
    "file",
    "dir",
    "identity",
    "unique",
    "nrow",
    "ncol",
    "rownames",
    "colnames",
    "array",
    "character",
    "numeric",
    "integer",
    "logical",
    "mode",
    "class",
    "body",
    "args",
    "environment",
    "search",
    "diff",
    "round",
    "sign",
    "trunc",
    "var",
    "sd",
    "median",
    "quantile",
    "weights",
];

/// JavaScript's `\s`.
fn is_js_space(c: char) -> bool {
    codegraph_core::generated_header::is_js_whitespace(c)
}

/// An ASCII word character or `.` (`[\w.]`).
fn is_r_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// Whether `name` is written as an R name (`^[A-Za-z_.][\w.]*$`).
fn is_r_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '.')
        && name.chars().all(is_r_name_char)
}

/// Line `line` (1-based) of `file`, as stored.
fn source_line(file: &str, line: i64, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(file)?;
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts.raw_line(index).map(str::to_string)
}

/// Whether an R call is a plain function call (`range(x)`, `vars(a)`), not
/// one through an object (`obj$m(…)`, `self$m(…)`, `obj@m(…)`, `pkg::f(…)`):
/// the first `name(` on the call's line not inside a longer name, with no
/// `$`, `@` or `::` written before it (upstream `isBareRCall`, #2177).
pub(crate) fn is_bare_r_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let name = reference.reference_name.as_str();
    if reference.language != Language::R
        || reference.reference_kind != EdgeKind::Calls
        || !is_r_name(name)
    {
        return false;
    }
    let Some(line) = source_line(&reference.file_path, reference.line, context) else {
        return false;
    };
    let Some((at, _)) = line.match_indices(name).find(|(at, _)| {
        !line[..*at].chars().next_back().is_some_and(is_r_name_char)
            && line[at + name.len()..]
                .trim_start_matches(is_js_space)
                .starts_with('(')
    }) else {
        return false;
    };
    let before = line[..at].trim_end_matches(is_js_space);
    !(before.ends_with('$') || before.ends_with('@') || before.ends_with("::"))
}

/// Whether an R binding holds a function, read from its line: `<-`, `<<-` or
/// `=` before `function` or a `\(x)` lambda (upstream `isRFunctionValue`).
fn is_r_function_value(node: &Node, context: &dyn ResolutionContext) -> bool {
    let Some(line) = source_line(&node.file_path, node.start_line, context) else {
        return false;
    };
    let after_arrows = line.match_indices("<-").map(|(at, _)| at + 2);
    let after_equals = line.match_indices('=').map(|(at, _)| at + 1);
    after_arrows.chain(after_equals).any(|at| {
        let rest = line[at..].trim_start_matches(is_js_space);
        rest.strip_prefix("function").is_some_and(|after| {
            !after
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        }) || rest
            .strip_prefix('\\')
            .is_some_and(|after| after.trim_start_matches(is_js_space).starts_with('('))
    })
}

/// An R call of a base function's name never reaches a data binding of it
/// (upstream #2239, in `matchReference`): ggplot2's tests' `c <-
/// data_frame(b = 3)` is not what `c(1, 2)` calls. A binding whose line
/// holds a function stays.
pub fn veto_r_data_binding(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    if reference.language == Language::R
        && reference.reference_kind == EdgeKind::Calls
        && R_BASE_FUNCTIONS.contains(&reference.reference_name.as_str())
        && context
            .get_node_by_id_shared(&result.target_node_id)
            .is_some_and(|target| {
                matches!(target.kind, NodeKind::Variable | NodeKind::Constant)
                    && !is_r_function_value(&target, context)
            })
    {
        return None;
    }
    Some(result)
}

/// Whether an R file runs in a test environment of its own: under a `tests/`
/// or `test/` folder, but no `helper-*.R` or `setup-*.R` file, which every
/// test sources (upstream #2144).
pub(crate) fn is_r_test_environment(file_path: &str) -> bool {
    let in_tests = file_path.starts_with("tests/")
        || file_path.starts_with("test/")
        || file_path.contains("/tests/")
        || file_path.contains("/test/");
    let name = file_path.rsplit('/').next().unwrap_or(file_path);
    let shared = (name.starts_with("helper") || name.starts_with("setup"))
        && (name.ends_with(".R") || name.ends_with(".r"));
    in_tests && !shared
}

/// The names in [`R_BASE_FUNCTIONS`] that `nodes` bind as an R variable or
/// constant: whether such a binding is what a call of its name means is read
/// from its line, which no node records.
pub fn r_base_bindings(nodes: &[Node]) -> impl Iterator<Item = String> + '_ {
    nodes
        .iter()
        .filter(|node| {
            node.language == Language::R
                && matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
                && R_BASE_FUNCTIONS.contains(&node.name.as_str())
        })
        .map(|node| node.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_environment_is_a_tests_folder_but_not_its_helpers() {
        for (path, expected) in [
            ("tests/testthat/test-facet.R", true),
            ("pkg/tests/testthat/test-facet.R", true),
            ("test/test-a.r", true),
            ("tests/testthat/helper-s3.R", false),
            ("tests/testthat/setup-data.r", false),
            ("tests/testthat/helper-s3.Rmd", true),
            ("R/geom-point.R", false),
            ("contests/x.R", false),
        ] {
            assert_eq!(is_r_test_environment(path), expected, "{path}");
        }
    }

    #[test]
    fn an_r_name_is_a_word_or_dotted_name() {
        assert!(is_r_name("geom_point"));
        assert!(is_r_name(".onLoad"));
        assert!(is_r_name("is.na"));
        assert!(!is_r_name("obj$m"));
        assert!(!is_r_name("pkg::f"));
        assert!(!is_r_name("2x"));
    }
}
