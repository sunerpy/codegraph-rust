//! What a Python name passed as a value is where it is written (upstream
//! #2219): a name the enclosing function binds (a parameter, an assignment, a
//! `for` / `with … as` / `except … as` target) is that local's value, a name a
//! `from … import` line binds is the import's, and a pytest fixture is what a
//! test's parameter of its name receives only in its own module or under its
//! `conftest.py`.
//!
//! The patterns are upstream's, with `\w` and `\b` kept ASCII as in
//! JavaScript, and `=(?!=)` written `=(?:[^=]|$)` (the same match test
//! without a lookahead).

use std::collections::HashMap;
use std::sync::OnceLock;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use crate::types::{RefView, ResolutionContext};

/// `name` → the module a `from <module> import name` line takes it from
/// (upstream `pythonFromImports`).
fn scan_from_imports(text: &str) -> HashMap<String, String> {
    static FROM: OnceLock<Regex> = OnceLock::new();
    static BOUND: OnceLock<Regex> = OnceLock::new();
    let from = FROM.get_or_init(|| {
        Regex::new(r"(?m)^\s*from\s+([0-9A-Za-z_.]+)\s+import\s+(\([^)]*\)|[^\n#]+)")
            .expect("python from import")
    });
    let bound =
        BOUND.get_or_init(|| Regex::new(r"([0-9A-Za-z_]+)\s*$").expect("python import binding"));
    let mut names = HashMap::new();
    for m in from.captures_iter(text) {
        for item in m[2].replace(['(', ')'], "").split(',') {
            if let Some(c) = bound.captures(item.trim()) {
                names.insert(c[1].to_string(), m[1].to_string());
            }
        }
    }
    names
}

/// Whether a `from … import` line of `file_path` binds `name`.
pub(crate) fn python_imports_name(
    file_path: &str,
    name: &str,
    context: &dyn ResolutionContext,
) -> bool {
    context.source_facts(file_path).is_some_and(|facts| {
        facts
            .python_from_imports(scan_from_imports)
            .contains_key(name)
    })
}

/// Whether the function around a Python reference, or its module, binds
/// `name` itself: an assignment (`view = X.as_view()`, `a, view = …`,
/// `view: T = …`), a parameter, a `for` / `with … as` / `except … as` target
/// (upstream `isPythonLocallyBound`). Answered per reference line: upstream
/// keeps the first answer for every later reference in the function, which
/// would make the answer depend on resolution order.
pub(crate) fn is_python_locally_bound(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    // An imported name is the import's, for the import resolver to follow.
    if python_imports_name(&reference.file_path, name, context) {
        return false;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let function = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|f| {
            matches!(f.kind, NodeKind::Function | NodeKind::Method)
                && f.start_line <= reference.line
                && f.end_line >= reference.line
        })
        .min_by(|a, b| {
            (a.end_line - a.start_line)
                .cmp(&(b.end_line - b.start_line))
                .then_with(|| a.id.cmp(&b.id))
        });
    let n = regex::escape(name);
    let function_key = function
        .as_ref()
        .map(|f| format!("{}\0{name}\0{}", f.id, reference.line));
    let locally = function_key.is_some_and(|key| {
        facts.node_decision("python_local_binding", &key, |facts| {
            // Code only: a docstring's `as user_display` binds nothing.
            let (code, lines) = facts.python_comment_free();
            let line = |index: usize| (index < lines.line_count()).then(|| lines.line(code, index));
            let (Ok(assigns), Ok(targets), Ok(params)) = (
                Regex::new(&format!(
                    r"^\s*(?:[0-9A-Za-z_\s,*()\[\]]*,\s*)?\(?\*?{n}\)?\s*(?:,[0-9A-Za-z_\s,*()\[\]]*)?(?::[^=]+)?=(?:[^=]|$)"
                )),
                Regex::new(&format!(
                    r"(?-u:\b)for\s+[0-9A-Za-z_\s,()]*(?-u:\b){n}(?-u:\b)[0-9A-Za-z_\s,()]*\s+in(?-u:\b)|(?-u:\b)as\s+{n}(?-u:\b)"
                )),
                Regex::new(&format!(r"[(,]\s*\*{{0,2}}{n}\s*(?:[:=,)]|$)")),
            ) else {
                return false;
            };
            static SIGNATURE_END: OnceLock<Regex> = OnceLock::new();
            let signature_end = SIGNATURE_END
                .get_or_init(|| Regex::new(r"\)\s*(?:->[^:]*)?:\s*(?:#.*)?$").expect("signature end"));
            let Some(function) = function.as_ref() else {
                return false;
            };
            // The signature up to its `:` (it may span lines), then the body
            // above the reference.
            let start = usize::try_from(function.start_line).unwrap_or(1).saturating_sub(1);
            let limit = lines.line_count().min(start + 21);
            let mut at = start;
            let mut signature = String::new();
            while at < limit {
                let text = line(at).unwrap_or_default();
                signature.push_str(text);
                if signature_end.is_match(text) {
                    break;
                }
                at += 1;
            }
            if params.is_match(signature.find('(').map_or("", |open| &signature[open..])) {
                return true;
            }
            let before_reference = usize::try_from(reference.line).unwrap_or(0).saturating_sub(1);
            (at + 1..before_reference).any(|index| {
                line(index).is_some_and(|text| assigns.is_match(text) || targets.is_match(text))
            })
        })
    });
    // A module-level binding (`view = api_view(['GET'])(handler)`).
    locally
        || facts.node_decision("python_module_binding", name, |facts| {
            let (code, lines) = facts.python_comment_free();
            Regex::new(&format!(
                r"^(?:[0-9A-Za-z_,\s]*,\s*)?{n}\s*(?:,[0-9A-Za-z_\s,]*)?(?::[^=]+)?=(?:[^=]|$)"
            ))
            .is_ok_and(|top| {
                (0..lines.line_count()).any(|index| top.is_match(lines.line(code, index)))
            })
        })
}

/// A pytest fixture: `@pytest.fixture` / `@fixture` above its `def`, or
/// anything a `conftest.py` defines (upstream `isPytestFixture`).
fn is_pytest_fixture(node: &Node, context: &dyn ResolutionContext) -> bool {
    static DECORATOR: OnceLock<Regex> = OnceLock::new();
    let decorator = DECORATOR
        .get_or_init(|| Regex::new(r"(?:^|\.)fixture(?-u:\b)").expect("fixture decorator name"));
    is_conftest(&node.file_path)
        || node.decorators.iter().any(|d| decorator.is_match(d))
        || is_decorated_fixture(node, context)
}

fn is_conftest(file_path: &str) -> bool {
    file_path == "conftest.py" || file_path.ends_with("/conftest.py")
}

/// Read upward through the decorator lines above a `def`, each starting with
/// `@` or sitting inside one's parentheses, for a `fixture` decorator
/// (upstream `isDecoratedFixture`).
fn is_decorated_fixture(node: &Node, context: &dyn ResolutionContext) -> bool {
    static FIXTURE: OnceLock<Regex> = OnceLock::new();
    let fixture = FIXTURE.get_or_init(|| {
        Regex::new(r"^@(?:[0-9A-Za-z_]+\.)*fixture(?-u:\b)").expect("fixture decorator")
    });
    if node.language != Language::Python || node.kind != NodeKind::Function {
        return false;
    }
    let Some(facts) = context.source_facts(&node.file_path) else {
        return false;
    };
    let mut open: i64 = 0;
    let mut index = node.start_line - 2;
    while index >= 0 && index >= node.start_line - 16 {
        let text = usize::try_from(index)
            .ok()
            .filter(|&i| i < facts.line_count())
            .map_or("", |i| facts.line(i).trim());
        open += text.matches(')').count() as i64 - text.matches('(').count() as i64;
        if open <= 0 {
            if !text.starts_with('@') {
                return false;
            }
            if fixture.is_match(text) {
                return true;
            }
        }
        index -= 1;
    }
    false
}

/// A fixture a test at `file_path` can take by name: one its own module
/// defines, or one a `conftest.py` of its directory or a parent does. A test
/// module's fixture is that module's alone (upstream `isFixtureInReach`).
pub(crate) fn is_fixture_in_reach(
    node: &Node,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> bool {
    if node.file_path == file_path {
        return is_pytest_fixture(node, context);
    }
    if node.file_path == "conftest.py" {
        return true;
    }
    node.file_path
        .strip_suffix("/conftest.py")
        .is_some_and(|dir| {
            file_path
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        })
}

#[cfg(test)]
mod tests {
    use super::scan_from_imports;

    #[test]
    fn from_imports_bind_their_last_word() {
        let names = scan_from_imports(
            "from a.b import c, d as e\nfrom .x import (\n    f,\n    g as h,\n)\nfrom m import *  # all\nimport z\n",
        );
        let mut found: Vec<(&str, &str)> = names
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        found.sort_unstable();
        assert_eq!(
            found,
            [("c", "a.b"), ("e", "a.b"), ("f", ".x"), ("h", ".x")]
        );
    }
}
