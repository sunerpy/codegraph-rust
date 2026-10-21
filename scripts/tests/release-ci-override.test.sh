#!/usr/bin/env bash
# Run the Release workflow's PR-CI fallback against a stub `gh`. The dispatch-only
# verify-ci step must accept only the release PR's own CI on an identical tree,
# defer to the exact-SHA gate when a push run exists, and reject everything else;
# the publish-time re-check must stop publication when a push run that appeared
# later fails.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
CG_WORKFLOW="$root/.github/workflows/release.yml" CG_OUT="$work" python3 - <<'PY'
import os,sys
try: import yaml
except ImportError: print('release-ci-override: ERROR: PyYAML is required',file=sys.stderr); raise SystemExit(2)
jobs=yaml.safe_load(open(os.environ['CG_WORKFLOW'],encoding='utf-8')).get('jobs') or {}
for job,step_id,name in (('verify-ci','pr_ci','pr_ci.sh'),('publish-release','exact_ci_recheck','recheck.sh')):
    steps=[s for s in (jobs.get(job) or {}).get('steps') or [] if isinstance(s,dict) and s.get('id')==step_id]
    if len(steps)!=1: print(f'release-ci-override: ERROR: expected one {job} step with id {step_id}, got {len(steps)}',file=sys.stderr); raise SystemExit(2)
    open(os.path.join(os.environ['CG_OUT'],name),'w',encoding='utf-8').write(steps[0]['run'])
PY

# `gh api <path> [--jq <filter>]` answered from $GH_STUB: call n of a path reads
# <key>.<n>.json when present, else <key>.json; a missing answer is an API failure.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = api ] || { echo "stub gh: unsupported: $*" >&2; exit 64; }
path=${2:?}; shift 2
filter=
if [ "${1:-}" = --jq ]; then filter=${2:?}; fi
base="$GH_STUB/$(printf '%s' "$path" | tr '/?&=.' '_____')"
calls=$(( $(cat "$base.calls" 2>/dev/null || echo 0) + 1 )); echo "$calls" >"$base.calls"
file="$base.$calls.json"; [ -f "$file" ] || file="$base.json"
[ -f "$file" ] || { echo "stub gh: HTTP 404 for $path" >&2; exit 1; }
if [ -n "$filter" ]; then jq -r "$filter" "$file"; else cat "$file"; fi
SH
printf '#!/usr/bin/env bash\nexit 0\n' >"$work/bin/sleep"
chmod +x "$work/bin/gh" "$work/bin/sleep"

repo=sunerpy/codegraph-rust
release=1111111111111111111111111111111111111111
parent=2222222222222222222222222222222222222222
head=3333333333333333333333333333333333333333
tree=4444444444444444444444444444444444444444
other=5555555555555555555555555555555555555555
run_id=39605156993
push_run=40000000001
push_runs="repos/$repo/actions/runs?head_sha=$release&event=push&branch=main&per_page=100"
run_path="repos/$repo/actions/runs/$run_id"
jobs_path="repos/$repo/actions/runs/$run_id/jobs?per_page=100"
push_jobs_path="repos/$repo/actions/runs/$push_run/jobs?per_page=100"
pulls_path="repos/$repo/commits/$release/pulls"
pr_path="repos/$repo/pulls/325"
release_path="repos/$repo/commits/$release"
compare_path="repos/$repo/compare/$parent...$head"
head_path="repos/$repo/commits/$head"

key() { printf '%s' "$1" | tr '/?&=.' '_____'; }
# answer <case> <path> <json> [call]
answer() { printf '%s\n' "$3" >"$work/$1/$(key "$2")${4:+.$4}.json"; }
# Every check holds: the shape of v0.55.0's release PR #325.
scenario() {
  mkdir -p "$work/$1"
  answer "$1" "$push_runs" '{"workflow_runs":[]}'
  answer "$1" "$run_path" "{\"name\":\"CI\",\"path\":\".github/workflows/ci.yml\",\"event\":\"pull_request\",\"status\":\"completed\",\"head_sha\":\"$head\",\"repository\":{\"full_name\":\"$repo\"},\"head_repository\":{\"full_name\":\"$repo\"}}"
  answer "$1" "$jobs_path" '{"jobs":[{"name":"Linux","conclusion":"success"},{"name":"CI Success","status":"completed","conclusion":"success"}]}'
  answer "$1" "$pulls_path" '[{"number":325}]'
  answer "$1" "$pr_path" "{\"merged\":true,\"base\":{\"ref\":\"main\"},\"merge_commit_sha\":\"$release\",\"head\":{\"sha\":\"$head\"}}"
  answer "$1" "$release_path" "{\"parents\":[{\"sha\":\"$parent\"}],\"commit\":{\"tree\":{\"sha\":\"$tree\"}}}"
  answer "$1" "$compare_path" '{"behind_by":0}'
  answer "$1" "$head_path" "{\"commit\":{\"tree\":{\"sha\":\"$tree\"}}}"
}
# A push CI run for the release commit whose CI Success job reads $2 on every call.
with_push_run() {
  answer "$1" "$push_runs" "{\"workflow_runs\":[{\"id\":$push_run,\"name\":\"CI\",\"created_at\":\"2026-10-22T00:00:00Z\"}]}"
  answer "$1" "$push_jobs_path" "{\"jobs\":[{\"name\":\"CI Success\",$2}]}"
}
edit() {
  local file
  file="$work/$1/$(key "$2").json"
  jq "$3" "$file" >"$file.new" && mv "$file.new" "$file"
}
drop() { rm -f "$work/$1/$(key "$2").json"; }

pass=0; fail=0
# check <case> pr_ci|recheck accept|defer|reject|pass <log regex> [run id]
check() {
  local name=$1 body=$2 want=$3 regex=$4 id=${5:-$run_id} rc ok=1
  : >"$work/$name.output"
  set +e
  PATH="$work/bin:$PATH" GH_STUB="$work/$name" GITHUB_OUTPUT="$work/$name.output" \
    CI_RUN_ID="$id" GH_TOKEN=stub RELEASE_SHA="$release" HEAD_SHA="$release" REPO="$repo" \
    bash "$work/$body.sh" >"$work/$name.log" 2>&1
  rc=$?
  set -e
  case $want in
  accept) [ "$rc" -eq 0 ] && grep -qx 'accepted=true' "$work/$name.output" || ok=0 ;;
  defer) [ "$rc" -eq 0 ] && grep -qx 'accepted=false' "$work/$name.output" || ok=0 ;;
  reject) [ "$rc" -ne 0 ] && ! grep -q 'accepted=' "$work/$name.output" || ok=0 ;;
  pass) [ "$rc" -eq 0 ] || ok=0 ;;
  esac
  grep -Eq -- "$regex" "$work/$name.log" || ok=0
  if [ "$ok" -eq 1 ]; then echo "PASS: $name"; pass=$((pass+1));
  else echo "FAIL: $name (exit $rc)" >&2; cat "$work/$name.log" "$work/$name.output" >&2; fail=$((fail+1)); fi
}

# verify-ci: the dispatch-only step that may accept the release PR's CI.
scenario A_accept; check A_accept pr_ci accept "::warning title=Release CI::.*accepted CI run $run_id of PR #325 \\(head $head, tree $tree\\)"
scenario B_push_run; edit B_push_run "$push_runs" '.workflow_runs=[{"name":"CI"}]'; check B_push_run pr_ci defer 'exact-SHA gate decides'
scenario C_run_id; check C_run_id pr_ci reject "ci_run_id must be a run ID, got '12a'" 12a
scenario D_repository; edit D_repository "$run_path" '.repository.full_name="someone/else"'; check D_repository pr_ci reject 'belongs to another repository'
scenario E_fork; edit E_fork "$run_path" '.head_repository.full_name="fork/codegraph-rust"'; check E_fork pr_ci reject 'tested a head outside'
scenario F_name; edit F_name "$run_path" '.name="Release"'; check F_name pr_ci reject 'is not the CI workflow'
scenario G_path; edit G_path "$run_path" '.path=".github/workflows/other.yml"'; check G_path pr_ci reject 'is not \.github/workflows/ci\.yml'
scenario H_event; edit H_event "$run_path" '.event="push"'; check H_event pr_ci reject 'is not a pull_request run'
scenario I_status; edit I_status "$run_path" '.status="in_progress"'; check I_status pr_ci reject 'has not completed'
scenario J_gate; edit J_gate "$jobs_path" '(.jobs[] | select(.name == "CI Success") | .conclusion)="failure"'; check J_gate pr_ci reject 'CI Success concluded failure'
scenario K_no_gate; edit K_no_gate "$jobs_path" '.jobs |= map(select(.name != "CI Success"))'; check K_no_gate pr_ci reject 'CI Success concluded <missing>'
scenario L_no_pr; edit L_no_pr "$pulls_path" '[]'; check L_no_pr pr_ci reject '0 merged pull requests'
scenario M_unmerged; edit M_unmerged "$pr_path" '.merged=false'; check M_unmerged pr_ci reject '0 merged pull requests'
scenario N_merge_sha; edit N_merge_sha "$pr_path" ".merge_commit_sha=\"$other\""; check N_merge_sha pr_ci reject '0 merged pull requests'
scenario O_pr_head; edit O_pr_head "$pr_path" ".head.sha=\"$other\""; check O_pr_head pr_ci reject '0 merged pull requests'
scenario P_base; edit P_base "$pr_path" '.base.ref="release"'; check P_base pr_ci reject '0 merged pull requests'
scenario Q_two_prs; edit Q_two_prs "$pulls_path" '[{"number":325},{"number":326}]'; cp "$work/Q_two_prs/$(key "$pr_path").json" "$work/Q_two_prs/$(key "repos/$repo/pulls/326").json"; check Q_two_prs pr_ci reject '2 merged pull requests'
scenario R_parents; edit R_parents "$release_path" ".parents += [{\"sha\":\"$other\"}]"; check R_parents pr_ci reject 'does not have exactly one parent'
scenario S_behind; edit S_behind "$compare_path" '.behind_by=1'; check S_behind pr_ci reject 'is 1 commit\(s\) behind'
scenario T_tree; edit T_tree "$head_path" ".commit.tree.sha=\"$other\""; check T_tree pr_ci reject "tree $other of $head differs from the release commit's tree $tree"
scenario U_run_api; drop U_run_api "$run_path"; check U_run_api pr_ci reject "cannot read CI run $run_id"
scenario V_push_api; drop V_push_api "$push_runs"; check V_push_api pr_ci reject 'cannot list the push runs'
scenario W_compare_api; drop W_compare_api "$compare_path"; check W_compare_api pr_ci reject 'cannot compare'

# publish-release: the re-check after the fallback was accepted.
scenario X1_still_none; check X1_still_none recheck pass 'the accepted release PR CI stands'
scenario X2_later_pass; with_push_run X2_later_pass '"status":"completed","conclusion":"success"'; check X2_later_pass recheck pass "CI Success passed in push run $push_run"
scenario X3_later_fail; with_push_run X3_later_fail '"status":"completed","conclusion":"failure"'; check X3_later_fail recheck reject "CI Success concluded failure in push run $push_run"
scenario X4_running_pass; with_push_run X4_running_pass '"status":"completed","conclusion":"success"'; answer X4_running_pass "$push_jobs_path" '{"jobs":[{"name":"CI Success","status":"in_progress"}]}' 1; check X4_running_pass recheck pass "CI Success passed in push run $push_run"
scenario X5_running_fail; with_push_run X5_running_fail '"status":"completed","conclusion":"cancelled"'; answer X5_running_fail "$push_jobs_path" '{"jobs":[]}' 1; check X5_running_fail recheck reject 'CI Success concluded cancelled'
scenario X6_never_done; with_push_run X6_never_done '"status":"queued"'; check X6_never_done recheck reject 'has not completed; re-run this job after it finishes'
scenario X7_list_api; drop X7_list_api "$push_runs"; check X7_list_api recheck reject 'cannot list the push runs'
scenario X8_jobs_api; with_push_run X8_jobs_api '"status":"completed","conclusion":"success"'; drop X8_jobs_api "$push_jobs_path"; check X8_jobs_api recheck reject "cannot read the jobs of push run $push_run"

echo "release-ci-override harness: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
