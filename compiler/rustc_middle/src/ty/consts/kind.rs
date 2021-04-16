use std::convert::TryInto;

use crate::mir::interpret::ConstValue;
use crate::mir::interpret::Scalar;
use crate::mir::Promoted;
use crate::ty::subst::{InternalSubsts, SubstsRef};
use crate::ty::ParamEnv;
use crate::ty::{self, TyCtxt, TypeFoldable};
use rustc_errors::ErrorReported;
use rustc_hir::def_id::DefId;
use rustc_macros::HashStable;
use rustc_target::abi::Size;

use super::ScalarInt;
/// An unevaluated, potentially generic, constant.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, TyEncodable, TyDecodable)]
#[derive(Hash, HashStable)]
pub struct Unevaluated<'tcx> {
    pub def: ty::WithOptConstParam<DefId>,
    pub substs: SubstsRef<'tcx>,
    pub promoted: Option<Promoted>,
}

/// Represents a constant in Rust.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, TyEncodable, TyDecodable)]
#[derive(Hash, HashStable)]
pub enum ConstKind<'tcx> {
    /// A const generic parameter.
    Param(ty::ParamConst),

    /// Infer the value of the const.
    Infer(InferConst<'tcx>),

    /// Bound const variable, used only when preparing a trait query.
    Bound(ty::DebruijnIndex, ty::BoundVar),

    /// A placeholder const - universally quantified higher-ranked const.
    Placeholder(ty::PlaceholderConst<'tcx>),

    /// Used in the HIR by using `Unevaluated` everywhere and later normalizing to one of the other
    /// variants when the code is monomorphic enough for that.
    Unevaluated(Unevaluated<'tcx>),

    /// Used to hold computed value.
    Value(ConstValue<'tcx>),

    /// A placeholder for a const which could not be computed; this is
    /// propagated to avoid useless error messages.
    Error(ty::DelaySpanBugEmitted),
}

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
static_assert_size!(ConstKind<'_>, 40);

impl<'tcx> ConstKind<'tcx> {
    #[inline]
    pub fn try_to_value(self) -> Option<ConstValue<'tcx>> {
        if let ConstKind::Value(val) = self { Some(val) } else { None }
    }

    #[inline]
    pub fn try_to_scalar(self) -> Option<Scalar> {
        self.try_to_value()?.try_to_scalar()
    }

    #[inline]
    pub fn try_to_scalar_int(self) -> Option<ScalarInt> {
        Some(self.try_to_value()?.try_to_scalar()?.assert_int())
    }

    #[inline]
    pub fn try_to_bits(self, size: Size) -> Option<u128> {
        self.try_to_scalar_int()?.to_bits(size).ok()
    }

    #[inline]
    pub fn try_to_bool(self) -> Option<bool> {
        self.try_to_scalar_int()?.try_into().ok()
    }

    #[inline]
    pub fn try_to_machine_usize(self, tcx: TyCtxt<'tcx>) -> Option<u64> {
        self.try_to_value()?.try_to_machine_usize(tcx)
    }
}

/// An inference variable for a const, for use in const generics.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, TyEncodable, TyDecodable, Hash)]
#[derive(HashStable)]
pub enum InferConst<'tcx> {
    /// Infer the value of the const.
    Var(ty::ConstVid<'tcx>),
    /// A fresh const variable. See `infer::freshen` for more details.
    Fresh(u32),
}

impl<'tcx> ConstKind<'tcx> {
    #[inline]
    /// Tries to evaluate the constant if it is `Unevaluated`. If that doesn't succeed, return the
    /// unevaluated constant.
    pub fn eval(self, tcx: TyCtxt<'tcx>, param_env: ParamEnv<'tcx>) -> Self {
        self.try_eval(tcx, param_env).and_then(Result::ok).map_or(self, ConstKind::Value)
    }

    #[inline]
    /// Tries to evaluate the constant if it is `Unevaluated`. If that isn't possible or necessary
    /// return `None`.
    pub(super) fn try_eval(
        self,
        tcx: TyCtxt<'tcx>,
        param_env: ParamEnv<'tcx>,
    ) -> Option<Result<ConstValue<'tcx>, ErrorReported>> {
        if let ConstKind::Unevaluated(Unevaluated { def, substs, promoted }) = self {
            use crate::mir::interpret::ErrorHandled;

            // HACK(eddyb) this erases lifetimes even though `const_eval_resolve`
            // also does later, but we want to do it before checking for
            // inference variables.
            // Note that we erase regions *before* calling `with_reveal_all_normalized`,
            // so that we don't try to invoke this query with
            // any region variables.
            let param_env_and_substs = tcx
                .erase_regions(param_env)
                .with_reveal_all_normalized(tcx)
                .and(tcx.erase_regions(substs));

            // HACK(eddyb) when the query key would contain inference variables,
            // attempt using identity substs and `ParamEnv` instead, that will succeed
            // when the expression doesn't depend on any parameters.
            // FIXME(eddyb, skinny121) pass `InferCtxt` into here when it's available, so that
            // we can call `infcx.const_eval_resolve` which handles inference variables.
            let param_env_and_substs = if param_env_and_substs.needs_infer() {
                tcx.param_env(def.did).and(InternalSubsts::identity_for_item(tcx, def.did))
            } else {
                param_env_and_substs
            };

            // FIXME(eddyb) maybe the `const_eval_*` methods should take
            // `ty::ParamEnvAnd<SubstsRef>` instead of having them separate.
            let (param_env, substs) = param_env_and_substs.into_parts();
            // try to resolve e.g. associated constants to their definition on an impl, and then
            // evaluate the const.
            match tcx.const_eval_resolve(param_env, ty::Unevaluated { def, substs, promoted }, None)
            {
                // NOTE(eddyb) `val` contains no lifetimes/types/consts,
                // and we use the original type, so nothing from `substs`
                // (which may be identity substs, see above),
                // can leak through `val` into the const we return.
                Ok(val) => Some(Ok(val)),
                Err(ErrorHandled::TooGeneric | ErrorHandled::Linted) => None,
                Err(ErrorHandled::Reported(e)) => Some(Err(e)),
            }
        } else {
            None
        }
    }
}
