//! Shared file-classification predicates.
//!
//! Both the CLI (`affected`) and the MCP engine (blast radius,
//! `excludeLowValueFiles`, symbol disambiguation, `explore` ranking) need to ask
//! "is this a test file?" and "does this path look tool-generated?". They used
//! to answer with two hand-maintained pattern lists that drifted apart in BOTH
//! directions (upstream #1507), so a Go `_test.go` file read as production code
//! on the CLI path while an `e2e/` tree read as production code on the MCP path.
//! This module is the single source of truth for both questions.
//!
//! Every predicate here is a pure, allocation-free function of the path string.
//! Both are RELEVANCE HINTS, never filters: a test or generated file stays fully
//! present in the graph and reachable in results — it only ranks after a real
//! implementation of the same name, or is skipped when a query has better
//! signal to offer.
//!
//! `codegraph-graph`'s `query::scoring::is_test_file` deliberately stays
//! separate. It answers a BROADER question for FTS relevance damping (it also
//! treats `examples/`, `demos/`, `benchmarks/`, camelCase `FooTests/` dirs and
//! `TestCase`-suffixed stems as non-production), so folding it in here would
//! change search ranking rather than converge a duplicate.

/// Path segments that mark a test tree. Matched as whole `/`-delimited
/// segments, so a repo-root `e2e/` counts and a `route2e2e.ts` filename
/// does not.
const TEST_DIR_SEGMENTS: &[&str] = &["test", "tests", "__tests__", "spec", "e2e"];

/// Filename markers that mark a test file. `_test.` is Go's convention
/// (`math_test.go`); `.test.` / `.spec.` are the JS/TS ones.
const TEST_FILE_MARKERS: &[&str] = &[".test.", ".spec.", "_test."];

/// Whether `path` names a test file, by directory segment or filename marker.
///
/// The union of the two predicates this replaces. Callers that also honor an
/// explicit user-supplied glob (the CLI's `affected --filter`) must apply that
/// glob FIRST and skip this heuristic entirely.
pub fn is_test_file(path: &str) -> bool {
    TEST_FILE_MARKERS.iter().any(|m| path.contains(m))
        || TEST_DIR_SEGMENTS
            .iter()
            .any(|seg| contains_path_segment(path, seg))
}

/// Whether `segment` appears in `path` as a complete `/`-delimited segment.
///
/// Scans forward over every occurrence rather than allocating `"/{segment}/"`,
/// because this runs per-file inside sort comparators. Offsets are tracked
/// against the ORIGINAL path so a rejected occurrence cannot make the next one
/// look segment-initial (`xe2e/a.ts` must not match `e2e`).
fn contains_path_segment(path: &str, segment: &str) -> bool {
    let bytes = path.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = path[from..].find(segment) {
        let start = from + rel;
        let end = start + segment.len();
        let starts_segment = start == 0 || bytes[start - 1] == b'/';
        let ends_segment = bytes.get(end) == Some(&b'/');
        if starts_segment && ends_segment {
            return true;
        }
        // `segment` is ASCII, so `start` indexes an ASCII byte and `start + 1`
        // is always a char boundary.
        from = start + 1;
    }
    false
}

/// Filename suffixes that mark tool-generated output, ported from upstream's
/// `GENERATED_PATTERNS` (`src/extraction/generated-detection.ts`). Upstream
/// writes them as end-anchored regexes containing no `/`, so a suffix test over
/// the whole path is equivalent — and case-sensitive, matching upstream's lack
/// of an `i` flag (`Grpc.cs`, `OuterClass.java`).
///
/// Mockgen's DEFAULT `mock_<src>.go` output is NOT here: upstream anchors it to
/// the basename (`/^mock_[^/]+\.go$/`), so it needs [`is_mockgen_default_output`].
const GENERATED_SUFFIXES: &[&str] = &[
    // Go — protobuf / gRPC / pulsar
    ".pb.go",
    ".pulsar.go",
    "_grpc.pb.go",
    // Go — mockgen output renamed by the project (cosmos-sdk uses
    // `expected_*_mocks.go`); both spellings are accepted upstream.
    "_mock.go",
    "_mocks.go",
    // TypeScript / JavaScript — Apollo/GraphQL codegen, Prisma, Hasura,
    // ts-proto, gRPC-web, swagger-codegen. Upstream's `[jt]sx?` character
    // class expands to these four spellings per stem.
    ".generated.ts",
    ".generated.tsx",
    ".generated.js",
    ".generated.jsx",
    ".gen.ts",
    ".gen.tsx",
    ".gen.js",
    ".gen.jsx",
    ".pb.ts",
    ".pb.js",
    "_pb.ts",
    "_pb.js",
    "_grpc_pb.ts",
    "_grpc_pb.js",
    // Minified bundles vendored into a repo: single-letter symbols make
    // name-based edges pure noise.
    ".min.js",
    ".min.mjs",
    // Python — protobuf / gRPC
    "_pb2.py",
    "_pb2_grpc.py",
    "_pb2.pyi",
    // C++ — protobuf
    ".pb.cc",
    ".pb.h",
    // C# — protobuf / gRPC
    ".g.cs",
    "Grpc.cs",
    // Java — protoc-gen-java / protoc-gen-grpc-java
    "OuterClass.java",
    "Grpc.java",
    // Swift — protobuf
    ".pb.swift",
    // Dart — build_runner / freezed / json_serializable / protobuf / chopper
    ".g.dart",
    ".freezed.dart",
    ".pb.dart",
    ".pbgrpc.dart",
    ".chopper.dart",
    // Rust — in-tree codegen convention
    ".generated.rs",
];

/// Whether `path` looks like tool-generated source, judged from the filename
/// alone. Never reads file content.
///
/// A relevance hint for disambiguation and ranking, not a hard claim and not a
/// filter: generated nodes stay in the graph and stay reachable. Content-banner
/// detection (Go's `// Code generated ... DO NOT EDIT.` convention) is a
/// separate, index-time signal and deliberately not part of this path check.
pub fn is_generated_file(path: &str) -> bool {
    GENERATED_SUFFIXES.iter().any(|s| path.ends_with(s)) || is_mockgen_default_output(path)
}

/// Mockgen's default output, `mock_<src>.go`, anchored to the final path
/// segment. Upstream's `/^mock_[^/]+\.go$/` is basename-scoped, so a nested
/// `src/mock_helpers.go` matches while a `mock_store/` DIRECTORY does not, and
/// `[^/]+` requires a non-empty stem between the prefix and the extension.
fn is_mockgen_default_output(path: &str) -> bool {
    basename(path)
        .strip_prefix("mock_")
        .and_then(|rest| rest.strip_suffix(".go"))
        .is_some_and(|stem| !stem.is_empty())
}

fn basename(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dir_segments_match_whole_segments_only() {
        // Nested and repo-root forms both classify.
        assert!(is_test_file("apps/web/e2e/login.ts"));
        assert!(is_test_file("e2e/login.ts"));
        assert!(is_test_file("tests/mod.rs"));
        assert!(is_test_file("app/tests/mod.rs"));
        assert!(is_test_file("pkg/__tests__/a.js"));
        assert!(is_test_file("app/spec/mod.rb"));
        assert!(is_test_file("spec/mod.rb"));
        // A segment name embedded in a longer segment does not.
        assert!(!is_test_file("src/route2e2e.ts"));
        assert!(!is_test_file("src/latest/mod.rs"));
        assert!(!is_test_file("src/contest/mod.rs"));
        assert!(!is_test_file("src/spectrum/mod.rs"));
        // A trailing segment with no following `/` is a filename, not a dir.
        assert!(!is_test_file("src/e2e"));
    }

    #[test]
    fn test_file_markers_cover_go_and_js_conventions() {
        assert!(is_test_file("pkg/math_test.go"));
        assert!(is_test_file("math_test.go"));
        assert!(is_test_file("src/foo.test.ts"));
        assert!(is_test_file("src/foo.spec.ts"));
        assert!(!is_test_file("src/lib.rs"));
        assert!(!is_test_file("internal/latest.go"));
        assert!(!is_test_file("src/testify.go"));
    }

    #[test]
    fn contains_path_segment_rejects_a_shifted_false_start() {
        // Regression for the offset bug: rejecting the `xe2e` occurrence must
        // not leave the scan believing the next byte is segment-initial.
        assert!(!contains_path_segment("xe2e/a.ts", "e2e"));
        assert!(contains_path_segment("x/e2e/a.ts", "e2e"));
        assert!(contains_path_segment("e2e/e2e/a.ts", "e2e"));
    }

    #[test]
    fn contains_path_segment_tolerates_multibyte_neighbors() {
        assert!(!contains_path_segment("src/日e2e/a.ts", "e2e"));
        assert!(contains_path_segment("src/日本/e2e/a.ts", "e2e"));
    }

    #[test]
    fn generated_suffixes_cover_every_ported_family() {
        for path in [
            "api.pb.go",
            "api.pulsar.go",
            "api_grpc.pb.go",
            "store_mock.go",
            "expected_keepers_mocks.go",
            "schema.generated.ts",
            "schema.generated.tsx",
            "schema.generated.js",
            "schema.generated.jsx",
            "api.gen.ts",
            "api.gen.tsx",
            "api.gen.js",
            "api.gen.jsx",
            "svc.pb.ts",
            "svc.pb.js",
            "svc_pb.ts",
            "svc_pb.js",
            "svc_grpc_pb.ts",
            "svc_grpc_pb.js",
            "vendor/chart.min.js",
            "vendor/chart.min.mjs",
            "svc_pb2.py",
            "svc_pb2_grpc.py",
            "svc_pb2.pyi",
            "svc.pb.cc",
            "svc.pb.h",
            "Model.g.cs",
            "GreeterGrpc.cs",
            "GreeterOuterClass.java",
            "GreeterGrpc.java",
            "Model.pb.swift",
            "model.g.dart",
            "model.freezed.dart",
            "model.pb.dart",
            "model.pbgrpc.dart",
            "api.chopper.dart",
            "src/proto.generated.rs",
        ] {
            assert!(is_generated_file(path), "must classify: {path}");
        }
    }

    #[test]
    fn generated_suffixes_leave_hand_written_files_alone() {
        for path in [
            "main.rs",
            "src/generator.ts",
            "src/mocks.go",
            "src/genesis.go",
            "src/pb.go",
            "src/table.js",
            "docs/min.css",
            "src/grpc.cs",
            "src/outerclass.java",
        ] {
            assert!(!is_generated_file(path), "must not classify: {path}");
        }
    }

    #[test]
    fn mockgen_default_output_is_basename_anchored() {
        assert!(is_generated_file("mock_store.go"));
        assert!(is_generated_file("src/mock_helpers.go"));
        assert!(is_generated_file("a/b/c/mock_x.go"));
        // A `mock_` DIRECTORY is not a generated file.
        assert!(!is_generated_file("mock_store/registry.go"));
        // `[^/]+` requires a non-empty stem.
        assert!(!is_generated_file("mock_.go"));
        assert!(!is_generated_file("src/mock_.go"));
        // The prefix needs its underscore separator.
        assert!(!is_generated_file("src/mockery_notgen.go"));
    }
}
