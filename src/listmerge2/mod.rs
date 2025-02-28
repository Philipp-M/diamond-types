mod action_plan;
mod test_conversion;

// #[cfg(feature = "dot_export")]
mod dot;

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use smallvec::{SmallVec, smallvec};
use rle::SplitableSpan;
use crate::causalgraph::graph::{Graph, GraphEntrySimple};
use crate::{CausalGraph, DTRange, Frontier, LV};
use crate::causalgraph::graph::tools::DiffFlag;
use crate::frontier::FrontierRef;
use crate::listmerge2::action_plan::EntryState;

type Index = usize;


#[derive(Debug, Clone)]
struct ActionGraphEntry {
    pub parents: SmallVec<[usize; 2]>, // 2+ items. These are indexes to sibling items, not LVs.
    pub span: DTRange,
    pub num_children: usize,
    pub state: EntryState,
    // flag: DiffFlag,
}

#[derive(Debug, Clone)]
pub(super) struct ConflictSubgraph {
    ops: Vec<ActionGraphEntry>,
    // last: usize,
}


// Sorted highest to lowest (so we compare the highest first).
#[derive(Debug, PartialEq, Eq, Clone)]
struct RevSortFrontier(SmallVec<[LV; 2]>);

impl Ord for RevSortFrontier {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for RevSortFrontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<LV> for RevSortFrontier {
    fn from(v: LV) -> Self { Self(smallvec![v]) }
}

impl From<&[LV]> for RevSortFrontier {
    fn from(f: &[LV]) -> Self {
        RevSortFrontier(f.into())
    }
}

// #[test]
// fn foo() {
//     let a = RevSortFrontier::from(1);
//     let b = RevSortFrontier::from([0usize, 1].as_slice());
//     dbg!(a.cmp(&b));
// }

#[derive(Debug, Clone)]
struct QueueEntry {
    version: RevSortFrontier,
    flag: DiffFlag,
    // These are indexes into the output for child items that need their parents updated when
    // they get inserted.
    child_index: usize,
}

impl PartialEq<Self> for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        // self.frontier == other.frontier
        self.version == other.version
    }
}
impl Eq for QueueEntry {}

impl PartialOrd<Self> for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // TODO: Could ditch RevSortFrontier above and just do the special sorting here.
        // self.frontier.cmp(&other.frontier)
        self.version.cmp(&other.version)
    }
}

fn peek_when_matches<T: Ord, F: FnOnce(&T) -> bool>(heap: &BinaryHeap<T>, pred: F) -> Option<&T> {
    if let Some(peeked) = heap.peek() {
        if pred(peeked) {
            return Some(peeked);
        }
    }
    None
}

impl CausalGraph {
    fn make_conflict_graph(&self) -> ConflictSubgraph {
        self.graph.make_conflict_graph_between(&[], self.version.as_ref())
    }
}


impl Graph {
    fn make_conflict_graph_between(&self, a: &[LV], b: &[LV]) -> ConflictSubgraph {
        // TODO: Short circuits.
        if a == b {
            // Nothing to do here.
            return ConflictSubgraph { ops: vec![] };
        }

        // let mut result: Vec<ActionGraphEntry> = vec![];
        let mut result: Vec<ActionGraphEntry> = vec![];

        // The "final" state needs to be in a single entry, and that entry needs to be at the start
        // of the resulting graph.
        //
        // We're merging b into a. There's essentially 2 cases here:
        //
        // 1. b is a direct descendant of a. The "last" (first) item will be b.
        // 2. a and b have concurrent operations. The last item will be a merge with multiple
        // parents.

        result.push(ActionGraphEntry {
            parents: Default::default(),
            span: Default::default(),
            num_children: 0,
            state: Default::default(),
        });

        let mut root_children: SmallVec<[usize; 2]> = smallvec![];

        // The heap is sorted such that we pull the highest items first.
        let mut queue: BinaryHeap<QueueEntry> = BinaryHeap::new();

        if !self.frontier_contains_frontier(b, a) {
            queue.push(QueueEntry { version: a.into(), flag: DiffFlag::OnlyA, child_index: 0 });
        }
        if !self.frontier_contains_frontier(a, b) {
            queue.push(QueueEntry { version: b.into(), flag: DiffFlag::OnlyB, child_index: 0 });
        }

        // Loop until we've collapsed the graph down to a single element.
        'outer: while let Some(entry) = queue.pop() {
            // println!("pop {:?}", &entry);
            let mut flag = entry.flag;
            let Some((&v, merged_with)) = entry.version.0.split_last() else {
                root_children.push(entry.child_index);
                continue;
            };

            let mut num_children = 1;

            // Regardless of if its a merge or not, we'll mark the entry as pointing here.
            let mut new_index = result.len();
            result[entry.child_index].parents.push(new_index);

            while let Some(peek_entry) = queue.peek() {
                if peek_entry.version == entry.version {
                    if peek_entry.flag != flag { flag = DiffFlag::Shared; }
                    num_children += 1;
                    result[peek_entry.child_index].parents.push(new_index);
                    queue.pop();
                } else { break; }
            }

            if !merged_with.is_empty() {
                // Merge. We'll make an entry just for this merge because it keeps the logic here
                // more simple.
                let mut process_here = true;
                if let Some(peek_entry) = queue.peek() {
                    if let Some((&peek_v, peek_rest)) = peek_entry.version.0.split_last() {
                        if peek_v == v && !peek_rest.is_empty() { process_here = false; }
                    }
                }

                // Shatter.
                for m in merged_with {
                    queue.push(QueueEntry { version: (*m).into(), flag, child_index: new_index });
                }

                result.push(ActionGraphEntry { // Push the merge entry.
                    parents: if process_here { smallvec![new_index + 1] } else { smallvec![] },
                    span: Default::default(),
                    num_children,
                    state: Default::default(),
                });

                if !process_here {
                    // Shatter this version too and continue.
                    queue.push(QueueEntry { version: v.into(), flag, child_index: new_index });
                    continue;
                }

                num_children = 1;

                // Then process v (the highest version) using merge_index as a parent.
                new_index += 1;
            }

            // Ok, now we're going to prepare all the items which exist within the txn containing v.
            let containing_txn = self.entries.find_packed(v);
            let mut last = v;
            // let mut start = containing_txn.span.start;

            // Consume all other changes within this txn.
            while let Some(peek_entry) = queue.peek() {
                // println!("peek {:?}", &peek_entry);
                // Might be simpler to use containing_txn.contains(peek_time.last).

                // A bit gross, but the best I can come up with for this logic.
                let Some((&peek_v, remainder)) = peek_entry.version.0.split_last() else { break; };
                if peek_v < containing_txn.span.start { break; } // Flush the rest of this txn.

                if !remainder.is_empty() {
                    debug_assert!(peek_v < last); // Guaranteed thanks to the queue ordering.
                    // There's a merge in the pipe. Push the range from peek_v+1..=last.

                    // Push the range from peek_entry.v to v.
                    result.push(ActionGraphEntry {
                        parents: smallvec![],
                        span: (peek_v+1 .. last+1).into(),
                        num_children,
                        state: Default::default(),
                    });

                    // We'll process the direct parent of this item after the merger we found in
                    // the queue. This is just in case the merger is duplicated - we need to process
                    // all that stuff before the rest of this txn.
                    queue.push(QueueEntry { version: peek_v.into(), flag, child_index: new_index });
                    continue 'outer;
                }

                // The next item is within this txn. Consume it.
                let peek_entry = queue.pop().unwrap();

                if peek_v == last {
                    // Just add to new_item.
                    num_children += 1;
                    result[peek_entry.child_index].parents.push(new_index);
                } else {
                    debug_assert!(peek_v < last);
                    // Push the range from peek_entry.v to v.
                    result.push(ActionGraphEntry {
                        parents: smallvec![new_index + 1],
                        span: (peek_v+1 .. last+1).into(),
                        num_children,
                        state: Default::default(),
                    });

                    new_index += 1;
                    num_children = 2;
                    result[peek_entry.child_index].parents.push(new_index);
                    last = peek_v;
                }

                if peek_entry.flag != flag { flag = DiffFlag::Shared; }
            }

            // Emit the remainder of this txn.
            debug_assert_eq!(result.len(), new_index);
            result.push(ActionGraphEntry {
                parents: smallvec![],
                span: (containing_txn.span.start..last+1).into(),
                num_children,
                state: Default::default(),
            });

            queue.push(QueueEntry {
                version: containing_txn.parents.as_ref().into(),
                flag,
                child_index: new_index,
            });
        };

        assert!(!root_children.is_empty());
        if root_children.as_ref() != &[result.len() - 1] {
            let root_index = result.len();
            result.push(ActionGraphEntry {
                parents: smallvec![],
                span: Default::default(),
                num_children: root_children.len(),
                state: Default::default(),
            });
            for r in root_children {
                result[r].parents.push(root_index);
            }
        }

        ConflictSubgraph {
            ops: result,
        }
    }
}


#[cfg(test)]
mod test {
    use std::fs::File;
    use std::io::Read;
    use rle::HasLength;
    use crate::causalgraph::graph::{Graph, GraphEntrySimple};
    use crate::causalgraph::graph::tools::test::fancy_graph;
    use crate::{Frontier, LV};
    use crate::list::ListOpLog;

    fn check(graph: &Graph, a: &[LV], b: &[LV]) {
        // dbg!(a, b);
        let mut result = graph.make_conflict_graph_between(a, b);
        println!("a {:?}, b {:?} => result {:#?}", a, b, &result);
        result.dbg_check();

        let plan = result.make_plan();
        plan.simulate_plan(&graph, &[]);
    }

    #[test]
    fn test_from_fancy_graph() {
        let graph = fancy_graph();
        check(&graph, &[], &[]);
        check(&graph, &[0], &[]);
        check(&graph, &[0], &[3]);
        check(&graph, &[0], &[6]);
        check(&graph, &[2], &[6]);
        check(&graph, &[], &[0, 3]);
        check(&graph, &[10], &[5]);
        check(&graph, &[], &[5, 10]);
        // let result = graph.find_conflicting_2(&[1], &[2]);
        // let result = graph.find_conflicting_2(&[5], &[9]);
    }

    #[test]
    fn combined_merge() {
        // let graph = Graph::from_simple_items(&[
        //     GraphEntrySimple { span: 0.into(), parents: Frontier::root() },
        //     GraphEntrySimple { span: 1.into(), parents: Frontier::root() },
        //     GraphEntrySimple { span: 2.into(), parents: Frontier::from(0) },
        //     GraphEntrySimple { span: 3.into(), parents: Frontier::from(1) },
        //     GraphEntrySimple { span: 4.into(), parents: Frontier::from(0) },
        //     GraphEntrySimple { span: 5.into(), parents: Frontier::from(1) },
        //
        //
        //     GraphEntrySimple { span: 4.into(), parents: Frontier::from_sorted(&[2, 3]) },
        //     GraphEntrySimple { span: 5.into(), parents: Frontier::from_sorted(&[4, 5]) },
        // ]);

        let graph = Graph::from_simple_items(&[
            GraphEntrySimple { span: 0.into(), parents: Frontier::root() },
            GraphEntrySimple { span: 1.into(), parents: Frontier::root() },

            GraphEntrySimple { span: 2.into(), parents: Frontier::from_sorted(&[0, 1]) },
            GraphEntrySimple { span: 3.into(), parents: Frontier::from_sorted(&[0, 1]) },
        ]);

        let mut result = graph.make_conflict_graph_between(&[2], &[3]);
        // let mut result = graph.find_conflicting_2(&[4], &[5]);
        // dbg!(&result);
        result.dbg_check();
        let plan = result.make_plan();
        plan.dbg_check(true);
        plan.dbg_print();
        plan.simulate_plan(&graph, &[]);
    }

    #[test]
    #[ignore]
    fn make_plan() {
        let mut bytes = vec![];
        File::open("benchmark_data/git-makefile.dt").unwrap().read_to_end(&mut bytes).unwrap();
        // File::open("benchmark_data/node_nodecc.dt").unwrap().read_to_end(&mut bytes).unwrap();
        let o = ListOpLog::load_from(&bytes).unwrap();
        let cg = &o.cg;

        // let mut conflict_subgraph = cg.graph.to_test_entry_list();
        let mut conflict_subgraph = cg.graph.make_conflict_graph_between(&[], cg.version.as_ref());

        conflict_subgraph.dbg_check();
        let plan = conflict_subgraph.make_plan();

        plan.dbg_check(true);

        // println!("Plan with {} steps, using {} indexes", plan.actions.len(), plan.indexes_used);
        plan.dbg_print();

        plan.simulate_plan(&cg.graph, &[]);

        plan.cost_estimate(|range| { o.estimate_cost(range) });
        plan.cost_estimate(|range| { range.len() });
    }
}

// git-makefile:
// spans: 1808985, forks: 668 maxes 1043 / 15950
//
// Or with identical items merged:
// Plan with 2710 steps, using 36 indexes
// spans: 101008, forks: 662 maxes 371
// spans: 1808985, forks: 662 maxes 371

// node_nodecc:
// spans: 1907491, forks: 45 maxes 1 / 53622


/*


            // Gather identical entries.
            //
            // The logic here is a bit awful. The problem is that if two items in the CG have
            // identical parents, we want to merge them both into a single output item with no spans
            // so that the action plan ends up simpler.
            if !merged_with.is_empty() {
                if let Some(peek_entry) = peek_when_matches(&queue, |e| e.frontier == frontier) {
                    // Identical merges detected. new_item above will contain all the parents and no
                    // span.
                    let mut merge_special = new_item;
                    let merge_special_index = new_index;

                    merge_special.parents.push(merge_special_index + 1);
                    while let Some(peek_entry) = peek_when_matches(&queue, |e| e.frontier == frontier) {
                        queue.pop().unwrap();
                        if peek_entry.flag != flag { flag = DiffFlag::Shared; }
                        result[peek_entry.child_index].parents.push(merge_special_index);
                        merge_special.num_children += 1;
                    }

                    result.push(merge_special);

                    for &t in merged_with { // TODO: Refactor to merge this with below.
                        queue.push(QueueEntry { frontier: t.into(), flag, child_index: merge_special_index });
                    }

                    // And we'll add another entry past this with only 1 child again (the merge special).
                    new_index += 1;
                    new_item = ActionGraphEntry {
                        parents: smallvec![],
                        span: Default::default(),
                        num_children: 1,
                        state: Default::default(),
                    };
                } else {
                    // Shatter the other merged_with items. We'll deal with v directly this loop
                    // iteration.
                    for &t in merged_with {
                        queue.push(QueueEntry { frontier: t.into(), flag, child_index });
                    }
                }
            }

 */