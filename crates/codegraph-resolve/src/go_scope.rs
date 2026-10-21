//! The parameters and locals a Go function declares, read from its file
//! (upstream #2448's scope reader, W4-GO-03).
//!
//! A call through a parameter or local that takes an import's name — etcd's
//! `jwt, err := newTokenProviderJWT(…)` beside `import ".../jwt/v5"`, a
//! testify suite's `suite` receiver — is a call on that variable, not into the
//! package. Go gives an identifier the innermost block that declares it, from
//! the end of its declaration on: a header's names live in its statement's
//! blocks (`else` included), a `select` case's in its clause, a parameter in
//! its function's body. The file is read once, comments and the contents of
//! strings blanked; where each name is bound is worked out the first time it
//! is asked for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;

/// What declares a Go name (upstream `GoDecl['kind']`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    /// A statement's local: `x := …`, `var x T`.
    Local,
    /// An `if` / `for` / `switch` header's.
    Header,
    /// A `select` / `switch` case's.
    Case,
    /// A receiver, parameter or result of the function whose body opens at
    /// `body`.
    Param,
}

/// One declaration of a Go name (upstream `GoDecl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoDecl {
    /// Offset of the declared name in the blanked code.
    at: usize,
    kind: DeclKind,
    body: Option<usize>,
    /// The declared type as written, when the declaration writes one.
    pub(crate) declared_type: Option<String>,
    /// The package a `:=` takes its value from: `clocktesting` in `clock :=
    /// clocktesting.NewFakeClock(…)`.
    pub(crate) from: Option<String>,
}

type Scope = (usize, usize, GoDecl);

/// A Go file's blocks and the names its functions declare (upstream
/// `GoScopeIndex`).
#[derive(Debug)]
pub(crate) struct GoScopeIndex {
    /// The source with CRLF read as LF, comments and string contents blanked,
    /// offsets kept.
    code: String,
    line_starts: Vec<usize>,
    /// Offsets of every `{` and `}`, with the innermost block open after each.
    events: Vec<usize>,
    open: Vec<Option<usize>>,
    close: HashMap<usize, usize>,
    decls: HashMap<String, Vec<GoDecl>>,
    /// Where each declared name is bound, read on demand.
    scopes: Mutex<HashMap<String, Arc<Vec<Scope>>>>,
}

/// Go's type keywords: `chan`, `func`, `interface`, `map`, `struct`.
fn is_type_keyword(word: &str) -> bool {
    matches!(word, "chan" | "func" | "interface" | "map" | "struct")
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

impl GoScopeIndex {
    /// Read `source` (upstream `readGoScopes(blankGoCode(…))`).
    pub(crate) fn read(source: &str) -> Self {
        let joined = source.lines().collect::<Vec<_>>().join("\n");
        read_go_scopes(blank_go_code(&joined))
    }

    /// The parameter or local `name` is at line `line` (1-based), column
    /// `column`, if one is: the innermost scope holding the position
    /// (upstream `goLocalDecl`).
    pub(crate) fn local_decl(&self, name: &str, line: i64, column: i64) -> Option<GoDecl> {
        if !self.decls.contains_key(name) {
            return None;
        }
        let start = usize::try_from(line.checked_sub(1)?)
            .ok()
            .and_then(|index| self.line_starts.get(index).copied())
            .unwrap_or(self.code.len());
        let at = start + usize::try_from(column.max(0)).unwrap_or(0);
        self.scopes_of(name)
            .iter()
            .filter(|(from, to, _)| at > *from && at < *to)
            .max_by_key(|(from, _, _)| *from)
            .map(|(_, _, decl)| decl.clone())
    }

    fn scopes_of(&self, name: &str) -> Arc<Vec<Scope>> {
        if let Some(hit) = self
            .scopes
            .lock()
            .expect("go scopes lock")
            .get(name)
            .cloned()
        {
            return hit;
        }
        static CLAUSE: OnceLock<Regex> = OnceLock::new();
        let clause = CLAUSE.get_or_init(|| {
            Regex::new(r"\n[ \t]*(?:case\b|default[ \t]*:)").expect("go case clause")
        });
        let mut scopes: Vec<Scope> = Vec::new();
        for decl in self.decls.get(name).into_iter().flatten() {
            if decl.kind == DeclKind::Param {
                let body = decl.body.unwrap_or(0);
                let end = self.close.get(&body).copied().unwrap_or(self.code.len());
                scopes.push((body, end, decl.clone()));
                continue;
            }
            // Outside every block it is a package-level name, which no import
            // shares.
            let Some(block) = self.block_at(decl.at) else {
                continue;
            };
            if decl.kind == DeclKind::Header {
                for (start, end) in self.header_blocks(decl.at) {
                    scopes.push((start, end, decl.clone()));
                }
                continue;
            }
            let from = statement_end(&self.code, decl.at);
            let mut to = block.1;
            if decl.kind == DeclKind::Case {
                for found in clause.find_iter(&self.code[from.min(self.code.len())..]) {
                    let at = from + found.start();
                    if at >= to {
                        break;
                    }
                    if self.block_at(at + 1).map(|(open, _)| open) == Some(block.0) {
                        to = at;
                        break;
                    }
                }
            }
            if from < to {
                scopes.push((from, to, decl.clone()));
            }
        }
        let scopes = Arc::new(scopes);
        self.scopes
            .lock()
            .expect("go scopes lock")
            .insert(name.to_string(), Arc::clone(&scopes));
        scopes
    }

    /// The innermost `{…}` holding an offset, as `(open, close)` — `None`
    /// outside every block (upstream `goBlockAt`).
    fn block_at(&self, offset: usize) -> Option<(usize, usize)> {
        let at = self.events.partition_point(|&event| event < offset);
        let open = at.checked_sub(1).and_then(|at| self.open[at])?;
        Some((
            open,
            self.close.get(&open).copied().unwrap_or(self.code.len()),
        ))
    }

    /// The blocks of the `if` / `for` / `switch` statement whose header
    /// declares at `at`: gofmt ends a header's line with its block's `{` (a
    /// composite literal's in the header has more on its line), and an `else`
    /// chain keeps the header's names (upstream `goHeaderBlocks`).
    fn header_blocks(&self, at: usize) -> Vec<(usize, usize)> {
        static ELSE: OnceLock<Regex> = OnceLock::new();
        let else_re = ELSE.get_or_init(|| Regex::new(r"^[ \t]*else\b[ \t]*").expect("go else"));
        let code = self.code.as_bytes();
        let mut blocks = Vec::new();
        let mut depth = 0i64;
        let mut i = at;
        while i < code.len() {
            match code[i] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'}' => break,
                b'{' => {
                    let end = self.close.get(&i).copied().unwrap_or(code.len());
                    let rest = slice_at(&self.code, i + 1, 80);
                    let line_end = rest
                        .trim_start_matches([' ', '\t'])
                        .chars()
                        .next()
                        .is_none_or(|c| c == '\n');
                    if depth != 0 || !line_end {
                        i = end + 1;
                        continue;
                    }
                    blocks.push((i, end));
                    let Some(chained) = else_re.find(slice_at(&self.code, end + 1, 40)) else {
                        break;
                    };
                    i = end + chained.end() + 1;
                    depth = 0;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        blocks
    }
}

/// At most `len` bytes of `code` from `start`, cut back to a char boundary.
fn slice_at(code: &str, start: usize, len: usize) -> &str {
    if start >= code.len() {
        return "";
    }
    let mut end = (start + len).min(code.len());
    while !code.is_char_boundary(end) {
        end -= 1;
    }
    &code[start..end]
}

/// The offset of the bracket closing the one at `at`, or the end of `code`
/// (upstream `goClosing`).
fn closing(code: &str, at: usize) -> usize {
    let mut depth = 0i64;
    for (i, &byte) in code.as_bytes().iter().enumerate().skip(at) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// Where the statement declaring at `at` ends: its line's end, or a `;`,
/// outside brackets (upstream `goStatementEnd`).
fn statement_end(code: &str, at: usize) -> usize {
    let mut depth = 0i64;
    for (i, &byte) in code.as_bytes().iter().enumerate().skip(at) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return i;
                }
            }
            b'\n' | b';' if depth == 0 => return i,
            _ => {}
        }
    }
    code.len()
}

fn read_go_scopes(code: String) -> GoScopeIndex {
    let bytes = code.as_bytes();
    let mut line_starts = vec![0];
    line_starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    let mut events = Vec::new();
    let mut open = Vec::new();
    let mut close = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'{' => stack.push(i),
            b'}' => {
                if let Some(at) = stack.pop() {
                    close.insert(at, i);
                }
            }
            _ => continue,
        }
        events.push(i);
        open.push(stack.last().copied());
    }
    let mut decls: HashMap<String, Vec<GoDecl>> = HashMap::new();
    let mut add = |name: &str, decl: GoDecl| {
        if name != "_" {
            decls.entry(name.to_string()).or_default().push(decl);
        }
    };
    short_variable_declarations(&code, &mut add);
    var_and_const_specs(&code, &mut add);
    function_parameters(&code, &mut add);
    GoScopeIndex {
        code,
        line_starts,
        events,
        open,
        close,
        decls,
        scopes: Mutex::new(HashMap::new()),
    }
}

/// `a, b := …`: the identifiers before each `:=`.
fn short_variable_declarations(code: &str, add: &mut impl FnMut(&str, GoDecl)) {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static CASE: OnceLock<Regex> = OnceLock::new();
    static TYPE: OnceLock<Regex> = OnceLock::new();
    static FROM: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(r"^\s*(?:\}\s*else\s+)?(?:if|for|switch)\b").expect("go header")
    });
    let case = CASE.get_or_init(|| Regex::new(r"^\s*case\b").expect("go case"));
    let type_re = TYPE.get_or_init(|| {
        Regex::new(r"^[ \t]*&?[ \t]*([A-Za-z_]\w*\.[A-Za-z_]\w*)[ \t]*\{").expect("go literal type")
    });
    let from_re = FROM.get_or_init(|| {
        Regex::new(r"^[ \t]*&?[ \t]*([A-Za-z_]\w*)\.[A-Za-z_]\w*[ \t]*[({]").expect("go value from")
    });
    let bytes = code.as_bytes();
    for (i, _) in code.match_indices(":=") {
        let mut names: Vec<(&str, usize)> = Vec::new();
        let mut p = i;
        loop {
            while p > 0 && matches!(bytes[p - 1], b' ' | b'\t') {
                p -= 1;
            }
            let mut s = p;
            while s > 0 && is_ident_byte(bytes[s - 1]) {
                s -= 1;
            }
            if s == p || bytes[s].is_ascii_digit() || (s > 0 && bytes[s - 1] == b'.') {
                break;
            }
            names.push((&code[s..p], s));
            p = s;
            while p > 0 && matches!(bytes[p - 1], b' ' | b'\t') {
                p -= 1;
            }
            if p == 0 || bytes[p - 1] != b',' {
                break;
            }
            p -= 1;
        }
        let Some(&(_, first)) = names.last() else {
            continue;
        };
        let line_start = code[..first].rfind('\n').map_or(0, |at| at + 1);
        let lead = &code[line_start..first];
        let kind = if header.is_match(lead) {
            DeclKind::Header
        } else if case.is_match(lead) {
            DeclKind::Case
        } else {
            DeclKind::Local
        };
        // `x := &pkg.T{…}` writes the type it declares; `x := pkg.New(…)`
        // the package its value comes from.
        let rhs = slice_at(code, i + 2, 160);
        let declared_type = (names.len() == 1)
            .then(|| type_re.captures(rhs).map(|caps| caps[1].to_string()))
            .flatten();
        let from = from_re.captures(rhs).map(|caps| caps[1].to_string());
        for (name, at) in names {
            add(
                name,
                GoDecl {
                    at,
                    kind,
                    body: None,
                    declared_type: declared_type.clone(),
                    from: from.clone(),
                },
            );
        }
    }
}

/// `var x T`, `var a, b = …`, `const (…)`: names before a type, an `=` or the
/// line's end — in a group, on lines outside the brackets of a value spanning
/// several (`http.MethodHead,` in a `[]string{…}` is no name).
fn var_and_const_specs(code: &str, add: &mut impl FnMut(&str, GoDecl)) {
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    let keyword = KEYWORD.get_or_init(|| Regex::new(r"\b(?:var|const)\b[ \t]*").expect("go var"));
    let bytes = code.as_bytes();
    for found in keyword.find_iter(code) {
        // `(?<![\w.])`: not a field or a longer word.
        if found.start() > 0
            && (is_ident_byte(bytes[found.start() - 1]) || bytes[found.start() - 1] == b'.')
        {
            continue;
        }
        let start = found.end();
        if bytes.get(start) != Some(&b'(') {
            add_spec(code, start, add);
            continue;
        }
        let end = closing(code, start);
        add_spec(code, start + 1, add);
        let mut depth = 0i64;
        for (i, &byte) in bytes.iter().enumerate().take(end).skip(start + 1) {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'\n' if depth == 0 => add_spec(code, i + 1, add),
                _ => {}
            }
        }
    }
}

/// One spec's names at `at` (upstream `addSpec`): `[ \t]*name(, name)*`
/// followed by a line's end, `=` (not `==`), or a type.
fn add_spec(code: &str, at: usize, add: &mut impl FnMut(&str, GoDecl)) {
    static TYPE: OnceLock<Regex> = OnceLock::new();
    let type_re = TYPE.get_or_init(|| {
        Regex::new(r"^[ \t]+([^\s=;][^=;\n]*?)[ \t]*(?:[=;\n]|$)").expect("go spec type")
    });
    let bytes = code.as_bytes();
    let mut p = at;
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t') {
        p += 1;
    }
    let names_start = p;
    // `name(?:[ \t]*,[ \t]*name)*`
    let mut names: Vec<(&str, usize)> = Vec::new();
    loop {
        let name_start = p;
        if !bytes
            .get(p)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            break;
        }
        while p < bytes.len() && is_ident_byte(bytes[p]) {
            p += 1;
        }
        names.push((&code[name_start..p], name_start));
        let mut q = p;
        while q < bytes.len() && matches!(bytes[q], b' ' | b'\t') {
            q += 1;
        }
        if bytes.get(q) != Some(&b',') {
            break;
        }
        q += 1;
        while q < bytes.len() && matches!(bytes[q], b' ' | b'\t') {
            q += 1;
        }
        if !bytes
            .get(q)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            break;
        }
        p = q;
    }
    if names.is_empty() || names_start == p {
        return;
    }
    let end = p;
    // `(?=[ \t]*(?:\n|$|=(?!=))|[ \t]+(?:[*[A-Za-z_(]|<-))`
    let after = &code[end..];
    let spaced = after.trim_start_matches([' ', '\t']);
    let ends = spaced.is_empty()
        || spaced.starts_with('\n')
        || (spaced.starts_with('=') && !spaced.starts_with("=="));
    let typed = after.len() > spaced.len()
        && (spaced.starts_with(['*', '[', '('])
            || spaced.starts_with("<-")
            || spaced
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_'));
    if !(ends || typed) {
        return;
    }
    let declared_type = type_re
        .captures(slice_at(code, end, 160))
        .map(|caps| caps[1].to_string());
    for (name, name_at) in names {
        add(
            name,
            GoDecl {
                at: name_at,
                kind: DeclKind::Local,
                body: None,
                declared_type: declared_type.clone(),
                from: None,
            },
        );
    }
}

/// Each `func`'s receiver, parameters and results, bound in its body. A
/// `[]func(…)` or `chan func(…)` is a type, whatever brace follows it.
fn function_parameters(code: &str, add: &mut impl FnMut(&str, GoDecl)) {
    static FUNC: OnceLock<Regex> = OnceLock::new();
    static FUNC_TYPE: OnceLock<Regex> = OnceLock::new();
    let func = FUNC.get_or_init(|| Regex::new(r"\bfunc\b").expect("go func"));
    let func_type =
        FUNC_TYPE.get_or_init(|| Regex::new(r"(?:\]|\bchan)[ \t]*$").expect("go func type"));
    let bytes = code.as_bytes();
    for found in func.find_iter(code) {
        let at = found.start();
        if at > 0 && (is_ident_byte(bytes[at - 1]) || bytes[at - 1] == b'.') {
            continue;
        }
        let mut lead_start = at.saturating_sub(8);
        while !code.is_char_boundary(lead_start) {
            lead_start -= 1;
        }
        if func_type.is_match(&code[lead_start..at]) {
            continue;
        }
        let Some((groups, body)) = signature(code, at + 4) else {
            continue;
        };
        for (from, to) in groups {
            for (name, name_at, declared_type) in param_names(code, from, to) {
                add(
                    name,
                    GoDecl {
                        at: name_at,
                        kind: DeclKind::Param,
                        body: Some(body),
                        declared_type: Some(declared_type),
                        from: None,
                    },
                );
            }
        }
    }
}

/// The parameter lists of the `func` whose keyword ends at `at` — receiver,
/// parameters, results — as the offsets inside their parentheses, and the
/// offset of its body's `{`; `None` for a function type, which has no body
/// (upstream `goSignature`).
fn signature(code: &str, at: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let bytes = code.as_bytes();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut p = at;
    let skip = |p: &mut usize| {
        while *p < bytes.len() && matches!(bytes[*p], b' ' | b'\t') {
            *p += 1;
        }
    };
    let group = |p: &mut usize, groups: &mut Vec<(usize, usize)>| -> bool {
        if bytes.get(*p) != Some(&b'(') {
            return false;
        }
        let end = closing(code, *p);
        groups.push((*p + 1, end));
        *p = end + 1;
        skip(p);
        true
    };
    // `func Name[T any](…)`, `func (r T) Name(…)`, or a literal / type
    // `func(…)`.
    let name = |p: &mut usize| -> bool {
        let start = *p;
        if !bytes
            .get(start)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return false;
        }
        let mut q = start;
        while q < bytes.len() && is_ident_byte(bytes[q]) {
            q += 1;
        }
        if is_type_keyword(&code[start..q]) {
            return false;
        }
        while q < bytes.len() && matches!(bytes[q], b' ' | b'\t') {
            q += 1;
        }
        if bytes.get(q) == Some(&b'[') {
            q = closing(code, q) + 1;
        }
        while q < bytes.len() && matches!(bytes[q], b' ' | b'\t') {
            q += 1;
        }
        if bytes.get(q) != Some(&b'(') {
            return false;
        }
        *p = q;
        true
    };
    skip(&mut p);
    if bytes.get(p) != Some(&b'(') {
        if !name(&mut p) || !group(&mut p, &mut groups) {
            return None;
        }
    } else {
        group(&mut p, &mut groups);
        if name(&mut p) && !group(&mut p, &mut groups) {
            return None;
        }
    }
    if bytes.get(p) == Some(&b'(') {
        group(&mut p, &mut groups);
    }
    // A result type up to the body; a function type ends at a `,`, `)`, `}`,
    // `=`, `;` or the line's end instead.
    let mut depth = 0i64;
    while p < bytes.len() {
        match bytes[p] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            b'{' => {
                if depth == 0 {
                    // `struct{…}` / `interface{…}` is the result type's own
                    // brace.
                    let mut lead_start = p.saturating_sub(12);
                    while !code.is_char_boundary(lead_start) {
                        lead_start -= 1;
                    }
                    let lead = code[lead_start..p].trim_end();
                    if lead.ends_with("struct") || lead.ends_with("interface") {
                        p = closing(code, p);
                    } else {
                        return Some((groups, p));
                    }
                }
            }
            b'\n' | b',' | b';' | b'=' | b'}' if depth == 0 => return None,
            _ => {}
        }
        p += 1;
    }
    None
}

/// The names a Go parameter list between `from` and `to` declares, with the
/// type each is declared as: `(a, b int, c string)` declares a and b as `int`
/// and c as `string`; `(int, pkg.T)` and `(*T, ...U)` declare none (upstream
/// `goParamNames`).
fn param_names(code: &str, from: usize, to: usize) -> Vec<(&str, usize, String)> {
    let bytes = code.as_bytes();
    let to = to.min(bytes.len());
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0i64;
    let mut start = from;
    for (i, &byte) in bytes.iter().enumerate().take(to).skip(from) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push((start, to));
    let mut names: Vec<(&str, usize, String)> = Vec::new();
    let mut grouped: Vec<(&str, usize)> = Vec::new();
    for (s, e) in parts {
        if s > e {
            continue;
        }
        let text = &code[s..e];
        let lead = text.len() - text.trim_start().len();
        let rest = &text[lead..];
        let name_len = rest
            .bytes()
            .position(|b| !is_ident_byte(b))
            .unwrap_or(rest.len());
        if name_len == 0
            || !rest
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        {
            continue;
        }
        let name = &rest[..name_len];
        if is_type_keyword(name) {
            continue;
        }
        let after = &rest[name_len..];
        // `pkg.T` is a type; a variadic `args ...T` is a name.
        let after_trimmed = after.trim_start();
        if after_trimmed.starts_with('.') && !after_trimmed.starts_with("...") {
            continue;
        }
        let at = s + lead;
        if after.trim().is_empty() {
            grouped.push((name, at));
            continue;
        }
        // `a, b int`: the names before a typed one share its type.
        let declared_type = after.trim().to_string();
        for (grouped_name, grouped_at) in grouped.drain(..) {
            names.push((grouped_name, grouped_at, declared_type.clone()));
        }
        names.push((name, at, declared_type));
    }
    // A list with no typed name names no parameters: its lone identifiers
    // are types.
    names
}

/// Go source with its comments and the contents of its strings and runes
/// blanked: offsets and newlines stay, so a match still maps to its line and
/// column (upstream `blankGoCode`).
fn blank_go_code(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = src[i..].find('\n').map_or(src.len(), |at| i + at);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..]
                    .find("*/")
                    .map_or(src.len(), |at| i + 2 + at + 2);
                blank(&mut out, i, end);
                i = end;
            }
            b'`' => {
                let end = src[i + 1..].find('`').map_or(src.len(), |at| i + 1 + at);
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            quote @ (b'"' | b'\'') => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == quote || bytes[j] == b'\n' {
                        break;
                    }
                    j += 1;
                }
                let j = j.min(bytes.len());
                blank(&mut out, i + 1, j);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(index: &GoScopeIndex, needle: &str) -> (i64, i64) {
        let offset = index.code.find(needle).expect("needle");
        let line = index.line_starts.partition_point(|&start| start <= offset);
        let column = offset - index.line_starts[line - 1];
        (line as i64, column as i64)
    }

    #[test]
    fn a_name_is_the_local_or_parameter_whose_scope_holds_it() {
        let source = "package auth\n\nimport \"github.com/golang-jwt/jwt/v5\"\n\nfunc TestJWTInfo(t *testing.T) {\n\tjwt, err := newTokenProviderJWT(\"key\")\n\t_ = err\n\tjwt.assign(\"abc\")\n}\n\nfunc TestJWTParse() {\n\tjwt.Parse(\"x\")\n}\n\nfunc (cache *store) Refresh() {\n\tcache.load()\n}\n\nfunc logError(logger logger.Interface, err error) {\n\tlogger.Error(err) // logger.Error here\n}\n";
        let index = GoScopeIndex::read(source);
        let (line, column) = at(&index, "jwt.assign");
        assert!(index.local_decl("jwt", line, column).is_some());
        let (line, column) = at(&index, "jwt.Parse");
        assert!(index.local_decl("jwt", line, column).is_none());
        let (line, column) = at(&index, "cache.load");
        assert!(index.local_decl("cache", line, column).is_some());
        let (line, column) = at(&index, "logger.Error(err)");
        let decl = index.local_decl("logger", line, column).expect("parameter");
        assert_eq!(decl.declared_type.as_deref(), Some("logger.Interface"));
    }

    #[test]
    fn a_header_name_lives_in_its_blocks_and_a_case_name_in_its_clause() {
        let source = "package p\n\nfunc f(ch chan int) {\n\tif cmp := pick(); cmp != nil {\n\t\tcmp.Compare(1, 2)\n\t} else {\n\t\tcmp.Compare(3, 4)\n\t}\n\tcmp.Diff(\"a\", \"b\")\n\tfor _, item := range items() {\n\t\titem.Close()\n\t}\n\titem.Open()\n\tselect {\n\tcase clock := <-ch:\n\t\tclock.Now()\n\tdefault:\n\t\tclock.Later()\n\t}\n\tlocal := clocktesting.NewFakeClock(0)\n\tlocal.Now()\n}\n";
        let index = GoScopeIndex::read(source);
        let (line, column) = at(&index, "cmp.Compare(1, 2)");
        assert!(index.local_decl("cmp", line, column).is_some());
        let (line, column) = at(&index, "cmp.Compare(3, 4)");
        assert!(index.local_decl("cmp", line, column).is_some());
        let (line, column) = at(&index, "cmp.Diff");
        assert!(index.local_decl("cmp", line, column).is_none());
        let (line, column) = at(&index, "item.Close()");
        assert!(index.local_decl("item", line, column).is_some());
        let (line, column) = at(&index, "item.Open()");
        assert!(index.local_decl("item", line, column).is_none());
        let (line, column) = at(&index, "clock.Now()");
        assert!(index.local_decl("clock", line, column).is_some());
        let (line, column) = at(&index, "clock.Later()");
        assert!(index.local_decl("clock", line, column).is_none());
        let (line, column) = at(&index, "local.Now()");
        let decl = index.local_decl("local", line, column).expect("local");
        assert_eq!(decl.from.as_deref(), Some("clocktesting"));
    }

    #[test]
    fn comments_and_strings_are_blanked_in_place() {
        let code = blank_go_code("a := \"x := 1\" // y := 2\nb := `z\n:= 3`\n");
        assert_eq!(code, "a := \"      \"          \nb := ` \n    `\n");
    }

    #[test]
    fn parameter_lists_name_what_they_declare() {
        let code = "(a, b int, c string, args ...T)";
        let names: Vec<(String, String)> = param_names(code, 1, code.len() - 1)
            .into_iter()
            .map(|(name, _, ty)| (name.to_string(), ty))
            .collect();
        assert_eq!(
            names,
            [
                ("a".into(), "int".into()),
                ("b".into(), "int".into()),
                ("c".into(), "string".into()),
                ("args".into(), "...T".into()),
            ]
        );
        let code = "(int, pkg.T)";
        assert!(param_names(code, 1, code.len() - 1).is_empty());
    }
}
