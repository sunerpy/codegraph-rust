//! C++ scopes read from the source's braces, for a file whose parse tree has
//! errors (upstream #2426 `cpp-brace-scopes.ts`).
//!
//! tree-sitter recovers from a construct it can't parse by inserting the
//! token it expected, and inside a class body that token is often a `}`: an
//! unknown macro in front of a member (`NODISCARD absl::string_view name()
//! const`) or in a class head (`struct cacheline_aligned(64U) HandleImpl`)
//! closes the class there. Every `}` after it then closes the scope one level
//! out, so the rest of the class, and of each namespace around it, parses as
//! declarations of an outer scope: protobuf's `FieldDescriptor` indexed as
//! `FieldDescriptor` instead of `google::protobuf::FieldDescriptor`, rocksdb's
//! `struct Opts` outside the class it is declared in. Recovery also runs the
//! other way and keeps a scope open past its own `}`.
//!
//! The braces are rarely what the parser misread, so the walker takes each
//! declaration's namespaces and enclosing classes from them instead. The scan
//! skips comments, string and character literals (raw strings and digit
//! separators too) and preprocessor lines. Each branch of an `#if` is read
//! from the braces open at the `#if`, and the scan continues after `#endif`
//! with the first branch's, so `#if A` `struct X : B {` `#else` `struct X {`
//! `#endif` opens one scope, not two. A file whose braces don't balance gets
//! no scopes and keeps the tree's.
//!
//! Offsets are bytes of the source the parser saw (after the pre-parse), as
//! the tree's own offsets are.

use std::collections::{BTreeMap, HashMap};

/// Intervals that nest properly, added in order of their opening offset.
#[derive(Debug, Clone)]
pub struct NestedIntervals<T> {
    opens: Vec<usize>,
    closes: Vec<usize>,
    parents: Vec<Option<usize>>,
    values: Vec<T>,
}

impl<T> Default for NestedIntervals<T> {
    fn default() -> Self {
        Self {
            opens: Vec::new(),
            closes: Vec::new(),
            parents: Vec::new(),
            values: Vec::new(),
        }
    }
}

impl<T> NestedIntervals<T> {
    /// Adds the interval `(open, close)`. One that opens before the last one
    /// added, or crosses one that holds it, is refused (false).
    pub fn add(&mut self, open: usize, close: usize, value: T) -> bool {
        if close <= open || self.opens.last().is_some_and(|&last| open <= last) {
            return false;
        }
        let parent = self.innermost(open);
        if parent.is_some_and(|parent| close >= self.closes[parent]) {
            return false;
        }
        self.opens.push(open);
        self.closes.push(close);
        self.parents.push(parent);
        self.values.push(value);
        true
    }

    /// The values of the intervals holding `offset` (open < offset < close),
    /// outermost first.
    pub fn at(&self, offset: usize) -> Vec<&T> {
        let mut out = Vec::new();
        let mut at = self.innermost(offset);
        while let Some(k) = at {
            out.push(&self.values[k]);
            at = self.parents[k];
        }
        out.reverse();
        out
    }

    /// The innermost interval holding `offset`: the last interval opening
    /// before `offset`, or one of the intervals around that one.
    fn innermost(&self, offset: usize) -> Option<usize> {
        let mut k = self
            .opens
            .partition_point(|&open| open < offset)
            .checked_sub(1);
        while let Some(at) = k {
            if self.closes[at] > offset {
                break;
            }
            k = self.parents[at];
        }
        k
    }
}

/// The braces and named namespaces of one C++ source.
#[derive(Debug, Clone)]
pub struct CppBraceScopes {
    closes: HashMap<usize, usize>,
    namespaces: NestedIntervals<String>,
    line_starts: Vec<usize>,
}

impl CppBraceScopes {
    /// The offset of the `}` that closes the `{` at `open`.
    pub fn close_of(&self, open: usize) -> Option<usize> {
        self.closes.get(&open).copied()
    }

    /// The named namespaces whose braces hold `offset`, outermost first, as
    /// written (`a::b`).
    pub fn namespaces_at(&self, offset: usize) -> Vec<String> {
        self.namespaces.at(offset).into_iter().cloned().collect()
    }

    /// The 1-based line and the 0-based byte column of `offset`, as the
    /// tree's positions are.
    pub fn position_of(&self, offset: usize) -> (i64, i64) {
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        (line as i64 + 1, (offset - self.line_starts[line]) as i64)
    }
}

fn is_blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | 0x0c | 0x0b)
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_word_char(c: u8) -> bool {
    is_word_start(c) || c.is_ascii_digit()
}

fn byte(source: &[u8], at: usize) -> u8 {
    source.get(at).copied().unwrap_or(0)
}

/// Whether `text` is a raw string's prefix (`R`, `u8R`, `uR`, `UR`, `LR`).
fn is_raw_string_prefix(text: &[u8]) -> bool {
    matches!(text, b"R" | b"u8R" | b"uR" | b"UR" | b"LR")
}

/// A raw string's delimiter: up to 16 printable ASCII characters but `"`,
/// `(`, `)`, `\` and the space.
fn is_raw_string_delimiter(text: &[u8]) -> bool {
    text.len() <= 16
        && text
            .iter()
            .all(|&c| (b'!'..=b'~').contains(&c) && !matches!(c, b'"' | b'(' | b')' | b'\\'))
}

/// Where `needle` next occurs in `source` from `from`.
fn find(source: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= source.len() {
        return None;
    }
    source[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// The end of a `//` comment starting at `at`: its newline (a `\` before it
/// continues the comment).
fn line_comment_end(source: &[u8], mut at: usize) -> usize {
    loop {
        let Some(newline) = find(source, at, b"\n") else {
            return source.len();
        };
        let mut k = newline;
        if k > 0 && source[k - 1] == b'\r' {
            k -= 1;
        }
        if k == 0 || source[k - 1] != b'\\' {
            return newline;
        }
        at = newline + 1;
    }
}

/// The end of a `/* */` comment opened at `at` (its `/*`).
fn block_comment_end(source: &[u8], at: usize) -> usize {
    find(source, at + 2, b"*/").map_or(source.len(), |end| end + 2)
}

/// Past whitespace and comments from `at`.
fn skip_blank(source: &[u8], mut at: usize) -> usize {
    while at < source.len() {
        let c = source[at];
        if is_blank(c) || c == b'\n' {
            at += 1;
        } else if c == b'/' && byte(source, at + 1) == b'/' {
            at = line_comment_end(source, at + 2);
        } else if c == b'/' && byte(source, at + 1) == b'*' {
            at = block_comment_end(source, at);
        } else {
            break;
        }
    }
    at
}

/// The end of the identifier at `at`, or `None` when none starts there.
fn word_end(source: &[u8], at: usize) -> Option<usize> {
    if !is_word_start(byte(source, at)) {
        return None;
    }
    let mut end = at + 1;
    while end < source.len() && is_word_char(source[end]) {
        end += 1;
    }
    Some(end)
}

/// Past the string or character literal whose quote is at `at` (it stops at
/// a newline).
fn quoted_end(source: &[u8], at: usize) -> usize {
    let quote = source[at];
    let mut k = at + 1;
    while k < source.len() {
        let c = source[k];
        if c == b'\\' {
            k += if byte(source, k + 1) == b'\r' && byte(source, k + 2) == b'\n' {
                3
            } else {
                2
            };
        } else if c == quote {
            return k + 1;
        } else if c == b'\n' {
            return k;
        } else {
            k += 1;
        }
    }
    source.len()
}

/// Past the `(…)` group at `at` (strings inside skipped), or `None`.
fn paren_group_end(source: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = at;
    while k < source.len() {
        match source[k] {
            b'"' | b'\'' => {
                k = quoted_end(source, k);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// After the `namespace` keyword ending at `from`: the name as written and
/// the offset of the `{` that opens the body. `None` for anything else, such
/// as `using namespace std;` or `namespace fs = std::filesystem;`. Takes C++17
/// attributes (`namespace [[deprecated]] old {`), nested and inline names
/// (`namespace a::inline b {`), and an attribute macro after the name
/// (`namespace std _GLIBCXX_VISIBILITY(default) {`).
fn namespace_head(source: &[u8], from: usize) -> Option<(String, usize)> {
    let mut at = skip_blank(source, from);
    while source[at..].starts_with(b"[[") {
        let end = find(source, at + 2, b"]]")?;
        at = skip_blank(source, end + 2);
    }
    let mut name: Option<(usize, usize)> = None;
    while let Some(end) = word_end(source, at) {
        let start = name.map_or(at, |(start, _)| start);
        name = Some((start, end));
        at = skip_blank(source, end);
        if !source[at..].starts_with(b"::") {
            break;
        }
        at = skip_blank(source, at + 2);
        if let Some(inline) = word_end(source, at)
            && &source[at..inline] == b"inline"
        {
            at = skip_blank(source, inline);
        }
    }
    if name.is_some() {
        while let Some(end) = word_end(source, at) {
            let paren = skip_blank(source, end);
            if byte(source, paren) != b'(' {
                return None;
            }
            at = skip_blank(source, paren_group_end(source, paren)?);
        }
    }
    if byte(source, at) != b'{' {
        return None;
    }
    let name = name.map_or(String::new(), |(start, end)| {
        String::from_utf8_lossy(&source[start..end]).into_owned()
    });
    Some((name, at))
}

/// Per `#if` group: the braces open at the `#if`, and those open after its
/// first branch (`None` until a second branch starts).
struct Branch {
    at_if: Vec<usize>,
    after_first: Option<Vec<usize>>,
}

/// Scan `source` (a C++ file as the parser saw it, after the pre-parse) for
/// its braces and namespaces. `None` when the braces don't balance.
pub fn scan_cpp_brace_scopes(text: &str) -> Option<CppBraceScopes> {
    let source = text.as_bytes();
    let n = source.len();
    let mut closes: HashMap<usize, usize> = HashMap::new();
    let mut open: Vec<usize> = Vec::new();
    let mut branches: Vec<Branch> = Vec::new();
    // `{` offset -> name, for named namespaces.
    let mut namespace_at: BTreeMap<usize, String> = BTreeMap::new();
    let mut pending_namespace: Option<(String, usize)> = None;
    let mut balanced = true;
    let mut line_start = true;
    // The last identifier or number read.
    let mut word = (usize::MAX, usize::MAX);
    // Past a byte-order mark, so line 1 can be a directive.
    let mut i = if source.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    while i < n {
        let c = source[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if is_blank(c) {
            i += 1;
            continue;
        }
        if c == b'/' && byte(source, i + 1) == b'/' {
            i = line_comment_end(source, i + 2);
            continue;
        }
        if c == b'/' && byte(source, i + 1) == b'*' {
            i = block_comment_end(source, i);
            continue;
        }
        if c == b'#' && line_start {
            // A directive runs to the end of its line, through `\`
            // continuations and comments. Only `"` strings are skipped in it:
            // an apostrophe in `#error Don't …` is no character literal.
            let mut k = i + 1;
            while k < n && is_blank(source[k]) {
                k += 1;
            }
            let directive = word_end(source, k).map_or(&b""[..], |end| &source[k..end]);
            while k < n {
                let d = source[k];
                if d == b'\n' {
                    let mut back = k;
                    if back > 0 && source[back - 1] == b'\r' {
                        back -= 1;
                    }
                    if back == 0 || source[back - 1] != b'\\' {
                        break;
                    }
                    k += 1;
                } else if d == b'/' && byte(source, k + 1) == b'/' {
                    k = line_comment_end(source, k + 2);
                } else if d == b'/' && byte(source, k + 1) == b'*' {
                    k = block_comment_end(source, k);
                } else if d == b'"' {
                    k = quoted_end(source, k);
                } else {
                    k += 1;
                }
            }
            match directive {
                b"if" | b"ifdef" | b"ifndef" => branches.push(Branch {
                    at_if: open.clone(),
                    after_first: None,
                }),
                b"else" | b"elif" | b"elifdef" | b"elifndef" => {
                    if let Some(group) = branches.last_mut() {
                        if group.after_first.is_none() {
                            group.after_first = Some(std::mem::take(&mut open));
                        }
                        open = group.at_if.clone();
                    }
                }
                b"endif" => {
                    if let Some(group) = branches.pop()
                        && let Some(after_first) = group.after_first
                    {
                        open = after_first;
                    }
                }
                _ => {}
            }
            i = k;
            continue;
        }
        line_start = false;
        if c == b'"' {
            // A raw string (`R"x(…)x"`) can hold any brace and quote; its
            // prefix is the word that ends right at the quote.
            if word.1 == i && i - word.0 <= 3 && is_raw_string_prefix(&source[word.0..i]) {
                if let Some(paren) = find(source, i + 1, b"(") {
                    let delimiter = &source[i + 1..paren];
                    if is_raw_string_delimiter(delimiter) {
                        let mut close = Vec::with_capacity(delimiter.len() + 2);
                        close.push(b')');
                        close.extend_from_slice(delimiter);
                        close.push(b'"');
                        i = find(source, paren + 1, &close)
                            .map_or(n, |end| end + delimiter.len() + 2);
                        continue;
                    }
                }
            }
            i = quoted_end(source, i);
            continue;
        }
        if c == b'\'' {
            i = quoted_end(source, i);
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && byte(source, i + 1).is_ascii_digit()) {
            // A number, digit separators (`1'000'000`) and exponent signs
            // included.
            let mut k = i + 1;
            while k < n {
                let d = source[k];
                if is_word_char(d) || d == b'.' {
                    k += 1;
                } else if d == b'\'' && is_word_char(byte(source, k + 1)) {
                    k += 2;
                } else if matches!(d, b'+' | b'-')
                    && matches!(source[k - 1], b'e' | b'E' | b'p' | b'P')
                {
                    k += 1;
                } else {
                    break;
                }
            }
            word = (i, k);
            i = k;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(source, i).unwrap_or(i + 1);
            word = (i, end);
            if &source[i..end] == b"namespace"
                && let Some(head) = namespace_head(source, end)
            {
                pending_namespace = Some(head);
            }
            i = end;
            continue;
        }
        if c == b'{' {
            open.push(i);
            if let Some((name, brace)) = pending_namespace.take() {
                if brace == i {
                    if !name.is_empty() {
                        namespace_at.insert(i, name);
                    }
                } else {
                    pending_namespace = Some((name, brace));
                }
            }
        } else if c == b'}' {
            match open.pop() {
                None => balanced = false,
                Some(from) => {
                    closes.entry(from).or_insert(i);
                }
            }
        }
        i += 1;
    }
    if !balanced || !open.is_empty() {
        return None;
    }
    let mut namespaces = NestedIntervals::default();
    for (brace, name) in namespace_at {
        if let Some(&close) = closes.get(&brace) {
            namespaces.add(brace, close, name);
        }
    }
    let mut line_starts = vec![0];
    line_starts.extend(
        source
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == b'\n')
            .map(|(at, _)| at + 1),
    );
    Some(CppBraceScopes {
        closes,
        namespaces,
        line_starts,
    })
}

#[cfg(test)]
mod tests {
    use super::{NestedIntervals, scan_cpp_brace_scopes};

    #[test]
    fn matches_braces_past_comments_literals_and_preprocessor_lines() {
        let source = [
            "namespace a {",
            "// } a comment",
            "/* { */ const char* s = \"}{\";",
            "char c = '}'; int n = 1'000'000;",
            "auto r = R\"x(})x\";",
            "#define OPEN {",
            "struct S { int x; };",
            "}",
            "",
        ]
        .join("\n");
        let scopes = scan_cpp_brace_scopes(&source).expect("balanced");
        let open = source.find('{').unwrap();
        assert_eq!(scopes.close_of(open), source.rfind('}'));
        let structure = source.find("S {").unwrap() + 2;
        let close = scopes.close_of(structure).unwrap();
        assert_eq!(&source[structure..=close], "{ int x; }");
        assert_eq!(scopes.namespaces_at(structure), ["a"]);
    }

    #[test]
    fn reads_each_if_branch_from_the_braces_open_at_the_if() {
        let source = [
            "namespace a {",
            "#if defined(X)",
            "struct S : Base {",
            "#else",
            "struct S {",
            "#endif",
            "  int x;",
            "};",
            "struct T {};",
            "}",
            "",
        ]
        .join("\n");
        let scopes = scan_cpp_brace_scopes(&source).expect("balanced");
        let first = source.find("Base {").unwrap() + 5;
        assert_eq!(scopes.close_of(first), source.find("};"));
        assert_eq!(
            scopes.namespaces_at(source.find("struct T").unwrap()),
            ["a"]
        );
    }

    #[test]
    fn names_namespaces_as_written_and_skips_what_declares_none() {
        let source = [
            "namespace a::b {",
            "namespace [[deprecated]] c {",
            "inline namespace v1 {",
            "namespace {",
            "using namespace std;",
            "namespace fs = std::filesystem;",
            "int here;",
            "}",
            "}",
            "}",
            "}",
            "namespace std _GLIBCXX_VISIBILITY(default) {",
            "int there;",
            "}",
            "",
        ]
        .join("\n");
        let scopes = scan_cpp_brace_scopes(&source).expect("balanced");
        assert_eq!(
            scopes.namespaces_at(source.find("int here").unwrap()),
            ["a::b", "c", "v1"]
        );
        assert_eq!(
            scopes.namespaces_at(source.find("int there").unwrap()),
            ["std"]
        );
    }

    #[test]
    fn gives_up_on_braces_that_do_not_balance() {
        assert!(scan_cpp_brace_scopes("namespace a {\nint x;\n").is_none());
        assert!(scan_cpp_brace_scopes("int x;\n}\n").is_none());
    }

    /// Upstream places offsets in UTF-16 columns, as its parser does; the
    /// tree here gives byte columns, and so do these.
    #[test]
    fn places_offsets_in_byte_columns_as_the_tree_does() {
        let source = "namespace n {\n/* é😀 */ class C { int x; };\n}\n";
        let scopes = scan_cpp_brace_scopes(source).expect("balanced");
        let close = scopes.close_of(source.find("{ int").unwrap()).unwrap();
        assert_eq!(scopes.position_of(close + 1), (2, 31));
    }

    #[test]
    fn nested_intervals_refuse_crossing_and_out_of_order_intervals() {
        let mut intervals = NestedIntervals::default();
        assert!(intervals.add(0, 100, "outer"));
        assert!(intervals.add(10, 20, "first"));
        assert!(!intervals.add(5, 30, "before the last"));
        assert!(!intervals.add(15, 40, "crosses"));
        assert!(intervals.add(30, 50, "second"));
        assert_eq!(intervals.at(12), [&"outer", &"first"]);
        assert_eq!(intervals.at(25), [&"outer"]);
        assert_eq!(intervals.at(40), [&"outer", &"second"]);
        assert!(intervals.at(100).is_empty());
    }
}

/// Upstream #2426's fixtures (`cpp-brace-scopes.test.ts`): classes that tree-sitter's
/// error recovery closes early, keeps open past their `}`, or swallows into an
/// ERROR, walked in the scopes their braces open.
#[cfg(test)]
mod fixtures {
    use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};

    const PROTOBUF_SHAPE: &str = r#"namespace google {
namespace protobuf {

class Names {
 public:
  NODISCARD absl::string_view name() const {
    return absl::string_view(payload_);
  }

 private:
  const char* payload_;
};

class FieldDescriptor : private internal::SymbolBase {
 public:
  int index() const;
};

}  // namespace protobuf
}  // namespace google
"#;
    const ROCKSDB_SHAPE: &str = r#"namespace rocksdb {
namespace clock_cache {

class FixedTable : public BaseTable {
 public:
  struct cacheline_aligned(64U) HandleImpl : public ClockHandle {
    RelaxedAtomic<uint32_t> displacements{};
    bool standalone = false;
  };

  struct Opts : public BaseOpts {
    explicit Opts(size_t size) : BaseOpts(0), size(size) {}
    size_t size;
  };

  void Insert(const Opts& opts);
};

class AutoTable : public BaseTable {
 public:
  void Erase();
};

}  // namespace clock_cache
}  // namespace rocksdb
"#;
    const BRANCHES_SHAPE: &str = r#"namespace net {

class Socket {
 public:
#ifdef _WIN32
  int Open(SOCKET handle) {
#else
  int Open(int fd) {
#endif
    return Bind();
  }

  int Bind();
};

class Listener {
 public:
  void Accept() {}
};

}  // namespace net
"#;
    const FMT_SHAPE: &str = r#"namespace fmt {
namespace detail {

template <typename Context> class basic_format_args {
 public:
  FMT_CONSTEXPR auto get(int id) const -> format_arg {
    auto arg = format_arg();
    if (!is_packed()) {
      if (unsigned(id) < unsigned(max_size())) arg = args_[id];
      return arg;
    }
    if (unsigned(id) >= detail::max_packed_args) return arg;
    return arg;
  }

  auto max_size() const -> int { return 0; }
};

class context {
 public:
  void advance_to(int it) {}
};

}  // namespace detail
}  // namespace fmt
"#;

    fn extract(file: &str, source: &str) -> ExtractionResult {
        crate::extract_source(file, source, Some(Language::Cpp))
    }

    fn named(result: &ExtractionResult, kind: NodeKind) -> Vec<&str> {
        result
            .nodes
            .iter()
            .filter(|node| node.kind == kind)
            .map(|node| node.qualified_name.as_str())
            .collect()
    }

    fn node_at<'a>(result: &'a ExtractionResult, qualified_name: &str) -> &'a Node {
        result
            .nodes
            .iter()
            .find(|node| node.qualified_name == qualified_name)
            .unwrap_or_else(|| panic!("no {qualified_name}: {:#?}", result.nodes))
    }

    fn has_error_after_pre_parse(file: &str, source: &str) -> bool {
        let text = crate::lang::cpp::pre_parse_cpp_source(source, file);
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp grammar");
        parser
            .parse(&text, None)
            .expect("tree")
            .root_node()
            .has_error()
    }

    #[test]
    fn every_fixture_still_misparses() {
        for (file, source) in [
            ("descriptor.h", PROTOBUF_SHAPE),
            ("clock_cache.h", ROCKSDB_SHAPE),
            ("socket.h", BRANCHES_SHAPE),
            ("core.h", FMT_SHAPE),
        ] {
            assert!(
                has_error_after_pre_parse(file, source),
                "{file} parses cleanly"
            );
        }
    }

    #[test]
    fn a_class_after_one_closed_early_keeps_its_namespaces() {
        let result = extract("src/google/protobuf/descriptor.h", PROTOBUF_SHAPE);
        assert_eq!(
            named(&result, NodeKind::Class),
            [
                "google::protobuf::Names",
                "google::protobuf::FieldDescriptor"
            ]
        );
        // `Names` is the one recovery glued to the tokens after it: it ends at its own `};`.
        assert_eq!(node_at(&result, "google::protobuf::Names").end_line, 12);
    }

    #[test]
    fn a_nested_struct_keeps_its_class_and_the_next_class_its_namespaces() {
        let result = extract("cache/clock_cache.h", ROCKSDB_SHAPE);
        assert_eq!(
            named(&result, NodeKind::Class),
            [
                "rocksdb::clock_cache::FixedTable",
                "rocksdb::clock_cache::AutoTable"
            ]
        );
        assert_eq!(
            named(&result, NodeKind::Struct),
            ["rocksdb::clock_cache::FixedTable::Opts"]
        );
        assert_eq!(
            named(&result, NodeKind::Method),
            ["rocksdb::clock_cache::FixedTable::Opts::Opts"]
        );
        // The class spans its whole body, so the struct it holds lies inside it.
        let fixed = node_at(&result, "rocksdb::clock_cache::FixedTable");
        assert_eq!((fixed.start_line, fixed.end_line), (4, 17));
        let opts = node_at(&result, "rocksdb::clock_cache::FixedTable::Opts");
        let contains = result
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Contains && edge.target == opts.id)
            .expect("contains edge");
        assert_eq!(contains.source, fixed.id);
    }

    #[test]
    fn a_class_the_tree_kept_open_past_its_brace_lets_go_of_the_next_class() {
        let result = extract("net/socket.h", BRANCHES_SHAPE);
        assert_eq!(
            named(&result, NodeKind::Class),
            ["net::Socket", "net::Listener"]
        );
        assert!(
            named(&result, NodeKind::Method).contains(&"net::Listener::Accept"),
            "{:?}",
            named(&result, NodeKind::Method)
        );
        assert_eq!(node_at(&result, "net::Socket").end_line, 14);
    }

    #[test]
    fn namespaces_the_tree_swallowed_into_an_error_still_prefix_what_they_hold() {
        let result = extract("include/fmt/core.h", FMT_SHAPE);
        assert_eq!(named(&result, NodeKind::Class), ["fmt::detail::context"]);
        assert_eq!(
            named(&result, NodeKind::Method),
            ["fmt::detail::context::advance_to"]
        );
    }

    #[test]
    fn a_file_that_parses_cleanly_keeps_the_tree_scopes() {
        let clean = PROTOBUF_SHAPE.replace("NODISCARD ", "");
        assert!(!has_error_after_pre_parse("descriptor.h", &clean));
        let result = extract("src/google/protobuf/descriptor.h", &clean);
        assert_eq!(
            named(&result, NodeKind::Class),
            [
                "google::protobuf::Names",
                "google::protobuf::FieldDescriptor"
            ]
        );
        assert_eq!(
            named(&result, NodeKind::Method),
            ["google::protobuf::Names::name"]
        );
    }
}
