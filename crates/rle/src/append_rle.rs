#[cfg(feature = "smallvec")]
use smallvec::SmallVec;

use crate::MergableSpan;

pub trait AppendRle<T: MergableSpan> {
    /// Push a new item to this list-like object. If the passed item can be merged into the
    /// last item in the list, do so instead of inserting a new item.
    ///
    /// Returns true if the item was merged into the previous last item, false if it was inserted.
    fn push_rle(&mut self, item: T) -> bool;

    /// Push a new item to the end of this list-like object. If the passed object can be merged
    /// to the front of the previously last item, do so. This is useful for appending to a list
    /// which is sorted in reverse.
    fn push_reversed_rle(&mut self, item: T) -> bool;
}

// Apparently the cleanest way to do this DRY is using macros.
impl<T: MergableSpan> AppendRle<T> for Vec<T> {
    fn push_rle(&mut self, item: T) -> bool {
        if let Some(v) = self.last_mut() {
            if v.can_append(&item) {
                v.append(item);
                return true;
            }
        }

        self.push(item);
        false
    }

    fn push_reversed_rle(&mut self, item: T) -> bool {
        if let Some(v) = self.last_mut() {
            if item.can_append(v) {
                v.prepend(item);
                return true;
            }
        }

        self.push(item);
        false
    }
}

#[cfg(feature = "smallvec")]
impl<A: smallvec::Array> AppendRle<A::Item> for SmallVec<A> where A::Item: MergableSpan {
    fn push_rle(&mut self, item: A::Item) -> bool {
        // debug_assert!(item.len() > 0);

        if let Some(v) = self.last_mut() {
            if v.can_append(&item) {
                v.append(item);
                return true;
            }
        }

        self.push(item);
        false
    }

    fn push_reversed_rle(&mut self, item: A::Item) -> bool {
    // debug_assert!(item.len() > 0);

        if let Some(v) = self.last_mut() {
            if item.can_append(v) {
                v.prepend(item);
                return true;
            }
        }

        self.push(item);
        false
    }
}
