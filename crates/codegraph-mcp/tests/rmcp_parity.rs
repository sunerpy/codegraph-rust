//! Parity: the 15 golden MCP fixtures through the rmcp stdio handler.
//!
//! For EACH `reference/golden/mcp/*.json` fixture, drive the request through the
//! rmcp stdio handler and assert structural parity against the GOLDEN response
//! itself — `assert_parity(fixture, golden_response, run_rmcp_stdio(req))`. The
//! golden JSON is the baseline; the SAME golden files remain the invariant, no new
//! fixtures are added.

#[path = "support/parity.rs"]
mod parity;

use parity::{GOLDEN_FIXTURES, assert_parity, load_golden, run_rmcp_stdio, setup_mini_project};

#[test]
fn all_15_golden_fixtures_reach_parity_over_rmcp_stdio() {
    let project = setup_mini_project();
    for fixture in GOLDEN_FIXTURES {
        let (req, golden_resp) = load_golden(fixture);
        let new = run_rmcp_stdio(project.path(), req);
        assert_parity(fixture, &golden_resp, &new, fixture);
    }
}
