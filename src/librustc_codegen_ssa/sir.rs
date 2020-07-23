use crate::mir::{FunctionCx, LocalRef};
use crate::traits::BuilderMethods;
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_middle::ty::AdtDef;
use rustc_middle::ty::{self, layout::TyAndLayout, TyCtxt};
use rustc_target::abi::FieldsShape;
use rustc_target::abi::VariantIdx;
use std::convert::TryFrom;

pub(crate) fn lower_local_ref<'a, 'l, 'tcx, Bx: BuilderMethods<'a, 'tcx>, V>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    fx: &FunctionCx<'a, 'tcx, Bx>,
    decl: &'l LocalRef<'tcx, V>,
) -> ykpack::LocalDecl {
    let ty_layout = match decl {
        LocalRef::Place(pref) => pref.layout,
        LocalRef::UnsizedPlace(..) => {
            let sir_ty = ykpack::Ty::Unimplemented(format!("LocalRef::UnsizedPlace"));
            return ykpack::LocalDecl {
                ty: (
                    tcx.crate_hash(LOCAL_CRATE).as_u64(),
                    tcx.sir_types.borrow_mut().index(sir_ty),
                ),
            };
        }
        LocalRef::Operand(opt_oref) => {
            if let Some(oref) = opt_oref {
                oref.layout
            } else {
                let sir_ty = ykpack::Ty::Unimplemented(format!("LocalRef::OperandRef is None"));
                return ykpack::LocalDecl {
                    ty: (
                        tcx.crate_hash(LOCAL_CRATE).as_u64(),
                        tcx.sir_types.borrow_mut().index(sir_ty),
                    ),
                };
            }
        }
    };

    ykpack::LocalDecl { ty: lower_ty_and_layout(tcx, bx, fx, &ty_layout) }
}

fn lower_ty_and_layout<'a, 'tcx, Bx: BuilderMethods<'a, 'tcx>>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    fx: &FunctionCx<'a, 'tcx, Bx>,
    ty_layout: &TyAndLayout<'tcx>,
) -> ykpack::TypeId {
    let sir_ty = match ty_layout.ty.kind {
        ty::Adt(adt_def, ..) => lower_adt(tcx, bx, fx, adt_def, &ty_layout),
        _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
    };

    (tcx.crate_hash(LOCAL_CRATE).as_u64(), tcx.sir_types.borrow_mut().index(sir_ty))
}

fn lower_adt<'a, 'tcx, Bx: BuilderMethods<'a, 'tcx>>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    fx: &FunctionCx<'a, 'tcx, Bx>,
    adt_def: &AdtDef,
    ty_layout: &TyAndLayout<'tcx>,
) -> ykpack::Ty {
    let align = i32::try_from(ty_layout.layout.align.abi.bytes()).unwrap();
    let size = i32::try_from(ty_layout.layout.size.bytes()).unwrap();

    if adt_def.variants.len() == 1 {
        // Plain old struct-like thing.
        let struct_layout = ty_layout.for_variant(bx, VariantIdx::from_u32(0));

        match &ty_layout.fields {
            FieldsShape::Arbitrary { offsets, .. } => {
                let mut sir_offsets = Vec::new();
                let mut sir_tys = Vec::new();
                for (idx, offs) in offsets.iter().enumerate() {
                    sir_tys.push(lower_ty_and_layout(tcx, bx, fx, &struct_layout.field(bx, idx)));
                    sir_offsets.push(offs.bytes());
                }

                ykpack::Ty::Struct(ykpack::StructTy {
                    offsets: sir_offsets,
                    align,
                    size,
                    tys: sir_tys,
                })
            }
            _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
        }
    } else {
        // An enum with variants.
        ykpack::Ty::Unimplemented(format!("{:?}", ty_layout))
    }
}
