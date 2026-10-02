//! The dead code report against a real index, and `GET /api/deadcode` — a
//! port of upstream `__tests__/dead-code.test.ts` (`v1.6.1`). Its pure-rule
//! cases live beside the rules in `codegraph-graph/src/dead_code.rs`.

mod support;

use std::time::{Duration, Instant};

use codegraph_core::types::NodeKind;
use codegraph_graph::dead_code::{
    DEAD_CODE_KINDS, DeadCodeQuery, DeadCodeReport, build_dead_code_report, default_source_reader,
};
use codegraph_store::Store;
use serde_json::Value;
use support::Project;

fn fixture() -> Project {
    Project::indexed(&[
        (
            "src/util.ts",
            "export function used(value: string): string {
  return value.trim();
}

function neverCalledAnywhere(value: string): string {
  return value.toUpperCase();
}

function alsoDeadButSmaller(): number {
  return 1;
}

// Exported and never called here — an outside caller may import it.
export function publicEntryPoint(): string {
  return 'hello';
}
",
        ),
        (
            "src/inner.ts",
            "export class Inner {\n  load(): string {\n    return 'inner';\n  }\n}\n",
        ),
        (
            "src/base.ts",
            "export class Base {\n  run(): string {\n    return 'base';\n  }\n}\n",
        ),
        (
            "src/child.ts",
            "import { Base } from './base';

export class Child extends Base {
  run(): string {
    return 'child';
  }
}
",
        ),
        (
            "src/facade.ts",
            "import { Inner } from './inner';
import { Base } from './base';
import { Child } from './child';
import { used } from './util';

function register(target: unknown, key: string): void {
  void target;
  void key;
}

export class Facade {
  inner = new Inner();
  child = new Child();

  load(): string {
    return this.inner.load();
  }

  go(): string {
    const base: Base = this.child;
    return used(base.run()) + this.load();
  }

  @register
  onEvent(): void {
    void 0;
  }
}
",
        ),
        // Mentioned in a template but never called anywhere the graph can see.
        (
            "src/handlers.ts",
            "export function mountHandlers(): string {
  return TEMPLATE;
}

function onSubmit(): void {
  void 0;
}

const TEMPLATE = '<form onsubmit=\"onSubmit()\"></form>';
",
        ),
        // Nothing imports this file at all: an island.
        (
            "src/orphan.ts",
            "function strandedHelper(): string {
  return 'nobody imports this file';
}

function alsoStranded(): number {
  return strandedHelper().length;
}
",
        ),
        (
            "src/index.ts",
            "import { Facade } from './facade';
import { mountHandlers } from './handlers';

export function start(): string {
  return new Facade().go() + mountHandlers();
}
",
        ),
        (
            "tests/helpers.ts",
            "export function sharedHelper(): string {
  return 'shared';
}

function helperNothingCalls(): void {
  void 0;
}
",
        ),
        (
            "tests/facade.test.ts",
            "import { Facade } from '../src/facade';
import { sharedHelper } from './helpers';

export function testFacade(): string {
  return new Facade().go() + sharedHelper();
}
",
        ),
    ])
}

fn store(project: &Project) -> Store {
    let paths = codegraph_core::IndexPaths::resolve(project.root(), None).unwrap();
    Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("read the index")
}

fn report(project: &Project, configure: impl FnOnce(&mut DeadCodeQuery<'_>)) -> DeadCodeReport {
    let store = store(project);
    let reader = default_source_reader(project.root());
    let mut query = DeadCodeQuery {
        read_source: Some(&reader),
        ..DeadCodeQuery::default()
    };
    configure(&mut query);
    build_dead_code_report(&store, &query).expect("the report")
}

fn names(report: &DeadCodeReport) -> Vec<String> {
    report.entries.iter().map(|e| e.node.name.clone()).collect()
}

/* --------------------------------------------------- buildDeadCodeReport -- */

#[test]
fn finds_the_symbol_nothing_references() {
    let project = fixture();
    assert!(names(&report(&project, |_| {})).contains(&"neverCalledAnywhere".to_string()));
}

#[test]
fn leaves_nothing_on_the_list_that_anything_reaches() {
    let project = fixture();
    let listed = names(&report(&project, |_| {}));
    for name in ["used", "start", "go", "mountHandlers", "load", "run"] {
        assert!(!listed.contains(&name.to_string()), "{name} in {listed:?}");
    }
}

#[test]
fn excludes_a_symbol_only_its_own_file_mentions_and_counts_it() {
    let project = fixture();
    let report = report(&project, |_| {});
    assert!(!names(&report).contains(&"onSubmit".to_string()));
    assert!(report.excluded.mentioned > 0);
    assert!(report.corroborated);
}

#[test]
fn makes_the_claim_when_corroboration_is_switched_off() {
    let project = fixture();
    let report = report(&project, |q| q.read_source = None);
    assert!(!report.corroborated);
    assert_eq!(report.excluded.mentioned, 0);
    assert!(names(&report).contains(&"onSubmit".to_string()));
}

#[test]
fn excludes_exported_symbols_by_default_and_includes_them_on_request() {
    let project = fixture();
    let strict = report(&project, |_| {});
    assert!(!names(&strict).contains(&"publicEntryPoint".to_string()));
    assert!(strict.excluded.exported > 0);
    assert!(!strict.include_exported);
    let wide = report(&project, |q| q.include_exported = true);
    assert!(names(&wide).contains(&"publicEntryPoint".to_string()));
    assert!(wide.include_exported);
    assert_eq!(wide.excluded.exported, 0);
}

#[test]
fn excludes_test_files_by_default_and_includes_them_on_request() {
    let project = fixture();
    let strict = report(&project, |_| {});
    assert!(!names(&strict).contains(&"helperNothingCalls".to_string()));
    assert!(strict.excluded.tests > 0);
    let wide = report(&project, |q| q.include_tests = true);
    assert!(names(&wide).contains(&"helperNothingCalls".to_string()));
}

#[test]
fn says_nothing_about_a_file_nothing_in_the_index_reaches() {
    let project = fixture();
    let report = report(&project, |q| q.include_exported = true);
    assert!(!names(&report).contains(&"strandedHelper".to_string()));
    assert!(report.excluded.unreachable_file > 0);
}

#[test]
fn excludes_a_decorated_member_a_framework_registers_it() {
    let project = fixture();
    let report = report(&project, |_| {});
    assert!(!names(&report).contains(&"onEvent".to_string()));
    assert!(report.excluded.decorated > 0);
}

#[test]
fn ranks_by_size_and_reports_the_real_total_when_capped() {
    let project = fixture();
    let full = report(&project, |_| {});
    let sizes: Vec<i64> = full.entries.iter().map(|e| e.lines).collect();
    let mut sorted = sizes.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted);
    let capped = report(&project, |q| q.limit = 1);
    assert_eq!(capped.entries.len(), 1);
    assert_eq!(capped.total, full.total);
    assert_eq!(capped.entries[0].node.name, full.entries[0].node.name);
}

#[test]
fn every_exclusion_count_is_a_number_of_candidates_and_they_add_up() {
    let project = fixture();
    let report = report(&project, |_| {});
    let excluded: usize = report.excluded.entries().iter().map(|(_, n)| n).sum();
    assert!(report.candidates > 0);
    assert!(excluded + report.entries.len() <= report.candidates);
    assert!(!report.bounded);
}

#[test]
fn restricts_to_the_kinds_asked_for_and_ignores_nonsense() {
    let project = fixture();
    let classes = report(&project, |q| q.kinds = Some(vec![NodeKind::Class]));
    assert_eq!(classes.kinds, vec![NodeKind::Class]);
    assert!(
        classes
            .entries
            .iter()
            .all(|e| e.node.kind == NodeKind::Class)
    );
    // A kind a request may not ask for falls back to the default set.
    let nonsense = report(&project, |q| q.kinds = Some(vec![NodeKind::Import]));
    assert_eq!(nonsense.kinds, DEAD_CODE_KINDS.to_vec());
}

/* --------------------------------------------------------- /api/deadcode -- */

#[tokio::test]
async fn groups_the_rows_by_file_and_keeps_the_totals_honest() {
    let project = fixture();
    let payload = project.viewer().json("/api/deadcode").await;
    let rows = payload["rows"]["items"].as_array().unwrap();
    assert_eq!(
        payload["rows"]["total"].as_u64().unwrap() as usize,
        rows.len()
    );
    assert_eq!(
        payload["rows"]["shown"].as_u64().unwrap() as usize,
        rows.len()
    );
    let groups = payload["groups"].as_array().unwrap();
    let grouped: usize = groups
        .iter()
        .map(|g| g["rows"].as_array().unwrap().len())
        .sum();
    assert_eq!(grouped, rows.len());
    let files: Vec<&str> = groups.iter().map(|g| g["file"].as_str().unwrap()).collect();
    let unique: std::collections::HashSet<&&str> = files.iter().collect();
    assert_eq!(unique.len(), files.len());
    assert!(files.contains(&"src/util.ts"), "{files:?}");
}

#[tokio::test]
async fn carries_the_exclusions_with_their_own_wording() {
    let project = fixture();
    let payload = project.viewer().json("/api/deadcode").await;
    let excluded = payload["excluded"].as_array().unwrap();
    assert!(!excluded.is_empty());
    for entry in excluded {
        assert!(entry["count"].as_u64().unwrap() > 0);
        assert!(!entry["label"].as_str().unwrap().is_empty());
    }
    let sum: u64 = excluded.iter().map(|e| e["count"].as_u64().unwrap()).sum();
    assert_eq!(payload["excludedTotal"].as_u64().unwrap(), sum);
    assert!(payload["candidates"].as_u64().unwrap() >= sum);
    assert_eq!(payload["corroborated"], true);
}

#[tokio::test]
async fn widens_on_exported_1_and_says_which_list_it_answered() {
    let project = fixture();
    let viewer = project.viewer();
    let strict = viewer.json("/api/deadcode").await;
    let wide = viewer.json("/api/deadcode?exported=1").await;
    assert_eq!(strict["includeExported"], false);
    assert_eq!(wide["includeExported"], true);
    assert!(wide["rows"]["total"].as_u64() > strict["rows"]["total"].as_u64());
    assert!(
        wide["rows"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "publicEntryPoint")
    );
}

#[tokio::test]
async fn honours_limit_without_lying_about_the_total() {
    let project = fixture();
    let viewer = project.viewer();
    let full = viewer.json("/api/deadcode").await;
    let capped = viewer.json("/api/deadcode?limit=1").await;
    assert_eq!(capped["rows"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(capped["rows"]["total"], full["rows"]["total"]);
    assert_eq!(
        capped["rows"]["truncated"],
        full["rows"]["total"].as_u64().unwrap() > 1
    );
}

#[tokio::test]
async fn is_listed_on_the_api_index() {
    let project = fixture();
    let index = project.viewer().json("/api").await;
    assert!(
        index["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "/api/deadcode")
    );
}

#[tokio::test]
async fn reports_how_many_of_a_modules_files_are_tool_generated() {
    let project = fixture();
    let payload = project.viewer().json("/api/map").await;
    for module in payload["modules"].as_array().unwrap() {
        assert!(module["generated"].as_u64().unwrap() <= module["files"].as_u64().unwrap());
        let shown: Vec<&Value> = module["fileList"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .collect();
        for file in module["generatedFiles"].as_array().unwrap() {
            assert!(shown.contains(&file));
        }
    }
}

/* ------------------------------------- an ancestor outside the index #1973 -- */

fn external_ancestors() -> Project {
    Project::indexed(&[
        (
            "src/clock.tsx",
            "import React from 'react';
import { Transform } from 'stream';
import { OnModuleInit } from '@nestjs/common';
export class Clock extends React.Component { componentDidMount() {} render() { return null; } }
export class Upper extends Transform { _transform(c, e, cb) { cb(null, c); } }
export class Boot implements OnModuleInit { onModuleInit() {} }
export class Ticker extends Clock { componentDidUpdate() {} }
export class Third extends Ticker { componentWillUnmount() {} componentDidCatch() {} }
export class Plain { neverCalledMember() {} }
function reallyUnused() {}
",
        ),
        (
            "src/main.ts",
            "import { Clock, Upper, Boot, Ticker, Third, Plain } from './clock';\nexport const all = [Clock, Upper, Boot, Ticker, Third, Plain];\n",
        ),
    ])
}

#[test]
fn does_not_list_members_a_framework_base_class_calls() {
    let project = external_ancestors();
    let report = report(&project, |_| {});
    let listed = names(&report);
    for name in [
        "componentDidMount",
        "componentWillUnmount",
        "render",
        "_transform",
        "onModuleInit",
        "componentDidUpdate",
        "componentDidCatch",
    ] {
        assert!(!listed.contains(&name.to_string()), "{name} in {listed:?}");
    }
    assert!(report.excluded.overriding >= 7, "{:?}", report.excluded);
}

#[test]
fn still_lists_what_nothing_reaches_outside_such_a_class() {
    let project = external_ancestors();
    let listed = names(&report(&project, |_| {}));
    for name in ["reallyUnused", "neverCalledMember"] {
        assert!(listed.contains(&name.to_string()), "{name} in {listed:?}");
    }
}
