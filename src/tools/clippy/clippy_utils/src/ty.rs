//! Util methods for [`rustc_middle::ty`]

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;

use rustc_ast::ast::Mutability;
use rustc_hir as hir;
use rustc_hir::def_id::DefId;
use rustc_hir::{TyKind, Unsafety};
use rustc_infer::infer::TyCtxtInferExt;
use rustc_lint::LateContext;
use rustc_middle::ty::subst::{GenericArg, GenericArgKind};
use rustc_middle::ty::{self, AdtDef, IntTy, Ty, TypeFoldable, UintTy};
use rustc_span::sym;
use rustc_span::symbol::Symbol;
use rustc_span::DUMMY_SP;
use rustc_trait_selection::traits::query::normalize::AtExt;

use crate::{match_def_path, must_use_attr};

pub fn is_copy<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    ty.is_copy_modulo_regions(cx.tcx.at(DUMMY_SP), cx.param_env)
}

/// Checks whether a type can be partially moved.
pub fn can_partially_move_ty(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    if has_drop(cx, ty) || is_copy(cx, ty) {
        return false;
    }
    match ty.kind() {
        ty::Param(_) => false,
        ty::Adt(def, subs) => def.all_fields().any(|f| !is_copy(cx, f.ty(cx.tcx, subs))),
        _ => true,
    }
}

/// Walks into `ty` and returns `true` if any inner type is the same as `other_ty`
pub fn contains_ty(ty: Ty<'_>, other_ty: Ty<'_>) -> bool {
    ty.walk().any(|inner| match inner.unpack() {
        GenericArgKind::Type(inner_ty) => ty::TyS::same_type(other_ty, inner_ty),
        GenericArgKind::Lifetime(_) | GenericArgKind::Const(_) => false,
    })
}

/// Walks into `ty` and returns `true` if any inner type is an instance of the given adt
/// constructor.
pub fn contains_adt_constructor(ty: Ty<'_>, adt: &AdtDef) -> bool {
    ty.walk().any(|inner| match inner.unpack() {
        GenericArgKind::Type(inner_ty) => inner_ty.ty_adt_def() == Some(adt),
        GenericArgKind::Lifetime(_) | GenericArgKind::Const(_) => false,
    })
}

/// Returns true if ty has `iter` or `iter_mut` methods
pub fn has_iter_method(cx: &LateContext<'_>, probably_ref_ty: Ty<'_>) -> Option<Symbol> {
    // FIXME: instead of this hard-coded list, we should check if `<adt>::iter`
    // exists and has the desired signature. Unfortunately FnCtxt is not exported
    // so we can't use its `lookup_method` method.
    let into_iter_collections: &[Symbol] = &[
        sym::vec_type,
        sym::option_type,
        sym::result_type,
        sym::BTreeMap,
        sym::BTreeSet,
        sym::vecdeque_type,
        sym::LinkedList,
        sym::BinaryHeap,
        sym::hashset_type,
        sym::hashmap_type,
        sym::PathBuf,
        sym::Path,
        sym::Receiver,
    ];

    let ty_to_check = match probably_ref_ty.kind() {
        ty::Ref(_, ty_to_check, _) => ty_to_check,
        _ => probably_ref_ty,
    };

    let def_id = match ty_to_check.kind() {
        ty::Array(..) => return Some(sym::array),
        ty::Slice(..) => return Some(sym::slice),
        ty::Adt(adt, _) => adt.did,
        _ => return None,
    };

    for &name in into_iter_collections {
        if cx.tcx.is_diagnostic_item(name, def_id) {
            return Some(cx.tcx.item_name(def_id));
        }
    }
    None
}

/// Checks whether a type implements a trait.
/// See also `get_trait_def_id`.
pub fn implements_trait<'tcx>(
    cx: &LateContext<'tcx>,
    ty: Ty<'tcx>,
    trait_id: DefId,
    ty_params: &[GenericArg<'tcx>],
) -> bool {
    // Do not check on infer_types to avoid panic in evaluate_obligation.
    if ty.has_infer_types() {
        return false;
    }
    let ty = cx.tcx.erase_regions(ty);
    if ty.has_escaping_bound_vars() {
        return false;
    }
    let ty_params = cx.tcx.mk_substs(ty_params.iter());
    cx.tcx.type_implements_trait((trait_id, ty, ty_params, cx.param_env))
}

/// Checks whether this type implements `Drop`.
pub fn has_drop<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    match ty.ty_adt_def() {
        Some(def) => def.has_dtor(cx.tcx),
        None => false,
    }
}

// Returns whether the type has #[must_use] attribute
pub fn is_must_use_ty<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    match ty.kind() {
        ty::Adt(ref adt, _) => must_use_attr(&cx.tcx.get_attrs(adt.did)).is_some(),
        ty::Foreign(ref did) => must_use_attr(&cx.tcx.get_attrs(*did)).is_some(),
        ty::Slice(ref ty)
        | ty::Array(ref ty, _)
        | ty::RawPtr(ty::TypeAndMut { ref ty, .. })
        | ty::Ref(_, ref ty, _) => {
            // for the Array case we don't need to care for the len == 0 case
            // because we don't want to lint functions returning empty arrays
            is_must_use_ty(cx, *ty)
        },
        ty::Tuple(ref substs) => substs.types().any(|ty| is_must_use_ty(cx, ty)),
        ty::Opaque(ref def_id, _) => {
            for (predicate, _) in cx.tcx.explicit_item_bounds(*def_id) {
                if let ty::PredicateKind::Trait(trait_predicate, _) = predicate.kind().skip_binder() {
                    if must_use_attr(&cx.tcx.get_attrs(trait_predicate.trait_ref.def_id)).is_some() {
                        return true;
                    }
                }
            }
            false
        },
        ty::Dynamic(binder, _) => {
            for predicate in binder.iter() {
                if let ty::ExistentialPredicate::Trait(ref trait_ref) = predicate.skip_binder() {
                    if must_use_attr(&cx.tcx.get_attrs(trait_ref.def_id)).is_some() {
                        return true;
                    }
                }
            }
            false
        },
        _ => false,
    }
}

// FIXME: Per https://doc.rust-lang.org/nightly/nightly-rustc/rustc_trait_selection/infer/at/struct.At.html#method.normalize
// this function can be removed once the `normalizie` method does not panic when normalization does
// not succeed
/// Checks if `Ty` is normalizable. This function is useful
/// to avoid crashes on `layout_of`.
pub fn is_normalizable<'tcx>(cx: &LateContext<'tcx>, param_env: ty::ParamEnv<'tcx>, ty: Ty<'tcx>) -> bool {
    is_normalizable_helper(cx, param_env, ty, &mut HashMap::new())
}

fn is_normalizable_helper<'tcx>(
    cx: &LateContext<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
    ty: Ty<'tcx>,
    cache: &mut HashMap<Ty<'tcx>, bool>,
) -> bool {
    if let Some(&cached_result) = cache.get(ty) {
        return cached_result;
    }
    // prevent recursive loops, false-negative is better than endless loop leading to stack overflow
    cache.insert(ty, false);
    let result = cx.tcx.infer_ctxt().enter(|infcx| {
        let cause = rustc_middle::traits::ObligationCause::dummy();
        if infcx.at(&cause, param_env).normalize(ty).is_ok() {
            match ty.kind() {
                ty::Adt(def, substs) => def.variants.iter().all(|variant| {
                    variant
                        .fields
                        .iter()
                        .all(|field| is_normalizable_helper(cx, param_env, field.ty(cx.tcx, substs), cache))
                }),
                _ => ty.walk().all(|generic_arg| match generic_arg.unpack() {
                    GenericArgKind::Type(inner_ty) if inner_ty != ty => {
                        is_normalizable_helper(cx, param_env, inner_ty, cache)
                    },
                    _ => true, // if inner_ty == ty, we've already checked it
                }),
            }
        } else {
            false
        }
    });
    cache.insert(ty, result);
    result
}

/// Returns true iff the given type is a primitive (a bool or char, any integer or floating-point
/// number type, a str, or an array, slice, or tuple of those types).
pub fn is_recursively_primitive_type(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::Bool | ty::Char | ty::Int(_) | ty::Uint(_) | ty::Float(_) | ty::Str => true,
        ty::Ref(_, inner, _) if *inner.kind() == ty::Str => true,
        ty::Array(inner_type, _) | ty::Slice(inner_type) => is_recursively_primitive_type(inner_type),
        ty::Tuple(inner_types) => inner_types.types().all(is_recursively_primitive_type),
        _ => false,
    }
}

/// Checks if the type is equal to a diagnostic item
///
/// If you change the signature, remember to update the internal lint `MatchTypeOnDiagItem`
pub fn is_type_diagnostic_item(cx: &LateContext<'_>, ty: Ty<'_>, diag_item: Symbol) -> bool {
    match ty.kind() {
        ty::Adt(adt, _) => cx.tcx.is_diagnostic_item(diag_item, adt.did),
        _ => false,
    }
}

/// Checks if the type is equal to a lang item
pub fn is_type_lang_item(cx: &LateContext<'_>, ty: Ty<'_>, lang_item: hir::LangItem) -> bool {
    match ty.kind() {
        ty::Adt(adt, _) => cx.tcx.lang_items().require(lang_item).unwrap() == adt.did,
        _ => false,
    }
}

/// Return `true` if the passed `typ` is `isize` or `usize`.
pub fn is_isize_or_usize(typ: Ty<'_>) -> bool {
    matches!(typ.kind(), ty::Int(IntTy::Isize) | ty::Uint(UintTy::Usize))
}

/// Checks if type is struct, enum or union type with the given def path.
///
/// If the type is a diagnostic item, use `is_type_diagnostic_item` instead.
/// If you change the signature, remember to update the internal lint `MatchTypeOnDiagItem`
pub fn match_type(cx: &LateContext<'_>, ty: Ty<'_>, path: &[&str]) -> bool {
    match ty.kind() {
        ty::Adt(adt, _) => match_def_path(cx, adt.did, path),
        _ => false,
    }
}

/// Peels off all references on the type. Returns the underlying type and the number of references
/// removed.
pub fn peel_mid_ty_refs(ty: Ty<'_>) -> (Ty<'_>, usize) {
    fn peel(ty: Ty<'_>, count: usize) -> (Ty<'_>, usize) {
        if let ty::Ref(_, ty, _) = ty.kind() {
            peel(ty, count + 1)
        } else {
            (ty, count)
        }
    }
    peel(ty, 0)
}

/// Peels off all references on the type.Returns the underlying type, the number of references
/// removed, and whether the pointer is ultimately mutable or not.
pub fn peel_mid_ty_refs_is_mutable(ty: Ty<'_>) -> (Ty<'_>, usize, Mutability) {
    fn f(ty: Ty<'_>, count: usize, mutability: Mutability) -> (Ty<'_>, usize, Mutability) {
        match ty.kind() {
            ty::Ref(_, ty, Mutability::Mut) => f(ty, count + 1, mutability),
            ty::Ref(_, ty, Mutability::Not) => f(ty, count + 1, Mutability::Not),
            _ => (ty, count, mutability),
        }
    }
    f(ty, 0, Mutability::Mut)
}

/// Returns `true` if the given type is an `unsafe` function.
pub fn type_is_unsafe_function<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    match ty.kind() {
        ty::FnDef(..) | ty::FnPtr(_) => ty.fn_sig(cx.tcx).unsafety() == Unsafety::Unsafe,
        _ => false,
    }
}

/// Returns the base type for HIR references and pointers.
pub fn walk_ptrs_hir_ty<'tcx>(ty: &'tcx hir::Ty<'tcx>) -> &'tcx hir::Ty<'tcx> {
    match ty.kind {
        TyKind::Ptr(ref mut_ty) | TyKind::Rptr(_, ref mut_ty) => walk_ptrs_hir_ty(&mut_ty.ty),
        _ => ty,
    }
}

/// Returns the base type for references and raw pointers, and count reference
/// depth.
pub fn walk_ptrs_ty_depth(ty: Ty<'_>) -> (Ty<'_>, usize) {
    fn inner(ty: Ty<'_>, depth: usize) -> (Ty<'_>, usize) {
        match ty.kind() {
            ty::Ref(_, ty, _) => inner(ty, depth + 1),
            _ => (ty, depth),
        }
    }
    inner(ty, 0)
}
