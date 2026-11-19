//! The anchor's body as the code reads it — the Steps view's second reading.
//! Upstream `src/ui-server/api/program.ts` (`e8ddc4e2`).
//!
//! `steps` walks FORWARD from the anchor and draws what it sets in motion, a
//! row per distance — right for a screen, where handlers fire on events and
//! nothing orders them, wrong for a handler: on a login, the lookup, the
//! token, the `200` and the `401` are each one step away and land side by
//! side, when the code says *first the lookup, then IF the password matches
//! sign a token and answer 200, ELSE answer 401*. This turns the same walk
//! into that reading: items in source order, forks where the code forks, an
//! arm that replies or leaves ending there, and a folded helper drawn in
//! place at the call. It is a pure function of the records the walk made
//! ([`ProgramSite`]): no graph, no source, no control-flow graph. A fork
//! exists only where a guard was READ, so a language without rules, or a
//! file that drifted since the index, yields a plain sequence.
//!
//! Two sites are arms of one fork when their guards agree on the branch (the
//! decision a guard belongs to, `BranchGuard::branch`) and disagree on the
//! arm, which a joined condition string can never say.

use std::collections::{HashMap, HashSet};

use codegraph_graph::branch_guards::{BranchGuard, GuardExit, GuardForm, guard_label};
use codegraph_graph::call_sites::{LoopKind, SiteLoop};
use serde::Serialize;

use super::wire::WireNodeRef;

// =============================================================================
// Wire shapes
// =============================================================================

/// How an arm of a fork leaves, when it does — the rail stops there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireArmEnd {
    Reply,
    Return,
    Throw,
    Exit,
}

impl From<GuardExit> for WireArmEnd {
    fn from(exit: GuardExit) -> Self {
        match exit {
            GuardExit::Return => Self::Return,
            GuardExit::Throw => Self::Throw,
            GuardExit::Exit => Self::Exit,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireArm {
    /// This arm's own condition, in the words the rest of the view uses.
    pub when: String,
    /// The arm taken when the fork's condition does NOT hold.
    #[serde(skip_serializing_if = "is_false")]
    pub not: bool,
    /// How it leaves; `None` = it runs on.
    pub ends: Option<WireArmEnd>,
    pub body: WireBlock,
}

pub type WireBlock = Vec<WireItem>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkForm {
    If,
    Switch,
    Ternary,
    Try,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockKind {
    /// A helper drawn where it is called.
    Inline,
    /// A body that runs for each item.
    Loop,
    /// Work that runs after this function returns.
    Later,
    /// Calls started together.
    Together,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CutWhy {
    Folded,
    Depth,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WireItem {
    /// A step of the picture, where the code writes it. `body` is what it
    /// does, when the walk entered it; `again` says it happens here too and
    /// was read above.
    Step {
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        link: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        within: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<WireBlock>,
        #[serde(skip_serializing_if = "is_false")]
        again: bool,
    },
    /// A decision: `if` / `else`, a `switch`, a ternary, a `try`, or an
    /// early exit.
    Fork {
        on: String,
        form: ForkForm,
        arms: Vec<WireArm>,
    },
    /// A run of items that is not plain sequence.
    Block {
        block: BlockKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
        loop_kind: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        via: Option<WireNodeRef>,
        #[serde(skip_serializing_if = "Option::is_none")]
        within: Option<String>,
        body: WireBlock,
        #[serde(skip_serializing_if = "is_false")]
        again: bool,
    },
    /// Where the reading stopped.
    Cut { why: CutWhy },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireProgram {
    pub root: WireBlock,
    /// Items the reading could not place — a recursion or a cap it hit.
    pub truncated: usize,
}

// =============================================================================
// What the walk records
// =============================================================================

/// A position in a file: a 1-based line and a 0-based column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SitePosition {
    pub line: i64,
    pub column: i64,
}

/// Where a call is written: its start, and the end of its span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteAt {
    pub line: i64,
    pub column: i64,
    pub end: SitePosition,
}

/// What fires a site, as far as the reading needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramTrigger {
    pub kind: String,
    pub name: String,
}

/// One thing that happens in one function, and where the code writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramSite {
    /// The step reached here, when one is.
    pub step: Option<String>,
    /// The link that step arrived on.
    pub link: Option<String>,
    /// The helper the walk folded into here; its own sites are its body.
    pub into: Option<String>,
    pub at: SiteAt,
    /// The call this one is written inside the arguments of.
    pub within: Option<String>,
    /// The conditions it runs under, outermost first.
    pub guards: Vec<BranchGuard>,
    /// The loops it is written inside, outermost first.
    pub loops: Vec<SiteLoop>,
    /// What fires it, when something binds it — a callback runs LATER.
    pub trigger: Option<ProgramTrigger>,
}

/// What a step is, for the two things the reading needs: whether it
/// ANSWERS, and the function to read on into when the walk entered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFacts {
    pub reply: bool,
    pub into: Option<String>,
}

pub struct ProgramInput<'a> {
    /// The sites of each function, by the function's node id (lookups only).
    pub sites: &'a HashMap<String, Vec<ProgramSite>>,
    /// Where the reading starts: the anchor's root function.
    pub root: Option<String>,
    /// A folded helper, for the words on the block it opens.
    pub node: &'a dyn Fn(&str) -> Option<WireNodeRef>,
    pub step: &'a dyn Fn(&str) -> Option<StepFacts>,
}

// =============================================================================
// The fold
// =============================================================================

/// Callbacks whose argument runs after this function returns.
const LATER_OF: &[&str] = &[
    "then",
    "catch",
    "finally",
    "setTimeout",
    "setInterval",
    "setImmediate",
    "queueMicrotask",
    "requestAnimationFrame",
    "useEffect",
    "useLayoutEffect",
    "addListener",
    "addEventListener",
    "on",
    "once",
    "subscribe",
    "nextTick",
    "process.nextTick",
];

/// Calls whose arguments are started TOGETHER.
const TOGETHER: &[&str] = &[
    "Promise.all",
    "Promise.allSettled",
    "Promise.any",
    "Promise.race",
    "asyncio.gather",
    "Task.WhenAll",
    "Task.WhenAny",
];

/// How deep a helper may be drawn inside a helper.
const MAX_INLINE: usize = 8;
/// Items in one reading.
const MAX_ITEMS: usize = 1200;

type BlockId = usize;

/// An item while the reading is built: blocks are arena slots, so a fork's
/// arm and a loop's body can be filled after they are placed.
enum Draft {
    Step {
        step: String,
        link: Option<String>,
        within: Option<String>,
        body: Option<BlockId>,
        again: bool,
    },
    Fork {
        on: String,
        form: ForkForm,
        arms: Vec<DraftArm>,
    },
    Block {
        block: BlockKind,
        by: Option<String>,
        loop_kind: Option<&'static str>,
        via: Option<WireNodeRef>,
        within: Option<String>,
        body: BlockId,
        again: bool,
    },
}

struct DraftArm {
    when: String,
    not: bool,
    ends: Option<WireArmEnd>,
    body: BlockId,
}

/// A construct the reading is inside: a loop, or one arm of a fork.
struct Open {
    branch: String,
    /// The fork, as (block, index in it); a loop has only its body.
    fork: Option<(BlockId, usize)>,
    arm_key: String,
    body: BlockId,
}

enum Scope<'s> {
    Guard {
        branch: &'s str,
        guard: &'s BranchGuard,
    },
    Loop {
        branch: &'s str,
        site_loop: &'s SiteLoop,
    },
}

impl Scope<'_> {
    fn branch(&self) -> &str {
        match self {
            Self::Guard { branch, .. } | Self::Loop { branch, .. } => branch,
        }
    }
}

/// What the reading has already said, so it says it once.
struct Reading<'i, 'a> {
    input: &'i ProgramInput<'a>,
    blocks: Vec<Vec<Draft>>,
    truncated: usize,
    items: usize,
    read: HashSet<String>,
}

/// The anchor's body in the code's order; `None` when there is none.
pub fn build_program(input: &ProgramInput<'_>) -> Option<WireProgram> {
    let root = input.root.clone()?;
    let mut reading = Reading {
        input,
        blocks: Vec::new(),
        truncated: 0,
        items: 0,
        read: HashSet::from([root.clone()]),
    };
    let block = reading.block_for(&root, std::slice::from_ref(&root));
    let root = reading.finish(block);
    if root.is_empty() {
        return None;
    }
    Some(WireProgram {
        root,
        truncated: reading.truncated,
    })
}

/// `x` written inside `y`'s span: after its start, before its end.
fn inside(x: &SiteAt, y: &SiteAt) -> bool {
    let after_start = x.line > y.line || (x.line == y.line && x.column > y.column);
    let before_end = x.line < y.end.line || (x.line == y.end.line && x.column < y.end.column);
    after_start && before_end
}

/// Source order: a call written inside another's arguments runs first; then
/// by position.
fn compare_sites(a: &ProgramSite, b: &ProgramSite) -> std::cmp::Ordering {
    if inside(&a.at, &b.at) {
        return std::cmp::Ordering::Less;
    }
    if inside(&b.at, &a.at) {
        return std::cmp::Ordering::Greater;
    }
    a.at.line
        .cmp(&b.at.line)
        .then(a.at.column.cmp(&b.at.column))
}

/// A stable binary insertion sort: `compare_sites` is not a total order (a
/// call inside another sorts first, against their positions), which the
/// standard sorts may reject, and on short runs this is what JavaScript's
/// sort does.
fn sort_sites(sites: &mut Vec<&ProgramSite>) {
    for index in 1..sites.len() {
        let pivot = sites[index];
        let (mut left, mut right) = (0, index);
        while left < right {
            let middle = (left + right) / 2;
            if compare_sites(pivot, sites[middle]) == std::cmp::Ordering::Less {
                right = middle;
            } else {
                left = middle + 1;
            }
        }
        sites.remove(index);
        sites.insert(left, pivot);
    }
}

/// A `line:column` branch as one number, for ordering constructs by where
/// they start.
fn position_of(branch: &str) -> i64 {
    let mut parts = branch.split(':');
    let number = |part: Option<&str>| {
        part.and_then(|part| part.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    let line = number(parts.next());
    let column = number(parts.next());
    line * 10000 + column
}

/// The constructs a site is written inside, outermost first: its guards and
/// its loops merged by where each one STARTS.
fn scopes_of(site: &ProgramSite) -> Vec<Scope<'_>> {
    let mut scopes: Vec<Scope<'_>> = site
        .guards
        .iter()
        .map(|guard| Scope::Guard {
            branch: guard.branch.as_str(),
            guard,
        })
        .collect();
    if site.loops.is_empty() {
        return scopes;
    }
    scopes.extend(site.loops.iter().map(|site_loop| Scope::Loop {
        branch: site_loop.branch.as_str(),
        site_loop,
    }));
    scopes.sort_by(|a, b| {
        position_of(a.branch())
            .cmp(&position_of(b.branch()))
            .then_with(|| codegraph_graph::collation::locale_compare(a.branch(), b.branch()))
    });
    scopes
}

/// `!` and the condition — the arm, not the decision.
fn arm_key(guard: &BranchGuard) -> String {
    format!("{}{}", if guard.negated { "!" } else { "" }, guard.text)
}

fn form_of(guard: &BranchGuard) -> ForkForm {
    match guard.form {
        GuardForm::Case => ForkForm::Switch,
        GuardForm::Ternary => ForkForm::Ternary,
        GuardForm::Catch => ForkForm::Try,
        _ => ForkForm::If,
    }
}

/// The decision as a reader says it: the guard, not negated.
fn decision(guard: &BranchGuard) -> String {
    let mut plain = guard.clone();
    plain.negated = false;
    guard_label(std::slice::from_ref(&plain))
}

/// What every arm of a switch is deciding on: the longest start they
/// share, cut at a word; '' when they share nothing.
fn subject_of(arms: &[String]) -> String {
    if arms.len() < 2 {
        return String::new();
    }
    let mut common: Vec<char> = arms[0].chars().collect();
    for arm in &arms[1..] {
        let shared = common
            .iter()
            .zip(arm.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        common.truncate(shared);
    }
    let common: String = common.into_iter().collect();
    common
        .trim()
        .chars()
        .take_while(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '_' | '$' | '.' | '?' | '[' | ']' | '\'' | '"')
        })
        .collect()
}

impl Reading<'_, '_> {
    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Vec::new());
        self.blocks.len() - 1
    }

    /// Whether this reading may open a function's body here.
    fn open(&mut self, function: &str, path: &[String]) -> bool {
        if path.iter().any(|seen| seen == function) || self.read.contains(function) {
            return false;
        }
        if path.len() >= MAX_INLINE || self.items >= MAX_ITEMS {
            self.truncated += 1;
            return false;
        }
        self.read.insert(function.to_string());
        true
    }

    /// One function's body, in the code's order.
    fn block_for(&mut self, function: &str, path: &[String]) -> BlockId {
        let root = self.new_block();
        let input = self.input;
        let mut sites: Vec<&ProgramSite> = input
            .sites
            .get(function)
            .map(|sites| sites.iter().collect())
            .unwrap_or_default();
        if sites.is_empty() {
            return root;
        }
        sort_sites(&mut sites);
        let mut stack: Vec<Open> = Vec::new();
        for site in sites {
            let scopes = scopes_of(site);
            // The longest run of open constructs the site still sits inside.
            let mut keep = 0;
            while keep < stack.len()
                && keep < scopes.len()
                && self.same_scope(&stack[keep], &scopes[keep])
            {
                keep += 1;
            }
            // The construct after it may still be the SAME decision taken
            // the other way: that keeps the fork and opens its other arm.
            let reopen = match (stack.get(keep), scopes.get(keep)) {
                (Some(open), Some(Scope::Guard { branch, guard }))
                    if open.fork.is_some() && open.branch == *branch =>
                {
                    Some((open.fork.expect("a fork"), *guard))
                }
                _ => None,
            };
            if let Some((fork, guard)) = reopen {
                stack.truncate(keep + 1);
                let body = self.arm_for(fork, guard);
                let open = &mut stack[keep];
                open.arm_key = arm_key(guard);
                open.body = body;
                keep += 1;
            } else {
                stack.truncate(keep);
            }
            for scope in &scopes[keep..] {
                let into = stack.last().map_or(root, |open| open.body);
                let open = self.open_scope(into, scope);
                stack.push(open);
            }
            if let Some(item) = self.item_for(site, path) {
                self.items += 1;
                let into = if scopes.is_empty() {
                    root
                } else {
                    stack[scopes.len() - 1].body
                };
                self.place(into, item, site);
            }
        }
        // An arm that answers the request, or whose code leaves, stops there.
        self.seal(root);
        root
    }

    fn same_scope(&self, open: &Open, scope: &Scope<'_>) -> bool {
        if open.branch != scope.branch() {
            return false;
        }
        match scope {
            Scope::Loop { .. } => open.fork.is_none(),
            Scope::Guard { guard, .. } => open.fork.is_some() && open.arm_key == arm_key(guard),
        }
    }

    /// Open a construct: a bracketed loop, or a fork with the arm this site
    /// is in.
    fn open_scope(&mut self, into: BlockId, scope: &Scope<'_>) -> Open {
        match scope {
            Scope::Loop { branch, site_loop } => {
                let body = self.new_block();
                self.blocks[into].push(Draft::Block {
                    block: BlockKind::Loop,
                    by: Some(site_loop.text.clone()),
                    loop_kind: Some(match site_loop.kind {
                        LoopKind::Each => "each",
                        LoopKind::While => "while",
                    }),
                    via: None,
                    within: None,
                    body,
                    again: false,
                });
                Open {
                    branch: branch.to_string(),
                    fork: None,
                    arm_key: String::new(),
                    body,
                }
            }
            Scope::Guard { branch, guard } => {
                let mut arms = Vec::new();
                // An early exit is a fork whose OTHER arm left before this
                // site could run: that arm is written first, and empty.
                if guard.form == GuardForm::Guard && guard.negated {
                    let body = self.new_block();
                    arms.push(DraftArm {
                        when: decision(guard),
                        not: false,
                        ends: Some(guard.exit.map_or(WireArmEnd::Return, WireArmEnd::from)),
                        body,
                    });
                }
                self.blocks[into].push(Draft::Fork {
                    on: decision(guard),
                    form: form_of(guard),
                    arms,
                });
                let fork = (into, self.blocks[into].len() - 1);
                let body = self.arm_for(fork, guard);
                Open {
                    branch: branch.to_string(),
                    fork: Some(fork),
                    arm_key: arm_key(guard),
                    body,
                }
            }
        }
    }

    /// An arm of a fork by its condition, reusing the one already open for
    /// it; its body's block.
    fn arm_for(&mut self, (block, index): (BlockId, usize), guard: &BranchGuard) -> BlockId {
        let when = guard_label(std::slice::from_ref(guard));
        if let Draft::Fork { arms, .. } = &self.blocks[block][index]
            && let Some(arm) = arms.iter().find(|arm| arm.when == when)
        {
            return arm.body;
        }
        let body = self.new_block();
        if let Draft::Fork { arms, .. } = &mut self.blocks[block][index] {
            arms.push(DraftArm {
                when,
                not: guard.negated,
                ends: guard.arm_exit.map(WireArmEnd::from),
                body,
            });
        }
        body
    }

    /// What one recorded site draws as.
    fn item_for(&mut self, site: &ProgramSite, path: &[String]) -> Option<Draft> {
        let input = self.input;
        if let Some(into) = &site.into {
            // A helper the walk folded: drawn where it is called, its own
            // body inside.
            let via = (input.node)(into);
            if !self.open(into, path) {
                let body = self.new_block();
                return Some(Draft::Block {
                    block: BlockKind::Inline,
                    by: None,
                    loop_kind: None,
                    via,
                    within: site.within.clone(),
                    body,
                    again: true,
                });
            }
            let mut deeper = path.to_vec();
            deeper.push(into.clone());
            let body = self.block_for(into, &deeper);
            if self.blocks[body].is_empty() {
                return None;
            }
            return Some(Draft::Block {
                block: BlockKind::Inline,
                by: None,
                loop_kind: None,
                via,
                within: site.within.clone(),
                body,
                again: false,
            });
        }
        let step = site.step.as_ref()?;
        let facts = (input.step)(step);
        let mut body = None;
        let mut again = false;
        // A step the walk entered reads on into what it does.
        if let Some(into) = facts.and_then(|facts| facts.into) {
            if self.open(&into, path) {
                let mut deeper = path.to_vec();
                deeper.push(into.clone());
                let block = self.block_for(&into, &deeper);
                if !self.blocks[block].is_empty() {
                    body = Some(block);
                }
            } else if input.sites.contains_key(&into) {
                again = true;
            }
        }
        Some(Draft::Step {
            step: step.clone(),
            link: site.link.clone(),
            within: site.within.clone(),
            body,
            again,
        })
    }

    /// Put an item in its block, opening the run it belongs to.
    fn place(&mut self, block: BlockId, item: Draft, site: &ProgramSite) {
        let Some((kind, by)) = run_for(site) else {
            self.blocks[block].push(item);
            return;
        };
        if let Some(Draft::Block {
            block: last_kind,
            by: last_by,
            body,
            ..
        }) = self.blocks[block].last()
            && *last_kind == kind
            && last_by.as_deref() == Some(by.as_str())
        {
            let body = *body;
            self.blocks[body].push(item);
            return;
        }
        let body = self.new_block();
        self.blocks[body].push(item);
        self.blocks[block].push(Draft::Block {
            block: kind,
            by: Some(by),
            loop_kind: None,
            via: None,
            within: None,
            body,
            again: false,
        });
    }

    /// How each arm ends, decided after its body is known: an arm whose last
    /// item answers the request ends with the reply.
    fn seal(&mut self, block: BlockId) {
        for index in 0..self.blocks[block].len() {
            let (children, is_switch): (Vec<BlockId>, bool) = match &self.blocks[block][index] {
                Draft::Fork { arms, form, .. } => (
                    arms.iter().map(|arm| arm.body).collect(),
                    *form == ForkForm::Switch,
                ),
                Draft::Block { body, .. } => (vec![*body], false),
                Draft::Step {
                    body: Some(body), ..
                } => (vec![*body], false),
                Draft::Step { body: None, .. } => (Vec::new(), false),
            };
            for child in &children {
                self.seal(*child);
            }
            let replies: Vec<bool> = children
                .iter()
                .map(|child| self.replies_last(*child))
                .collect();
            if let Draft::Fork { on, arms, .. } = &mut self.blocks[block][index] {
                // A switch's head is the thing being decided on, which only
                // its arms together say.
                if is_switch {
                    *on = subject_of(&arms.iter().map(|arm| arm.when.clone()).collect::<Vec<_>>());
                }
                for (arm, replies) in arms.iter_mut().zip(replies) {
                    if replies {
                        arm.ends = Some(WireArmEnd::Reply);
                    }
                }
            }
        }
    }

    /// Whether the last thing a block does is answer the request.
    fn replies_last(&self, block: BlockId) -> bool {
        match self.blocks[block].last() {
            Some(Draft::Step { step, .. }) => {
                (self.input.step)(step).is_some_and(|facts| facts.reply)
            }
            Some(Draft::Block { body, .. }) => self.replies_last(*body),
            _ => false,
        }
    }

    /// The finished tree of a block.
    fn finish(&mut self, block: BlockId) -> WireBlock {
        let drafts = std::mem::take(&mut self.blocks[block]);
        drafts
            .into_iter()
            .map(|draft| match draft {
                Draft::Step {
                    step,
                    link,
                    within,
                    body,
                    again,
                } => WireItem::Step {
                    step,
                    link,
                    within,
                    body: body.map(|body| self.finish(body)),
                    again,
                },
                Draft::Fork { on, form, arms } => WireItem::Fork {
                    on,
                    form,
                    arms: arms
                        .into_iter()
                        .map(|arm| WireArm {
                            when: arm.when,
                            not: arm.not,
                            ends: arm.ends,
                            body: self.finish(arm.body),
                        })
                        .collect(),
                },
                Draft::Block {
                    block,
                    by,
                    loop_kind,
                    via,
                    within,
                    body,
                    again,
                } => WireItem::Block {
                    block,
                    by,
                    loop_kind,
                    via,
                    within,
                    body: self.finish(body),
                    again,
                },
            })
            .collect()
    }
}

/// The run a site belongs to — registered to run later, started together —
/// or `None` for plain sequence.
fn run_for(site: &ProgramSite) -> Option<(BlockKind, String)> {
    if let Some(trigger) = &site.trigger
        && trigger.kind == "callback"
        && LATER_OF.contains(&trigger.name.as_str())
    {
        return Some((BlockKind::Later, trigger.name.clone()));
    }
    let within = site.within.as_deref()?;
    TOGETHER
        .contains(&within)
        .then(|| (BlockKind::Together, within.to_string()))
}

#[cfg(test)]
mod tests {
    //! Ported from upstream `__tests__/ui-steps-program.test.ts:75-270`
    //! (`e8ddc4e2`): the fold over hand-made records, read back as one line
    //! per item, indented — the shapes are upstream's.
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static NEXT_LINE: Cell<i64> = const { Cell::new(1) };
    }

    fn next_line() -> i64 {
        NEXT_LINE.with(|line| {
            let current = line.get();
            line.set(current + 1);
            current
        })
    }

    /// A guard, with the fields the fold reads.
    fn g(text: &str) -> BranchGuard {
        BranchGuard {
            text: text.to_string(),
            negated: false,
            form: GuardForm::If,
            line: 1,
            branch: format!("b:{text}"),
            arm_exit: None,
            exit: None,
        }
    }

    fn negated(mut guard: BranchGuard, form: GuardForm) -> BranchGuard {
        guard.negated = true;
        guard.form = form;
        guard
    }

    fn with_branch(mut guard: BranchGuard, branch: &str) -> BranchGuard {
        guard.branch = branch.to_string();
        guard
    }

    fn site_at(line: i64) -> SiteAt {
        SiteAt {
            line,
            column: 0,
            end: SitePosition { line, column: 40 },
        }
    }

    /// A site at the next line, reaching a step.
    fn at(step: &str, guards: Vec<BranchGuard>) -> ProgramSite {
        ProgramSite {
            step: Some(step.to_string()),
            link: Some(format!("l:{step}")),
            into: None,
            at: site_at(next_line()),
            within: None,
            guards,
            loops: Vec::new(),
            trigger: None,
        }
    }

    /// A site that folds into a helper.
    fn into(function: &str) -> ProgramSite {
        ProgramSite {
            step: None,
            link: None,
            into: Some(function.to_string()),
            at: site_at(next_line()),
            within: None,
            guards: Vec::new(),
            loops: Vec::new(),
            trigger: None,
        }
    }

    fn site_loop(text: &str, branch: &str) -> SiteLoop {
        SiteLoop {
            text: text.to_string(),
            kind: LoopKind::Each,
            branch: branch.to_string(),
        }
    }

    fn node_ref(id: &str) -> WireNodeRef {
        WireNodeRef {
            id: id.to_string(),
            kind: "function",
            name: id.to_string(),
            qualified_name: id.to_string(),
            file: "a.ts".to_string(),
            line: 1,
            end_line: 2,
            language: "typescript",
            signature: None,
            exported: None,
            test: false,
            generated: None,
        }
    }

    fn program_with(
        sites: Vec<(&str, Vec<ProgramSite>)>,
        replies: &[&str],
        into: &[(&str, &str)],
    ) -> Option<WireProgram> {
        let sites: HashMap<String, Vec<ProgramSite>> = sites
            .into_iter()
            .map(|(id, sites)| (id.to_string(), sites))
            .collect();
        let node = |id: &str| Some(node_ref(id));
        let step = |id: &str| {
            Some(StepFacts {
                reply: replies.contains(&id),
                into: into
                    .iter()
                    .find(|(step, _)| *step == id)
                    .map(|(_, function)| function.to_string()),
            })
        };
        build_program(&ProgramInput {
            sites: &sites,
            root: Some("root".to_string()),
            node: &node,
            step: &step,
        })
    }

    /// Build the sites (fresh line numbers), then the program.
    fn program(
        build: impl FnOnce() -> Vec<(&'static str, Vec<ProgramSite>)>,
        replies: &[&str],
        into: &[(&str, &str)],
    ) -> Option<WireProgram> {
        NEXT_LINE.with(|line| line.set(1));
        program_with(build(), replies, into)
    }

    /// The shape of a block, one line per item, indented.
    fn shape(block: &WireBlock, indent: &str) -> Vec<String> {
        let mut out = Vec::new();
        let deeper = format!("{indent}  ");
        for item in block {
            match item {
                WireItem::Step {
                    step,
                    within,
                    body,
                    again,
                    ..
                } => {
                    let again = if *again { " (again)" } else { "" };
                    let within = within
                        .as_ref()
                        .map(|within| format!(" inside {within}"))
                        .unwrap_or_default();
                    out.push(format!("{indent}{step}{again}{within}"));
                    if let Some(body) = body {
                        out.extend(shape(body, &deeper));
                    }
                }
                WireItem::Fork { on, form, arms } => {
                    let form = match form {
                        ForkForm::If => "if",
                        ForkForm::Switch => "switch",
                        ForkForm::Ternary => "ternary",
                        ForkForm::Try => "try",
                    };
                    out.push(format!("{indent}{form} {on}"));
                    for arm in arms {
                        let ends = arm
                            .ends
                            .map(|ends| {
                                format!(
                                    " ends:{}",
                                    match ends {
                                        WireArmEnd::Reply => "reply",
                                        WireArmEnd::Return => "return",
                                        WireArmEnd::Throw => "throw",
                                        WireArmEnd::Exit => "exit",
                                    }
                                )
                            })
                            .unwrap_or_default();
                        out.push(format!("{indent}  arm {}{ends}", arm.when));
                        out.extend(shape(&arm.body, &format!("{indent}    ")));
                    }
                }
                WireItem::Block {
                    block,
                    by,
                    via,
                    body,
                    again,
                    ..
                } => {
                    let kind = match block {
                        BlockKind::Inline => "inline",
                        BlockKind::Loop => "loop",
                        BlockKind::Later => "later",
                        BlockKind::Together => "together",
                    };
                    let label = match (via, by) {
                        (Some(via), _) => format!(" via {}", via.name),
                        (None, Some(by)) => format!(" {by}"),
                        (None, None) => String::new(),
                    };
                    let again = if *again { " (again)" } else { "" };
                    out.push(format!("{indent}{kind}{label}{again}"));
                    out.extend(shape(body, &deeper));
                }
                WireItem::Cut { why } => {
                    out.push(format!(
                        "{indent}cut {}",
                        match why {
                            CutWhy::Folded => "folded",
                            CutWhy::Depth => "depth",
                        }
                    ));
                }
            }
        }
        out
    }

    fn lines(program: &Option<WireProgram>) -> Vec<String> {
        shape(&program.as_ref().expect("a program").root, "")
    }

    #[test]
    fn reads_a_straight_line_in_the_codes_order() {
        let p = program(
            || {
                vec![(
                    "root",
                    vec![at("a", vec![]), at("b", vec![]), at("c", vec![])],
                )]
            },
            &[],
            &[],
        );
        assert_eq!(lines(&p), ["a", "b", "c"]);
    }

    #[test]
    fn is_nothing_when_the_anchor_has_no_body_to_read() {
        assert_eq!(program(Vec::new, &[], &[]), None);
        let sites = HashMap::new();
        let node = |_: &str| None;
        let step = |_: &str| None;
        assert_eq!(
            build_program(&ProgramInput {
                sites: &sites,
                root: None,
                node: &node,
                step: &step,
            }),
            None
        );
    }

    #[test]
    fn makes_an_if_and_its_else_two_arms_of_one_fork() {
        let cond = "user && ok";
        let p = program(
            || {
                vec![(
                    "root",
                    vec![
                        at("lookup", vec![]),
                        at("sign", vec![g(cond)]),
                        at("200", vec![g(cond)]),
                        at("401", vec![negated(g(cond), GuardForm::Else)]),
                    ],
                )]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "lookup",
                "if user && ok",
                "  arm user && ok",
                "    sign",
                "    200",
                "  arm !(user && ok)",
                "    401",
            ]
        );
        let WireItem::Fork { arms, .. } = &p.as_ref().unwrap().root[1] else {
            panic!("a fork");
        };
        assert_eq!(arms.len(), 2);
    }

    #[test]
    fn ends_an_arm_that_answers_the_request() {
        let p = program(
            || {
                vec![(
                    "root",
                    vec![
                        at("200", vec![g("user")]),
                        at("401", vec![negated(g("user"), GuardForm::Else)]),
                    ],
                )]
            },
            &["200", "401"],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "if user",
                "  arm user ends:reply",
                "    200",
                "  arm !user ends:reply",
                "    401",
            ]
        );
    }

    #[test]
    fn draws_an_early_exit_as_the_forks_other_arm_with_how_it_leaves() {
        let p = program(
            || {
                let mut exit = negated(with_branch(g("!product"), "b:1"), GuardForm::Guard);
                exit.exit = Some(GuardExit::Throw);
                vec![(
                    "root",
                    vec![
                        at("404", vec![with_branch(g("!product"), "b:1")]),
                        at("save", vec![exit]),
                    ],
                )]
            },
            &["404"],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "if !product",
                "  arm !product ends:reply",
                "    404",
                "  arm product",
                "    save",
            ]
        );
    }

    #[test]
    fn draws_an_early_exit_whose_arm_holds_nothing_as_a_terminal() {
        let p = program(
            || {
                let mut exit = negated(g("busy"), GuardForm::Guard);
                exit.exit = Some(GuardExit::Return);
                vec![("root", vec![at("go", vec![exit])])]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            ["if busy", "  arm busy ends:return", "  arm !busy", "    go"]
        );
    }

    #[test]
    fn nests_forks_the_way_the_code_nests_them() {
        let p = program(
            || {
                let outer = with_branch(g("product"), "b:outer");
                let inner = with_branch(g("reviewed"), "b:inner");
                let mut inner_exit = negated(inner.clone(), GuardForm::Guard);
                inner_exit.exit = Some(GuardExit::Throw);
                vec![(
                    "root",
                    vec![
                        at("400", vec![outer.clone(), inner]),
                        at("201", vec![outer.clone(), inner_exit]),
                        at("404", vec![negated(outer, GuardForm::Else)]),
                    ],
                )]
            },
            &["400", "201", "404"],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "if product",
                "  arm product",
                "    if reviewed",
                "      arm reviewed ends:reply",
                "        400",
                "      arm !reviewed ends:reply",
                "        201",
                "  arm !product ends:reply",
                "    404",
            ]
        );
    }

    #[test]
    fn puts_every_case_of_one_switch_under_one_fork() {
        let p = program(
            || {
                let case = |text: &str| {
                    let mut guard = with_branch(g(text), "b:switch");
                    guard.form = GuardForm::Case;
                    guard
                };
                vec![(
                    "root",
                    vec![
                        at("a", vec![case("kind === 'a'")]),
                        at("b", vec![case("kind === 'b'")]),
                        at("d", vec![case("kind: default")]),
                    ],
                )]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "switch kind",
                "  arm kind === 'a'",
                "    a",
                "  arm kind === 'b'",
                "    b",
                "  arm kind: default",
                "    d",
            ]
        );
    }

    #[test]
    fn keeps_two_try_catch_blocks_apart() {
        let p = program(
            || {
                let catch = |branch: &str| {
                    let mut guard = with_branch(g("on error"), branch);
                    guard.form = GuardForm::Catch;
                    guard
                };
                vec![(
                    "root",
                    vec![
                        at("first", vec![catch("b:try1")]),
                        at("second", vec![catch("b:try2")]),
                    ],
                )]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "try on error",
                "  arm on error",
                "    first",
                "try on error",
                "  arm on error",
                "    second",
            ]
        );
    }

    #[test]
    fn draws_a_folded_helper_where_it_is_called_and_says_what_it_is_inside() {
        let p = program(
            || {
                let mut call = into("helper");
                call.within = Some("res.json".to_string());
                vec![
                    ("root", vec![call, at("200", vec![])]),
                    ("helper", vec![at("sign", vec![])]),
                ]
            },
            &[],
            &[],
        );
        assert_eq!(lines(&p), ["inline via helper", "  sign", "200"]);
        let WireItem::Block { within, via, .. } = &p.as_ref().unwrap().root[0] else {
            panic!("a block");
        };
        assert_eq!(within.as_deref(), Some("res.json"));
        assert_eq!(via.as_ref().map(|via| via.name.as_str()), Some("helper"));
    }

    #[test]
    fn puts_a_call_written_inside_another_calls_arguments_first() {
        let reply = ProgramSite {
            step: Some("200".to_string()),
            link: None,
            into: None,
            at: SiteAt {
                line: 14,
                column: 4,
                end: SitePosition {
                    line: 21,
                    column: 6,
                },
            },
            within: None,
            guards: Vec::new(),
            loops: Vec::new(),
            trigger: None,
        };
        let signed = ProgramSite {
            step: Some("sign".to_string()),
            at: SiteAt {
                line: 19,
                column: 13,
                end: SitePosition {
                    line: 19,
                    column: 34,
                },
            },
            ..reply.clone()
        };
        let p = program(move || vec![("root", vec![reply, signed])], &[], &[]);
        assert_eq!(lines(&p), ["sign", "200"]);
    }

    #[test]
    fn reads_a_function_once_however_many_times_it_is_called() {
        let p = program(
            || {
                vec![
                    (
                        "root",
                        vec![into("helper"), at("x", vec![]), into("helper")],
                    ),
                    ("helper", vec![at("work", vec![])]),
                ]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "inline via helper",
                "  work",
                "x",
                "inline via helper (again)"
            ]
        );
    }

    #[test]
    fn reads_on_into_a_step_the_walk_entered_and_stops_at_one_it_did_not() {
        let entered = program(
            || {
                vec![
                    ("root", vec![at("store", vec![])]),
                    ("storeFn", vec![at("write", vec![])]),
                ]
            },
            &[],
            &[("store", "storeFn")],
        );
        assert_eq!(lines(&entered), ["store", "  write"]);
        let boundary = program(
            || {
                vec![
                    ("root", vec![at("store", vec![])]),
                    ("storeFn", vec![at("write", vec![])]),
                ]
            },
            &[],
            &[],
        );
        assert_eq!(lines(&boundary), ["store"]);
    }

    #[test]
    fn says_a_helper_that_calls_itself_was_already_read() {
        let p = program(
            || {
                vec![
                    ("root", vec![into("a")]),
                    ("a", vec![at("x", vec![]), into("a")]),
                ]
            },
            &[],
            &[],
        );
        assert_eq!(lines(&p), ["inline via a", "  x", "  inline via a (again)"]);
    }

    #[test]
    fn puts_work_registered_to_run_later_in_a_block_of_its_own() {
        let p = program(
            || {
                let now = at("now", vec![]);
                let mut later = at("afterwards", vec![]);
                later.trigger = Some(ProgramTrigger {
                    kind: "callback".to_string(),
                    name: "then".to_string(),
                });
                vec![("root", vec![now, later])]
            },
            &[],
            &[],
        );
        assert_eq!(lines(&p), ["now", "later then", "  afterwards"]);
    }

    #[test]
    fn puts_calls_started_together_in_one_block() {
        let p = program(
            || {
                let together = |step: &str| {
                    let mut site = at(step, vec![]);
                    site.within = Some("Promise.all".to_string());
                    site
                };
                vec![("root", vec![together("a"), together("b"), at("c", vec![])])]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "together Promise.all",
                "  a inside Promise.all",
                "  b inside Promise.all",
                "c",
            ]
        );
    }

    #[test]
    fn says_a_run_of_calls_happens_once_per_item() {
        let p = program(
            || {
                let before = at("before", vec![]);
                let mut each = at("each", vec![]);
                each.loops = vec![site_loop("item of items", "l:item of items")];
                vec![("root", vec![before, each, at("after", vec![])])]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            ["before", "loop item of items", "  each", "after"]
        );
    }

    #[test]
    fn nests_a_loop_and_a_fork_by_which_one_is_written_outside_the_other() {
        let p = program(
            || {
                let mut go = at("go", vec![with_branch(g("ready"), "9:4")]);
                go.loops = vec![site_loop("item of items", "8:2")];
                vec![("root", vec![go])]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            [
                "loop item of items",
                "  if ready",
                "    arm ready",
                "      go"
            ]
        );
        let q = program(
            || {
                let mut go = at("go", vec![with_branch(g("ready"), "8:2")]);
                go.loops = vec![site_loop("item of items", "9:4")];
                vec![("root", vec![go])]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&q),
            [
                "if ready",
                "  arm ready",
                "    loop item of items",
                "      go"
            ]
        );
    }

    #[test]
    fn closes_a_fork_when_the_code_leaves_it() {
        let p = program(
            || {
                vec![(
                    "root",
                    vec![at("inside", vec![g("ready")]), at("after", vec![])],
                )]
            },
            &[],
            &[],
        );
        assert_eq!(
            lines(&p),
            ["if ready", "  arm ready", "    inside", "after"]
        );
    }
}
