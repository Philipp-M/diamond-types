use std::ops::Range;
use rle::{HasLength, MergableSpan, SplitableSpan};
use crate::list::Time;
use crate::localtime::TimeSpan;

/// This is a TimeSpan which can be either a forwards range (1,2,3) or backwards (3,2,1), depending
/// on what is most efficient.
///
/// The inner span is always "forwards" - where span.start <= span.end. But if rev is true, this
/// span should be iterated in the reverse order.
#[derive(Copy, Clone, Debug, Eq, Default)] // Default needed for ContentTree.
pub struct TimeSpanRev {
    /// The inner span.
    pub span: TimeSpan,

    /// If target is `1..4` then we either reference `1,2,3` (rev=false) or `3,2,1` (rev=true).
    /// TODO: Consider swapping this, and making it a fwd variable (default true).
    pub reversed: bool,
}

impl TimeSpanRev {
    // Works, but unused.
    // pub fn offset_at_time(&self, time: Time) -> usize {
    //     if self.reversed {
    //         self.span.end - time - 1
    //     } else {
    //         time - self.span.start
    //     }
    // }

    #[allow(unused)]
    pub fn time_at_offset(&self, offset: usize) -> usize {
        if self.reversed {
            self.span.end - offset - 1
        } else {
            self.span.start + offset
        }
    }

    // pub fn first(&self) -> Time {
    //     if self.rev { self.span.last() } else { self.span.start }
    // }

    /// Get the relative range from start + offset_start to start + offset_end.
    ///
    /// This is useful because reversed ranges are weird.
    #[allow(unused)]
    pub fn range(&self, offset_start: usize, offset_end: usize) -> TimeSpan {
        debug_assert!(offset_start <= offset_end);
        debug_assert!(self.span.start + offset_start <= self.span.end);
        debug_assert!(self.span.start + offset_end <= self.span.end);

        if self.reversed {
            TimeSpan {
                start: self.span.end - offset_end,
                end: self.span.end - offset_start
            }
        } else {
            // Simple case.
            TimeSpan {
                start: self.span.start + offset_start,
                end: self.span.start + offset_end
            }
        }
    }
    //
    // pub fn range_start(&self, offset_start: usize, offset_end: usize) -> Time {
    //     debug_assert!(offset_start <= offset_end);
    //     debug_assert!(self.span.start + offset_start <= self.span.end);
    //     debug_assert!(self.span.start + offset_end <= self.span.end);
    //
    //     if self.reversed {
    //         self.span.end - offset_end
    //     } else {
    //         // Simple case.
    //         self.span.start + offset_start
    //     }
    // }
}

impl From<TimeSpan> for TimeSpanRev {
    fn from(target: TimeSpan) -> Self {
        TimeSpanRev {
            span: target,
            reversed: false,
        }
    }
}
impl From<Range<usize>> for TimeSpanRev {
    fn from(range: Range<usize>) -> Self {
        TimeSpanRev {
            span: range.into(),
            reversed: false,
        }
    }
}

impl PartialEq for TimeSpanRev {
    fn eq(&self, other: &Self) -> bool {
        // Custom eq because if the two deletes have length 1, we don't care about rev.
        self.span == other.span && (self.reversed == other.reversed || self.span.len() <= 1)
    }
}

impl HasLength for TimeSpanRev {
    fn len(&self) -> usize { self.span.len() }
}

impl SplitableSpan for TimeSpanRev {
    fn truncate(&mut self, at: usize) -> Self {
        TimeSpanRev {
            span: if self.reversed {
                self.span.truncate_keeping_right(self.len() - at)
            } else {
                self.span.truncate(at)
            },
            reversed: self.reversed,
        }
    }
}

impl MergableSpan for TimeSpanRev {
    fn can_append(&self, other: &Self) -> bool {
        // Can we append forward?
        let self_len_1 = self.len() == 1;
        let other_len_1 = other.len() == 1;
        if (self_len_1 || !self.reversed) && (other_len_1 || !other.reversed)
            && other.span.start == self.span.end {
            return true;
        }

        // Can we append backwards?
        if (self_len_1 || self.reversed) && (other_len_1 || other.reversed)
            && other.span.end == self.span.start {
            return true;
        }

        false
    }

    fn append(&mut self, other: Self) {
        self.reversed = other.span.start < self.span.start;

        if self.reversed {
            self.span.start = other.span.start;
        } else {
            self.span.end = other.span.end;
        }
    }
}

// impl M2Tracker {
//     /// This method is the equivalent of RleVec::find_sparse.
//     // TODO: Move this into ContentTree or something. This is a terrible place for it.
//     fn find_delete_sparse(&self, time: usize) -> (Result<&KVPair<Delete>, TimeSpan>, usize) {
//         if time >= self.deletes.offset_len() {
//             Err(self.deletes.offset_len())
//         }
//         let cursor = self.deletes.cursor_at_offset_pos(time, false);
//         let entry = cursor.get_raw_entry();
//
//     }
// }

// pub(super) fn btree_set<E: SplitableSpan + MergableSpan + HasLength>(map: &mut BTreeMap<usize, E>, key: usize, val: E) {
//     let end = key + val.len();
//     let mut range = map.range_mut((Included(0), Included(end)));
//     range.next_back()
// }

#[cfg(test)]
mod test {
    use rle::test_splitable_methods_valid;
    use super::*;

    #[test]
    fn split_fwd_rev() {
        let fwd = TimeSpanRev {
            span: (1..4).into(),
            reversed: false
        };
        assert_eq!(fwd.split(1), (
            TimeSpanRev {
                span: (1..2).into(),
                reversed: false
            },
            TimeSpanRev {
                span: (2..4).into(),
                reversed: false
            }
        ));

        let rev = TimeSpanRev {
            span: (1..4).into(),
            reversed: true
        };
        assert_eq!(rev.split(1), (
            TimeSpanRev {
                span: (3..4).into(),
                reversed: true
            },
            TimeSpanRev {
                span: (1..3).into(),
                reversed: true
            }
        ));
    }

    #[test]
    fn splitable_mergable() {
        test_splitable_methods_valid(TimeSpanRev {
            span: (1..5).into(),
            reversed: false
        });

        test_splitable_methods_valid(TimeSpanRev {
            span: (1..5).into(),
            reversed: true
        });
    }

    #[test]
    fn at_offset() {
        for rev in [true, false] {
            let span = TimeSpanRev {
                span: (1..5).into(),
                reversed: rev
            };

            for offset in 1..span.len() {
                let (a, b) = span.split(offset);
                assert_eq!(span.time_at_offset(offset - 1), a.time_at_offset(offset - 1));
                assert_eq!(span.time_at_offset(offset), b.time_at_offset(0));
                // assert_eq!(span.time_at_offset(offset), a.time_at_offset(0));
                // assert_eq!(span.offset_at_time())
            }
        }
    }
}