//! The supersession graph at query time (M5 §2.3, §3.4, §3.5).
//!
//! `Snapshot::edges` is a flat build-time table. Every reader that asks "who
//! replaced whom" needs the same symmetric closure, the same deterministic
//! cycle break and the same cross-scope rule. This module owns all three, and
//! [`Graph::for_snapshot`] is the one function that turns a generation into a
//! graph. `janus index --json` reports its counts and the readers traverse it,
//! so a second traversal elsewhere would let the two answers drift apart.
//!
//! `predecessors` are the documents this one replaced; `successors` are the
//! documents that replace it. The names follow time, not the edge, because every
//! reader asks a temporal question (R2-3).
//!
//! Everything here is pure and reads no clock. An edge whose scopes do not
//! intersect is stored in the snapshot and **inert** here: it contributes no
//! relation, so it cannot close a window or change a status (M5-8).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use crate::config::Config;
use crate::document::Status;
use crate::id::DocId;
use crate::index::{Edge, Snapshot};
use crate::score::ScopeFilter;

/// The scope a document has when it declares none (§6.12).
const GLOBAL_SCOPE: &str = "global";

/// One resolved relation, `(from, to)`.
///
/// The unit the graph keeps, sorts and drops. Named once because a `BTreeSet` of
/// it appears in every stage: scope filtering, cycle breaking and reporting.
type Pair = (DocId, DocId);

/// The facts one endpoint of an edge carries, live or retired.
///
/// The graph owns them, so one builder can serve every caller from the snapshot
/// alone (M5-28). Only what a traversal, a window or a status rule reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    /// The document's id.
    pub id: DocId,
    /// Vault-relative path, `/`-separated. The cycle rule compares these.
    pub rel_path: String,
    /// Owning scope, `global` when absent.
    pub scope: String,
    /// Retrieval tags; part of the scope set `S(d)` (§6.12 rule 3).
    pub shared_with: Vec<String>,
    /// `effective_from ?? date`, or a tombstone's last valid start.
    pub valid_from: Option<NaiveDate>,
    /// The declared valid-time end, exclusive. A tombstone records none.
    pub effective_to: Option<NaiveDate>,
    /// The declared status, or `None` for a retired document, which has no
    /// current metadata to read (R4-1).
    pub declared_status: Option<Status>,
}

/// A node endpoint: a live document, or a retired one that keeps its place in
/// the graph so it can still close a predecessor's window (R2-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A document this generation indexed.
    Live(NodeFacts),
    /// A document that vanished and left a tombstone.
    Retired(NodeFacts),
}

impl Endpoint {
    /// The facts the graph reads, whichever state the endpoint is in.
    #[must_use]
    pub fn facts(&self) -> &NodeFacts {
        match self {
            Self::Live(facts) | Self::Retired(facts) => facts,
        }
    }
}

/// One document's place in the graph, after closure and cycle breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The document this node describes.
    pub id: DocId,
    /// The documents this one replaced.
    pub predecessors: Vec<DocId>,
    /// The documents that replace this one.
    pub successors: Vec<DocId>,
}

impl Node {
    fn new(id: DocId) -> Self {
        Self {
            id,
            predecessors: Vec::new(),
            successors: Vec::new(),
        }
    }
}

/// The resolved graph for one generation.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    nodes: BTreeMap<DocId, Node>,
    endpoints: BTreeMap<DocId, Endpoint>,
    ignored: Vec<Pair>,
    inert: BTreeSet<Pair>,
}

impl Graph {
    /// Builds the closed, cycle-broken graph from resolved edges.
    ///
    /// `endpoints` must hold one row per live document and one per tombstone,
    /// so an edge to a retired document still has a node. `allow_disjoint`
    /// makes a disjoint edge effective and silences the scope rule (§3.5,
    /// M5-28).
    #[must_use]
    pub fn build(
        edges: &[Edge],
        endpoints: impl IntoIterator<Item = Endpoint>,
        allow_disjoint: bool,
    ) -> Self {
        let mut facts: BTreeMap<DocId, Endpoint> = BTreeMap::new();
        let mut nodes: BTreeMap<DocId, Node> = BTreeMap::new();
        for endpoint in endpoints {
            let id = endpoint.facts().id;
            nodes.entry(id).or_insert_with(|| Node::new(id));
            facts.insert(id, endpoint);
        }

        let mut kept: BTreeSet<Pair> = BTreeSet::new();
        let mut inert: BTreeSet<Pair> = BTreeSet::new();
        for edge in edges {
            let pair = (edge.from, edge.to);
            if allow_disjoint || effective(&facts, pair) {
                kept.insert(pair);
            } else {
                inert.insert(pair);
            }
        }

        let (kept, ignored) = break_cycles(kept, &facts);
        let mut graph = Self {
            nodes,
            endpoints: facts,
            ignored,
            inert,
        };
        for &(from, to) in &kept {
            // Both endpoints got a node above; a missing one would mean an edge
            // named an id the generation does not hold, which resolution cannot
            // produce. Creating the node keeps the build total.
            graph
                .nodes
                .entry(from)
                .or_insert_with(|| Node::new(from))
                .successors
                .push(to);
            graph
                .nodes
                .entry(to)
                .or_insert_with(|| Node::new(to))
                .predecessors
                .push(from);
        }
        // Edges are visited in `(from, to)` order, so both lists arrive sorted.
        // Sorting anyway makes the invariant local and survives a future change
        // to the iteration order.
        for node in graph.nodes.values_mut() {
            node.predecessors.sort_unstable();
            node.successors.sort_unstable();
        }
        graph
    }

    /// The graph of one published generation.
    ///
    /// The one place both `janus index --json` and a read build a graph: the
    /// counts and the answers cannot disagree while only one function decides
    /// what a node is.
    #[must_use]
    pub fn for_snapshot(snapshot: &Snapshot, config: &Config) -> Self {
        let mut endpoints = Vec::with_capacity(snapshot.docs.len() + snapshot.tombstones.len());
        for doc in &snapshot.docs {
            endpoints.push(Endpoint::Live(NodeFacts {
                id: doc.id,
                rel_path: text(snapshot, doc.rel_path),
                scope: text(snapshot, doc.scope),
                shared_with: texts(snapshot, &doc.shared_with),
                valid_from: doc.effective_from.or(doc.date),
                effective_to: doc.effective_to,
                declared_status: Some(doc.status),
            }));
        }
        for meta in &snapshot.tombstones {
            // A retired path is the last entry of its history; nothing else
            // stores it (§2.1, R2-5).
            let rel_path = snapshot
                .path_history
                .get(&meta.id)
                .and_then(|entries| entries.last())
                .map(|entry| entry.rel_path.clone())
                .unwrap_or_default();
            endpoints.push(Endpoint::Retired(NodeFacts {
                id: meta.id,
                rel_path,
                scope: meta.scope.clone(),
                shared_with: meta.shared_with.clone(),
                valid_from: meta.valid_from,
                // A tombstone holds no valid-time end and no declared status.
                effective_to: None,
                declared_status: None,
            }));
        }
        Self::build(
            &snapshot.edges,
            endpoints,
            config.links.allow_disjoint_supersession,
        )
    }

    /// One node, live or retired, when the generation holds it.
    #[must_use]
    pub fn node(&self, id: DocId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Every node, keyed by id, in id order.
    #[must_use]
    pub fn nodes(&self) -> &BTreeMap<DocId, Node> {
        &self.nodes
    }

    /// The endpoint behind one node, so a caller reads scope and dates from one
    /// place whether the document is live or retired.
    #[must_use]
    pub fn endpoint(&self, id: DocId) -> Option<&Endpoint> {
        self.endpoints.get(&id)
    }

    /// One [`Window`] per node, resolved for one query scope set (§3.6).
    ///
    /// The query computes this map once, before its row loop, and never walks
    /// the successors again: a window per row would rebuild the same answer for
    /// every document on every query (P3 scope 10).
    #[must_use]
    pub fn windows(&self, scopes: &ScopeFilter) -> BTreeMap<DocId, Window> {
        self.nodes
            .keys()
            .filter_map(|id| {
                let facts = self.endpoint(*id)?.facts();
                Some((*id, Window::of(facts, self, scopes)))
            })
            .collect()
    }

    /// One [`EffectiveStatus`] per live node, resolved for one query scope set
    /// (§3.7). A retired node has no status to report, so it has no entry.
    #[must_use]
    pub fn effective_statuses(&self, scopes: &ScopeFilter) -> BTreeMap<DocId, EffectiveStatus> {
        self.nodes
            .keys()
            .filter_map(|id| {
                let facts = self.endpoint(*id)?.facts();
                Some((*id, EffectiveStatus::of(facts, self, scopes)?))
            })
            .collect()
    }

    /// The edges the cycle breaker ignored, sorted. They stay in the snapshot.
    #[must_use]
    pub fn ignored(&self) -> &[Pair] {
        &self.ignored
    }

    /// The edges that are stored but contribute no relation (§3.5).
    #[must_use]
    pub fn inert(&self) -> &BTreeSet<Pair> {
        &self.inert
    }
}

/// True when the edge is effective: `S(A) ∩ S(B) ≠ ∅`, or one side is global.
fn effective(endpoints: &BTreeMap<DocId, Endpoint>, (from, to): Pair) -> bool {
    let (Some(left), Some(right)) = (endpoints.get(&from), endpoints.get(&to)) else {
        // An edge whose endpoint the generation cannot describe proves no
        // disjointness, so it stays effective. Resolution makes this
        // unreachable, and a hidden edge would be worse than an extra one.
        return true;
    };
    let left = left.facts();
    let right = right.facts();
    // `S(d)` is the scope and the tags together (§6.12 rule 3), so the test is
    // an exact set intersection, not just an equality of scopes.
    left.scope == right.scope
        || left.scope == GLOBAL_SCOPE
        || right.scope == GLOBAL_SCOPE
        || right.shared_with.contains(&left.scope)
        || left.shared_with.contains(&right.scope)
        || left
            .shared_with
            .iter()
            .any(|tag| right.shared_with.contains(tag))
}

/// A document's valid-time interval, resolved for one query scope set (§3.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Window {
    /// `effective_from ?? date`, or a tombstone's last valid start.
    pub valid_from: Option<NaiveDate>,
    /// Exclusive. The declared end, clamped by the earliest matching successor.
    pub valid_to: Option<NaiveDate>,
    /// True when `valid_to <= valid_from`. The window is then empty.
    pub invalid: bool,
}

impl Window {
    /// Resolves the window for one document against one query scope set.
    ///
    /// `valid_to = min(effective_to, min over matching successors of their
    /// valid_from)`. An inert edge contributes nothing because [`Graph`] never
    /// lists it as a successor. A successor with no date closes no window: the
    /// document is still `Superseded`, and an `--as-of` query still returns it,
    /// because the index holds no date to close the window with (§3.6).
    #[must_use]
    pub fn of(facts: &NodeFacts, graph: &Graph, scopes: &ScopeFilter) -> Self {
        let mut valid_to = facts.effective_to;
        for successor in graph
            .node(facts.id)
            .map_or(&[][..], |node| &node.successors)
        {
            let Some(facts) = graph.endpoint(*successor).map(Endpoint::facts) else {
                continue;
            };
            if !scopes.matches(&facts.scope, &facts.shared_with) {
                continue;
            }
            let Some(start) = facts.valid_from else {
                continue;
            };
            valid_to = Some(valid_to.map_or(start, |current| current.min(start)));
        }

        let invalid = matches!((facts.valid_from, valid_to), (Some(from), Some(to)) if to <= from);
        Self {
            valid_from: facts.valid_from,
            valid_to,
            invalid,
        }
    }

    /// Whether this window contains `date`: `valid_from <= date < valid_to`.
    ///
    /// A document with no `valid_from` is never inside a window, so an
    /// `--as-of` query excludes it (§3.8). An invalid window contains nothing.
    #[must_use]
    pub fn contains(&self, date: NaiveDate) -> bool {
        let Some(from) = self.valid_from else {
            return false;
        };
        from <= date && self.valid_to.is_none_or(|to| date < to)
    }

    /// Whether this window contains `today`, which is `current`'s gate (§3.7).
    ///
    /// A missing `valid_from` passes: §6.10 rule 9 keeps an undated document in
    /// `current`, and the eval set asserts it (R-4). An invalid window fails
    /// both comparisons by construction.
    #[must_use]
    pub fn covers_today(&self, today: NaiveDate) -> bool {
        self.valid_from.is_none_or(|from| from <= today)
            && self.valid_to.is_none_or(|to| today < to)
    }
}

/// The status a reader must use. Not persisted, and never a sixth value.
///
/// There is no `expired` variant: expiry is a window property and nothing else
/// (§6.10 rule 5, M5-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveStatus {
    /// Current and in force.
    Active,
    /// Work in progress.
    Draft,
    /// Retired with no successor.
    Deprecated,
    /// Replaced by an effective successor, or declared superseded.
    Superseded,
    /// Declared value the corpus does not know.
    Unknown,
}

impl EffectiveStatus {
    /// Resolves the status for one document against one query scope set.
    ///
    /// A document is `Superseded` when the graph gives it at least one successor
    /// that matches `Q`; otherwise it keeps its declared status. `None` means a
    /// retired endpoint, which has no current metadata to read (R4-1).
    #[must_use]
    pub fn of(facts: &NodeFacts, graph: &Graph, scopes: &ScopeFilter) -> Option<Self> {
        let declared = Self::from(facts.declared_status?);
        let superseded = graph
            .node(facts.id)
            .map_or(&[][..], |node| &node.successors)
            .iter()
            .filter_map(|id| graph.endpoint(*id).map(Endpoint::facts))
            .any(|facts| scopes.matches(&facts.scope, &facts.shared_with));
        Some(if superseded {
            Self::Superseded
        } else {
            declared
        })
    }

    /// The stable lowercase name used in JSON output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draft => "draft",
            Self::Deprecated => "deprecated",
            Self::Superseded => "superseded",
            Self::Unknown => "unknown",
        }
    }
}

impl From<EffectiveStatus> for Status {
    fn from(status: EffectiveStatus) -> Self {
        match status {
            EffectiveStatus::Active => Self::Active,
            EffectiveStatus::Draft => Self::Draft,
            EffectiveStatus::Deprecated => Self::Deprecated,
            EffectiveStatus::Superseded => Self::Superseded,
            EffectiveStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<Status> for EffectiveStatus {
    fn from(status: Status) -> Self {
        match status {
            Status::Active => Self::Active,
            Status::Draft => Self::Draft,
            Status::Deprecated => Self::Deprecated,
            Status::Superseded => Self::Superseded,
            Status::Unknown => Self::Unknown,
        }
    }
}

/// Removes one edge per cycle until the graph is acyclic.
///
/// The dropped edge is the one whose `(from path, to path)` byte pair sorts
/// greatest on the cycle (M5-7). Paths rather than ids, so the rule is readable
/// in `doctor` output and hand-derivable in a test.
fn break_cycles(
    mut edges: BTreeSet<Pair>,
    endpoints: &BTreeMap<DocId, Endpoint>,
) -> (BTreeSet<Pair>, Vec<Pair>) {
    let mut ignored = Vec::new();
    while let Some(cycle) = find_cycle(&edges) {
        let path_of = |id: DocId| {
            endpoints
                .get(&id)
                .map_or("", |endpoint| endpoint.facts().rel_path.as_str())
        };
        let key = |pair: Pair| (path_of(pair.0), path_of(pair.1));
        let Some(victim) = cycle
            .iter()
            .copied()
            .max_by(|left, right| key(*left).cmp(&key(*right)))
        else {
            break;
        };
        edges.remove(&victim);
        ignored.push(victim);
    }
    ignored.sort_unstable();
    (edges, ignored)
}

/// One directed cycle, as the pairs that close it. `None` when acyclic.
///
/// An iterative depth-first search: the stack holds the open path, and a grey
/// neighbour is the edge that closes a cycle. Recursion would risk the stack on
/// a pathological vault, and every traversal in this milestone is iterative.
fn find_cycle(edges: &BTreeSet<Pair>) -> Option<Vec<Pair>> {
    let mut successors: BTreeMap<DocId, Vec<DocId>> = BTreeMap::new();
    for &(from, to) in edges {
        successors.entry(from).or_default().push(to);
    }

    // 1 = on the open path, 2 = finished.
    let mut colour: BTreeMap<DocId, u8> = BTreeMap::new();
    for &start in successors.keys() {
        if colour.contains_key(&start) {
            continue;
        }
        let mut stack: Vec<(DocId, usize)> = vec![(start, 0)];
        let mut path: Vec<DocId> = vec![start];
        colour.insert(start, 1);
        while let Some(&(node, index)) = stack.last() {
            let neighbours = successors.get(&node).map_or(&[][..], Vec::as_slice);
            let Some(&next) = neighbours.get(index) else {
                colour.insert(node, 2);
                stack.pop();
                path.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            match colour.get(&next).copied().unwrap_or(0) {
                0 => {
                    colour.insert(next, 1);
                    path.push(next);
                    stack.push((next, 0));
                }
                1 => {
                    // Every node on the open path is in `path`, so the position
                    // always exists; falling back to the whole path keeps the
                    // search total rather than dropping a real cycle.
                    let position = path.iter().position(|id| *id == next).unwrap_or(0);
                    let mut cycle: Vec<Pair> = path[position..]
                        .windows(2)
                        .map(|pair| (pair[0], pair[1]))
                        .collect();
                    cycle.push((node, next));
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }
    None
}

fn text(snapshot: &Snapshot, id: crate::index::StringId) -> String {
    snapshot.strings.get(id).unwrap_or_default().to_owned()
}

fn texts(snapshot: &Snapshot, ids: &[crate::index::StringId]) -> Vec<String> {
    ids.iter().map(|id| text(snapshot, *id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> DocId {
        DocId::from_bytes([byte; crate::id::DOC_ID_LEN])
    }

    /// A live endpoint. The `valid_from` value is not read by the graph rules.
    fn live(number: u8, path: &str, scope: &str, shared_with: &[&str]) -> Endpoint {
        Endpoint::Live(facts(number, path, scope, shared_with))
    }

    fn facts(number: u8, path: &str, scope: &str, shared_with: &[&str]) -> NodeFacts {
        NodeFacts {
            id: id(number),
            rel_path: path.to_owned(),
            scope: scope.to_owned(),
            shared_with: shared_with.iter().map(|tag| (*tag).to_owned()).collect(),
            valid_from: None,
            effective_to: None,
            declared_status: Some(Status::Active),
        }
    }

    /// The same facts with a valid-time start, still declared active.
    fn dated(number: u8, path: &str, start: (i32, u32, u32)) -> NodeFacts {
        NodeFacts {
            valid_from: Some(date(start)),
            ..facts(number, path, "global", &[])
        }
    }

    fn edge(from: u8, to: u8) -> Edge {
        Edge {
            from: id(from),
            to: id(to),
            kind: crate::index::EdgeKind::Supersedes,
        }
    }

    /// `(from, to)` pairs, in order, for a readable assertion.
    fn pairs(graph: &Graph) -> Vec<Pair> {
        let mut out = Vec::new();
        for (id, node) in graph.nodes() {
            for successor in &node.successors {
                out.push((*id, *successor));
            }
        }
        out.sort_unstable();
        out
    }

    fn date((year, month, day): (i32, u32, u32)) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    /// The query scope set; `&[]` is the universal set (§6.12 rule 3).
    fn scopes(list: &[&str]) -> ScopeFilter {
        let owned: Vec<String> = list.iter().map(|scope| (*scope).to_owned()).collect();
        ScopeFilter::new(&owned, false)
    }

    /// The graph fixture chain by hand: A 2020-01-01, B 2020-07-01, C
    /// 2021-01-01, D 2021-07-01, E 2022-01-01, each replacing the one before.
    fn chain() -> Graph {
        let edges = [edge(1, 2), edge(2, 3), edge(3, 4), edge(4, 5)];
        let endpoints = [
            (1, "chain/2020-01-01-adopt-postgres.md", (2020, 1, 1)),
            (2, "chain/2020-07-01-move-to-redis.md", (2020, 7, 1)),
            (3, "chain/2021-01-01-cache-in-front.md", (2021, 1, 1)),
            (4, "chain/2021-07-01-drop-cache.md", (2021, 7, 1)),
            (5, "chain/2022-01-01-cache-returns.md", (2022, 1, 1)),
        ]
        .map(|(number, path, start)| Endpoint::Live(dated(number, path, start)));
        Graph::build(&edges, endpoints, false)
    }

    /// G, a global document of 2020-02-01, replaced by H, an `org:acme`
    /// document of 2020-03-01 (the fixture's `scopes/global-claim.md` pair).
    fn scope_pair() -> Graph {
        let edges = [edge(91, 92)];
        let endpoints = [
            Endpoint::Live(dated(91, "scopes/global-claim.md", (2020, 2, 1))),
            Endpoint::Live(NodeFacts {
                scope: "org:acme".to_owned(),
                ..dated(92, "scopes/acme-claim.md", (2020, 3, 1))
            }),
        ];
        Graph::build(&edges, endpoints, false)
    }

    fn window_of(graph: &Graph, number: u8, query: &ScopeFilter) -> Window {
        let facts = graph.endpoint(id(number)).expect("endpoint").facts();
        Window::of(facts, graph, query)
    }

    fn status_of(graph: &Graph, number: u8, query: &ScopeFilter) -> Option<EffectiveStatus> {
        let facts = graph.endpoint(id(number)).expect("endpoint").facts();
        EffectiveStatus::of(facts, graph, query)
    }

    /// The chain nodes whose window contains `at`, in id order.
    fn members(graph: &Graph, at: NaiveDate) -> Vec<u8> {
        (1..=5)
            .filter(|number| window_of(graph, *number, &scopes(&[])).contains(at))
            .collect()
    }

    #[test]
    fn case_4_1_the_cycle_drops_the_greatest_path_pair() {
        // The main fixture cycle, with the three real fixture paths:
        //   (redis, postgres)      redis.superseded_by = postgres
        //   (revisited, redis)     revisited.supersedes = redis
        //   (postgres, revisited)  postgres.supersedes = revisited
        // Every edge is on the cycle. The greatest `from` path is
        // `decisions/2025-02-01-cache-revisited.md`, so `(revisited, redis)` is
        // ignored and the surviving chain is redis → postgres → revisited.
        let edges = [edge(11, 12), edge(13, 11), edge(12, 13)];
        let endpoints = [
            live(11, "decisions/2024-12-01-redis-cache.md", "global", &[]),
            live(12, "decisions/2024-11-02-postgres-cache.md", "global", &[]),
            live(13, "decisions/2025-02-01-cache-revisited.md", "global", &[]),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(graph.ignored(), [(id(13), id(11))], "one ignored pair");
        assert_eq!(pairs(&graph), [(id(11), id(12)), (id(12), id(13))]);
        assert_eq!(graph.node(id(11)).expect("redis").successors, [id(12)]);
        assert!(graph.node(id(11)).expect("redis").predecessors.is_empty());
        assert_eq!(
            graph.node(id(13)).expect("revisited").predecessors,
            [id(12)]
        );
        assert!(graph.node(id(13)).expect("revisited").successors.is_empty());
    }

    #[test]
    fn case_4_3_a_two_document_cycle_keeps_one_chain() {
        // A (21).superseded_by = B (22) and B.superseded_by = A. The greatest
        // `from` path is `b.md`, so (B, A) is ignored and A → B survives. Both
        // documents keep one relation each, and neither is isolated.
        let edges = [edge(21, 22), edge(22, 21)];
        let endpoints = [
            live(21, "a.md", "global", &[]),
            live(22, "b.md", "global", &[]),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(graph.ignored(), [(id(22), id(21))]);
        assert_eq!(pairs(&graph), [(id(21), id(22))]);
        assert_eq!(graph.node(id(21)).expect("a").successors, [id(22)]);
        assert_eq!(graph.node(id(22)).expect("b").predecessors, [id(21)]);
    }

    #[test]
    fn case_4_4_a_self_link_is_a_cycle_of_one() {
        // `supersedes: <own path>` is a cycle of length one. Its only pair is
        // the greatest on that cycle by construction, so it is ignored.
        let edges = [edge(31, 31)];
        let endpoints = [live(31, "solo.md", "global", &[])];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(graph.ignored(), [(id(31), id(31))]);
        assert!(pairs(&graph).is_empty(), "{:?}", pairs(&graph));
    }

    #[test]
    fn case_5_1_a_disjoint_edge_is_stored_but_inert() {
        // `org:acme` (41) replaces `org:beta` (42). The sets do not intersect,
        // so the edge is inert: no relation, and the pair is reported.
        let edges = [edge(42, 41)];
        let endpoints = [
            live(41, "scopes/acme-vs-beta.md", "org:acme", &[]),
            live(42, "scopes/beta-claim.md", "org:beta", &[]),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert!(pairs(&graph).is_empty(), "{:?}", pairs(&graph));
        assert_eq!(graph.inert(), &BTreeSet::from([(id(42), id(41))]));
        assert!(graph.ignored().is_empty());
    }

    #[test]
    fn case_5_2_a_shared_tag_makes_the_edge_effective() {
        // Same scopes as case 5.1, plus `shared_with: [shared:cache]` on both
        // sides, so the two sets intersect at that tag.
        let edges = [edge(52, 51)];
        let endpoints = [
            live(51, "scopes/shared-new.md", "org:beta", &["shared:cache"]),
            live(52, "scopes/shared-old.md", "org:acme", &["shared:cache"]),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(pairs(&graph), [(id(52), id(51))]);
        assert!(graph.inert().is_empty());
    }

    #[test]
    fn case_5_3_a_global_side_makes_the_edge_effective() {
        // The replaced document is global, so the edge is effective whatever
        // the successor's scope says.
        let edges = [edge(61, 62)];
        let endpoints = [
            live(61, "scopes/global-claim.md", "global", &[]),
            live(62, "scopes/acme-claim.md", "org:acme", &[]),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(pairs(&graph), [(id(61), id(62))]);
        assert!(graph.inert().is_empty());
    }

    #[test]
    fn case_5_4_the_flag_makes_a_disjoint_edge_effective() {
        // Case 5.1 again, with `[links] allow_disjoint_supersession = true`.
        // The pair is effective, so nothing is inert and nothing is ignored.
        let edges = [edge(42, 41)];
        let endpoints = [
            live(41, "scopes/acme-vs-beta.md", "org:acme", &[]),
            live(42, "scopes/beta-claim.md", "org:beta", &[]),
        ];
        let graph = Graph::build(&edges, endpoints, true);

        assert_eq!(pairs(&graph), [(id(42), id(41))]);
        assert!(graph.inert().is_empty());
        assert!(graph.ignored().is_empty());
    }

    #[test]
    fn a_retired_endpoint_keeps_its_node_and_its_successor() {
        // A tombstoned successor must still close a predecessor's window, so it
        // keeps a node and its facts even with no metadata row (R2-2).
        let edges = [edge(71, 72)];
        let endpoints = [
            live(71, "a.md", "global", &[]),
            Endpoint::Retired(facts(72, "b.md", "org:acme", &[])),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(graph.node(id(71)).expect("a").successors, [id(72)]);
        assert_eq!(graph.node(id(72)).expect("b").predecessors, [id(71)]);
        assert!(matches!(graph.endpoint(id(72)), Some(Endpoint::Retired(_))));
    }

    #[test]
    fn case_6_1_only_a_was_valid_on_2020_03_01() {
        // The chain windows are A [2020-01-01, 2020-07-01), B [2020-07-01,
        // 2021-01-01), C [2021-01-01, 2021-07-01), D [2021-07-01, 2022-01-01)
        // and E [2022-01-01, None). 2020-03-01 is inside A alone.
        assert_eq!(members(&chain(), date((2020, 3, 1))), [1]);
    }

    #[test]
    fn case_6_2_the_successors_start_is_exclusive() {
        // 2020-07-01 is B's first day and A's last day plus one, so only B.
        assert_eq!(members(&chain(), date((2020, 7, 1))), [2]);
    }

    #[test]
    fn case_6_3_the_last_link_has_no_end() {
        // E starts 2022-01-01 and nothing replaces it, so it alone spans 2025.
        let window = window_of(&chain(), 5, &scopes(&[]));
        assert_eq!(window.valid_to, None);
        assert_eq!(members(&chain(), date((2025, 1, 1))), [5]);
    }

    #[test]
    fn case_6_4_a_declared_end_wins_over_the_successor() {
        // A declares `effective_to: 2020-04-01`. The successor would end the
        // window at 2020-07-01; the smaller value wins (§6.10 rule 2).
        let mut end = dated(1, "chain/a.md", (2020, 1, 1));
        end.effective_to = Some(date((2020, 4, 1)));
        let edges = [edge(1, 2)];
        let endpoints = [
            Endpoint::Live(end),
            Endpoint::Live(dated(2, "chain/b.md", (2020, 7, 1))),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        let window = window_of(&graph, 1, &scopes(&[]));
        assert_eq!(window.valid_from, Some(date((2020, 1, 1))));
        assert_eq!(window.valid_to, Some(date((2020, 4, 1))));
        assert!(window.contains(date((2020, 3, 31))));
        assert!(!window.contains(date((2020, 4, 1))));
    }

    #[test]
    fn case_6_5_an_invalid_window_contains_no_date() {
        // `bad/invalid-window.md`: effective_from 2021-01-01, effective_to
        // 2020-01-01, so the window is empty and no as-of date can enter it.
        let mut invalid = dated(31, "bad/invalid-window.md", (2021, 1, 1));
        invalid.effective_to = Some(date((2020, 1, 1)));
        let graph = Graph::build(&[], [Endpoint::Live(invalid)], false);

        let window = window_of(&graph, 31, &scopes(&[]));
        assert_eq!(window.valid_to, Some(date((2020, 1, 1))));
        assert!(window.invalid, "effective_to <= effective_from");
        assert!(!window.contains(date((2020, 6, 1))));
        assert!(!window.contains(date((2021, 6, 1))));
        assert!(!window.covers_today(date((2025, 4, 1))));
    }

    #[test]
    fn case_6_6_a_retired_successor_still_closes_the_window() {
        // `bad/one-sided.md` (2020-10-01) is replaced by `bad/one-sided-new.md`
        // (2020-11-01), which then leaves the corpus and becomes a tombstone.
        // The tombstone keeps its valid_from, so the window still ends there.
        let edges = [edge(81, 82)];
        let endpoints = [
            Endpoint::Live(dated(81, "bad/one-sided.md", (2020, 10, 1))),
            Endpoint::Retired(dated(82, "bad/one-sided-new.md", (2020, 11, 1))),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        let window = window_of(&graph, 81, &scopes(&[]));
        assert_eq!(window.valid_from, Some(date((2020, 10, 1))));
        assert_eq!(window.valid_to, Some(date((2020, 11, 1))));
    }

    #[test]
    fn case_6_7_an_acme_successor_closes_a_global_window_only_for_acme() {
        // `scopes/global-claim.md` (2020-02-01) is replaced by
        // `scopes/acme-claim.md` (org:acme, 2020-03-01). For Q = {org:beta} the
        // successor does not match, so the window stays open; for Q =
        // {org:acme} it ends at 2020-03-01.
        let graph = scope_pair();
        assert_eq!(window_of(&graph, 91, &scopes(&["org:beta"])).valid_to, None);
        assert_eq!(
            window_of(&graph, 91, &scopes(&["org:acme"])).valid_to,
            Some(date((2020, 3, 1)))
        );
    }

    #[test]
    fn case_6_8_the_default_query_is_universal() {
        // With no `--scope` every successor matches, so H closes G's window.
        let graph = scope_pair();
        assert_eq!(
            window_of(&graph, 91, &scopes(&[])).valid_to,
            Some(date((2020, 3, 1)))
        );
        assert!(!window_of(&graph, 91, &scopes(&[])).contains(date((2020, 3, 1))));
    }

    #[test]
    fn case_7_1_a_link_beats_a_declared_active() {
        // `bad/status-lies.md` declares `status: active` and is replaced by
        // `bad/one-sided-new.md` through `superseded_by`.
        let edges = [edge(101, 102)];
        let endpoints = [
            Endpoint::Live(dated(101, "bad/status-lies.md", (2021, 2, 1))),
            Endpoint::Live(dated(102, "bad/one-sided-new.md", (2020, 11, 1))),
        ];
        let graph = Graph::build(&edges, endpoints, false);

        assert_eq!(
            status_of(&graph, 101, &scopes(&[])),
            Some(EffectiveStatus::Superseded)
        );
        assert_eq!(
            status_of(&graph, 102, &scopes(&[])),
            Some(EffectiveStatus::Active)
        );
    }

    #[test]
    fn case_7_2_quiet_expiry_keeps_the_declared_superseded() {
        // `bad/quiet-expiry.md` declares `superseded` and has no successor, so
        // the derived status equals the declared one.
        let mut quiet = dated(111, "bad/quiet-expiry.md", (2020, 12, 1));
        quiet.declared_status = Some(Status::Superseded);
        let graph = Graph::build(&[], [Endpoint::Live(quiet)], false);

        assert_eq!(
            status_of(&graph, 111, &scopes(&[])),
            Some(EffectiveStatus::Superseded)
        );
    }

    #[test]
    fn case_7_3_expiry_is_a_window_property_not_a_status() {
        // `bad/expired.md` declares `status: active` and `effective_to:
        // 2020-01-01`. It stays active and lies outside its window.
        let mut expired = dated(121, "bad/expired.md", (2021, 3, 1));
        expired.effective_to = Some(date((2020, 1, 1)));
        let graph = Graph::build(&[], [Endpoint::Live(expired)], false);

        let window = window_of(&graph, 121, &scopes(&[]));
        assert_eq!(
            status_of(&graph, 121, &scopes(&[])),
            Some(EffectiveStatus::Active)
        );
        assert!(window.invalid);
        assert!(!window.covers_today(date((2025, 4, 1))));
    }

    #[test]
    fn case_7_6_no_successor_and_no_end_stays_current() {
        // `chain/2022-01-01-cache-returns.md`: active, replaced by nothing, so
        // its window has no end and today is inside it.
        let window = window_of(&chain(), 5, &scopes(&[]));
        assert_eq!(window.valid_to, None);
        assert!(window.covers_today(date((2025, 4, 1))));
        assert_eq!(
            status_of(&chain(), 5, &scopes(&[])),
            Some(EffectiveStatus::Active)
        );
    }
}
