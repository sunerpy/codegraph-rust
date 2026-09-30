# Documentation instructions

These rules apply to `docs/**` and supplement the root [`AGENTS.md`](../AGENTS.md).

- Write canonical technical references in English and describe current AS-BUILT
  behavior. A design, plan, issue, or upstream behavior is not shipped behavior.
- Update the canonical page alongside the code. README is a landing page, not a
  second CLI, MCP, language, installer, or architecture reference.
- Keep [`../README.md`](../README.md) and
  [`readme/README.zh-CN.md`](readme/README.zh-CN.md) structurally synchronized:
  same commands, links, security posture, distribution paths, and claims.
- Avoid volatile counts, dependency versions, runner labels, protocol matrices,
  and coverage percentages in prose. When a count is valuable, derive it from
  source and lock it in `scripts/docs-check.py` or a product test.
- Never claim latency, memory, throughput, or scale without a reproducible result
  that names the commit, corpus, environment, command, run count, and dispersion.
- Keep relative links repository-portable. Run `python3 scripts/docs-check.py`
  after editing headings or links and `oxfmt --check` before handoff.
- Treat `upstream-sync/UPSTREAM.md` log entries and dated audit reports as
  append-only evidence. Update the mutable Current alignment block and add a new
  dated entry; do not rewrite older conclusions with present-day facts.
- Golden regeneration instructions must name the corpus, exact commands, expected
  changed artifacts, and why the graph change is intentional.
- A future UI design is documented as a proposal until the owner selects a Penpot
  direction and a code PR lands. Do not turn design-session status into product
  documentation.
