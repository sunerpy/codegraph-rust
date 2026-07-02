//! L2 parity: the 15 golden MCP fixtures through the rmcp stdio handler.
//!
//! For EACH `reference/golden/mcp/*.json` fixture, drive the request through
//! BOTH transports and assert structural parity —
//! `assert_parity(run_old(req), run_rmcp_stdio(req))`. RED until
//! `CodeGraphHandler` exists; the SAME golden files are the invariant, no new
//! fixtures are added.
#![cfg(feature = "rmcp")]

#[path = "support/parity.rs"]
mod parity;

use parity::{
    assert_parity, load_golden, run_old, run_rmcp_stdio, setup_mini_project, GOLDEN_FIXTURES,
};

#[test]
fn all_15_golden_fixtures_reach_parity_over_rmcp_stdio() {
    let project = setup_mini_project();
    for fixture in GOLDEN_FIXTURES {
        let (req, _golden_resp) = load_golden(fixture);
        let old = run_old(project.path(), req.clone());
        let new = run_rmcp_stdio(project.path(), req);
        assert_parity(fixture, &old, &new, fixture);
    }
}
