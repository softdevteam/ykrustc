//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during code-generation (in rustc_codegen_ssa), and finally placed
//! into an ELF section at link time.

use crate::mir::LocalRef;
use crate::traits::{BuilderMethods, SirMethods};
use indexmap::IndexMap;
use rustc_ast::ast;
use rustc_ast::ast::{IntTy, UintTy};
use rustc_data_structures::fx::FxHasher;
use rustc_hir::{self, def_id::LOCAL_CRATE};
use rustc_middle::mir;
use rustc_middle::ty::print::with_no_trimmed_paths;
use rustc_middle::ty::AdtDef;
use rustc_middle::ty::{self, layout::TyAndLayout, TyCtxt};
use rustc_middle::ty::{Instance, Ty};
use rustc_span::sym;
use rustc_target::abi::FieldsShape;
use rustc_target::abi::VariantIdx;
use std::cell::RefCell;
use std::convert::TryFrom;
use std::default::Default;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::io;
use ykpack;

pub(crate) fn lower_local_ref<'a, 'l, 'tcx, Bx: BuilderMethods<'a, 'tcx>, V>(
    tcx: TyCtxt<'tcx>,
    bx: &Bx,
    decl: &'l LocalRef<'tcx, V>,
) -> ykpack::LocalDecl {
    let ty_layout = match decl {
        LocalRef::Place(pref) => pref.layout,
        LocalRef::UnsizedPlace(..) => {
            let sir_ty = ykpack::Ty::Unimplemented(format!("LocalRef::UnsizedPlace"));
            return ykpack::LocalDecl { ty: bx.cx().define_sir_type(sir_ty) };
        }
        LocalRef::Operand(opt_oref) => {
            if let Some(oref) = opt_oref {
                oref.layout
            } else {
                let sir_ty = ykpack::Ty::Unimplemented(format!("LocalRef::OperandRef is None"));
                return ykpack::LocalDecl { ty: bx.cx().define_sir_type(sir_ty) };
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
    let sir_ty = match ty_layout.ty.kind() {
        ty::Int(si) => lower_signed_int(*si),
        ty::Uint(ui) => lower_unsigned_int(*ui),
        ty::Adt(adt_def, ..) => lower_adt(tcx, bx, adt_def, &ty_layout),
        ty::Array(typ, _) => ykpack::Ty::Array(lower_ty_and_layout(tcx, bx, &bx.layout_of(typ))),
        ty::Slice(typ) => ykpack::Ty::Slice(lower_ty_and_layout(tcx, bx, &bx.layout_of(typ))),
        ty::Ref(_, typ, _) => ykpack::Ty::Ref(lower_ty_and_layout(tcx, bx, &bx.layout_of(typ))),
        ty::Bool => ykpack::Ty::Bool,
        ty::Tuple(..) => lower_tuple(tcx, bx, ty_layout),
        _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
    };
    bx.cx().define_sir_type(sir_ty)
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
                    sir_tys.push(lower_ty_and_layout(tcx, bx, &struct_layout.field(bx, idx)));
                    sir_offsets.push(offs.bytes());
                }

                ykpack::Ty::Struct(ykpack::StructTy {
                    fields: ykpack::Fields { offsets: sir_offsets, tys: sir_tys },
                    size_align: ykpack::SizeAndAlign { align, size },
                })
            }
            _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
        }
    } else {
        // An enum with variants.
        ykpack::Ty::Unimplemented(format!("{:?}", ty_layout))
    }
}

const BUILD_SCRIPT_CRATE: &str = "build_script_build";
const CHECKABLE_BINOPS: [ykpack::BinOp; 5] = [
    ykpack::BinOp::Add,
    ykpack::BinOp::Sub,
    ykpack::BinOp::Mul,
    ykpack::BinOp::Shl,
    ykpack::BinOp::Shr,
];

// Generates a big `match` statement for the binary operation lowerings.
macro_rules! binop_lowerings {
    ( $the_op:expr, $($op:ident ),* ) => {
        match $the_op {
            $(mir::BinOp::$op => ykpack::BinOp::$op,)*
        }
    }
}

/// A collection of in-memory SIR data structures to be serialised.
/// Each codegen unit builds one instance of this which is then merged into a "global" instance
/// when the unit completes.
pub struct Sir {
    pub types: RefCell<SirTypes>,
    pub funcs: RefCell<Vec<ykpack::Body>>,
}

impl Sir {
    pub fn new(tcx: TyCtxt<'_>, cgu_name: &str) -> Self {
        // Build the CGU hash.
        //
        // This must be a globally unique hash for this compilation unit. It might have been
        // tempting to use the `tcx.crate_hash()` as part of the CGU hash, but this query is
        // invalidated on every source code change to the crate. In turn, that would mean lots of
        // unnecessary rebuilds.
        //
        // We settle on:
        // CGU hash = crate name + crate disambiguator + codegen unit name.
        let mut cgu_hasher = FxHasher::default();
        tcx.crate_name(LOCAL_CRATE).hash(&mut cgu_hasher);
        tcx.crate_disambiguator(LOCAL_CRATE).hash(&mut cgu_hasher);
        cgu_name.hash(&mut cgu_hasher);

        Sir {
            types: RefCell::new(SirTypes {
                cgu_hash: ykpack::CguHash(cgu_hasher.finish()),
                map: Default::default(),
                next_idx: Default::default(),
            }),
            funcs: Default::default(),
        }
    }

    /// Returns `true` if we should collect SIR for the current crate.
    pub fn is_required(tcx: TyCtxt<'_>) -> bool {
        tcx.sess.opts.cg.tracer.encode_sir()
            && tcx.crate_name(LOCAL_CRATE).as_str() != BUILD_SCRIPT_CRATE
    }

    /// Returns true if there is nothing inside.
    pub fn is_empty(&self) -> bool {
        self.funcs.borrow().len() == 0
    }

    /// Writes a textual representation of the SIR to `w`. Used for `--emit yk-sir`.
    pub fn dump(&self, w: &mut dyn io::Write) -> Result<(), io::Error> {
        for f in self.funcs.borrow().iter() {
            writeln!(w, "{}", f)?;
        }
        Ok(())
    }
}

/// A structure for building the SIR of a function.
pub struct SirFuncCx<'tcx> {
    pub func: ykpack::Body,
    tcx: TyCtxt<'tcx>,
}

impl SirFuncCx<'tcx> {
    pub fn new(tcx: TyCtxt<'tcx>, instance: &Instance<'tcx>, mir: &mir::Body<'_>) -> Self {
        let mut flags = 0;
        for attr in tcx.get_attrs(instance.def_id()).iter() {
            if tcx.sess.check_name(attr, sym::do_not_trace) {
                flags |= ykpack::bodyflags::DO_NOT_TRACE;
            } else if tcx.sess.check_name(attr, sym::interp_step) {
                // Check various properties of the interp_step at compile time.
                if mir.args_iter().count() != 1 {
                    tcx.sess
                        .struct_err("The #[interp_step] function must accept only one argument")
                        .emit();
                }

                let arg_ok = if let ty::Ref(_, inner_ty, rustc_hir::Mutability::Mut) =
                    mir.local_decls[mir::Local::from_u32(1)].ty.kind()
                {
                    if let ty::Adt(def, _) = inner_ty.kind() { def.is_struct() } else { false }
                } else {
                    false
                };
                if !arg_ok {
                    tcx.sess
                        .struct_err(
                            "The #[interp_step] function must accept a mutable reference to a struct"
                        )
                        .emit();
                }

                if !mir.return_ty().is_unit() {
                    tcx.sess.struct_err("The #[interp_step] function must return unit").emit();
                }

                if !tcx.upvars_mentioned(instance.def_id()).is_none() {
                    tcx.sess
                        .struct_err(
                            "The #[interp_step] function must not capture from its environment",
                        )
                        .emit();
                }

                flags |= ykpack::bodyflags::INTERP_STEP;
            }
        }

        // Since there's a one-to-one mapping between MIR and SIR blocks, we know how many SIR
        // blocks we will need and can allocate empty SIR blocks ahead of time.
        let blocks = vec![
            ykpack::BasicBlock {
                stmts: Default::default(),
                term: ykpack::Terminator::Unreachable,
            };
            mir.basic_blocks().len()
        ];

        let local_decls = Vec::with_capacity(mir.local_decls.len());
        let symbol_name = String::from(&*tcx.symbol_name(*instance).name);

        let crate_name = tcx.crate_name(instance.def_id().krate).as_str();
        if crate_name == "core" || crate_name == "alloc" {
            flags |= ykpack::bodyflags::DO_NOT_TRACE;
        }

        Self {
            func: ykpack::Body { symbol_name, blocks, flags, local_decls, num_args: mir.arg_count },
            tcx,
        }
    }

    /// Returns true if there are no basic blocks.
    pub fn is_empty(&self) -> bool {
        self.func.blocks.len() == 0
    }

    /// Appends a statement to the specified basic block.
    fn push_stmt(&mut self, bb: ykpack::BasicBlockIndex, stmt: ykpack::Statement) {
        self.func.blocks[usize::try_from(bb).unwrap()].stmts.push(stmt);
    }

    /// Sets the terminator of the specified block.
    pub fn set_terminator(&mut self, bb: ykpack::BasicBlockIndex, new_term: ykpack::Terminator) {
        let term = &mut self.func.blocks[usize::try_from(bb).unwrap()].term;
        // We should only ever replace the default unreachable terminator assigned at allocation time.
        debug_assert!(*term == ykpack::Terminator::Unreachable);
        *term = new_term
    }

    /// Converts a MIR statement to SIR, appending the result to `bb`.
    pub fn lower_statement(&mut self, bb: ykpack::BasicBlockIndex, stmt: &mir::Statement<'_>) {
        match stmt.kind {
            mir::StatementKind::Assign(box (ref place, ref rvalue)) => {
                let assign = self.lower_assign_stmt(place, rvalue);
                self.push_stmt(bb, assign);
            }
            // We compute our own liveness in Yorick, so these are ignored.
            mir::StatementKind::StorageLive(_) | mir::StatementKind::StorageDead(_) => {}
            _ => self.push_stmt(bb, ykpack::Statement::Unimplemented(format!("{:?}", stmt))),
        }
    }

    fn lower_assign_stmt(
        &self,
        lvalue: &mir::Place<'_>,
        rvalue: &mir::Rvalue<'_>,
    ) -> ykpack::Statement {
        let lhs = self.lower_place(lvalue);
        let rhs = self.lower_rvalue(rvalue);
        ykpack::Statement::Assign(lhs, rhs)
    }

    pub fn lower_operand(&self, operand: &mir::Operand<'_>) -> ykpack::Operand {
        match operand {
            mir::Operand::Copy(place) | mir::Operand::Move(place) => {
                ykpack::Operand::Place(self.lower_place(place))
            }
            mir::Operand::Constant(cst) => ykpack::Operand::Constant(self.lower_constant(cst)),
        }
    }

    fn lower_rvalue(&self, rvalue: &mir::Rvalue<'_>) -> ykpack::Rvalue {
        match rvalue {
            mir::Rvalue::Use(opnd) => ykpack::Rvalue::Use(self.lower_operand(opnd)),
            mir::Rvalue::BinaryOp(op, opnd1, opnd2) => self.lower_binop(*op, opnd1, opnd2, false),
            mir::Rvalue::CheckedBinaryOp(op, opnd1, opnd2) => {
                self.lower_binop(*op, opnd1, opnd2, true)
            }
            mir::Rvalue::Ref(_, _, place) => self.lower_ref(place),
            mir::Rvalue::Len(place) => ykpack::Rvalue::Len(self.lower_place(place)),
            _ => ykpack::Rvalue::Unimplemented(with_no_trimmed_paths(|| {
                format!("unimplemented rvalue: {:?}", rvalue)
            })),
        }
    }

    pub fn lower_place(&self, place: &mir::Place<'_>) -> ykpack::Place {
        ykpack::Place {
            local: self.lower_local(place.local),
            // FIXME projections not yet implemented.
            projection: place.projection.iter().map(|p| self.lower_projection(&p)).collect(),
        }
    }

    pub fn lower_projection(&self, pe: &mir::PlaceElem<'_>) -> ykpack::Projection {
        match pe {
            mir::ProjectionElem::Field(field, ..) => ykpack::Projection::Field(field.as_u32()),
            mir::ProjectionElem::Deref => ykpack::Projection::Deref,
            mir::ProjectionElem::Index(local) => {
                ykpack::Projection::Index(self.lower_local(*local))
            }
            _ => ykpack::Projection::Unimplemented(format!("{:?}", pe)),
        }
    }

    pub fn lower_local(&self, local: mir::Local) -> ykpack::Local {
        // For the lowering of `Local`s we currently assume a 1:1 mapping from MIR to SIR. If this
        // mapping turns out to be impossible or impractial, this is the place to change it.
        ykpack::Local(local.as_u32())
    }

    fn lower_constant(&self, constant: &mir::Constant<'_>) -> ykpack::Constant {
        match constant.literal.val {
            ty::ConstKind::Value(mir::interpret::ConstValue::Scalar(s)) => {
                self.lower_scalar(constant.literal.ty, s)
            }
            _ => ykpack::Constant::Unimplemented(with_no_trimmed_paths(|| {
                format!("unimplemented constant: {:?}", constant)
            })),
        }
    }

    fn lower_scalar(&self, ty: Ty<'_>, s: mir::interpret::Scalar) -> ykpack::Constant {
        match ty.kind() {
            ty::Uint(uint) => self
                .lower_uint(*uint, s)
                .map(|i| ykpack::Constant::Int(ykpack::ConstantInt::UnsignedInt(i)))
                .unwrap_or_else(|_| {
                    with_no_trimmed_paths(|| {
                        ykpack::Constant::Unimplemented(format!(
                            "unimplemented uint scalar: {:?}",
                            ty.kind()
                        ))
                    })
                }),
            ty::Int(int) => self
                .lower_int(*int, s)
                .map(|i| ykpack::Constant::Int(ykpack::ConstantInt::SignedInt(i)))
                .unwrap_or_else(|_| {
                    ykpack::Constant::Unimplemented(format!(
                        "unimplemented signed int scalar: {:?}",
                        ty.kind()
                    ))
                }),
            ty::Bool => self.lower_bool(s),
            _ => ykpack::Constant::Unimplemented(format!("unimplemented scalar: {:?}", ty.kind())),
        }
    }

    /// Lower an unsigned integer.
    fn lower_uint(
        &self,
        uint: ast::UintTy,
        s: mir::interpret::Scalar,
    ) -> Result<ykpack::UnsignedInt, ()> {
        match uint {
            ast::UintTy::U8 => match s.to_u8() {
                Ok(val) => Ok(ykpack::UnsignedInt::U8(val)),
                Err(e) => panic!("Could not lower scalar to u8: {}", e),
            },
            ast::UintTy::U16 => match s.to_u16() {
                Ok(val) => Ok(ykpack::UnsignedInt::U16(val)),
                Err(e) => panic!("Could not lower scalar to u16: {}", e),
            },
            ast::UintTy::U32 => match s.to_u32() {
                Ok(val) => Ok(ykpack::UnsignedInt::U32(val)),
                Err(e) => panic!("Could not lower scalar to u32: {}", e),
            },
            ast::UintTy::U64 => match s.to_u64() {
                Ok(val) => Ok(ykpack::UnsignedInt::U64(val)),
                Err(e) => panic!("Could not lower scalar to u64: {}", e),
            },
            ast::UintTy::Usize => match s.to_machine_usize(&self.tcx) {
                Ok(val) => Ok(ykpack::UnsignedInt::Usize(val as usize)),
                Err(e) => panic!("Could not lower scalar to usize: {}", e),
            },
            _ => Err(()),
        }
    }

    /// Lower a signed integer.
    fn lower_int(
        &self,
        int: ast::IntTy,
        s: mir::interpret::Scalar,
    ) -> Result<ykpack::SignedInt, ()> {
        match int {
            ast::IntTy::I8 => match s.to_i8() {
                Ok(val) => Ok(ykpack::SignedInt::I8(val)),
                Err(e) => panic!("Could not lower scalar to i8: {}", e),
            },
            ast::IntTy::I16 => match s.to_i16() {
                Ok(val) => Ok(ykpack::SignedInt::I16(val)),
                Err(e) => panic!("Could not lower scalar to i16: {}", e),
            },
            ast::IntTy::I32 => match s.to_i32() {
                Ok(val) => Ok(ykpack::SignedInt::I32(val)),
                Err(e) => panic!("Could not lower scalar to i32: {}", e),
            },
            ast::IntTy::I64 => match s.to_i64() {
                Ok(val) => Ok(ykpack::SignedInt::I64(val)),
                Err(e) => panic!("Could not lower scalar to i64: {}", e),
            },
            ast::IntTy::Isize => match s.to_machine_isize(&self.tcx) {
                Ok(val) => Ok(ykpack::SignedInt::Isize(val as isize)),
                Err(e) => panic!("Could not lower scalar to isize: {}", e),
            },
            _ => Err(()),
        }
    }
    fn lower_binop(
        &self,
        op: mir::BinOp,
        opnd1: &mir::Operand<'_>,
        opnd2: &mir::Operand<'_>,
        checked: bool,
    ) -> ykpack::Rvalue {
        let sir_op = binop_lowerings!(
            op, Add, Sub, Mul, Div, Rem, BitXor, BitAnd, BitOr, Shl, Shr, Eq, Lt, Le, Ne, Ge, Gt,
            Offset
        );
        let sir_opnd1 = self.lower_operand(opnd1);
        let sir_opnd2 = self.lower_operand(opnd2);

        if checked {
            debug_assert!(CHECKABLE_BINOPS.contains(&sir_op));
            ykpack::Rvalue::CheckedBinaryOp(sir_op, sir_opnd1, sir_opnd2)
        } else {
            ykpack::Rvalue::BinaryOp(sir_op, sir_opnd1, sir_opnd2)
        }
    }

    fn lower_bool(&self, s: mir::interpret::Scalar) -> ykpack::Constant {
        match s.to_bool() {
            Ok(val) => ykpack::Constant::Bool(val),
            Err(e) => panic!("Could not lower scalar (bool) to u8: {}", e),
        }
    }

    fn lower_ref(&self, place: &mir::Place<'_>) -> ykpack::Rvalue {
        let sir_place = self.lower_place(place);
        ykpack::Rvalue::Ref(sir_place)
    }
}

pub struct SirTypes {
    /// A globally unique identifier for the codegen unit.
    pub cgu_hash: ykpack::CguHash,
    /// Maps types to their index. Ordered by insertion via `IndexMap`.
    pub map: IndexMap<ykpack::Ty, ykpack::TyIndex, BuildHasherDefault<FxHasher>>,
    /// The next available type index.
    next_idx: ykpack::TyIndex,
}

impl SirTypes {
    /// Get the index of a type. If this is the first time we have seen this type, a new index is
    /// allocated and returned.
    ///
    /// Note that the index is only unique within the scope of the current compilation unit.
    /// To make a globally unique ID, we pair the index with CGU hash (see ykpack::CguHash).
    pub fn index(&mut self, t: ykpack::Ty) -> ykpack::TyIndex {
        let next_idx = &mut self.next_idx;
        *self.map.entry(t).or_insert_with(|| {
            let idx = *next_idx;
            *next_idx += 1;
            idx
        })
    }
}
