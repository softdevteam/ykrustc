use crate::def_id::{LocalDefId, CRATE_DEF_INDEX};
use std::fmt;

/// Uniquely identifies a node in the HIR of the current crate. It is
/// composed of the `owner`, which is the `LocalDefId` of the directly enclosing
/// `hir::Item`, `hir::TraitItem`, or `hir::ImplItem` (i.e., the closest "item-like"),
/// and the `local_id` which is unique within the given owner.
///
/// This two-level structure makes for more stable values: One can move an item
/// around within the source code, or add or remove stuff before it, without
/// the `local_id` part of the `HirId` changing, which is a very useful property in
/// incremental compilation where we have to persist things through changes to
/// the code base.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, RustcEncodable, RustcDecodable)]
pub struct HirId {
    pub owner: LocalDefId,
    pub local_id: ItemLocalId,
}

impl fmt::Display for HirId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

rustc_data_structures::define_id_collections!(HirIdMap, HirIdSet, HirId);
rustc_data_structures::define_id_collections!(ItemLocalMap, ItemLocalSet, ItemLocalId);

rustc_index::newtype_index! {
    /// An `ItemLocalId` uniquely identifies something within a given "item-like";
    /// that is, within a `hir::Item`, `hir::TraitItem`, or `hir::ImplItem`. There is no
    /// guarantee that the numerical value of a given `ItemLocalId` corresponds to
    /// the node's position within the owning item in any way, but there is a
    /// guarantee that the `LocalItemId`s within an owner occupy a dense range of
    /// integers starting at zero, so a mapping that maps all or most nodes within
    /// an "item-like" to something else can be implemented by a `Vec` instead of a
    /// tree or hash map.
    pub struct ItemLocalId { .. }
}
rustc_data_structures::impl_stable_hash_via_hash!(ItemLocalId);

/// The `HirId` corresponding to `CRATE_NODE_ID` and `CRATE_DEF_INDEX`.
pub const CRATE_HIR_ID: HirId = HirId {
    owner: LocalDefId { local_def_index: CRATE_DEF_INDEX },
    local_id: ItemLocalId::from_u32(0),
};

pub const DUMMY_HIR_ID: HirId =
    HirId { owner: LocalDefId { local_def_index: CRATE_DEF_INDEX }, local_id: DUMMY_ITEM_LOCAL_ID };

pub const DUMMY_ITEM_LOCAL_ID: ItemLocalId = ItemLocalId::MAX;
