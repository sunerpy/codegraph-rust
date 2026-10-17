//! `.inc` is PHP's include extension (Drupal) and Pascal's too (`{$I
//! defs.inc}`: directive blocks, declaration fragments), so a file of it is
//! read by its content (upstream #2279, in the shape at `b635dd46`).
//!
//! A PHP include always opens a PHP tag somewhere, so a tag anywhere keeps
//! the file PHP. Without one, a line shape only Pascal writes (a compiler
//! directive, a `;`-closed routine header, `unit` / `uses`, a `const` /
//! `var` / `type` section, a `begin` … `end;` pair, Lazarus' `{%MainUnit}`)
//! makes it Pascal. Anything else keeps the PHP mapping, under which untagged
//! text is inline HTML with nothing to extract, rather than handing a C,
//! assembly, POV-Ray, SourcePawn or ASP include to the Pascal grammar. The
//! answer depends on the file's own bytes only, so a full index, a sync of
//! one edited include and a fresh index always agree.
//!
//! The shapes are upstream's patterns, read as JavaScript reads them: `\w`
//! is ASCII, and the negative lookahead after a `var` declaration's colon
//! (`:(?!:)`) is written as the class it stands for.

use std::sync::OnceLock;

use regex::{Regex, RegexSet};

/// A line start: indentation and the BOM Windows editors write.
const LINE: &str = r"^[ \t\x{FEFF}]*";
/// A name, with type arguments.
const NAME: &str = r"[a-z_][0-9A-Za-z_]*(?:<[^>\n]*>)?";
/// A dotted name, with type arguments.
const QNAME: &str = r"[a-z_][0-9A-Za-z_.]*(?:<[^>\n]*>)?";
/// A comment closing a line: `// …` or `{ … }`, never a `{$directive}`.
const COMMENT: &str = r"(?://[^\n]*|\{[^$}\n][^}\n]*\}[ \t]*)?";

/// A PHP open tag: `<?php` in any case, or the short echo `<?=`.
fn php_open_tag() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<\?(?:php|=)").expect("PHP open tag regex"))
}

/// The line shapes only Pascal writes, any one of which makes an untagged
/// `.inc` Pascal (upstream `PASCAL_INCLUDE_SIGNALS`).
fn pascal_signals() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        // A section keyword's break runs to the end of its line and over
        // blank or comment-only lines, up to the first declaration.
        let section_break = format!(r"[ \t]*{COMMENT}(?:\r?\n[ \t]*{COMMENT})+");
        let shapes = [
            // A compiler directive: a name and an argument (`{$IFDEF X}`,
            // `{$I file.inc}`), a bare `{$ELSE}` / `{$ENDIF}` / `{$IFEND}`, a
            // switch (`{$R-}`, `{$A+,B-}`), or Lazarus' `{%MainUnit x.pp}`.
            format!(
                r"{LINE}\{{(?:\$(?:[a-z][0-9A-Za-z_]*[ \t]+[^\s}}]|(?:else|endif|ifend)[ \t]*\}}|[a-z][+-][,}}])|%MainUnit(?-u:\b))"
            ),
            // A routine header closed by `;`, a function's result type after
            // its parameters (bare `function Bar;` is the implementation
            // section's short form).
            format!(
                r"{LINE}(?:class[ \t]+)?(?:(?:procedure|constructor|destructor)[ \t]+{QNAME}[ \t]*(?:\([^()]*\)[ \t]*)?|function[ \t]+{QNAME}[ \t]*(?:(?:\([^()]*\)[ \t]*)?:[ \t]*[0-9A-Za-z_.]+(?:<[^>\n]*>)?[ \t]*)?);"
            ),
            // `unit Foo;` and a `uses A, B;` clause.
            format!(r"{LINE}unit[ \t]+[a-z_][0-9A-Za-z_.]*[ \t]*;"),
            format!(
                r"{LINE}uses\s+[a-z_][0-9A-Za-z_.]*(?:\s*,\s*[a-z_][0-9A-Za-z_.]*)*\s*;"
            ),
            // A `const` section, then `X = …` / `X: T = …`; or a typed
            // constant on one line. The type never holds a `:`.
            format!(
                r"{LINE}(?:const|resourcestring)(?:{section_break}[a-z_][0-9A-Za-z_]*[ \t]*(?::[^=;:\n]+)?=|[ \t]+[a-z_][0-9A-Za-z_]*[ \t]*:[^=;:\n]+=)"
            ),
            // A `var` section: `G, H: Integer;`, on the keyword's line or
            // below it; the colon is no `::`.
            format!(
                r"{LINE}(?:var|threadvar)(?:{section_break}|[ \t]+)[a-z_][0-9A-Za-z_]*(?:[ \t]*,[ \t]*[a-z_][0-9A-Za-z_]*)*[ \t]*:(?:[^:;\n][^;\n]*)?;"
            ),
            // A `type` section, then `TFoo =`; or `type TFoo = class…`
            // (record, interface, set of, array, procedure type) on one line.
            format!(
                r"{LINE}type(?:{section_break}{NAME}[ \t]*=|[ \t]+{NAME}[ \t]*=[ \t]*(?:packed[ \t]+)?(?:class|record|object|interface|dispinterface|set[ \t]+of|array|reference[ \t]+to|procedure|function)(?-u:\b))"
            ),
        ];
        RegexSet::new(shapes.iter().map(|shape| format!("(?im){shape}")))
            .expect("Pascal include signal regexes")
    })
}

/// A `begin` … `end;` block: both halves needed, so neither alone flips a
/// file.
fn begin_end() -> &'static (Regex, Regex) {
    static PAIR: OnceLock<(Regex, Regex)> = OnceLock::new();
    PAIR.get_or_init(|| {
        (
            Regex::new(&format!(r"(?im){LINE}begin(?-u:\b)")).expect("Pascal begin regex"),
            Regex::new(&format!(r"(?im){LINE}end[ \t]*[;.][ \t\r]*$")).expect("Pascal end regex"),
        )
    })
}

/// Whether an `.inc` file's `source` is a Pascal include rather than a PHP
/// one (upstream `looksLikePascalInclude`).
pub(crate) fn looks_like_pascal_include(source: &str) -> bool {
    if php_open_tag().is_match(source) {
        return false;
    }
    if pascal_signals().is_match(source) {
        return true;
    }
    let (begin, end) = begin_end();
    begin.is_match(source) && end.is_match(source)
}

#[cfg(test)]
mod tests {
    use super::looks_like_pascal_include as pascal;

    const DIRECTIVES: &str = "{ Compiler version switches }\n{$IFDEF VER150}\n  {$DEFINE DELPHI7}\n{$ENDIF}\n{$IFNDEF FPC}\n  {$WARN SYMBOL_PLATFORM OFF}\n{$ELSE}\n  {$MODE DELPHI}{$H+}\n{$ENDIF}\n";
    const ROUTINES: &str = "procedure Foo;\nbegin\n  WriteLn('foo');\nend;\n\nfunction Bar(A: Integer): Integer;\nbegin\n  Result := A + 1;\nend;\n";

    /// Upstream `pascal-inc-detection.test.ts`: `reads a directive-only
    /// include as Pascal`, `skips a leading UTF-8 BOM and CRLF line endings`.
    #[test]
    fn pr2289_directives_bom_and_crlf_read_as_pascal() {
        assert!(pascal(DIRECTIVES));
        assert!(pascal("{$I other.inc}\n"));
        assert!(pascal("{$R-}\n{$Q-}\n"));
        assert!(pascal("{$define FOO}\n"));
        assert!(pascal("{%MainUnit ../controls.pp}\n{ comment }\n"));
        assert!(pascal(
            "\u{FEFF}{$IFDEF WIN32}\r\n{$DEFINE X}\r\n{$ENDIF}\r\n"
        ));
        assert!(pascal("\u{FEFF}procedure Foo;\r\n"));
    }

    /// Upstream: `reads a declaration fragment as Pascal`.
    #[test]
    fn pr2289_declaration_fragments_read_as_pascal() {
        for source in [
            ROUTINES,
            "procedure Foo;\nfunction Bar(A: Integer): Integer;\n",
            "procedure TForm1.Button1Click(Sender: TObject);\n",
            "const\n  X = 1;\n",
            "const // ids\n{ Internal functions }\n   fpc_in_lo = 1;\n",
            "const Max: Integer = 10;\n",
            "type\n  TColor = (Red, Green);\n",
            "type TFoo = class(TObject)\nend;\n",
            "var\n  G, H: Integer;\n",
            "uses\n  SysUtils, Classes;\n",
            "unit Foo;\ninterface\nimplementation\nend.\n",
        ] {
            assert!(pascal(source), "{source:?}");
        }
    }

    /// Upstream: `keeps an include with a PHP open tag as PHP`.
    #[test]
    fn pr2289_a_php_open_tag_keeps_php() {
        for source in [
            "<?php\nfunction mymodule_help() {}\n",
            "\u{FEFF}  <?PHP\nfunction a() {}\n",
            "<div><?= $title ?></div>\n",
            "<p>\nprocedure Foo;\n</p>\n<?php echo 1; ?>\n",
        ] {
            assert!(!pascal(source), "{source:?}");
        }
    }

    /// Upstream: `leaves non-Pascal, untagged includes on the PHP mapping`.
    #[test]
    fn pr2289_other_includes_stay_off_pascal() {
        for source in [
            "#define X 1\n#define Y 2\nconst int z = 3;\n",
            "OPCODE(ADD, 0x01)\nOPCODE(SUB, 0x02)\n",
            "const file_t::value_type file_t::Invalid = nullptr;\n",
            "typeset MenuHandler\n{\n\tfunction int (Menu menu, MenuAction action);\n\tfunction void (Menu menu);\n};\n",
            "%define STACK 0x1000\n; comment\nfoo equ 1\nmov eax, 1\n",
            "#declare R = 1;\n#macro M(a)\n#end\n",
            "Const X = 1\nFunction Foo(a)\n  Foo = a\nEnd Function\n",
            "CC := gcc\nCFLAGS := -O2\n",
            "<script>\n  const x = 1;\n  var y = 2;\n  function go() { return x; }\n</script>\n{$title}\n",
            "<li>\n  Implementation\n</li>\n<li>\n  Begin\n</li>\n",
            "",
        ] {
            assert!(!pascal(source), "{source:?}");
        }
    }

    /// The `var` shape's colon is no `::` (upstream's `(?!:)`), and a
    /// `begin` needs its `end;`, a CRLF one included.
    #[test]
    fn keep_rust_the_lookahead_and_line_ends_read_as_upstream() {
        assert!(pascal("var G:;\n"));
        assert!(!pascal("var G::T;\n"));
        assert!(pascal("begin\r\n  X := 1;\r\nend;\r\n"));
        assert!(!pascal("begin\n  X := 1;\n"));
    }
}
