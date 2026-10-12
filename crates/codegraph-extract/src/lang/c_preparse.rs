//! C pre-parse passes of upstream's deferral round 2 (#1353, `b9d0f57`):
//! kernel-style annotations, macro arguments and declaration macros that
//! tree-sitter-c reads as errors, rewritten so the declaration around them
//! parses. Every pass keeps every byte offset: it replaces bytes with ASCII
//! spaces (newlines and carriage returns kept), except
//! [`rewrite_c_prefixed_decl_macro_initializers`], which moves a type token
//! left of a name whose column it keeps. C only.

use std::sync::OnceLock;

use regex::Regex;

fn regex(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("C pre-parse regex"))
}

/// Blank `bytes[range]`, newlines and carriage returns kept.
fn blank(bytes: &mut [u8], range: std::ops::Range<usize>) {
    for byte in &mut bytes[range] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn into_text(bytes: Vec<u8>, source: &str) -> String {
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

fn is_word_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// Lowercase compiler annotations a storage class can carry before the rest
/// of a declaration (the kernel's `compiler.h` markers).
const C_SANDWICHED_ANNOTATIONS: &str =
    "noinline_for_stack|nokprobe_inline|noinline|notrace|noinstr";

/// Blank a lowercase compiler-annotation word sandwiched between a storage
/// class and the rest of a declaration (upstream `blankCSandwichedAnnotations`):
/// `static notrace void tick(…)`, `static nokprobe_inline void arm(…)`. The
/// word counts only right after `static` / `extern` / `inline` and before
/// another word, where it cannot be a variable (`int notrace = 1;` stays).
pub(crate) fn blank_c_sandwiched_annotations(source: &str) -> String {
    static SANDWICH: OnceLock<Regex> = OnceLock::new();
    static NEXT_WORD: OnceLock<Regex> = OnceLock::new();
    let sandwich = SANDWICH.get_or_init(|| {
        Regex::new(&format!(
            r"\b(?:static|extern|inline)[ \t]+({C_SANDWICHED_ANNOTATIONS})\b"
        ))
        .expect("sandwiched annotation regex")
    });
    let next_word = regex(r"^[ \t]+[A-Za-z_]", &NEXT_WORD);
    let mut bytes: Option<Vec<u8>> = None;
    for found in sandwich.captures_iter(source) {
        let whole = found.get(0).expect("match");
        if !next_word.is_match(&source[whole.end()..]) {
            continue;
        }
        let annotation = found.get(1).expect("annotation");
        blank(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            annotation.range(),
        );
    }
    bytes.map_or_else(|| source.to_string(), |bytes| into_text(bytes, source))
}

/// Blank a C23 `auto` type-inference keyword (upstream `blankCAutoInference`):
/// `auto hb = hbr.hb;` leaves a plain assignment. The storage-class reading
/// has a type between `auto` and the `=` (`auto int x = 1;`) and stays.
pub(crate) fn blank_c_auto_inference(source: &str) -> String {
    static AUTO: OnceLock<Regex> = OnceLock::new();
    let auto = regex(r"\bauto[ \t]+[A-Za-z_][0-9A-Za-z_]*[ \t]*=", &AUTO);
    if !source.contains("auto") || !auto.is_match(source) {
        return source.to_string();
    }
    let mut bytes = source.as_bytes().to_vec();
    for found in auto.find_iter(source) {
        blank(&mut bytes, found.start()..found.start() + "auto".len());
    }
    into_text(bytes, source)
}

/// Annotations whose argument list goes with them, longest first.
const C_PARAMETERIZED_ANNOTATIONS: &str = "__assume_aligned|__pt_guarded_by|__counted_by_le|__counted_by_be|__bpf_md_ptr|__counted_by|__guarded_by|__must_hold|__acquires|__releases|__section|__aligned|__cleanup|__printf|__scanf|__free";

/// Blank a parameterized sparse or compiler annotation whole, name and
/// argument list (upstream `blankCParameterizedAnnotationMacros`): `struct
/// file *f __free(fput) = NULL;`, `static void __printf(4, 0) log_it(…)`,
/// `} owners[] __counted_by(count);`. Blanking only the name would strand the
/// parenthesis. The list is curated: `__hash(key)` is a real call of the same
/// shape. One nesting level of parentheses is allowed. A match that is the
/// whole field of its line (`__bpf_md_ptr(struct bpf_iter_meta *, meta);`)
/// takes its `;` too: a lone `;` field is itself a parse error.
pub(crate) fn blank_c_parameterized_annotation_macros(source: &str) -> String {
    static ANNOTATION: OnceLock<Regex> = OnceLock::new();
    static FIELD_SEMICOLON: OnceLock<Regex> = OnceLock::new();
    if !source.contains("__") {
        return source.to_string();
    }
    let annotation = ANNOTATION.get_or_init(|| {
        Regex::new(&format!(
            r"\b(?:{C_PARAMETERIZED_ANNOTATIONS})[ \t]*\((?:[^()]|\([^()]*\))*\)"
        ))
        .expect("parameterized annotation regex")
    });
    let field_semicolon = regex(r"^[ \t]*;", &FIELD_SEMICOLON);
    if !annotation.is_match(source) {
        return source.to_string();
    }
    let mut bytes = source.as_bytes().to_vec();
    for found in annotation.find_iter(source) {
        let start = found.start();
        let mut end = found.end();
        let line_start = source[..start].rfind('\n').map_or(0, |at| at + 1);
        if source[line_start..start]
            .bytes()
            .all(|byte| byte == b' ' || byte == b'\t')
            && let Some(semicolon) = field_semicolon.find(&source[end..])
        {
            end += semicolon.end();
        }
        blank(&mut bytes, start..end);
    }
    into_text(bytes, source)
}

/// Heads whose `struct T` argument is valid C (or has its own pass).
const C_TYPE_ARG_HEAD_EXCLUSIONS: [&str; 14] = [
    "sizeof",
    "alignof",
    "_Alignof",
    "typeof",
    "__typeof__",
    "__typeof",
    "offsetof",
    "_Generic",
    "va_arg",
    "if",
    "while",
    "for",
    "switch",
    "case",
];

/// How far past a call's `(` the argument scan reads.
const C_TYPE_ARG_SCAN_CAP: usize = 600;

/// Blank a bare `struct` / `union` / `enum` type-keyword argument of a macro
/// call (upstream `blankCTypeKeywordArgs`): `kzalloc_obj(struct
/// bpf_mount_opts)`, `list_first_entry(&pending,\n struct async_entry,
/// domain_list)`, and the stars of `DEFINE_PER_CPU(struct task_struct *,
/// ksoftirqd)`. A bare type is no C expression, so tree-sitter recovers at
/// every such call; without the keyword the argument is an identifier.
/// Valid look-alikes keep theirs: `sizeof` / `offsetof` / `_Generic` heads; a
/// head preceded by a type (`int wf(struct a, struct b);`, a wrapped
/// prototype), unless the word before it is `return`; a cast (`f((struct T
/// *)p)`) and a declaration argument (`TP_PROTO(struct foo *bar)`). A bounded
/// hand scanner, not a nested regex, reads the arguments.
pub(crate) fn blank_c_type_keyword_args(source: &str) -> String {
    static ANY_KEYWORD: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static OPENER: OnceLock<Regex> = OnceLock::new();
    if !regex(r"\b(?:struct|union|enum)[ \t\r\n]", &ANY_KEYWORD).is_match(source) {
        return source.to_string();
    }
    let head = regex(r"\b([A-Za-z_][0-9A-Za-z_]*)[ \t]*\(", &HEAD);
    let opener = regex(
        r"^(struct|union|enum)([ \t\r\n]+)([A-Za-z_][0-9A-Za-z_]*)([ \t\r\n]*\*+)?",
        &OPENER,
    );
    let src = source.as_bytes();
    let mut bytes: Option<Vec<u8>> = None;
    // `return` precedes real calls; any other word or `*` before the head
    // makes it a declaration.
    let returns_before = |at: usize| -> bool {
        let mut j = at;
        while j > 0 && matches!(src[j - 1], b' ' | b'\t') {
            j -= 1;
        }
        if j == 0 {
            return true;
        }
        let previous = src[j - 1];
        if !(is_word_byte(previous) || previous == b'*') {
            return true;
        }
        let mut w = j;
        while w > 0 && is_word_byte(src[w - 1]) {
            w -= 1;
        }
        &source[w..j] == "return"
    };
    // After an opener, the argument must end, within the opener's window:
    // blanks, then `,` or `)`.
    let ends_argument = |at: usize, window_end: usize| -> bool {
        src[at..window_end]
            .iter()
            .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            .is_some_and(|byte| matches!(byte, b',' | b')'))
    };
    for found in head.captures_iter(source) {
        let whole = found.get(0).expect("match");
        let name = found.get(1).expect("head").as_str();
        if C_TYPE_ARG_HEAD_EXCLUSIONS.contains(&name) || !returns_before(whole.start()) {
            continue;
        }
        let open = whole.end() - 1;
        let mut depth = 1usize;
        let mut at_argument_start = true;
        let mut k = open + 1;
        while k < src.len() && k - open <= C_TYPE_ARG_SCAN_CAP {
            let c = src[k];
            if c == b'"' || c == b'\'' {
                k += 1;
                while k < src.len() && src[k] != c {
                    if src[k] == b'\\' {
                        k += 1;
                    }
                    k += 1;
                }
                at_argument_start = false;
                k += 1;
                continue;
            }
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => {
                    k += 1;
                    continue;
                }
                b'(' => {
                    depth += 1;
                    at_argument_start = false;
                    k += 1;
                    continue;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    at_argument_start = false;
                    k += 1;
                    continue;
                }
                b';' | b'{' | b'}' => break,
                b',' => {
                    if depth == 1 {
                        at_argument_start = true;
                    }
                    k += 1;
                    continue;
                }
                _ => {}
            }
            if depth == 1 && at_argument_start {
                let window_end = (k + 160).min(src.len());
                let window = source.get(k..window_end).or_else(|| source.get(k..));
                if let Some(window) = window
                    && let Some(open_type) = opener.captures(window)
                {
                    let keyword = open_type.get(1).expect("keyword");
                    let ident = open_type.get(3).expect("type name");
                    // With its stars when they end the argument, else without.
                    let window_end = k + window.len();
                    let stars = open_type
                        .get(4)
                        .filter(|stars| ends_argument(k + stars.end(), window_end));
                    let matched_end = match stars {
                        Some(stars) => Some(stars.end()),
                        None if ends_argument(k + ident.end(), window_end) => Some(ident.end()),
                        None => None,
                    };
                    if let Some(matched_end) = matched_end {
                        let out = bytes.get_or_insert_with(|| src.to_vec());
                        blank(out, k + keyword.start()..k + keyword.end());
                        if let Some(stars) = stars {
                            let star_count = stars
                                .as_str()
                                .bytes()
                                .rev()
                                .take_while(|b| *b == b'*')
                                .count();
                            blank(out, k + stars.end() - star_count..k + stars.end());
                        }
                        k += matched_end;
                        at_argument_start = false;
                        continue;
                    }
                }
            }
            at_argument_start = false;
            k += 1;
        }
    }
    bytes.map_or_else(|| source.to_string(), |bytes| into_text(bytes, source))
}

/// The end of the `( … )` that opens at byte `open` of `line`, string and
/// character literals skipped; `None` when it does not close on the line.
fn paren_close_on_line(line: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = open;
    while k < line.len() {
        let c = line[k];
        if c == b'"' || c == b'\'' {
            k += 1;
            while k < line.len() && line[k] != c {
                if line[k] == b'\\' {
                    k += 1;
                }
                k += 1;
            }
            k += 1;
            continue;
        }
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(k);
            }
        }
        k += 1;
    }
    None
}

/// Blank a `static` / `extern`-prefixed all-caps declaration macro line at
/// any scope (upstream `blankCFileScopePrefixedDeclMacros`): `static
/// DEFINE_PER_CPU(struct llist_head, rstat_backlog_list);`, `static
/// DECLARE_WORK(init_free_wq, do_free_init);`. A storage class before a call
/// is never valid C. The parentheses must close on the line and `;` end it;
/// an initializer form belongs to
/// [`rewrite_c_prefixed_decl_macro_initializers`], and an unprefixed
/// `EXPORT_SYMBOL(x);` parses as a K&R declaration and stays.
pub(crate) fn blank_c_file_scope_prefixed_decl_macros(source: &str) -> String {
    static ANY: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    if !regex(r"(?m)^[ \t]*(?:static|extern)[ \t]+[A-Z]", &ANY).is_match(source) {
        return source.to_string();
    }
    let line_head = regex(
        r"^[ \t]*(?:static|extern)[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(",
        &LINE,
    );
    let mut bytes: Option<Vec<u8>> = None;
    let mut offset = 0usize;
    for line in source.split('\n') {
        if let Some(found) = line_head.find(line) {
            let open = found.end() - 1;
            if let Some(close) = paren_close_on_line(line.as_bytes(), open)
                && line[close + 1..].trim_end_matches('\r').trim() == ";"
            {
                blank(
                    bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
                    offset..offset + line.len(),
                );
            }
        }
        offset += line.len() + 1;
    }
    bytes.map_or_else(|| source.to_string(), |bytes| into_text(bytes, source))
}

/// Rewrite a two-argument `static CAPS_MACRO(type, name) = {` declaration
/// macro into the declaration it expands to (upstream
/// `rewriteCPrefixedDeclMacroInitializers`): `static
/// DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {` becomes `static
/// struct cpuhp_cpu_state` padded up to `cpuhp_state`, which keeps its exact
/// column, and the `= {` tail its exact offsets; the initializer's function
/// references (`.startup.single = bringup_cpu`) survive. Three-argument
/// macros and `;`-terminated forms never match.
pub(crate) fn rewrite_c_prefixed_decl_macro_initializers(source: &str) -> String {
    static ANY: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    static TYPE_RUN: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    if !regex(r"(?m)^[ \t]*static[ \t]+[A-Z]", &ANY).is_match(source) {
        return source.to_string();
    }
    let line_re = regex(
        r#"^([ \t]*)static[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(([^();'"]*?),[ \t]*([A-Za-z_][0-9A-Za-z_]*)[ \t]*\)([ \t]*=[ \t]*\{?[ \t]*\r?)$"#,
        &LINE,
    );
    let type_run = regex(r"^[A-Za-z_][0-9A-Za-z_ \t*]*$", &TYPE_RUN);
    let spaces = regex(r"[ \t]+", &SPACES);
    let mut changed = false;
    let mut lines: Vec<String> = Vec::new();
    for line in source.split('\n') {
        let Some(found) = line_re.captures(line) else {
            lines.push(line.to_string());
            continue;
        };
        let indent = found.get(1).expect("indent").as_str();
        let arg1 = spaces
            .replace_all(found.get(2).expect("type").as_str().trim(), " ")
            .into_owned();
        let name = found.get(3).expect("name");
        let tail = found.get(4).expect("tail");
        let prefix = format!("{indent}static {arg1}");
        if !type_run.is_match(&arg1) || prefix.len() + 1 > name.start() {
            lines.push(line.to_string());
            continue;
        }
        lines.push(format!(
            "{prefix}{}{}{}{}",
            " ".repeat(name.start() - prefix.len()),
            name.as_str(),
            " ".repeat(tail.start() - name.end()),
            &line[tail.start()..]
        ));
        changed = true;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

/// Blank a qualified or pointer type argument of `va_arg` (upstream
/// `blankCVaArgQualifiedTypeArgs`): `va_arg(ap, const char *)` becomes a
/// one-argument call; `va_arg(ap, int)` parses as it is and stays. A
/// function-pointer type contains parentheses and is left alone. (Upstream
/// rewrites the head as `va_arg(`, which drops blanks before the `(`; the
/// head is kept as written here, so no offset moves.)
pub(crate) fn blank_c_va_arg_qualified_type_args(source: &str) -> String {
    static VA_ARG: OnceLock<Regex> = OnceLock::new();
    static SINGLE: OnceLock<Regex> = OnceLock::new();
    if !source.contains("va_arg") {
        return source.to_string();
    }
    let va_arg = regex(r"\bva_arg[ \t]*\(([^(),]+)(,[^()]*)\)", &VA_ARG);
    let single = regex(r"^,[ \t]*[A-Za-z_][0-9A-Za-z_]*[ \t]*$", &SINGLE);
    let mut bytes: Option<Vec<u8>> = None;
    for found in va_arg.captures_iter(source) {
        let rest = found.get(2).expect("type argument");
        if single.is_match(rest.as_str()) {
            continue;
        }
        blank(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            rest.range(),
        );
    }
    bytes.map_or_else(|| source.to_string(), |bytes| into_text(bytes, source))
}

/// Blank the dots of a GNU named-variadic `#define` parameter (upstream
/// `blankCNamedVariadicDefineDots`): `#define verbose(env, fmt, args...)`
/// becomes `args   `, which the grammar reads as an ordinary parameter; the
/// body (`##args` included) is untouched. Runs after the directive lines are
/// restored, on purpose.
pub(crate) fn blank_c_named_variadic_define_dots(source: &str) -> String {
    static DEFINE: OnceLock<Regex> = OnceLock::new();
    if !source.contains("...") {
        return source.to_string();
    }
    let define = regex(
        r"^[ \t]*#[ \t]*define[ \t]+[A-Za-z_][0-9A-Za-z_]*\([^()\n]*?\b[0-9A-Za-z_]+\.\.\.",
        &DEFINE,
    );
    let mut bytes: Option<Vec<u8>> = None;
    let mut offset = 0usize;
    for line in source.split('\n') {
        if let Some(found) = define.find(line) {
            blank(
                bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
                offset + found.end() - 3..offset + found.end(),
            );
        }
        offset += line.len() + 1;
    }
    bytes.map_or_else(|| source.to_string(), |bytes| into_text(bytes, source))
}

/// Round 2's passes before the leading attribute macro, in upstream's order.
pub(crate) fn blank_c_round_two(source: &str) -> String {
    rewrite_c_prefixed_decl_macro_initializers(&blank_c_file_scope_prefixed_decl_macros(
        &blank_c_va_arg_qualified_type_args(&blank_c_type_keyword_args(
            &blank_c_parameterized_annotation_macros(&blank_c_auto_inference(
                &blank_c_sandwiched_annotations(source),
            )),
        )),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> String {
        text.join("\n")
    }

    /// Whether tree-sitter-c reads `source` with an error anywhere.
    fn parses_with_errors(source: &str) -> bool {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("C grammar");
        parser
            .parse(source, None)
            .expect("a tree")
            .root_node()
            .has_error()
    }

    /// Each pass's target shape errors as written and parses once the C
    /// pre-parse has run (upstream's aim: the file stops deferring).
    #[test]
    fn each_shape_parses_without_errors_after_the_pre_parse() {
        use crate::spec::LanguageSpec as _;
        let spec = crate::lang::c::CSpec;
        for source in [
            "static notrace void tick_do(void) { do_tick(); }\n",
            "void q(void) { auto hb = get_hb(); use(hb); }\n",
            "struct file *f __free(fput) = NULL;\nstatic void __printf(4, 0) log_it(int a, const char *fmt, ...) { }\n",
            "void j(void *head) {\n\tvoid *p = kzalloc_obj(struct bpf_mount_opts);\n\tuse(p);\n}\n",
            "static DEFINE_PER_CPU(struct llist_head, rstat_backlog_list);\nint after(void) { return 4; }\n",
            "static DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {\n\t.startup = bringup_cpu,\n};\n",
            "void f(va_list ap) {\n\tconst char *s = va_arg(ap, const char *);\n\tuse(s);\n}\n",
            "#define verbose(env, fmt, args...) log_write(env, fmt, ##args)\nint after(void) { return 7; }\n",
        ] {
            assert!(parses_with_errors(source), "already parses: {source:?}");
            let pre_parsed = spec.pre_parse(source, "src/kernel.c");
            assert_eq!(pre_parsed.len(), source.len(), "{source:?}");
            assert!(!parses_with_errors(&pre_parsed), "{pre_parsed:?}");
        }
    }

    #[test]
    fn a_parameterized_annotation_blanks_whole_and_a_field_eats_its_semicolon() {
        let src = lines(&[
            "struct file *f __free(fput) = NULL;",
            "static void __printf(4, 0) log_it(int a, const char *fmt, ...);",
            "struct ctx {",
            "\t__bpf_md_ptr(struct bpf_iter_meta *, meta);",
            "};",
            "int keep = __hash(key);",
            "",
        ]);
        let out = blank_c_parameterized_annotation_macros(&src);
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("__free") && !out.contains("__printf"));
        assert!(out.contains("= NULL;"));
        assert!(!out.contains("__bpf_md_ptr"));
        assert_eq!(out.split('\n').nth(3).map(str::trim), Some(""));
        assert!(out.contains("__hash(key)"));
    }

    #[test]
    fn a_type_keyword_argument_blanks_and_valid_look_alikes_stay() {
        let src = lines(&[
            "void j(void *head, void *map) {",
            "\tvoid *p = kzalloc_obj(struct bpf_mount_opts);",
            "\tvoid *e = list_first_entry(head,",
            "\t\t\tstruct async_entry, domain_list);",
            "\tvoid *n = hlist_entry_safe(rcu_dereference_raw(hlist_next_rcu(head)),",
            "\t\t\tstruct bpf_dtab_netdev, index_hlist);",
            "\treturn container_of(map, struct bpf_map, inner);",
            "}",
            "DEFINE_PER_CPU(struct task_struct *, ksoftirqd);",
            "",
        ]);
        let out = blank_c_type_keyword_args(&src);
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("struct bpf_mount_opts"));
        assert!(out.contains("       bpf_mount_opts"));
        assert!(out.contains("       async_entry, domain_list"));
        assert!(out.contains("       bpf_dtab_netdev"));
        assert!(out.contains("       bpf_map, inner"));
        assert!(out.contains("       task_struct  , ksoftirqd"));
        for valid in [
            "int a = sizeof(struct point);",
            "int b = offsetof(struct point, y);",
            "int c = _Generic(x, struct foo *: 1, default: 0);",
            "int wf(struct a,\n\tstruct b);",
            "void cast(void *p) { use((struct foo *)p); }",
            "struct ops { int (*probe)(struct device *dev); };",
        ] {
            assert_eq!(blank_c_type_keyword_args(valid), valid, "{valid:?}");
        }
    }

    #[test]
    fn a_prefixed_declaration_macro_line_blanks_at_any_scope() {
        let src = lines(&[
            "static DEFINE_PER_CPU(struct llist_head, rstat_backlog_list);",
            "void f(void) {",
            "\tstatic DEFINE_RATELIMIT_STATE(ratelimit, 5 * HZ, 5);",
            "}",
            "EXPORT_SYMBOL(vmalloc);",
            "static DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {",
            "",
        ]);
        let out = blank_c_file_scope_prefixed_decl_macros(&src);
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("DEFINE_PER_CPU(struct llist_head"));
        assert!(!out.contains("DEFINE_RATELIMIT_STATE"));
        assert!(out.contains("EXPORT_SYMBOL(vmalloc);"));
        assert!(out.contains("cpuhp_state) = {"));
    }

    #[test]
    fn an_initialized_declaration_macro_rewrites_into_its_declaration() {
        let line = "static DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {";
        let src = lines(&[line, "\t.fail = CPUHP_INVALID,", "};", ""]);
        let out = rewrite_c_prefixed_decl_macro_initializers(&src);
        assert_eq!(out.len(), src.len());
        let rewritten = out.split('\n').next().expect("first line");
        assert!(rewritten.contains("static struct cpuhp_cpu_state"));
        assert!(!rewritten.contains("DEFINE_PER_CPU"));
        assert_eq!(
            rewritten[30..].find("cpuhp_state").map(|at| at + 30),
            line[30..].find("cpuhp_state").map(|at| at + 30)
        );
        assert_eq!(rewritten.find("= {"), line.find("= {"));
        let three = "static DEFINE_TIMER(t, fn, 0) = {\n};\n";
        assert_eq!(rewrite_c_prefixed_decl_macro_initializers(three), three);
    }

    #[test]
    fn a_multi_token_va_arg_type_blanks_and_a_single_token_stays() {
        let src = "void f(va_list ap) {\n\tconst char *s = va_arg(ap, const char *);\n\tint n = va_arg(ap, int);\n}\n";
        let out = blank_c_va_arg_qualified_type_args(src);
        assert_eq!(out.len(), src.len());
        assert!(out.contains("va_arg(ap              );"));
        assert!(out.contains("va_arg(ap, int);"));
        // The head keeps its blanks (upstream drops them, moving offsets).
        let spaced = "x = va_arg (ap, const char *);\n";
        let out = blank_c_va_arg_qualified_type_args(spaced);
        assert_eq!(out.len(), spaced.len());
        assert!(out.starts_with("x = va_arg (ap"));
    }

    #[test]
    fn only_the_dots_of_a_named_variadic_parameter_blank() {
        let named = "#define verbose(env, fmt, args...) log_write(env, fmt, ##args)\nint x;\n";
        let out = blank_c_named_variadic_define_dots(named);
        assert_eq!(out.len(), named.len());
        assert!(out.contains("args   )"));
        assert!(out.contains("##args"));
        let standard = "#define pr(fmt, ...) printk(fmt, __VA_ARGS__)\nint y;\n";
        assert_eq!(blank_c_named_variadic_define_dots(standard), standard);
    }

    #[test]
    fn a_sandwiched_annotation_and_c23_auto_blank_and_look_alikes_stay() {
        let src =
            "static notrace void tick_do(void) { }\nstatic nokprobe_inline void arm(void) { }\n";
        let out = blank_c_sandwiched_annotations(src);
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("notrace") && !out.contains("nokprobe_inline"));
        let variable = "void f(void) { int notrace = 1; use(notrace); }\n";
        assert_eq!(blank_c_sandwiched_annotations(variable), variable);
        let c23 = "void q(void) { auto hb = get_hb(); }\n";
        let out = blank_c_auto_inference(c23);
        assert_eq!(out.len(), c23.len());
        assert!(out.contains("     hb = get_hb();"));
        let storage = "void s(void) { auto int x = 1; }\n";
        assert_eq!(blank_c_auto_inference(storage), storage);
    }
}
