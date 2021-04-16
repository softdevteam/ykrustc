use super::Pass;
use crate::clean::*;
use crate::core::DocContext;
use crate::fold::DocFolder;

use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_middle::ty::DefIdTree;
use rustc_span::symbol::sym;

crate const COLLECT_TRAIT_IMPLS: Pass = Pass {
    name: "collect-trait-impls",
    run: collect_trait_impls,
    description: "retrieves trait impls for items in the crate",
};

crate fn collect_trait_impls(krate: Crate, cx: &mut DocContext<'_>) -> Crate {
    let (mut krate, synth_impls) = cx.sess().time("collect_synthetic_impls", || {
        let mut synth = SyntheticImplCollector { cx, impls: Vec::new() };
        (synth.fold_crate(krate), synth.impls)
    });

    let prims: FxHashSet<PrimitiveType> = krate.primitives.iter().map(|p| p.1).collect();

    let crate_items = {
        let mut coll = ItemCollector::new();
        krate = cx.sess().time("collect_items_for_trait_impls", || coll.fold_crate(krate));
        coll.items
    };

    let mut new_items = Vec::new();

    for &cnum in cx.tcx.crates().iter() {
        for &(did, _) in cx.tcx.all_trait_implementations(cnum).iter() {
            cx.tcx.sess.prof.generic_activity("build_extern_trait_impl").run(|| {
                inline::build_impl(cx, None, did, None, &mut new_items);
            });
        }
    }

    // Also try to inline primitive impls from other crates.
    for &def_id in PrimitiveType::all_impls(cx.tcx).values().flatten() {
        if !def_id.is_local() {
            cx.tcx.sess.prof.generic_activity("build_primitive_trait_impls").run(|| {
                inline::build_impl(cx, None, def_id, None, &mut new_items);

                // FIXME(eddyb) is this `doc(hidden)` check needed?
                if !cx.tcx.get_attrs(def_id).lists(sym::doc).has_word(sym::hidden) {
                    let impls = get_auto_trait_and_blanket_impls(cx, def_id);
                    new_items.extend(impls.filter(|i| cx.inlined.insert(i.def_id)));
                }
            });
        }
    }

    // `tcx.crates()` doesn't include the local crate, and `tcx.all_trait_implementations`
    // doesn't work with it anyway, so pull them from the HIR map instead
    let mut extra_attrs = Vec::new();
    for &trait_did in cx.tcx.all_traits(LOCAL_CRATE).iter() {
        for &impl_did in cx.tcx.hir().trait_impls(trait_did) {
            let impl_did = impl_did.to_def_id();
            cx.tcx.sess.prof.generic_activity("build_local_trait_impl").run(|| {
                let mut parent = cx.tcx.parent(impl_did);
                while let Some(did) = parent {
                    extra_attrs.extend(
                        cx.tcx
                            .get_attrs(did)
                            .iter()
                            .filter(|attr| attr.has_name(sym::doc))
                            .filter(|attr| {
                                if let Some([attr]) = attr.meta_item_list().as_deref() {
                                    attr.has_name(sym::cfg)
                                } else {
                                    false
                                }
                            })
                            .cloned(),
                    );
                    parent = cx.tcx.parent(did);
                }
                inline::build_impl(cx, None, impl_did, Some(&extra_attrs), &mut new_items);
                extra_attrs.clear();
            });
        }
    }

    let mut cleaner = BadImplStripper { prims, items: crate_items };

    let mut type_did_to_deref_target: FxHashMap<DefId, &Type> = FxHashMap::default();
    // Gather all type to `Deref` target edges.
    for it in &new_items {
        if let ImplItem(Impl { ref for_, ref trait_, ref items, .. }) = *it.kind {
            if trait_.def_id() == cx.tcx.lang_items().deref_trait() {
                let target = items.iter().find_map(|item| match *item.kind {
                    TypedefItem(ref t, true) => Some(&t.type_),
                    _ => None,
                });
                if let (Some(for_did), Some(target)) = (for_.def_id(), target) {
                    type_did_to_deref_target.insert(for_did, target);
                }
            }
        }
    }
    // Follow all `Deref` targets of included items and recursively add them as valid
    fn add_deref_target(
        map: &FxHashMap<DefId, &Type>,
        cleaner: &mut BadImplStripper,
        type_did: &DefId,
    ) {
        if let Some(target) = map.get(type_did) {
            debug!("add_deref_target: type {:?}, target {:?}", type_did, target);
            if let Some(target_prim) = target.primitive_type() {
                cleaner.prims.insert(target_prim);
            } else if let Some(target_did) = target.def_id() {
                // `impl Deref<Target = S> for S`
                if target_did == *type_did {
                    // Avoid infinite cycles
                    return;
                }
                cleaner.items.insert(target_did);
                add_deref_target(map, cleaner, &target_did);
            }
        }
    }
    for type_did in type_did_to_deref_target.keys() {
        // Since only the `DefId` portion of the `Type` instances is known to be same for both the
        // `Deref` target type and the impl for type positions, this map of types is keyed by
        // `DefId` and for convenience uses a special cleaner that accepts `DefId`s directly.
        if cleaner.keep_impl_with_def_id(type_did) {
            add_deref_target(&type_did_to_deref_target, &mut cleaner, type_did);
        }
    }

    let items = if let ModuleItem(Module { ref mut items, .. }) = *krate.module.kind {
        items
    } else {
        panic!("collect-trait-impls can't run");
    };

    items.extend(synth_impls);
    for it in new_items.drain(..) {
        if let ImplItem(Impl { ref for_, ref trait_, ref blanket_impl, .. }) = *it.kind {
            if !(cleaner.keep_impl(for_)
                || trait_.as_ref().map_or(false, |t| cleaner.keep_impl(t))
                || blanket_impl.is_some())
            {
                continue;
            }
        }

        items.push(it);
    }

    krate
}

struct SyntheticImplCollector<'a, 'tcx> {
    cx: &'a mut DocContext<'tcx>,
    impls: Vec<Item>,
}

impl<'a, 'tcx> DocFolder for SyntheticImplCollector<'a, 'tcx> {
    fn fold_item(&mut self, i: Item) -> Option<Item> {
        if i.is_struct() || i.is_enum() || i.is_union() {
            // FIXME(eddyb) is this `doc(hidden)` check needed?
            if !self.cx.tcx.get_attrs(i.def_id).lists(sym::doc).has_word(sym::hidden) {
                self.impls.extend(get_auto_trait_and_blanket_impls(self.cx, i.def_id));
            }
        }

        Some(self.fold_item_recur(i))
    }
}

#[derive(Default)]
struct ItemCollector {
    items: FxHashSet<DefId>,
}

impl ItemCollector {
    fn new() -> Self {
        Self::default()
    }
}

impl DocFolder for ItemCollector {
    fn fold_item(&mut self, i: Item) -> Option<Item> {
        self.items.insert(i.def_id);

        Some(self.fold_item_recur(i))
    }
}

struct BadImplStripper {
    prims: FxHashSet<PrimitiveType>,
    items: FxHashSet<DefId>,
}

impl BadImplStripper {
    fn keep_impl(&self, ty: &Type) -> bool {
        if let Generic(_) = ty {
            // keep impls made on generics
            true
        } else if let Some(prim) = ty.primitive_type() {
            self.prims.contains(&prim)
        } else if let Some(did) = ty.def_id() {
            self.keep_impl_with_def_id(&did)
        } else {
            false
        }
    }

    fn keep_impl_with_def_id(&self, did: &DefId) -> bool {
        self.items.contains(did)
    }
}
