use std::pin::Pin;

use smartstring::alias::{String as SmartString};
use ropey::Rope;
use smallvec::SmallVec;

pub use markers::MarkerEntry;

use crate::common::{ClientName, CRDTLocation};
use crate::automerge::order::OrderMarker;
use crate::range_tree::{RangeTree, ContentIndex};
use crate::split_list::SplitList;

mod markers;
mod txn;
mod order;
mod sibling_range;

use lazy_static::lazy_static;

#[derive(Debug, Clone)]
pub struct CRDTLocationExternal {
    pub agent: SmartString,
    pub seq: u32,
}

lazy_static! {
    pub static ref CRDT_DOC_ROOT_EXTERNAL: CRDTLocationExternal = CRDTLocationExternal {
        agent: SmartString::from("ROOT"),
        seq: 0
    };
}

#[derive(Clone, Debug)]
pub enum OpExternal {
    Insert {
        content: SmartString,
        parent: CRDTLocationExternal,
        // parent: CRDTLocation,
    },
    // Deleted characters in sequence. In a CRDT these characters must be
    // contiguous from a single client.
    Delete {
        target: CRDTLocationExternal,
        // target: CRDTLocation,
        span: usize,
    }
}

#[derive(Clone, Debug)]
pub struct TxnExternal {
    id: CRDTLocationExternal,
    insert_seq_start: u32,
    parents: SmallVec<[CRDTLocationExternal; 2]>,
    ops: SmallVec<[OpExternal; 1]>,
}


pub type Order = usize; // Feeling cute, might change later to u48 for less ram use.

#[derive(Clone, Debug)]
pub enum Op {
    Insert {
        content: SmartString,
        parent: Order,
    },
    Delete {
        target: Order,
        span: usize,
    }
}

#[derive(Clone, Debug)]
pub struct TxnInternal {
    id: CRDTLocation,
    order: Order, // TODO: Remove this.
    parents: SmallVec<[Order; 2]>,

    insert_seq_start: u32, // From external op.
    insert_order_start: Order,
    num_inserts: usize, // Cached from ops.

    // dominates: Order,
    // submits: Order,

    ops: SmallVec<[Op; 1]>,
}

// /// A run of inserts either describe a lot of inserts at a single location
// #[derive(Clone, Debug)]
// struct InsertRun {
//     // TODO: IS this the best way to do this?
//     content: InlinableString,
//
//     length: usize,
// }

#[derive(Debug)]
struct ClientData {
    /// Used to map from client's name / hash to its numerical ID.
    name: ClientName,

    /// This is an in-order list of the order of each transaction we've seen from this client.
    /// So `txn_orders[10] == 50` means CRDTLocation{..., loc: 10} has order 50.
    ///
    /// TODO: Run-length encode this. Make spans of (base_order, len) then binary search.
    txn_orders: Vec<usize>,
}


// This supports scanning by txn order, by indexing. Or scanning by insert with a binary search.
#[derive(Debug)]
pub struct DocumentState {
    /// All transactions we've seen, indexed by txn order.
    txns: Vec<TxnInternal>,

    // inserts: Vec<

    /// The set of txn orders with no children in the document. With a single writer this will
    /// always just be the last order we've seen.
    ///
    /// Never empty. Starts at usize::max (which is the root order).
    frontier: SmallVec<[Order; 4]>,

    /// For each client, we store some data (above). This is indexed by AgentId.
    client_data: Vec<ClientData>,

    /// The marker tree maps from order positions to btree entries, so we can map between orders and
    /// document locations.
    range_tree: Pin<Box<RangeTree<OrderMarker, ContentIndex>>>,

    // We need to be able to map each location to an item in the associated BST.
    // Note for inserts which insert a lot of contiguous characters, this will
    // contain a lot of repeated pointers. I'm trading off memory for simplicity
    // here - which might or might not be the right approach.
    // markers: Vec<NonNull<NodeLeaf>>
    markers: SplitList<MarkerEntry<OrderMarker, ContentIndex>>,

    // next_sibling_tree: Pin<Box<RangeTree<SiblingRange, RawPositionIndex>>>,

    // Probably temporary, eventually.
    text_content: Rope,
}

pub const ROOT_ORDER: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOp {
    pub pos: usize,
    pub ins_content: SmartString,
    pub del_span: usize
}