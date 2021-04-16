//! Removes assignments to ZST places.

use crate::transform::MirPass;
use rustc_middle::mir::tcx::PlaceTy;
use rustc_middle::mir::{Body, LocalDecls, Place, StatementKind};
use rustc_middle::ty::{self, Ty, TyCtxt};

pub struct RemoveZsts;

impl<'tcx> MirPass<'tcx> for RemoveZsts {
    fn run_pass(&self, tcx: TyCtxt<'tcx>, body: &mut Body<'tcx>) {
        if tcx.sess.mir_opt_level() < 3 {
            return;
        }
        let param_env = tcx.param_env(body.source.def_id());
        let (basic_blocks, local_decls) = body.basic_blocks_and_local_decls_mut();
        for block in basic_blocks.iter_mut() {
            for statement in block.statements.iter_mut() {
                match statement.kind {
                    StatementKind::Assign(box (place, _)) => {
                        let place_ty = place.ty(local_decls, tcx).ty;
                        if !maybe_zst(place_ty) {
                            continue;
                        }
                        let layout = match tcx.layout_of(param_env.and(place_ty)) {
                            Ok(layout) => layout,
                            Err(_) => continue,
                        };
                        if !layout.is_zst() {
                            continue;
                        }
                        if involves_a_union(place, local_decls, tcx) {
                            continue;
                        }
                        if tcx.consider_optimizing(|| {
                            format!(
                                "RemoveZsts - Place: {:?} SourceInfo: {:?}",
                                place, statement.source_info
                            )
                        }) {
                            statement.make_nop();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A cheap, approximate check to avoid unnecessary `layout_of` calls.
fn maybe_zst(ty: Ty<'_>) -> bool {
    match ty.kind() {
        // maybe ZST (could be more precise)
        ty::Adt(..) | ty::Array(..) | ty::Closure(..) | ty::Tuple(..) | ty::Opaque(..) => true,
        // definitely ZST
        ty::FnDef(..) | ty::Never => true,
        // unreachable or can't be ZST
        _ => false,
    }
}

/// Miri lazily allocates memory for locals on assignment,
/// so we must preserve writes to unions and union fields,
/// or it will ICE on reads of those fields.
fn involves_a_union<'tcx>(
    place: Place<'tcx>,
    local_decls: &LocalDecls<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let mut place_ty = PlaceTy::from_ty(local_decls[place.local].ty);
    if is_union(place_ty.ty) {
        return true;
    }
    for elem in place.projection {
        place_ty = place_ty.projection_ty(tcx, elem);
        if is_union(place_ty.ty) {
            return true;
        }
    }
    return false;
}

fn is_union(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::Adt(def, _) if def.is_union() => true,
        _ => false,
    }
}
