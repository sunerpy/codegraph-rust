//! Erlang calls written without a module across modules (upstream #2178 (b),
//! in the shape at `b635dd46`): another module's function is called bare
//! only through `-import(Mod, [f/N])`, or from a `.hrl` a module includes;
//! with neither, nothing the project declares. cowboy's
//! `-import(req_SUITE, [do_get/3])` went to compress_SUITE's `do_get/3`.

use std::sync::OnceLock;

use codegraph_core::generated_header::is_js_whitespace;
use regex::Regex;

/// What one `-import(Mod, [f/N, …]).` attribute brings in: the module and
/// each `(name, arity)` its list names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ErlangImport {
    pub(crate) module: String,
    pub(crate) functions: Vec<(String, String)>,
}

/// `/^-import\(\s*'?([A-Za-z_][\w@]*)'?\s*,\s*\[([^\]]*)\]\s*\)\s*\./gm`.
fn import_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?m)^-import\(\s*'?([A-Za-z_][A-Za-z0-9_@]*)'?\s*,\s*\[([^\]]*)\]\s*\)\s*\.")
            .expect("Erlang import pattern")
    })
}

/// The `-import` attributes of an Erlang source, in order (upstream reads
/// them only when the text has `-import`).
pub(crate) fn erlang_imports_of(source: &str) -> Vec<ErlangImport> {
    if !source.contains("-import") {
        return Vec::new();
    }
    import_pattern()
        .captures_iter(source)
        .map(|captures| ErlangImport {
            module: captures[1].to_string(),
            functions: listed_functions(&captures[2]),
        })
        .collect()
}

/// The `name/arity` entries of an import list, each read as Erlang writes
/// it: a quoted atom as one name (`'foo,bar'`, which may hold a comma or a
/// slash; `\'` escapes a quote), its quotes dropped as the extractor drops
/// them, else the bare atom; then `/` and the arity's digits. An entry that
/// reads otherwise is passed over to the next comma outside quotes, and a
/// `%` comment outside quotes is read as the blanks it stands for.
/// KEEP-RUST: upstream tests each called function against the list with
/// `(?:^|[\s,])'?name'?\s*/\s*arity\b`, which also finds `foo/2` inside
/// `'x,foo'/2`.
fn listed_functions(list: &str) -> Vec<(String, String)> {
    let list = blank_comments(list);
    let list = list.as_str();
    let bytes = list.as_bytes();
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b',' || list[at..].starts_with(is_js_whitespace) {
            at += list[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let (entry, end) = read_entry(list, at);
        entries.extend(entry);
        at = next_comma(list, end);
    }
    entries
}

/// `list` with each `%` comment outside a quoted atom blanked to the end
/// of its line.
fn blank_comments(list: &str) -> String {
    let mut out = String::with_capacity(list.len());
    let (mut quoted, mut escaped, mut comment) = (false, false, false);
    for c in list.chars() {
        if comment {
            if c == '\n' {
                comment = false;
                out.push(c);
            } else {
                out.push(' ');
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                quoted = false;
            }
        } else if c == '\'' {
            quoted = true;
        } else if c == '%' {
            comment = true;
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// One `name/arity` entry starting at `at`, and where reading it stopped.
fn read_entry(list: &str, at: usize) -> (Option<(String, String)>, usize) {
    let bytes = list.as_bytes();
    let (name, mut at) = if bytes[at] == b'\'' {
        let mut end = at + 1;
        while end < bytes.len() && bytes[end] != b'\'' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        if end >= bytes.len() {
            return (None, bytes.len());
        }
        (&list[at + 1..end], end + 1)
    } else {
        let len = list[at..]
            .find(|c: char| c == ',' || c == '/' || is_js_whitespace(c))
            .unwrap_or(list.len() - at);
        (&list[at..at + len], at + len)
    };
    at += list[at..].len() - list[at..].trim_start_matches(is_js_whitespace).len();
    if !list[at..].starts_with('/') {
        return (None, at);
    }
    at += 1;
    at += list[at..].len() - list[at..].trim_start_matches(is_js_whitespace).len();
    let digits = list[at..].len()
        - list[at..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let bounded = !list[at + digits..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
    let entry = (!name.is_empty() && digits > 0 && bounded)
        .then(|| (name.to_string(), list[at..at + digits].to_string()));
    (entry, at + digits)
}

/// The first comma at or after `at` outside a quoted atom, or the end.
fn next_comma(list: &str, mut at: usize) -> usize {
    let bytes = list.as_bytes();
    let mut quoted = false;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if quoted => at += 1,
            b'\'' => quoted = !quoted,
            b',' if !quoted => return at,
            _ => {}
        }
        at += 1;
    }
    bytes.len()
}

/// The module an Erlang file's `-import` attributes bring `name/arity` in
/// from, the first that does (upstream `erlangImportedModule`).
pub(crate) fn imported_module<'a>(
    imports: &'a [ErlangImport],
    name: &str,
    arity: &str,
) -> Option<&'a str> {
    imports
        .iter()
        .find(|import| {
            import
                .functions
                .iter()
                .any(|(listed, listed_arity)| listed == name && listed_arity == arity)
        })
        .map(|import| import.module.as_str())
}

/// Whether `path` is an Erlang header (`/\.hrl$/`).
pub(crate) fn is_erlang_header(path: &str) -> bool {
    path.ends_with(".hrl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_import_attribute_is_read_as_written() {
        let source = "-module(m).\n-import(req_SUITE, [do_get/3]).\n-import('ct_helper',\n        [config/2,\n         'doc' / 1]).\n  -import(indented, [x/1]).\n-import(broken, [y/1])\n";
        let imports = erlang_imports_of(source);
        assert_eq!(
            imports,
            [
                ErlangImport {
                    module: "req_SUITE".to_string(),
                    functions: vec![("do_get".to_string(), "3".to_string())],
                },
                ErlangImport {
                    module: "ct_helper".to_string(),
                    functions: vec![
                        ("config".to_string(), "2".to_string()),
                        ("doc".to_string(), "1".to_string()),
                    ],
                },
            ]
        );
        assert_eq!(imported_module(&imports, "doc", "1"), Some("ct_helper"));
        assert_eq!(imported_module(&imports, "do_get", "2"), None);
        assert!(erlang_imports_of("-module(m).\n").is_empty());
    }

    #[test]
    fn a_quoted_atom_is_one_name_whatever_it_holds() {
        let pairs = |list: &str| -> Vec<(String, String)> { listed_functions(list) };
        let expect = |entries: &[(&str, &str)]| -> Vec<(String, String)> {
            entries
                .iter()
                .map(|(name, arity)| ((*name).to_string(), (*arity).to_string()))
                .collect()
        };
        assert_eq!(
            pairs("'foo,bar'/1, 'a/b'/2, plain/0, 'it\\'s'/3"),
            expect(&[
                ("foo,bar", "1"),
                ("a/b", "2"),
                ("plain", "0"),
                ("it\\'s", "3")
            ])
        );
        // No `foo/2` inside the quoted atom, a malformed entry passed over.
        assert_eq!(
            pairs("'x,foo'/2, junk, ok/12x, f/1"),
            expect(&[("x,foo", "2"), ("f", "1")])
        );
        // A comment between entries, one holding a comma, and a `%` inside a
        // quoted atom, which is no comment.
        assert_eq!(
            pairs("foo/1, % note, not/9\n bar/2, '100%'/0 % trailing\n"),
            expect(&[("foo", "1"), ("bar", "2"), ("100%", "0")])
        );
    }
}
