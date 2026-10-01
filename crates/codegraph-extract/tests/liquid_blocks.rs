//! Tags inside a `{% liquid %}` block (upstream v1.6.1 #1906): each body line is
//! a tag without braces of its own. Ported from upstream `extraction.test.ts`.

use codegraph_core::types::{ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

fn extract(file: &str, lines: &[&str]) -> ExtractionResult {
    extract_with(file, lines, "\n")
}

fn extract_with(file: &str, lines: &[&str], newline: &str) -> ExtractionResult {
    let result = extract_source(file, &lines.join(newline), Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{file}: {:?}", result.errors);
    result
}

fn names_of(result: &ExtractionResult, kind: NodeKind) -> Vec<&str> {
    result
        .nodes
        .iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.name.as_str())
        .collect()
}

fn reference_names(result: &ExtractionResult) -> Vec<&str> {
    result
        .unresolved_references
        .iter()
        .map(|reference| reference.reference_name.as_str())
        .collect()
}

#[test]
fn render_assign_and_section_inside_a_liquid_block_are_extracted() {
    let result = extract(
        "sections/featured.liquid",
        &[
            "{% liquid",
            "  assign heading = section.settings.title",
            "  render 'card', title: heading",
            "%}",
        ],
    );
    assert!(names_of(&result, NodeKind::Import).contains(&"card"));
    assert!(names_of(&result, NodeKind::Variable).contains(&"heading"));

    let result = extract(
        "layout/theme.liquid",
        &["{% liquid", "  section 'header'", "%}"],
    );
    assert!(names_of(&result, NodeKind::Import).contains(&"header"));
}

#[test]
fn a_tag_inside_a_liquid_block_reports_its_real_line() {
    let result = extract(
        "sections/featured.liquid",
        &[
            "<div>",
            "{% liquid",
            "  assign x = 1",
            "  render 'card'",
            "%}",
        ],
    );
    let card = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Import && node.name == "card")
        .expect("card import");
    assert_eq!(card.start_line, 4);
}

#[test]
fn both_spellings_are_counted_once_each() {
    let result = extract(
        "sections/featured.liquid",
        &["{% render 'card' %}", "{% liquid", "  render 'card'", "%}"],
    );
    let lines = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Import && node.name == "card")
        .map(|node| node.start_line)
        .collect::<Vec<_>>();
    assert_eq!(lines, vec![1, 3]);
}

#[test]
fn a_bare_render_outside_a_liquid_block_is_prose() {
    let result = extract(
        "sections/featured.liquid",
        &[
            "<p>We render the card below.</p>",
            "{{ product | render_as: 'card' }}",
        ],
    );
    assert!(!names_of(&result, NodeKind::Import).contains(&"card"));
}

#[test]
fn whitespace_control_on_the_liquid_tag_is_accepted() {
    let result = extract(
        "sections/featured.liquid",
        &["{%- liquid", "  render 'card'", "-%}"],
    );
    assert!(names_of(&result, NodeKind::Import).contains(&"card"));
}

#[test]
fn tag_like_strings_and_inline_comments_are_not_scanned_twice() {
    let result = extract(
        "sections/featured.liquid",
        &[
            "{% assign example = \"{% render 'ghost'\" %}",
            "{% # {% render 'ghost' %}",
            "{% liquid",
            "  assign example = \"{% include 'ghost'\"",
            "  # section 'ghost'",
            "  echo 'render ghost'",
            "%}",
            "{% liquid render 'live' %}",
            "{% liquid include 'after' %}",
        ],
    );
    let imports = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Import)
        .map(|node| (node.name.as_str(), node.start_line, node.start_column))
        .collect::<Vec<_>>();
    assert_eq!(imports, vec![("live", 8, 10), ("after", 9, 10)]);
    assert_eq!(
        reference_names(&result),
        vec!["snippets/live.liquid", "snippets/after.liquid"]
    );
}

#[test]
fn liquid_block_positions_survive_both_line_endings() {
    for newline in ["\n", "\r\n"] {
        let result = extract_with(
            "sections/featured.liquid",
            &[
                "<div>",
                "{%- liquid",
                "  assign heading = 'x'",
                "\tinclude 'legacy'",
                "  render 'card'",
                "  section 'footer'",
                "-%}",
                "  {% render 'after' %}",
            ],
            newline,
        );
        let mut nodes = result
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Variable | NodeKind::Import))
            .map(|node| {
                (
                    node.name.as_str(),
                    node.start_line,
                    node.start_column,
                    node.end_column,
                )
            })
            .collect::<Vec<_>>();
        nodes.sort();
        assert_eq!(
            nodes,
            vec![
                ("after", 8, 2, 19),
                ("card", 5, 2, 15),
                ("footer", 6, 2, 18),
                ("heading", 3, 2, 18),
                ("legacy", 4, 1, 17),
            ],
            "{newline:?}"
        );
        let mut references = result
            .unresolved_references
            .iter()
            .map(|reference| {
                (
                    reference.reference_name.as_str(),
                    reference.line,
                    reference.col,
                )
            })
            .collect::<Vec<_>>();
        references.sort();
        assert_eq!(
            references,
            vec![
                ("sections/footer.liquid", 6, 2),
                ("snippets/after.liquid", 8, 2),
                ("snippets/card.liquid", 5, 2),
                ("snippets/legacy.liquid", 4, 1),
            ],
            "{newline:?}"
        );
    }
}

#[test]
fn comment_and_raw_regions_are_ignored_in_both_spellings() {
    for tag in ["comment", "raw"] {
        let open = format!("{{%- {tag} -%}}");
        let close = format!("{{%- end{tag} -%}}");
        let bare_open = format!("  {tag}");
        let bare_close = format!("  end{tag}");
        let result = extract(
            "sections/featured.liquid",
            &[
                &open,
                "{% render 'ghost' %}",
                "{% include 'ghost' %}",
                "{% section 'ghost' %}",
                "{% assign ghost = 1 %}",
                "{% liquid",
                "  render 'ghost'",
                "%}",
                &close,
                "{% liquid",
                &bare_open,
                "  render 'ghost'",
                "  include 'ghost'",
                "  section 'ghost'",
                "  assign ghost = 1",
                &bare_close,
                "  # render 'ghost'",
                "  render 'live'",
                "%}",
                "{% include 'after' %}",
            ],
        );
        let names = result
            .nodes
            .iter()
            .filter(|node| node.kind != NodeKind::File)
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["live", "live", "after", "after"], "{tag}");
        let references = result
            .unresolved_references
            .iter()
            .map(|reference| (reference.reference_name.as_str(), reference.line))
            .collect::<Vec<_>>();
        assert_eq!(
            references,
            vec![("snippets/live.liquid", 18), ("snippets/after.liquid", 20)],
            "{tag}"
        );
    }
}
