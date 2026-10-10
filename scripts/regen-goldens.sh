#!/usr/bin/env bash
# Regenerate or check the golden corpora under reference/golden/.
#
# usage:
#   scripts/regen-goldens.sh --check [corpus...]   index each corpus afresh and compare
#                                                  its canonical artifacts with the
#                                                  committed golden (exit 1 on any
#                                                  difference); writes nothing into
#                                                  the repository
#   scripts/regen-goldens.sh --write corpus...     rewrite colby.db and the canonical
#                                                  artifacts of the named corpora only
#   scripts/regen-goldens.sh --mini-transplant     rebuild mini on the current schema,
#                                                  keeping its committed (upstream) rows
#
# With no corpus named, --check covers every directory under reference/golden/
# that has a source corpus under crates/codegraph-bench/fixtures/. mini is never
# re-indexed: it has no re-indexable provenance (see docs/equivalence.md, "Mini
# schema rebuild"), so only --mini-transplant touches it.
#
# Binaries default to target/release/codegraph and target/release/bench; build
# them with `cargo build --locked --release -p codegraph-rs -p codegraph-bench`
# or point CODEGRAPH_BIN / BENCH_BIN elsewhere. Scratch directories come from
# mktemp and are removed on exit.
set -euo pipefail
shopt -s inherit_errexit

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$script_dir/.." && pwd)"
codegraph_bin="${CODEGRAPH_BIN:-$repo/target/release/codegraph}"
bench_bin="${BENCH_BIN:-$repo/target/release/bench}"
artifacts=(nodes.json edges.json refs.json files.json schema.sql)

scratch=()
cleanup() {
    local dir
    for dir in "${scratch[@]+"${scratch[@]}"}"; do
        rm -rf -- "$dir"
    done
}
trap cleanup EXIT

die() {
    printf 'regen-goldens: %s\n' "$*" >&2
    exit 2
}

usage() {
    sed -n '2,23p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

# Sets scratch_dir. Called directly (never in a command substitution) so the
# EXIT trap sees every directory it created.
new_scratch() {
    scratch_dir="$(mktemp -d "${TMPDIR:-/tmp}/codegraph-regen-$1.XXXXXX")"
    scratch+=("$scratch_dir")
}

require_binaries() {
    [ -x "$codegraph_bin" ] || die "codegraph binary not found at $codegraph_bin (build it or set CODEGRAPH_BIN)"
    [ -x "$bench_bin" ] || die "bench binary not found at $bench_bin (build it or set BENCH_BIN)"
}

default_corpora() {
    local dir name
    for dir in "$repo"/reference/golden/*/; do
        name="$(basename "$dir")"
        [ "$name" = mini ] && continue
        [ -d "$repo/crates/codegraph-bench/fixtures/$name" ] || continue
        printf '%s\n' "$name"
    done
}

validate_corpus() {
    local name="$1"
    case "$name" in
        "" | *[!A-Za-z0-9_-]*) die "invalid corpus name: '$name'" ;;
        mini) die "mini is not re-indexable; use --mini-transplant" ;;
    esac
    [ -d "$repo/crates/codegraph-bench/fixtures/$name" ] || die "no source corpus at crates/codegraph-bench/fixtures/$name"
}

# Index one corpus into a scratch directory; sets indexed_db.
index_corpus() {
    local name="$1" work log
    new_scratch "$name"
    work="$scratch_dir"
    cp -a "$repo/crates/codegraph-bench/fixtures/$name/." "$work/"
    new_scratch "$name-init-log"
    log="$scratch_dir/init.log"
    if ! env -u CODEGRAPH_DIR CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
        "$codegraph_bin" init "$work" >"$log" 2>&1; then
        cat "$log" >&2
        die "codegraph init failed for corpus $name"
    fi
    indexed_db="$work/.codegraph/codegraph.db"
}

# Dump canonical artifacts of a database into a directory. The log goes to a
# scratch directory of its own, so a write never touches anything beside the
# named corpus.
dump_golden() {
    local db="$1" out="$2" log
    mkdir -p "$out"
    new_scratch "gen-log"
    log="$scratch_dir/gen.log"
    if ! "$bench_bin" --gen-golden "$db" "$out" >"$log" 2>&1; then
        cat "$log" >&2
        die "bench --gen-golden failed for $db"
    fi
}

check_corpus() {
    local name="$1" out changed=() file
    index_corpus "$name"
    new_scratch "$name-golden"
    out="$scratch_dir"
    dump_golden "$indexed_db" "$out"
    for file in "${artifacts[@]}"; do
        if ! cmp -s "$out/$file" "$repo/reference/golden/$name/$file"; then
            changed+=("$file")
        fi
    done
    if [ "${#changed[@]}" -eq 0 ]; then
        printf '%-12s identical\n' "$name"
        return 0
    fi
    printf '%-12s CHANGED: %s\n' "$name" "${changed[*]}"
    return 1
}

write_corpus() {
    local name="$1" target
    index_corpus "$name"
    target="$repo/reference/golden/$name"
    mkdir -p "$target"
    cp "$indexed_db" "$target/colby.db"
    dump_golden "$target/colby.db" "$target"
    printf '%-12s written\n' "$name"
}

mini_transplant() {
    command -v sqlite3 >/dev/null 2>&1 || die "--mini-transplant needs the sqlite3 CLI"
    local committed="$repo/reference/golden/mini/colby.db" fresh work
    [ -f "$committed" ] || die "missing $committed"
    new_scratch mini
    work="$scratch_dir"
    cp -a "$repo/crates/codegraph-bench/fixtures/mini/." "$work/"
    if ! env -u CODEGRAPH_DIR CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
        "$codegraph_bin" init "$work" >"$work.init.log" 2>&1; then
        cat "$work.init.log" >&2
        die "codegraph init failed for mini"
    fi
    scratch+=("$work.init.log")
    fresh="$work/.codegraph/codegraph.db"
    # Keep the fresh schema_versions rows: copying mini's would leave
    # MAX(version) behind the schema the fresh database already has.
    sqlite3 "$fresh" "
        ATTACH DATABASE '$committed' AS src;
        BEGIN;
        DELETE FROM unresolved_refs; DELETE FROM edges; DELETE FROM files;
        DELETE FROM nodes; DELETE FROM project_metadata;
        INSERT INTO nodes            SELECT * FROM src.nodes;
        INSERT INTO edges            SELECT * FROM src.edges;
        INSERT INTO unresolved_refs  SELECT * FROM src.unresolved_refs;
        INSERT INTO project_metadata SELECT * FROM src.project_metadata;
        INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
          SELECT path, content_hash, language, size, modified_at, indexed_at, node_count, errors FROM src.files;
        COMMIT;
        DETACH src;"
    cp "$fresh" "$committed"
    dump_golden "$committed" "$repo/reference/golden/mini"
    printf '%-12s transplanted\n' mini
}

[ "$#" -ge 1 ] || usage
mode="$1"
shift

case "$mode" in
    --check)
        require_binaries
        corpora=("$@")
        if [ "${#corpora[@]}" -eq 0 ]; then
            mapfile -t corpora < <(default_corpora)
        fi
        status=0
        for name in "${corpora[@]}"; do
            validate_corpus "$name"
            check_corpus "$name" || status=1
        done
        exit "$status"
        ;;
    --write)
        [ "$#" -ge 1 ] || die "--write needs at least one corpus name"
        require_binaries
        for name in "$@"; do
            validate_corpus "$name"
        done
        for name in "$@"; do
            write_corpus "$name"
        done
        ;;
    --mini-transplant)
        [ "$#" -eq 0 ] || die "--mini-transplant takes no corpus names"
        require_binaries
        mini_transplant
        ;;
    -h | --help)
        usage
        ;;
    *)
        die "unknown mode '$mode' (use --check, --write or --mini-transplant)"
        ;;
esac
