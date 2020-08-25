use crate::mir::LocalRef;
use crate::traits::BuilderMethods;
use rustc_ast::ast::{IntTy, UintTy};
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_middle::ty::AdtDef;
use rustc_middle::ty::{self, layout::TyAndLayout, TyCtxt};
use rustc_span::sym;
use rustc_target::abi::FieldsShape;
use rustc_target::abi::VariantIdx;
use std::convert::TryFrom;

pub(crate) fn lower_local_ref<'a, 'l, 'tcx, Bx: BuilderMethods<'a, 'tcx>, V>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
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

    ykpack::LocalDecl { ty: lower_ty_and_layout(tcx, bx, &ty_layout) }
}

fn lower_ty_and_layout<'a, 'tcx, Bx: BuilderMethods<'a, 'tcx>>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    ty_layout: &TyAndLayout<'tcx>,
) -> ykpack::TypeId {
    let (sir_ty, is_thread_tracer) = match ty_layout.ty.kind {
        ty::Int(si) => (lower_signed_int(si), false),
        ty::Uint(ui) => (lower_unsigned_int(ui), false),
        ty::Adt(adt_def, ..) => lower_adt(tcx, bx, adt_def, &ty_layout),
        ty::Ref(_, typ, _) => {
            (ykpack::Ty::Ref(lower_ty_and_layout(tcx, bx, &bx.layout_of(typ))), false)
        }
        ty::Bool => (ykpack::Ty::Bool, false),
        ty::Tuple(..) => (lower_tuple(tcx, bx, ty_layout), false),
        _ => (ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)), false),
    };

    let mut sir_types = tcx.sir_types.borrow_mut();
    let type_id = (tcx.crate_hash(LOCAL_CRATE).as_u64(), sir_types.index(sir_ty));
    if is_thread_tracer {
        sir_types.thread_tracers.insert(u32::try_from(type_id.1).unwrap());
    }
    type_id
}

fn lower_signed_int(si: IntTy) -> ykpack::Ty {
    match si {
        IntTy::Isize => ykpack::Ty::SignedInt(ykpack::SignedIntTy::Isize),
        IntTy::I8 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I8),
        IntTy::I16 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I16),
        IntTy::I32 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I32),
        IntTy::I64 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I64),
        IntTy::I128 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I128),
    }
}

fn lower_unsigned_int(ui: UintTy) -> ykpack::Ty {
    match ui {
        UintTy::Usize => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::Usize),
        UintTy::U8 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U8),
        UintTy::U16 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U16),
        UintTy::U32 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U32),
        UintTy::U64 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U64),
        UintTy::U128 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U128),
    }
}

fn lower_tuple<'a, 'tcx, Bx: BuilderMethods<'a, 'tcx>>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    ty_layout: &TyAndLayout<'tcx>,
) -> ykpack::Ty {
    let align = i32::try_from(ty_layout.layout.align.abi.bytes()).unwrap();
    let size = i32::try_from(ty_layout.layout.size.bytes()).unwrap();

    match &ty_layout.fields {
        FieldsShape::Arbitrary { offsets, .. } => {
            let mut sir_offsets = Vec::new();
            let mut sir_tys = Vec::new();
            for (idx, offs) in offsets.iter().enumerate() {
                sir_tys.push(lower_ty_and_layout(tcx, bx, &ty_layout.field(bx, idx)));
                sir_offsets.push(offs.bytes());
            }

            ykpack::Ty::Tuple(ykpack::TupleTy {
                fields: ykpack::Fields { offsets: sir_offsets, tys: sir_tys },
                size_align: ykpack::SizeAndAlign { size, align },
            })
        }
        _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
    }
}

fn lower_adt<'a, 'tcx, Bx: BuilderMethods<'a, 'tcx>>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    adt_def: &AdtDef,
    ty_layout: &TyAndLayout<'tcx>,
) -> (ykpack::Ty, bool) {
    let align = i32::try_from(ty_layout.layout.align.abi.bytes()).unwrap();
    let size = i32::try_from(ty_layout.layout.size.bytes()).unwrap();

    let mut is_thread_tracer = false;
    for attr in tcx.get_attrs(adt_def.did).iter() {
        if tcx.sess.check_name(attr, sym::thread_tracer) {
            is_thread_tracer = true;
        }
    }

    if adt_def.variants.len() == 1 {
        // Plain old struct-like thing.
        let struct_layout = ty_layout.for_variant(bx, VariantIdx::from_u32(0));

        match &ty_layout.fields {
            FieldsShape::Arbitrary { offsets, .. } => {
                let mut sir_offsets = Vec::new();
                let mut sir_tys = Vec::new();
                for (idx, offs) in offsets.iter().enumerate() {
                    sir_tys.push(lower_ty_and_layout(tcx, bx, &struct_layout.field(bx, idx)));
                    sir_offsets.push(offs.bytes());
                }

                (
                    ykpack::Ty::Struct(ykpack::StructTy {
                        fields: ykpack::Fields { offsets: sir_offsets, tys: sir_tys },
                        size_align: ykpack::SizeAndAlign { align, size },
                    }),
                    is_thread_tracer,
                )
            }
            _ => (ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)), false),
        }
    } else {
        // An enum with variants.
        (ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)), false)
    }
}
