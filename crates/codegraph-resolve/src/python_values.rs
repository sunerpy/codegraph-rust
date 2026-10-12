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

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
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

/// The shape of a Python call that reaches the resolver as a bare name, read
/// at the call's column (upstream #2125 `pythonCallShape`): `Bare` is
/// `get(1)`, which cannot mean a method (Python has no implicit self);
/// `Chained` is a call through a receiver the extractor did not keep, which
/// can only mean a member of what the chain names last. `self.x()` /
/// `cls.x()`, and a chain split across lines, have no shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PythonCallShape {
    Bare(BareCall),
    Chained { owner: String },
}

/// What a bare call's file says of the name, read on the first candidate of
/// another file and kept for the rest: it does not depend on the candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BareCall {
    imported_from_outside: OnceCell<bool>,
    locally_bound: OnceCell<bool>,
}

pub(crate) fn python_call_shape(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<PythonCallShape> {
    if reference.language != Language::Python || reference.reference_kind != EdgeKind::Calls {
        return None;
    }
    let name = reference.reference_name.as_str();
    let plain = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !plain {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let line = facts.raw_line(usize::try_from(reference.line).ok()?.checked_sub(1)?)?;
    let mut column = usize::try_from(reference.column)
        .unwrap_or(0)
        .min(line.len());
    while !line.is_char_boundary(column) {
        column -= 1;
    }
    shape_of_call(&line[column..], name)
}

/// Whether a Python call that reaches the resolver as a bare name was
/// written on the instance, `self.get_ip(request)` (upstream #2290
/// `isPythonSelfCall`): the extractor drops the `self.`, so a name the file
/// imports would otherwise claim the method call.
pub(crate) fn is_python_self_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    use crate::name_matcher::{BareCallReceiver, bare_call_receiver};
    if reference.language != Language::Python
        || !matches!(
            bare_call_receiver(reference, context),
            Some(BareCallReceiver::OwnObject)
        )
    {
        return false;
    }
    // `OwnObject` is `super()` and `this` too: the receiver is `self` alone.
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    usize::try_from(reference.column)
        .ok()
        .and_then(|column| line.get(column..))
        .and_then(|text| text.strip_prefix("self"))
        .is_some_and(|rest| rest.trim_start().starts_with('.'))
}

/// The shape of the call of `name` that `text` (its line from the call's
/// column on) starts with.
fn shape_of_call(text: &str, name: &str) -> Option<PythonCallShape> {
    static OWNER: OnceLock<Regex> = OnceLock::new();
    // `name(`, whitespace allowed before the parenthesis.
    let calls_name = |rest: &str| {
        rest.strip_prefix(name)
            .is_some_and(|after| after.trim_start().starts_with('('))
    };
    if calls_name(text) {
        return Some(PythonCallShape::Bare(BareCall::default()));
    }
    // `self.name(` / `cls.name(`, read by hand: a pattern built per call
    // would be compiled once for every Python call of the project.
    let after_dot = |rest: &str| {
        rest.trim_start()
            .strip_prefix('.')
            .is_some_and(|member| calls_name(member.trim_start()))
    };
    if ["self", "cls"]
        .iter()
        .any(|own| text.strip_prefix(own).is_some_and(after_dot))
    {
        return None;
    }
    // The call starts at its receiver, so everything up to the first
    // `.name(` is the receiver chain.
    let receiver = text
        .match_indices('.')
        .find(|(at, _)| calls_name(text[at + 1..].trim_start()))
        .map(|(at, _)| &text[..at])?;
    let owner = OWNER
        .get_or_init(|| {
            Regex::new(r"([0-9A-Za-z_]+)\s*(?:\([^()]*\)|\[[^\[\]]*\])?\s*$").expect("chain owner")
        })
        .captures(receiver)?
        .get(1)?
        .as_str()
        .to_string();
    Some(PythonCallShape::Chained { owner })
}

/// Whether a Python call of `shape` can mean `node` (upstream
/// `fitsPythonCallShape`): a bare call never a method, nor another file's
/// symbol when the file takes the name `from` a module outside the project
/// (`from django.shortcuts import render`) or binds it itself (`view =
/// SomeView.as_view()`, upstream #2198) unless it is a fixture in reach; a
/// chained call only a method of a class the chain's last link names
/// (`self.user_service` → `UserService`) or a function or class of a module
/// of that name (`app.helpers.slugify()`).
pub(crate) fn fits_python_call_shape(
    node: &Node,
    shape: &PythonCallShape,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    match shape {
        PythonCallShape::Bare(call) => {
            let name = reference.reference_name.as_str();
            if node.kind == NodeKind::Method {
                return false;
            }
            if node.file_path == reference.file_path {
                return true;
            }
            if *call
                .imported_from_outside
                .get_or_init(|| is_python_name_imported_from_outside(name, reference, context))
            {
                return false;
            }
            is_fixture_in_reach(node, &reference.file_path, context)
                || !*call
                    .locally_bound
                    .get_or_init(|| is_python_locally_bound(name, reference, context))
        }
        PythonCallShape::Chained { owner } => {
            let plain = |text: &str| text.replace('_', "").to_lowercase();
            if node.kind == NodeKind::Method {
                let class = node
                    .qualified_name
                    .rsplit_once("::")
                    .map(|(parent, _)| parent.rsplit("::").next().unwrap_or(parent))
                    .unwrap_or_default();
                return !class.is_empty() && plain(class) == plain(owner);
            }
            let mut parts = node.file_path.rsplit('/');
            let file = parts.next().unwrap_or_default();
            let stem = file
                .strip_suffix(".pyi")
                .or_else(|| file.strip_suffix(".py"))
                .unwrap_or(file);
            stem == owner || (stem == "__init__" && parts.next() == Some(owner.as_str()))
        }
    }
}

/// Whether `file_path` takes `name` `from` a module the project does not
/// contain (upstream `isPythonNameImportedFromOutside`): `from
/// django.shortcuts import render`. A relative import is the project's.
fn is_python_name_imported_from_outside(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(module) = facts
        .python_from_imports(scan_from_imports)
        .get(name)
        .cloned()
    else {
        return false;
    };
    if module.starts_with('.') {
        return false;
    }
    !facts.node_decision("python_module_local", &module, |_| {
        let rel = module.replace('.', "/");
        let (file, package) = (format!("{rel}.py"), format!("{rel}/__init__.py"));
        context.get_all_files_shared().iter().any(|f| {
            *f == file
                || f.ends_with(&format!("/{file}"))
                || *f == package
                || f.ends_with(&format!("/{package}"))
        })
    })
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
/// defines, one a `conftest.py` of its directory or a parent does, or one a
/// module such a conftest pulls in defines (`from tests.fixtures.cli import
/// *`, `pytest_plugins = ["tests.fixtures.cli"]`, upstream #2228). A test
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
    if let Some(dir) = node.file_path.strip_suffix("/conftest.py") {
        return file_path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'));
    }
    is_pytest_fixture(node, context)
        && plugged_fixture_modules(file_path, context)
            .iter()
            .any(|module| {
                node.file_path == *module || node.file_path.ends_with(&format!("/{module}"))
            })
}

/// The module files (`tests/fixtures/cli.py`) the `conftest.py` files above
/// `file_path` star-import or list in `pytest_plugins` (upstream
/// `pluggedFixtureModules`).
fn plugged_fixture_modules(file_path: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let scan = || {
        let mut modules = Vec::new();
        let mut dir = file_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        loop {
            let conftest = if dir.is_empty() {
                "conftest.py".to_string()
            } else {
                format!("{dir}/conftest.py")
            };
            if let Some(text) = context.read_file_shared(&conftest) {
                modules.extend(plugged_modules_in(&text));
            }
            if dir.is_empty() {
                break;
            }
            dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        modules
    };
    match context.source_facts(file_path) {
        Some(facts) => facts.python_plugged_modules(scan).to_vec(),
        None => scan(),
    }
}

/// The module files a `conftest.py`'s text pulls fixtures in from: what it
/// star-imports, and what its `pytest_plugins` lists.
pub(crate) fn plugged_modules_in(text: &str) -> Vec<String> {
    static STAR: OnceLock<Regex> = OnceLock::new();
    static PLUGINS: OnceLock<Regex> = OnceLock::new();
    let star = STAR.get_or_init(|| {
        Regex::new(r"(?m)^\s*from\s+([0-9A-Za-z_.]+)\s+import\s+\*").expect("star import")
    });
    let plugins = PLUGINS.get_or_init(|| {
        Regex::new(r"(?m)^\s*pytest_plugins\s*=\s*[\[(]([^\])]*)[\])]").expect("pytest_plugins")
    });
    let mut modules: Vec<String> = star
        .captures_iter(text)
        .map(|found| format!("{}.py", found[1].replace('.', "/")))
        .collect();
    if let Some(list) = plugins.captures(text).and_then(|found| found.get(1)) {
        modules.extend(quoted_module_files(list.as_str()));
    }
    modules
}

/// The module files the quoted dotted names in `text` spell
/// (`"tests.fixtures.cli"` is `tests/fixtures/cli.py`).
pub(crate) fn quoted_module_files(text: &str) -> Vec<String> {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    QUOTED
        .get_or_init(|| Regex::new(r#"["']([0-9A-Za-z_.]+)["']"#).expect("quoted module"))
        .captures_iter(text)
        .map(|found| format!("{}.py", found[1].replace('.', "/")))
        .collect()
}

/// The class a pytest fixture returns, for a test's parameter of its name
/// (upstream #2228 `pythonFixtureReturnType`): the fixture in reach (the
/// test's module first, then the nearest `conftest.py` above it) whose body
/// returns or yields `Cls(…)`, directly or through a local assigned
/// `Cls(…)`. `None` for anything else: a receiver that is no parameter, a
/// fixture returning a call of a function.
pub(crate) fn python_fixture_return_type(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    static SIGNATURE_END: OnceLock<Regex> = OnceLock::new();
    static RETURNED: OnceLock<Regex> = OnceLock::new();
    let lower_word = receiver
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && receiver
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !lower_word || receiver == "self" || receiver == "cls" {
        return None;
    }
    let compute = || {
        let caller = context.get_node_by_id_shared(&reference.from_node_id)?;
        if !matches!(caller.kind, NodeKind::Function | NodeKind::Method) {
            return None;
        }
        let caller_facts = context.source_facts(&caller.file_path)?;
        // The receiver must be the test's own parameter.
        let start = usize::try_from(caller.start_line).ok()?.checked_sub(1)?;
        let header = (start..start + 5)
            .filter_map(|index| caller_facts.raw_line(index))
            .map(|line| line.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join(" ");
        let signature_end =
            SIGNATURE_END.get_or_init(|| Regex::new(r"\)\s*(?:->[^:]*)?:").expect("signature end"));
        let signature = signature_end
            .find(&header)
            .map_or(header.as_str(), |end| &header[..end.start()]);
        let param = Regex::new(&format!(
            r"[(,]\s*{}\s*(?:[:=,)]|$)",
            regex::escape(receiver)
        ))
        .ok()?;
        if !param.is_match(signature) {
            return None;
        }
        let mut fixtures: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(receiver)
            .into_iter()
            .filter(|node| {
                node.kind == NodeKind::Function
                    && node.language == Language::Python
                    && is_fixture_in_reach(node, &reference.file_path, context)
            })
            .collect();
        // The test's own module first, then the deepest conftest.
        fixtures.sort_by(|a, b| {
            (b.file_path == reference.file_path)
                .cmp(&(a.file_path == reference.file_path))
                .then_with(|| b.file_path.len().cmp(&a.file_path.len()))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        let fixture = fixtures.first()?;
        let facts = context.source_facts(&fixture.file_path)?;
        let from = usize::try_from(fixture.start_line).ok()?;
        let to = usize::try_from(fixture.end_line).ok()?;
        let body = (from..to)
            .filter_map(|index| facts.raw_line(index))
            .map(|line| line.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join("\n");
        let returned = RETURNED.get_or_init(|| {
            Regex::new(r"(?m)^\s*(?:return|yield)\s+([A-Za-z_][0-9A-Za-z_.]*)\s*(\()?")
                .expect("fixture return")
        });
        let found = returned.captures(&body)?;
        let value = found.get(1)?.as_str();
        let direct = if found.get(2).is_some() {
            Some(value.to_string())
        } else {
            Regex::new(&format!(
                r"(?m)^\s*{}\s*=\s*([A-Za-z_][0-9A-Za-z_.]*)\s*\(",
                regex::escape(value)
            ))
            .ok()?
            .captures(&body)
            .and_then(|assigned| assigned.get(1))
            .map(|callee| callee.as_str().to_string())
        }?;
        let class = direct.rsplit('.').next()?;
        class
            .starts_with(|c: char| c.is_ascii_uppercase())
            .then(|| class.to_string())
    };
    let key = format!("{}\0{receiver}", reference.from_node_id);
    match context.source_facts(&reference.file_path) {
        Some(facts) => facts.python_fixture_type(&key, compute),
        None => compute(),
    }
}

#[cfg(test)]
mod tests {
    use super::{BareCall, PythonCallShape, scan_from_imports, shape_of_call};

    #[test]
    fn a_call_shape_is_read_from_its_column() {
        let bare = Some(PythonCallShape::Bare(BareCall::default()));
        let chained = |owner: &str| {
            Some(PythonCallShape::Chained {
                owner: owner.to_string(),
            })
        };
        assert_eq!(shape_of_call("all(1)", "all"), bare);
        assert_eq!(shape_of_call("render (request, 'x.html')", "render"), bare);
        assert_eq!(
            shape_of_call("User.objects.get(pk=1)", "get"),
            chained("objects")
        );
        assert_eq!(
            shape_of_call("self.client.login(user)", "login"),
            chained("client")
        );
        assert_eq!(
            shape_of_call("request.POST.get('next')", "get"),
            chained("POST")
        );
        assert_eq!(shape_of_call("make().get(1)", "get"), chained("make"));
        assert_eq!(shape_of_call("rows[0] . get (1)", "get"), chained("rows"));
        // The first `.get(` ends the chain: `.getx(` is not it.
        assert_eq!(shape_of_call("a.getx(1).get(2)", "get"), chained("getx"));
        // A class's own call, and a chain the line does not hold, have none.
        assert_eq!(shape_of_call("self.get(1)", "get"), None);
        assert_eq!(shape_of_call("cls . get(1)", "get"), None);
        assert_eq!(shape_of_call("get", "get"), None);
        assert_eq!(shape_of_call("selfish.get(1)", "get"), chained("selfish"));
    }

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
