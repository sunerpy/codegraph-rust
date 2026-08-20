//! Content-header detection for machine-generated files (upstream #1500).
//!
//! [`crate::file_class::is_generated_file`] answers the same question from the
//! PATH alone, and for most ecosystems that is enough: codegen output follows a
//! `<basename>.<tool>.<ext>` convention (`.pb.go`, `_pb2.py`, `.g.dart`) that a
//! suffix check catches for free. Go is the exception that motivates this
//! module. Its convention is a CONTENT marker rather than a filename one, so a
//! machine-written `payroll.go` sitting beside hand-written use-cases is
//! invisible to a path check — and renaming it is not an option, because
//! `payroll.go` is a legal, ordinary Go filename.
//!
//! The two signals stay deliberately separate:
//!
//! 1. the path check is pure, allocation-free, and safe to call inside a sort
//!    comparator;
//! 2. this module reads the head of a file's text, so it is evaluated ONCE per
//!    file at index time (the content is already in memory for parsing) and the
//!    verdict is persisted to `files.generated`. Readers take it from the
//!    database instead of re-reading headers per request, which is what keeps
//!    ranking a function of the index rather than of the filesystem's current
//!    state.
//!
//! The two are combined by [`detect_generated_file`], which is what the indexer
//! writes. Consumers UNION the stored flag with the path check rather than
//! trusting the column alone: a database written before this module existed has
//! `0` on every row, and reading it alone would silently drop the path demotion
//! those indexes already had.
//!
//! Both remain RELEVANCE HINTS, never filters, exactly as `file_class` says: a
//! machine-written file stays fully present in the graph and reachable in
//! results, and only ranks after a real implementation of the same name.
//!
//! # Three fences, all load-bearing
//!
//! Precision is the whole game here — a false positive silently demotes
//! hand-written code in every ranking path — so the marker must clear all
//! three:
//!
//! 1. **A bounded window.** [`HEADER_SCAN_CHARS`] AND [`HEADER_SCAN_LINES`],
//!    both, not either. Generous enough for a build-tag block plus an
//!    Apache-2.0 preamble above the marker, tight enough that the same text
//!    quoted in the BODY of a generator's own source does not qualify.
//! 2. **A cheap stem pre-filter.** Every marker contains the stem `generat`, so
//!    one unanchored case-insensitive scan rejects nearly every hand-written
//!    file before any line splitting. This exists for SPEED, not precision.
//! 3. **A comment line.** The marker must sit on a line with a comment leader,
//!    or inside an open block comment. Generators always emit theirs as a
//!    comment, and requiring it is what rules out string literals and
//!    identifiers that merely contain the words.
//!
//! # NOTE for future editors
//!
//! The marker literals in this file sit BELOW the line window this detector
//! scans, so the module does not classify its own source. A test pins that: if
//! you move the pattern table above [`HEADER_SCAN_LINES`], the test fails
//! rather than the repo silently demoting this file. The same hazard is live in
//! `file_class.rs`, whose own doc comment quotes the Go marker inside the
//! character window and is spared by the line bound alone; a second test pins
//! that too.

use crate::file_class::is_generated_file;

/// How much of a file's head counts as "the header", in bytes of the scanned
/// prefix. Paired with [`HEADER_SCAN_LINES`] — a marker must clear BOTH.
const HEADER_SCAN_CHARS: usize = 8192;

/// How many lines of the window are examined. This bound is what spares this
/// module's own pattern table, so it is load-bearing rather than decorative.
const HEADER_SCAN_LINES: usize = 60;

/// Line-comment leaders across the languages indexed here, matched after
/// leading whitespace and case-insensitively. `--` covers SQL/Haskell/Lua, `%`
/// LaTeX/Erlang/Prolog, `;` Lisp/asm/ini, `'` VB, `!` Fortran, and a bare `*` a
/// continuation line inside a `/* … */` block.
const COMMENT_LEADERS: &[&str] = &[
    "//", "/*", "*", "#", "--", "<!--", "%", ";", "'", "!", "(*", "{-", "\"\"\"", "'''", "=begin",
    "<#", "@rem", "rem",
];

/// Block-comment open/close pairs, so a marker on an unprefixed line INSIDE a
/// block still counts. Tested in order; the first opener on a line wins.
const BLOCK_DELIMS: &[(&str, &str)] = &[
    ("/*", "*/"),
    ("<!--", "-->"),
    ("\"\"\"", "\"\"\""),
    ("'''", "'''"),
    ("=begin", "=end"),
    ("<#", "#>"),
];

/// Whether the head of `content` carries a recognized machine-generation
/// banner.
///
/// Bounded to [`HEADER_SCAN_CHARS`] and [`HEADER_SCAN_LINES`], and the marker
/// must sit on a comment line, so a generator's own source — which holds the
/// same text as a string constant in its body — is not flagged.
pub fn has_generated_header(content: &str) -> bool {
    if content.is_empty() {
        return false;
    }

    let head = header_window(content);
    // Fast reject for nearly every hand-written file: every marker contains the
    // stem, so this runs before any line splitting. Speed, not precision.
    if !contains_ignore_ascii_case(head, "generat") {
        return false;
    }

    let mut open_block: Option<(&str, &str)> = None;
    for line in head.lines().take(HEADER_SCAN_LINES) {
        let lower = line.to_ascii_lowercase();

        let on_comment_line = open_block.is_some() || starts_with_comment_leader(&lower);
        if on_comment_line && matches_generated_banner(&lower) {
            return true;
        }

        // Advance the block state AFTER testing, so the opening line of a
        // `/* Code generated … */` block is itself matched by the leader rule.
        if let Some((_, close)) = open_block {
            if lower.contains(close) {
                open_block = None;
            }
            continue;
        }
        for &(open, close) in BLOCK_DELIMS {
            let Some(at) = lower.find(open) else { continue };
            // A same-line close (`/* … */`, a one-line docstring) leaves no open
            // block, so a later bare code line is not read as commented out.
            if !lower[at + open.len()..].contains(close) {
                open_block = Some((open, close));
            }
            break;
        }
    }

    false
}

/// The leading [`HEADER_SCAN_CHARS`] bytes, truncated at a char boundary.
///
/// Slicing on the raw byte index would panic mid-codepoint, which a multibyte
/// file reaches routinely; `floor_char_boundary` is still unstable, so the
/// boundary is walked down by hand.
fn header_window(content: &str) -> &str {
    if content.len() <= HEADER_SCAN_CHARS {
        return content;
    }
    let mut end = HEADER_SCAN_CHARS;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// Whether `haystack` holds `needle` (already lowercase) anywhere, comparing
/// ASCII-case-insensitively without allocating.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return n.is_empty();
    }
    h.windows(n.len())
        .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// Whether `lower` — a lowercased line — opens with a comment leader.
fn starts_with_comment_leader(lower: &str) -> bool {
    let trimmed = lower.trim_start();
    COMMENT_LEADERS
        .iter()
        .any(|leader| trimmed.starts_with(leader))
}

/// Whether a lowercased comment line matches one of the eight banner shapes.
///
/// The workspace deliberately carries no regex dependency (the sibling path
/// check is hand-written too), so each upstream pattern is expressed with
/// `find` plus a bounded-gap helper. Every gap bound is upstream's.
fn matches_generated_banner(lower: &str) -> bool {
    // 1. Go's codified convention, honored by gofmt/linguist and emitted by
    //    protoc-gen-go, mockgen, sqlc, ent, wire, stringer.
    if phrase_then(lower, "code generated", &["do not edit"], 200) {
        return true;
    }

    // 2. protoc's Java/C#/Python banner, ANTLR, Dagger, FlatBuffers,
    //    rust-bindgen, Xcode asset catalogs, Bazel.
    for lead in ["generated by", "generated from", "generated with"] {
        if phrase_then(
            lower,
            lead,
            &["do not edit", "do not modify", "do not change"],
            200,
        ) {
            return true;
        }
    }

    // 3. The `@generated` tag, guarded so `foo@generated` and `@@generated` do
    //    not qualify: the preceding byte must not be alphanumeric, `_`, or `@`.
    if let Some(rest) = find_standalone_at_generated(lower) {
        return rest;
    }

    // 4. .NET's `<auto-generated>` / `<autogenerated />` doc tag.
    for open in ["<auto-generated", "<autogenerated"] {
        if let Some(at) = lower.find(open) {
            let tail = lower[at + open.len()..].trim_start();
            if tail.starts_with('>') || tail.starts_with("/>") {
                return true;
            }
        }
    }

    // 5. swagger-codegen / OpenAPI Generator, Thrift, FlatBuffers. The trailing
    //    `by` is REQUIRED — bare "automatically generated" is ordinary prose.
    for form in [
        "automatically generated by",
        "auto-generated by",
        "auto generated by",
        "autogenerated by",
    ] {
        if lower.contains(form) {
            return true;
        }
    }

    // 6. CG-25 — the "run this to regenerate" shape (Cloudflare Wrangler).
    //    TWO separate `by` clauses is the discriminator: the banner must name a
    //    tool AND then say `by running`, so "generated by running the nightly
    //    job" (one clause) is correctly rejected.
    if let Some(at) = lower.find("generated by") {
        let rest = &lower[at + "generated by".len()..];
        let named_tool = rest.trim_start();
        if named_tool.len() < rest.len()
            && !named_tool.is_empty()
            && within(named_tool, "by running", 80)
        {
            return true;
        }
    }

    // 7. Self-declaring in-house banners that name no tool.
    for subject in ["this file ", "this class ", "this code ", "this module "] {
        if let Some(at) = lower.find(subject) {
            let rest = &lower[at + subject.len()..];
            for verb in ["is ", "was "] {
                if let Some(tail) = rest.strip_prefix(verb) {
                    for form in [
                        "generated",
                        "auto-generated",
                        "auto generated",
                        "autogenerated",
                    ] {
                        if tail.starts_with(form) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // 8. The reverse ordering: "DO NOT EDIT — this is a generated file".
    for lead in ["do not edit", "do not modify"] {
        if phrase_then(
            lower,
            lead,
            &[
                "auto-generated",
                "auto generated",
                "autogenerated",
                "generated file",
                "generated code",
            ],
            120,
        ) {
            return true;
        }
    }

    false
}

/// Whether `lead` occurs and any of `tails` follows it within `gap` bytes.
fn phrase_then(lower: &str, lead: &str, tails: &[&str], gap: usize) -> bool {
    let mut from = 0;
    while let Some(rel) = lower[from..].find(lead) {
        let after = from + rel + lead.len();
        if tails.iter().any(|t| within(&lower[after..], t, gap)) {
            return true;
        }
        from = after;
    }
    false
}

/// Whether `needle` starts within the first `gap` bytes of `rest`.
fn within(rest: &str, needle: &str, gap: usize) -> bool {
    let mut end = rest.len().min(gap + needle.len());
    while end > 0 && !rest.is_char_boundary(end) {
        end -= 1;
    }
    rest[..end]
        .find(needle)
        .is_some_and(|at| at <= gap.min(rest.len()))
}

/// Whether the line carries a STANDALONE `@generated` tag.
///
/// Returns `Some(true)` on a match. The byte before the `@` must not be a
/// letter, digit, `_`, or another `@`, which is what rejects `foo@generated`
/// (an address-shaped token) and `@@generated`.
fn find_standalone_at_generated(lower: &str) -> Option<bool> {
    let tag = "@generated";
    let mut from = 0;
    while let Some(rel) = lower[from..].find(tag) {
        let at = from + rel;
        let prev_ok = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '@'));
        let next_ok = lower[at + tag.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if prev_ok && next_ok {
            return Some(true);
        }
        from = at + tag.len();
    }
    None
}

/// The union signal — path convention OR content banner — and the value the
/// indexer persists to `files.generated`.
///
/// The content half only ever ADDS: a file the path check already flags stays
/// flagged whatever its header says, so this can never REMOVE a demotion an
/// existing index applied.
pub fn detect_generated_file(path: &str, content: &str) -> bool {
    is_generated_file(path) || has_generated_header(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pattern 1: Go's codified convention (the #1500 defect itself) -----

    #[test]
    fn go_banner_in_header_is_detected() {
        assert!(has_generated_header(
            "// Code generated by fkit-crud. DO NOT EDIT.\n\npackage payroll\n"
        ));
    }

    // ---- patterns 2-5, 7, 8 ------------------------------------------------

    #[test]
    fn protoc_generated_by_do_not_edit_banner_is_detected() {
        assert!(has_generated_header(
            "// Generated by the protocol buffer compiler.  DO NOT EDIT!\n// source: a.proto\n"
        ));
        // "from" and "with", and the DO NOT MODIFY / CHANGE spellings.
        assert!(has_generated_header(
            "# Generated from Foo.g4 - DO NOT EDIT\n"
        ));
        assert!(has_generated_header(
            "/* generated with rust-bindgen; do not modify */\n"
        ));
        assert!(has_generated_header(
            "-- automatically generated by sqlc. do not change.\n"
        ));
    }

    #[test]
    fn standalone_at_generated_tag_is_detected() {
        assert!(has_generated_header("// @generated\n"));
        assert!(has_generated_header(
            "/* @generated SignedSource<<0123456789abcdef>> */\n"
        ));
        // Start-of-line with no leader is still a comment line via `#`.
        assert!(has_generated_header("# @generated by protobuf-es\n"));
    }

    #[test]
    fn dotnet_auto_generated_doc_tag_is_detected() {
        assert!(has_generated_header("// <auto-generated />\n"));
        assert!(has_generated_header("// <autogenerated>\n"));
        assert!(has_generated_header(
            "//------------------------------------------------------------------------------\n\
             // <auto-generated>\n\
             //     This code was generated by a tool.\n\
             // </auto-generated>\n"
        ));
    }

    #[test]
    fn openapi_and_thrift_autogenerated_by_banner_is_detected() {
        assert!(has_generated_header(
            "// NOTE: This class is auto generated by OpenAPI Generator.\n"
        ));
        assert!(has_generated_header(
            "// Autogenerated by Thrift Compiler (0.14.1)\n"
        ));
        assert!(has_generated_header(
            "// automatically generated by the FlatBuffers compiler, do not modify\n"
        ));
    }

    #[test]
    fn bare_automatically_generated_prose_is_not_a_banner() {
        // Pattern 5 REQUIRES a following "by": without it the phrase is ordinary
        // prose, and flagging it would demote hand-written code.
        assert!(!has_generated_header(
            "// The table below is automatically generated at runtime.\n"
        ));
    }

    #[test]
    fn self_declaring_in_house_banner_is_detected() {
        assert!(has_generated_header("// This file is auto-generated.\n"));
        assert!(has_generated_header(
            "# This module was generated -- edits will be lost\n"
        ));
        assert!(has_generated_header("// This class is autogenerated\n"));
    }

    #[test]
    fn reverse_ordered_do_not_edit_banner_is_detected() {
        assert!(has_generated_header(
            "// DO NOT EDIT -- this is a generated file.\n"
        ));
        assert!(has_generated_header(
            "# do not modify: generated code, regenerate instead\n"
        ));
    }

    // ---- pattern 6 / CG-25: the Wrangler double-`by` discriminator ---------

    #[test]
    fn wrangler_double_by_banner_is_detected() {
        assert!(has_generated_header(
            "// Generated by Wrangler by running `wrangler types` (hash: abc123)\n"
        ));
    }

    #[test]
    fn single_by_prose_is_not_a_banner() {
        // ONE "by" clause. Bare "generated by" is deliberately not enough, or
        // every changelog line describing a nightly job would be a banner.
        assert!(!has_generated_header(
            "// the report is generated by running the nightly job\n"
        ));
        assert!(!has_generated_header(
            "// This chart is generated by the dashboard service.\n"
        ));
    }

    // ---- the three fences -------------------------------------------------

    #[test]
    fn banner_below_the_line_bound_is_not_detected() {
        // 60 filler lines, so the banner lands on line 61 — one past the bound.
        let mut content = String::new();
        for _ in 0..HEADER_SCAN_LINES {
            content.push_str("// padding\n");
        }
        content.push_str("// Code generated by protoc-gen-go. DO NOT EDIT.\n");
        assert!(!has_generated_header(&content));

        // Moving it one line UP makes it the 60th line and it IS found, so the
        // test pins the boundary rather than merely "deep banners are missed".
        let mut inside = String::new();
        for _ in 0..(HEADER_SCAN_LINES - 1) {
            inside.push_str("// padding\n");
        }
        inside.push_str("// Code generated by protoc-gen-go. DO NOT EDIT.\n");
        assert!(has_generated_header(&inside));
    }

    #[test]
    fn banner_beyond_the_char_window_is_not_detected() {
        // ONE long comment line pushes the second line past the byte window, so
        // this isolates the CHAR fence: the line count is 2, far under the line
        // bound, so only the window can reject it.
        let mut content = String::from("// ");
        content.push_str(&"a".repeat(HEADER_SCAN_CHARS + 8));
        content.push('\n');
        content.push_str("// Code generated by protoc-gen-go. DO NOT EDIT.\n");
        assert!(content.lines().count() < HEADER_SCAN_LINES);
        assert!(!has_generated_header(&content));
    }

    #[test]
    fn banner_off_a_comment_line_is_not_detected() {
        // A generator's OWN source holds the banner as a string constant in its
        // body. Flagging that would demote the generator itself.
        assert!(!has_generated_header(
            "package gen\n\nconst Banner = \"Code generated by fkit-crud. DO NOT EDIT.\"\n"
        ));
        assert!(!has_generated_header(
            "banner = 'Code generated by tool. DO NOT EDIT.'\n"
        ));
    }

    #[test]
    fn banner_inside_an_open_block_comment_is_detected() {
        // Line 2 carries NO comment leader of its own — only the open `/*`
        // state makes it count.
        assert!(has_generated_header(
            "/*\nCode generated by protoc-gen-go. DO NOT EDIT.\n*/\npackage p\n"
        ));
        assert!(has_generated_header(
            "<!--\nThis file is auto-generated.\n-->\n"
        ));
        assert!(has_generated_header(
            "\"\"\"\nGenerated by protoc. DO NOT EDIT.\n\"\"\"\n"
        ));
    }

    #[test]
    fn a_closed_block_comment_does_not_leak_its_state() {
        // A same-line close leaves NO open block, so a later bare code line
        // holding the words must not be read as still-inside a comment.
        assert!(!has_generated_header(
            "/* header */\nbanner := \"Code generated by tool. DO NOT EDIT.\"\n"
        ));
    }

    #[test]
    fn at_generated_requires_a_standalone_tag() {
        // Guarded against an e-mail-ish `foo@generated` and a doubled `@@`.
        assert!(!has_generated_header("// foo@generated\n"));
        assert!(!has_generated_header("// @@generated\n"));
        // The standalone form still matches, so the guard is not over-broad.
        assert!(has_generated_header("// @generated\n"));
    }

    #[test]
    fn multibyte_content_does_not_panic_at_the_window_boundary() {
        // A file whose 8192nd BYTE lands INSIDE a 3-byte codepoint, so a naive
        // `&content[..HEADER_SCAN_CHARS]` panics instead of returning a verdict.
        // The `is_char_boundary` assertions pin that precondition, so the test
        // cannot silently degrade into scanning ordinary ASCII.
        let filler = "€".repeat(4000);
        assert!(!filler.is_char_boundary(HEADER_SCAN_CHARS));
        assert!(!has_generated_header(&filler));

        // Same hazard WITH a real banner on line 1: the verdict must still be
        // reachable, so the fix cannot be "give up on multibyte input".
        let banner = format!("// Code generated by protoc-gen-go. DO NOT EDIT.\n{filler}");
        assert!(!banner.is_char_boundary(HEADER_SCAN_CHARS));
        assert!(has_generated_header(&banner));
    }

    #[test]
    fn empty_and_stemless_content_is_rejected_cheaply() {
        assert!(!has_generated_header(""));
        assert!(!has_generated_header("package main\n\nfunc main() {}\n"));
    }

    // ---- self-classification guards (D4) ----------------------------------

    #[test]
    fn this_module_does_not_classify_itself() {
        // This file quotes all eight banner shapes. They sit below the line
        // bound, so the detector must not flag its own source. Moving the
        // pattern table above HEADER_SCAN_LINES fails HERE rather than silently
        // demoting this module in every ranking path.
        let own_source = include_str!("generated_header.rs");
        assert!(
            !has_generated_header(own_source),
            "generated_header.rs classified ITSELF as generated: the pattern \
             table has moved inside the first {HEADER_SCAN_LINES} lines"
        );
        // The guard is the LINE bound, so prove the banner text really is
        // present and inside the char window — otherwise this test could pass
        // for the trivial reason that nothing matches anywhere.
        let window = header_window(own_source);
        assert!(contains_ignore_ascii_case(window, "generat"));

        // MEASURED, and thinner than it looks: the first banner LITERAL sits at
        // byte ~8089 of this file, i.e. ~100 bytes inside the 8192 window, so
        // widening HEADER_SCAN_LINES alone does NOT trip the assertion above —
        // the char window clips the literals instead. Both fences must stay.
        // Pin the line distance so growth above the table fails here rather
        // than quietly consuming the cushion.
        let first_literal = own_source
            .find("\"code generated\"")
            .expect("the pattern table must still hold the Go marker literal");
        let literal_line = own_source[..first_literal].lines().count();
        assert!(
            literal_line > HEADER_SCAN_LINES,
            "the first banner literal moved to line {literal_line}, inside the \
             {HEADER_SCAN_LINES}-line bound"
        );
    }

    #[test]
    fn file_class_rs_is_not_classified_by_its_own_doc_comment() {
        // file_class.rs's doc comment quotes Go's marker on a `///` line, INSIDE
        // the char window (measured: byte offset ~5519, line ~136). Only the
        // line bound spares it, which is what makes that bound load-bearing.
        let sibling = include_str!("file_class.rs");
        assert!(
            !has_generated_header(sibling),
            "file_class.rs classified as generated by its own doc comment"
        );
        let window = header_window(sibling);
        assert!(contains_ignore_ascii_case(window, "generat"));
    }

    #[test]
    fn no_first_party_source_in_this_crate_is_classified() {
        // A Rust `///` doc comment IS a comment line, so any of this crate's
        // files could trip the detector the same way. Sweep the whole crate
        // rather than pinning the two files already known to be at risk.
        for (name, source) in [
            ("lib.rs", include_str!("lib.rs")),
            ("types.rs", include_str!("types.rs")),
            ("traits.rs", include_str!("traits.rs")),
            ("node_id.rs", include_str!("node_id.rs")),
            ("config.rs", include_str!("config.rs")),
            ("errors.rs", include_str!("errors.rs")),
            ("index_paths.rs", include_str!("index_paths.rs")),
            ("logger.rs", include_str!("logger.rs")),
        ] {
            assert!(
                !has_generated_header(source),
                "{name} was classified as generated"
            );
        }
    }

    // ---- the union written to files.generated -----------------------------

    #[test]
    fn detect_generated_file_unions_path_and_content() {
        // Path signal alone: a `.pb.go` with NO banner is still generated.
        assert!(detect_generated_file("api.pb.go", "package api\n"));
        // Content signal alone: an ordinary Go filename WITH a banner.
        assert!(detect_generated_file(
            "payroll.go",
            "// Code generated by fkit-crud. DO NOT EDIT.\npackage payroll\n"
        ));
        // Both.
        assert!(detect_generated_file(
            "api.pb.go",
            "// Code generated by protoc-gen-go. DO NOT EDIT.\n"
        ));
        // Neither — a hand-written file must never be flagged.
        assert!(!detect_generated_file(
            "payroll_usecase.go",
            "package payroll\n\nfunc ComputePay() {}\n"
        ));
    }

    #[test]
    fn the_content_half_never_removes_the_path_signal() {
        // D1's direction: content can only ADD. A path-flagged file stays
        // flagged even when its content is plainly hand-written, so the union
        // cannot re-rank an existing index in the wrong direction.
        let handwritten = "package api\n\nfunc Real() {}\n";
        assert!(!has_generated_header(handwritten));
        assert!(detect_generated_file("api.pb.go", handwritten));
    }
}
