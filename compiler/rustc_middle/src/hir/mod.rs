//! HIR datatypes. See the [rustc dev guide] for more info.
//!
//! [rustc dev guide]: https://rustc-dev-guide.rust-lang.org/hir.html

pub mod exports;
pub mod map;
pub mod place;

use crate::ich::StableHashingContext;
use crate::ty::query::Providers;
use crate::ty::TyCtxt;
use rustc_ast::Attribute;
use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::stable_hasher::{HashStable, StableHasher};
use rustc_hir::def_id::{LocalDefId, LOCAL_CRATE};
use rustc_hir::*;
use rustc_index::vec::IndexVec;
use rustc_span::DUMMY_SP;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct Owner<'tcx> {
    parent: HirId,
    node: Node<'tcx>,
}

impl<'a, 'tcx> HashStable<StableHashingContext<'a>> for Owner<'tcx> {
    fn hash_stable(&self, hcx: &mut StableHashingContext<'a>, hasher: &mut StableHasher) {
        let Owner { parent, node } = self;
        hcx.while_hashing_hir_bodies(false, |hcx| {
            parent.hash_stable(hcx, hasher);
            node.hash_stable(hcx, hasher);
        });
    }
}

#[derive(Clone, Debug)]
pub struct ParentedNode<'tcx> {
    parent: ItemLocalId,
    node: Node<'tcx>,
}

#[derive(Debug)]
pub struct OwnerNodes<'tcx> {
    hash: Fingerprint,
    nodes: IndexVec<ItemLocalId, Option<ParentedNode<'tcx>>>,
    bodies: FxHashMap<ItemLocalId, &'tcx Body<'tcx>>,
}

impl<'a, 'tcx> HashStable<StableHashingContext<'a>> for OwnerNodes<'tcx> {
    fn hash_stable(&self, hcx: &mut StableHashingContext<'a>, hasher: &mut StableHasher) {
        // We ignore the `nodes` and `bodies` fields since these refer to information included in
        // `hash` which is hashed in the collector and used for the crate hash.
        let OwnerNodes { hash, nodes: _, bodies: _ } = *self;
        hash.hash_stable(hcx, hasher);
    }
}

#[derive(Copy, Clone)]
pub struct AttributeMap<'tcx> {
    map: &'tcx BTreeMap<HirId, &'tcx [Attribute]>,
    prefix: LocalDefId,
}

impl<'a, 'tcx> HashStable<StableHashingContext<'a>> for AttributeMap<'tcx> {
    fn hash_stable(&self, hcx: &mut StableHashingContext<'a>, hasher: &mut StableHasher) {
        let range = self.range();

        range.clone().count().hash_stable(hcx, hasher);
        for (key, value) in range {
            key.hash_stable(hcx, hasher);
            value.hash_stable(hcx, hasher);
        }
    }
}

impl<'tcx> std::fmt::Debug for AttributeMap<'tcx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttributeMap")
            .field("prefix", &self.prefix)
            .field("range", &&self.range().collect::<Vec<_>>()[..])
            .finish()
    }
}

impl<'tcx> AttributeMap<'tcx> {
    fn get(&self, id: ItemLocalId) -> &'tcx [Attribute] {
        self.map.get(&HirId { owner: self.prefix, local_id: id }).copied().unwrap_or(&[])
    }

    fn range(&self) -> std::collections::btree_map::Range<'_, rustc_hir::HirId, &[Attribute]> {
        let local_zero = ItemLocalId::from_u32(0);
        let range = HirId { owner: self.prefix, local_id: local_zero }..HirId {
            owner: LocalDefId { local_def_index: self.prefix.local_def_index + 1 },
            local_id: local_zero,
        };
        self.map.range(range)
    }
}

impl<'tcx> TyCtxt<'tcx> {
    #[inline(always)]
    pub fn hir(self) -> map::Map<'tcx> {
        map::Map { tcx: self }
    }

    pub fn parent_module(self, id: HirId) -> LocalDefId {
        self.parent_module_from_def_id(id.owner)
    }
}

pub fn provide(providers: &mut Providers) {
    providers.parent_module_from_def_id = |tcx, id| {
        let hir = tcx.hir();
        hir.local_def_id(hir.get_module_parent_node(hir.local_def_id_to_hir_id(id)))
    };
    providers.hir_crate = |tcx, _| tcx.untracked_crate;
    providers.index_hir = map::index_hir;
    providers.hir_module_items = |tcx, id| &tcx.untracked_crate.modules[&id];
    providers.hir_owner = |tcx, id| tcx.index_hir(LOCAL_CRATE).map[id].signature;
    providers.hir_owner_nodes = |tcx, id| tcx.index_hir(LOCAL_CRATE).map[id].with_bodies.as_deref();
    providers.hir_attrs = |tcx, id| AttributeMap { map: &tcx.untracked_crate.attrs, prefix: id };
    providers.def_span = |tcx, def_id| tcx.hir().span_if_local(def_id).unwrap_or(DUMMY_SP);
    providers.fn_arg_names = |tcx, id| {
        let hir = tcx.hir();
        let hir_id = hir.local_def_id_to_hir_id(id.expect_local());
        if let Some(body_id) = hir.maybe_body_owned_by(hir_id) {
            tcx.arena.alloc_from_iter(hir.body_param_names(body_id))
        } else if let Node::TraitItem(&TraitItem {
            kind: TraitItemKind::Fn(_, TraitFn::Required(idents)),
            ..
        }) = hir.get(hir_id)
        {
            tcx.arena.alloc_slice(idents)
        } else {
            span_bug!(hir.span(hir_id), "fn_arg_names: unexpected item {:?}", id);
        }
    };
    providers.opt_def_kind = |tcx, def_id| tcx.hir().opt_def_kind(def_id.expect_local());
}
