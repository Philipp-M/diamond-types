use std::cmp::Ordering;
use std::collections::BinaryHeap;

use smallvec::{SmallVec, smallvec};

use rle::{AppendRle, SplitableSpan};

use crate::list::{Branch, ListCRDT, Time};
use crate::list::branch::{branch_is_root, branch_is_sorted};
use crate::list::history::{History, HistoryEntry};
use crate::localtime::TimeSpan;
use crate::rle::RleVec;
use crate::ROOT_TIME;

// use smartstring::alias::{String as SmartString};

// Diff function needs to tag each entry in the queue based on whether its part of a's history or
// b's history or both, and do so without changing the sort order for the heap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Flag { OnlyA, OnlyB, Shared }


impl History {
    fn shadow_of(&self, time: Time) -> Time {
        if time == ROOT_TIME {
            ROOT_TIME
        } else {
            self.entries.find(time).unwrap().shadow
        }
    }

    /// Does the frontier `[a]` contain `[b]` as a direct ancestor according to its shadow?
    fn txn_shadow_contains(&self, a: Time, b: Time) -> bool {
        let a_1 = a.wrapping_add(1);
        let b_1 = b.wrapping_add(1);
        a_1 == b_1 || (a_1 > b_1 && self.shadow_of(a).wrapping_add(1) <= b_1)
    }

    // This is similar to txn_shadow_contains, but it also checks that a doesn't have any other
    // ancestors which aren't included in b's history.
    fn is_direct_descendant(&self, a: Time, b: Time) -> bool {
        // This is a bit more strict than we technically need, but its fast for short circuit
        // evaluation.
        a == b
            || (b == ROOT_TIME && self.txn_shadow_contains(a, ROOT_TIME))
            || (a != ROOT_TIME && a > b && self.entries.find(a).unwrap().contains(b))
    }

    pub(crate) fn branch_contains_order(&self, branch: &[Time], target: Time) -> bool {
        assert!(!branch.is_empty());
        if target == ROOT_TIME || branch.contains(&target) { return true; }
        if branch == [ROOT_TIME] { return false; }

        // Fast path. This causes extra calls to find_packed(), but you usually have a branch with
        // a shadow less than target. Usually the root document. And in that case this codepath
        // avoids the allocation from BinaryHeap.
        for &o in branch {
            if o > target {
                let txn = self.entries.find(o).unwrap();
                if txn.shadow_contains(target) { return true; }
            }
        }

        // So I don't *need* to use a priority queue here. The options are:
        // 1. Use a priority queue, scanning from the highest to lowest orders
        // 2. Use a simple list and do DFS, potentially scanning some items twice
        // 3. Use a simple list and do DFS, with another structure to mark which items we've
        //    visited.
        //
        // Honestly any approach should be obnoxiously fast in any real editing session anyway.

        // TODO: Consider moving queue into a threadlocal variable so we don't need to reallocate it
        // with each call to branch_contains_order.
        let mut queue = BinaryHeap::new();

        // This code could be written to use parent_indexes but its a bit tricky, as an index isn't
        // enough specificity. We'd need the parent and the parent_index. Eh...
        for &o in branch {
            debug_assert_ne!(o, target);
            if o > target { queue.push(o); }
        }

        while let Some(order) = queue.pop() {
            debug_assert!(order > target);
            // dbg!((order, &queue));

            // TODO: Skip these calls to find() using parent_index.
            let entry = self.entries.find(order).unwrap();
            if entry.shadow_contains(target) { return true; }

            while let Some(&next_time) = queue.peek() {
                if next_time >= entry.span.start {
                    // dbg!(next_order);
                    queue.pop();
                } else { break; }
            }

            // dbg!(order);
            for &p in &entry.parents {
                if p == target { return true; }
                else if p != ROOT_TIME && p > target { queue.push(p); }
            }
        }

        false
    }

    /// Returns (spans only in a, spans only in b). Spans are in reverse (descending) order.
    ///
    /// Also find which operation is the greatest common ancestor.
    pub(crate) fn diff(&self, a: &[Time], b: &[Time]) -> (SmallVec<[TimeSpan; 4]>, SmallVec<[TimeSpan; 4]>) {
        assert!(!a.is_empty());
        assert!(!b.is_empty());

        // First some simple short circuit checks to avoid needless work in common cases.
        // Note most of the time this method is called, one of these early short circuit cases will
        // fire.
        if a == b { return (smallvec![], smallvec![]); }

        if a.len() == 1 && b.len() == 1 {
            // Check if either operation naively dominates the other. We could do this for more
            // cases, but we may as well use the code below instead.
            let a = a[0];
            let b = b[0];
            if a == b { return (smallvec![], smallvec![]); }

            if self.is_direct_descendant(a, b) {
                // a >= b.
                return (smallvec![(b.wrapping_add(1)..a.wrapping_add(1)).into()], smallvec![]);
                // return (smallvec![(b.wrapping_add(1)..a.wrapping_add(1)).into()], smallvec![], b);
            }
            if self.is_direct_descendant(b, a) {
                // b >= a.
                return (smallvec![], smallvec![(a.wrapping_add(1)..b.wrapping_add(1)).into()]);
                // return (smallvec![], smallvec![(a.wrapping_add(1)..b.wrapping_add(1)).into()], a);
            }
        }

        // Otherwise fall through to the slow version.
        self.diff_slow(a, b)
    }

    fn diff_slow(&self, a: &[Time], b: &[Time]) -> (SmallVec<[TimeSpan; 4]>, SmallVec<[TimeSpan; 4]>) {
        let mut only_a = smallvec![];
        let mut only_b = smallvec![];

        // marks range [ord_start..ord_end] *inclusive* with flag in our output.
        let mark_run = |ord_start, ord_end, flag: Flag| {
            let target = match flag {
                Flag::OnlyA => { &mut only_a }
                Flag::OnlyB => { &mut only_b }
                Flag::Shared => { return; }
            };
            // dbg!((ord_start, ord_end));

            target.push_reversed_rle(TimeSpan::new(ord_start, ord_end + 1));
        };

        self.diff_slow_internal(a, b, mark_run);
        (only_a, only_b)
    }

    fn diff_slow_internal<F>(&self, a: &[Time], b: &[Time], mut mark_run: F)
        where F: FnMut(Time, Time, Flag) {
        // Sorted highest to lowest.
        let mut queue: BinaryHeap<(Time, Flag)> = BinaryHeap::new();
        for a_ord in a {
            if *a_ord != ROOT_TIME { queue.push((*a_ord, Flag::OnlyA)); }
        }
        for b_ord in b {
            if *b_ord != ROOT_TIME { queue.push((*b_ord, Flag::OnlyB)); }
        }

        let mut num_shared_entries = 0;

        while let Some((mut ord, mut flag)) = queue.pop() {
            if flag == Flag::Shared { num_shared_entries -= 1; }

            // dbg!((ord, flag));
            while let Some((peek_ord, peek_flag)) = queue.peek() {
                if *peek_ord != ord { break; } // Normal case.
                else {
                    // 3 cases if peek_flag != flag. We set flag = Shared in all cases.
                    if *peek_flag != flag { flag = Flag::Shared; }
                    if *peek_flag == Flag::Shared { num_shared_entries -= 1; }
                    queue.pop();
                }
            }

            // Grab the txn containing ord. This will usually be at prev_txn_idx - 1.
            // TODO: Remove usually redundant binary search

            let containing_txn = self.entries.find(ord).unwrap();

            // There's essentially 2 cases here:
            // 1. This item and the first item in the queue are part of the same txn. Mark down to
            //    the queue head and continue.
            // 2. Its not. Mark the whole txn and queue parents.

            // 1:
            while let Some((peek_ord, peek_flag)) = queue.peek() {
                // dbg!((peek_ord, peek_flag));
                if *peek_ord < containing_txn.span.start { break; } else {
                    if *peek_flag != flag {
                        // Mark from peek_ord..=ord and continue.
                        // Note we'll mark this whole txn from ord, but we might do so with
                        // different flags.
                        mark_run(*peek_ord + 1, ord, flag);
                        ord = *peek_ord;
                        // offset -= ord - peek_ord;
                        flag = Flag::Shared;
                    }
                    if *peek_flag == Flag::Shared { num_shared_entries -= 1; }
                    queue.pop();
                }
            }

            // 2: Mark the rest of the txn in our current color and repeat. Note we still need to
            // mark the run even if ord == containing_txn.order because the spans are inclusive.
            mark_run(containing_txn.span.start, ord, flag);

            for p in containing_txn.parents.iter() {
                if *p != ROOT_TIME {
                    queue.push((*p, flag));
                    if flag == Flag::Shared { num_shared_entries += 1; }
                }
            }

            // If there's only shared entries left, abort.
            if queue.len() == num_shared_entries { break; }
        }
    }
}

#[derive(Debug)]
pub struct ConflictSpans {
    pub(crate) common_branch: Branch,
    pub(crate) spans: SmallVec<[TimeSpan; 4]>,
}

impl History {
    fn find_conflicting_slow(&self, a: &[Time], b: &[Time]) -> ConflictSpans {
    // fn find_conflicting_slow(&self, a: &[Time], b: &[Time]) -> SmallVec<[TimeSpan; 4]> {
        // Sorted highest to lowest (so we get the highest item first).
        #[derive(Debug, Ord, PartialEq, Eq, Clone)]
        struct TimePoint {
            last: Time, // For merges this is the highest time.
            // TODO: Compare performance here with actually using a vec.
            merged_with: SmallVec<[Time; 1]>, // Always sorted. Usually empty.
            // merged_with: Vec<Time>, // Always sorted. Usually empty.
        }

        impl PartialOrd for TimePoint {
            #[inline(always)]
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                // wrapping_add(1) converts ROOT into 0 for proper comparisons.
                // TODO: Consider pulling this out
                let ord = self.last.wrapping_add(1).cmp(&other.last.wrapping_add(1));

                // All merges should come before all single items, and sort all merges.
                Some(if ord == Ordering::Equal {
                    other.merged_with.is_empty().cmp(&self.merged_with.is_empty())
                } else { ord })
            }
        }

        impl From<Time> for TimePoint {
            fn from(time: Time) -> Self {
                Self { last: time, merged_with: Default::default() }
            }
        }

        impl From<&[Time]> for TimePoint {
            fn from(branch: &[Time]) -> Self {
                debug_assert!(branch_is_sorted(branch));
                assert!(branch.len() >= 1);

                let mut result = Self {
                    // Bleh.
                    last: *branch.last().unwrap(), merged_with: smallvec![]
                };

                if branch.len() > 1 {
                    // TODO: Clean this up. I'm sure there's nicer constructions
                    for t in &branch[..branch.len() - 1] {
                        result.merged_with.push(*t);
                    }
                }

                result
            }
        }

        // The heap is sorted such that we pull the highest items first.
        let mut queue: BinaryHeap<TimePoint> = BinaryHeap::new();
        queue.push(a.into());
        queue.push(b.into());

        let mut result = smallvec![];

        // dbg!(&queue);

        let branch: Branch = 'outer: loop {
            let time = queue.pop().unwrap();

            let t = time.last;

            if t == ROOT_TIME { break smallvec![ROOT_TIME]; }
            // dbg!(&time);

            // Discard duplicate entries.

            // I could write this with an inner loop and a match statement, but this is shorter and
            // more readable. The optimizer has to earn its keep somehow.
            while queue.peek() == Some(&time) { queue.pop(); }
            if queue.is_empty() {
                let mut branch: Branch = smallvec![];
                // In this order because time.last > time.merged_with.
                branch.extend(time.merged_with.into_iter());
                branch.push(t);
                break branch;
            }

            // If this node is a merger, shatter it.
            if !time.merged_with.is_empty() {
                queue.push(t.into());
                for t in time.merged_with {
                    queue.push(t.into());
                }
            }

            let containing_txn = self.entries.find(t).unwrap();

            let mut range = TimeSpan { start: containing_txn.span.start, end: t + 1 };

            // Consume all other changes within this txn.
            loop {
                if let Some(peek_time) = queue.peek() {
                    // println!("peek {:?}", &peek_time);
                    // Might be simpler to use containing_txn.contains(peek_time.last).
                    if peek_time.last != ROOT_TIME && peek_time.last >= containing_txn.span.start {
                        // The next item is within this txn. Consume it.
                        let time = queue.pop().unwrap();

                        // +1 because we don't want to include the actual merge point in the returned set.
                        let offset = time.last + 1 - containing_txn.span.start;
                        debug_assert!(offset > 0);
                        let rem = range.truncate(offset);
                        result.push_reversed_rle(rem);

                        if !time.merged_with.is_empty() {
                            // We've run into a merged item which uses part of this entry.
                            // We've already pushed the necessary span to the result. Do the
                            // normal merge & shatter logic with this item next.
                            // let time = queue.pop().unwrap();
                            for t in time.merged_with {
                                queue.push(t.into());
                            }
                        }
                    } else {
                        // Emit the remainder of this txn.
                        result.push_reversed_rle(range);

                        // If this entry has multiple parents, we'll push a merge here then
                        // immediately pop it. This is so we stop at the merge point.
                        queue.push(containing_txn.parents.as_slice().into());
                        break;
                    }
                } else {
                    // println!("XXXX {:?}", &range.last());
                    break 'outer smallvec![range.last()];
                }
            }
        };

        ConflictSpans {
            common_branch: branch,
            spans: result
        }
        // result
    }

    /// This method is used to find the operation ranges we need to look at that might be concurrent
    /// with incoming edits.
    ///
    /// We need to track all spans back to a *single point in time*. This point in time is usually
    /// a single localtime, but it might be the result of a merge of multiple edits.
    ///
    /// I'm assuming b is a parent of a, but it should all work if thats not the case.
    // pub(crate) fn find_conflicting(&self, a: &[Time], b: &[Time]) -> SmallVec<[TimeSpan; 4]> {
    pub(crate) fn find_conflicting(&self, a: &[Time], b: &[Time]) -> ConflictSpans {
        debug_assert!(!a.is_empty());
        debug_assert!(!b.is_empty());

        // First some simple short circuit checks to avoid needless work in common cases.
        // Note most of the time this method is called, one of these early short circuit cases will
        // fire.
        if a == b {
            return ConflictSpans {
                common_branch: a.into(),
                spans: smallvec![],
            };
        }

        if a.len() == 1 && b.len() == 1 {
            // Check if either operation naively dominates the other. We could do this for more
            // cases, but we may as well use the code below instead.
            let a = a[0];
            let b = b[0];

            if self.txn_shadow_contains(a, b) {
                // a >= b.
                return ConflictSpans {
                    common_branch: smallvec![b],
                    spans: smallvec![(b.wrapping_add(1)..a.wrapping_add(1)).into()]
                };
            }
            if self.txn_shadow_contains(b, a) {
                // b >= a.
                return ConflictSpans {
                    common_branch: smallvec![a],
                    spans: smallvec![(a.wrapping_add(1)..b.wrapping_add(1)).into()]
                };
            }
        }

        // Otherwise fall through to the slow version.
        self.find_conflicting_slow(a, b)
    }
}

/// This file contains tools to manage the document as a time dag. Specifically, tools to tell us
/// about branches, find diffs and move between branches.
impl ListCRDT {
    // Exported for the fuzzer. Not sure if I actually want this exposed.
    pub fn branch_contains_order(&self, branch: &[Time], target: Time) -> bool {
        self.history.branch_contains_order(branch, target)
    }

    pub fn linear_changes_since(&self, start: Time) -> TimeSpan {
        TimeSpan::new(start, self.get_next_time())
    }
}

#[cfg(test)]
pub mod test {
    use std::ops::Range;
    use smallvec::smallvec;

    use crate::list::Time;
    use crate::list::history::{History, HistoryEntry};
    use crate::localtime::TimeSpan;
    use crate::rle::RleVec;
    use crate::ROOT_TIME;

    fn assert_conflicting(history: &History, a: &[Time], b: &[Time], expect_spans: &[Range<usize>], expect_common: &[Time]) {
        let expect: Vec<TimeSpan> = expect_spans.iter().rev().map(|e| e.clone().into()).collect();
        let slow = history.find_conflicting_slow(a, b);
        assert_eq!(slow.spans.as_slice(), expect);
        assert_eq!(slow.common_branch.as_slice(), expect_common);
        let fast = history.find_conflicting(a, b);
        assert_eq!(fast.spans.as_slice(), expect);
        assert_eq!(fast.common_branch.as_slice(), expect_common);
    }

    fn assert_diff_eq(history: &History, a: &[Time], b: &[Time], expect_a: &[TimeSpan], expect_b: &[TimeSpan]) {
        let slow_result = history.diff_slow(a, b);
        let fast_result = history.diff(a, b);
        assert_eq!(slow_result, fast_result);

        assert_eq!(slow_result.0.as_slice(), expect_a);
        assert_eq!(slow_result.1.as_slice(), expect_b);

        for &(branch, spans, other) in &[(a, expect_a, b), (b, expect_b, a)] {
            for o in spans {
                assert!(history.branch_contains_order(branch, o.start));
                assert!(history.branch_contains_order(branch, o.last()));
            }

            if branch.len() == 1 {
                // dbg!(&other, branch[0]);
                let expect = spans.is_empty();
                assert_eq!(expect, history.branch_contains_order(other, branch[0]));
            }
        }
    }

    fn fancy_history() -> History {
        History {
            entries: RleVec(vec![
                HistoryEntry { // 0-2
                    span: (0..3).into(), shadow: 0,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![2, 3],
                },
                HistoryEntry { // 3-5
                    span: (3..6).into(), shadow: 3,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![2],
                },
                HistoryEntry { // 6-8
                    span: (6..9).into(), shadow: 6,
                    parents: smallvec![1, 4],
                    parent_indexes: smallvec![0, 1], child_indexes: smallvec![3],
                },
                HistoryEntry { // 9
                    span: (9..11).into(), shadow: ROOT_TIME,
                    parents: smallvec![2, 8],
                    parent_indexes: smallvec![2, 0], child_indexes: smallvec![],
                },
            ]),
            root_child_indexes: smallvec![0, 1],
        }
    }

    #[test]
    fn common_item_smoke_test() {
        let history = fancy_history();

        for t in 0..=9 {
            // dbg!(t);
            // The same item should never conflict with itself.
            assert_conflicting(&history, &[t], &[t], &[], &[t]);
        }
        assert_conflicting(&history, &[5, 6], &[5, 6], &[], &[5, 6]);

        assert_conflicting(&history, &[1], &[2], &[2..3], &[1]);
        assert_conflicting(&history, &[0], &[2], &[1..3], &[0]);
        assert_conflicting(&history, &[ROOT_TIME], &[ROOT_TIME], &[], &[ROOT_TIME]);
        assert_conflicting(&history, &[ROOT_TIME], &[2], &[0..3], &[ROOT_TIME]);

        assert_conflicting(&history, &[2], &[3], &[0..4], &[ROOT_TIME]); // 0,1,2 and 3.

        assert_conflicting(&history, &[1, 4], &[4], &[0..2, 3..5], &[ROOT_TIME]); // 0,1,2 and 3.
        assert_conflicting(&history, &[6], &[2], &[0..5, 6..7], &[ROOT_TIME]);
        assert_conflicting(&history, &[6], &[5], &[0..2, 3..7], &[ROOT_TIME]); // 6 includes 1, 0.
        assert_conflicting(&history, &[5, 6], &[5], &[0..2, 3..7], &[ROOT_TIME]);
        assert_conflicting(&history, &[5, 6], &[2], &[0..7], &[ROOT_TIME]);
        assert_conflicting(&history, &[2, 6], &[5], &[0..7], &[ROOT_TIME]);
        assert_conflicting(&history, &[9], &[10], &[10..11], &[9]);
        assert_conflicting(&history, &[6], &[7], &[7..8], &[6]);

        // This looks weird, but its right because 9 shares the same parents.
        assert_conflicting(&history, &[9], &[2, 8], &[9..10], &[2, 8]);

        // Everything! Just because we need to rebase operation 8 on top of 7, and can't produce
        // that without basically all of time. Hopefully this doesn't come up a lot in practice.
        assert_conflicting(&history, &[9], &[2, 7], &[0..5, 6..10], &[ROOT_TIME]);
    }

    #[test]
    fn branch_contains_smoke_test() {
        // let mut doc = ListCRDT::new();
        // assert!(doc.txns.branch_contains_order(&doc.frontier, ROOT_TIME));
        //
        // doc.get_or_create_agent_id("a");
        // doc.local_insert(0, 0, "S".into()); // Shared history.
        // assert!(doc.txns.branch_contains_order(&doc.frontier, ROOT_TIME));
        // assert!(doc.txns.branch_contains_order(&doc.frontier, 0));
        // assert!(!doc.txns.branch_contains_order(&[ROOT_TIME], 0));

        let history = fancy_history();

        assert!(history.branch_contains_order(&[ROOT_TIME], ROOT_TIME));
        assert!(history.branch_contains_order(&[0], 0));
        assert!(history.branch_contains_order(&[0], ROOT_TIME));

        assert!(history.branch_contains_order(&[2], 0));
        assert!(history.branch_contains_order(&[2], 1));
        assert!(history.branch_contains_order(&[2], 2));

        assert!(!history.branch_contains_order(&[0], 1));
        assert!(!history.branch_contains_order(&[1], 2));

        assert!(history.branch_contains_order(&[8], 0));
        assert!(history.branch_contains_order(&[8], 1));
        assert!(!history.branch_contains_order(&[8], 2));
        assert!(!history.branch_contains_order(&[8], 5));

        assert!(history.branch_contains_order(&[1,4], 0));
        assert!(history.branch_contains_order(&[1,4], 1));
        assert!(!history.branch_contains_order(&[1,4], 2));
        assert!(!history.branch_contains_order(&[1,4], 5));

        assert!(history.branch_contains_order(&[9], 2));
        assert!(history.branch_contains_order(&[9], 1));
        assert!(history.branch_contains_order(&[9], 0));
    }

    // #[test]
    // fn diff_smoke_test() {
    //     let mut doc1 = ListCRDT::new();
    //     assert_diff_eq(&doc1.history, &doc1.frontier, &doc1.frontier, &[], &[]);
    //
    //     doc1.get_or_create_agent_id("a");
    //     doc1.local_insert(0, 0, "S".into()); // Shared history.
    //
    //     let mut doc2 = ListCRDT::new();
    //     doc2.get_or_create_agent_id("b");
    //     doc1.replicate_into(&mut doc2); // "S".
    //
    //     // Ok now make some concurrent history.
    //     doc1.local_insert(0, 1, "aaa".into());
    //     let b1 = doc1.frontier.clone();
    //
    //     assert_diff_eq(&doc1.txns, &b1, &b1, &[], &[]);
    //     assert_diff_eq(&doc1.txns, &[ROOT_TIME], &[ROOT_TIME], &[], &[]);
    //     // dbg!(&doc1.frontier);
    //
    //     // There are 4 items in doc1 - "Saaa".
    //     // dbg!(&doc1.frontier); // [3]
    //     assert_diff_eq(&doc1.txns, &[1], &[3], &[], &[2..4]);
    //
    //     doc2.local_insert(0, 1, "bbb".into());
    //
    //     doc2.replicate_into(&mut doc1);
    //
    //     // doc1 has "Saaabbb".
    //
    //     // dbg!(doc1.diff(&b1, &doc1.frontier));
    //
    //     assert_diff_eq(&doc1.txns, &b1, &doc1.frontier, &[], &[4..7]);
    //     assert_diff_eq(&doc1.txns, &[3], &[6], &[1..4], &[4..7]);
    //     assert_diff_eq(&doc1.txns, &[2], &[5], &[1..3], &[4..6]);
    //
    //     // doc1.replicate_into(&mut doc2); // Also "Saaabbb" but different txns.
    //     // dbg!(&doc1.txns, &doc2.txns);
    // }

    // fn root_id() -> RemoteId {
    //     RemoteId {
    //         agent: "ROOT".into(),
    //         seq: u32::MAX
    //     }
    // }
    //
    // pub fn complex_multientry_doc() -> ListCRDT {
    //     let mut doc = ListCRDT::new();
    //     doc.get_or_create_agent_id("a");
    //     doc.get_or_create_agent_id("b");
    //
    //     assert_eq!(doc.frontier.as_slice(), &[ROOT_TIME]);
    //
    //     doc.local_insert(0, 0, "aaa".into());
    //
    //     assert_eq!(doc.frontier.as_slice(), &[2]);
    //
    //     // Need to do this manually to make the change concurrent.
    //     doc.apply_remote_txn(&RemoteTxn {
    //         id: RemoteId { agent: "b".into(), seq: 0 },
    //         parents: smallvec![root_id()],
    //         ops: smallvec![RemoteCRDTOp::Ins {
    //             origin_left: root_id(),
    //             origin_right: root_id(),
    //             len: 2,
    //             content_known: true,
    //         }],
    //         ins_content: "bb".into(),
    //     });
    //
    //     assert_eq!(doc.frontier.as_slice(), &[2, 4]);
    //
    //     // And need to do this manually to make the change not merge time.
    //     doc.apply_remote_txn(&RemoteTxn {
    //         id: RemoteId { agent: "a".into(), seq: 3 },
    //         parents: smallvec![RemoteId { agent: "a".into(), seq: 2 }],
    //         ops: smallvec![RemoteCRDTOp::Ins {
    //             origin_left: RemoteId { agent: "a".into(), seq: 2 },
    //             origin_right: root_id(),
    //             len: 2,
    //             content_known: true,
    //         }],
    //         ins_content: "AA".into(),
    //     });
    //
    //     assert_eq!(doc.frontier.as_slice(), &[4, 6]);
    //
    //     if let Some(ref text) = doc.text_content {
    //         assert_eq!(text, "aaaAAbb");
    //     }
    //
    //     doc
    // }

    // #[test]
    // fn diff_with_multiple_entries() {
    //     let doc = complex_multientry_doc();
    //
    //     // dbg!(&doc.txns);
    //     // dbg!(doc.diff(&smallvec![6], &smallvec![ROOT_TIME]));
    //     // dbg!(&doc);
    //
    //     assert_diff_eq(&doc.txns, &[6], &[ROOT_TIME], &[5..7, 0..3], &[]);
    //     assert_diff_eq(&doc.txns, &[6], &[4], &[5..7, 0..3], &[3..5]);
    //     assert_diff_eq(&doc.txns, &[4, 6], &[ROOT_TIME], &[0..7], &[]);
    // }

    #[test]
    fn diff_for_flat_txns() {
        // Regression.
        let history = History {
            entries: RleVec(vec![
                HistoryEntry {
                    span: (0..1).into(), shadow: ROOT_TIME,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![2]
                },
                HistoryEntry {
                    span: (1..2).into(), shadow: ROOT_TIME,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![3]
                },
                HistoryEntry {
                    span: (2..3).into(), shadow: 2,
                    parents: smallvec![0],
                    parent_indexes: smallvec![0], child_indexes: smallvec![4]
                },
            ]),
            root_child_indexes: smallvec![0, 1],
        };

        assert_diff_eq(&history, &[2], &[ROOT_TIME], &[(2..3).into(), (0..1).into()], &[]);
        assert_diff_eq(&history, &[2], &[1], &[(2..3).into(), (0..1).into()], &[(1..2).into()]);
    }

    #[test]
    fn diff_three_root_txns() {
        // Regression.
        let history = History {
            entries: RleVec(vec![
                HistoryEntry {
                    span: (0..1).into(),
                    shadow: ROOT_TIME,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![],
                },
                HistoryEntry {
                    span: (1..2).into(),
                    shadow: 1,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![],
                },
                HistoryEntry {
                    span: (2..3).into(),
                    shadow: 2,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![], child_indexes: smallvec![],
                },
            ]),
            root_child_indexes: smallvec![0, 1, 2],
        };

        for time in [0, 1, 2] {
            assert_diff_eq(&history, &[time], &[ROOT_TIME], &[(time..time+1).into()], &[]);
            assert_diff_eq(&history, &[ROOT_TIME], &[time], &[], &[(time..time+1).into()]);
        }

        assert_diff_eq(&history, &[0], &[1], &[(0..1).into()], &[(1..2).into()]);
    }

    #[test]
    fn diff_shadow_bubble() {
        // regression
        let history = History {
            entries: RleVec(vec![
                HistoryEntry {
                    span: (0..3).into(),
                    shadow: ROOT_TIME,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![],
                    child_indexes: smallvec![2],
                },
                HistoryEntry {
                    span: (3..5).into(),
                    shadow: 3,
                    parents: smallvec![ROOT_TIME],
                    parent_indexes: smallvec![],
                    child_indexes: smallvec![2],
                },
                HistoryEntry {
                    span: (5..6).into(),
                    shadow: ROOT_TIME,
                    parents: smallvec![2,4],
                    parent_indexes: smallvec![0,1],
                    child_indexes: smallvec![],
                },
            ]),
            root_child_indexes: smallvec![0, 1],
        };

        assert_diff_eq(&history, &[4], &[ROOT_TIME], &[(3..5).into()], &[]);
        assert_diff_eq(&history, &[4], &[5], &[], &[(5..6).into(), (0..3).into()]);
    }
}