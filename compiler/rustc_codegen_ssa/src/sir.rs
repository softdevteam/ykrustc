//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during code-generation (in rustc_codegen_ssa), and finally placed
//! into an ELF section at link time.

use crate::traits::{BuilderMethods, SirMethods};
use indexmap::IndexMap;
use rustc_ast::ast;
use rustc_ast::ast::{IntTy, UintTy};
use rustc_data_structures::fx::{FxHashMap, FxHasher};
use rustc_hir::{self, def_id::LOCAL_CRATE};
use rustc_middle::mir;
use rustc_middle::ty::print::with_no_trimmed_paths;
use rustc_middle::ty::AdtDef;
use rustc_middle::ty::TypeFoldable;
use rustc_middle::ty::{self, layout::TyAndLayout, TyCtxt};
use rustc_middle::ty::{Instance, Ty};
use rustc_span::sym;
use rustc_target::abi::FieldsShape;
use rustc_target::abi::VariantIdx;
use std::cell::RefCell;
use std::convert::{TryFrom, TryInto};
use std::default::Default;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::io;
use ykpack;

pub const BUILD_SCRIPT_CRATE: &str = "build_script_build";
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
    /// The instance we are lowering.
    instance: Instance<'tcx>,
    /// The MIR body of the above instance.
    mir: &'tcx mir::Body<'tcx>,
    /// The SIR function we are building.
    pub func: ykpack::Body,
    /// Maps each MIR local to a SIR IPlace.
    var_map: FxHashMap<mir::Local, ykpack::IPlace>,
    /// The next SIR local variable index to be allocated.
    next_sir_local: ykpack::LocalIndex,
    /// The compiler's type context.
    tcx: TyCtxt<'tcx>,
}

impl SirFuncCx<'tcx> {
    pub fn new<Bx: BuilderMethods<'a, 'tcx>>(
        bx: &Bx,
        tcx: TyCtxt<'tcx>,
        instance: &Instance<'tcx>,
        mir: &'tcx mir::Body<'tcx>,
    ) -> Self {
        let mut flags = ykpack::BodyFlags::empty();
        for attr in tcx.get_attrs(instance.def_id()).iter() {
            if tcx.sess.check_name(attr, sym::do_not_trace) {
                flags |= ykpack::BodyFlags::DO_NOT_TRACE;
            } else if tcx.sess.check_name(attr, sym::interp_step) {
                // Check various properties of the interp_step at compile time.
                if !tcx.upvars_mentioned(instance.def_id()).is_none() {
                    tcx.sess.span_fatal(
                        tcx.def_span(instance.def_id()),
                        "The #[interp_step] function must not capture from its environment",
                    );
                }

                if mir.args_iter().count() != 1 {
                    tcx.sess.span_fatal(
                        tcx.def_span(instance.def_id()),
                        "The #[interp_step] function must accept only one argument",
                    );
                }

                let arg_ok = if let ty::Ref(_, inner_ty, rustc_hir::Mutability::Mut) =
                    mir.local_decls[mir::Local::from_u32(1)].ty.kind()
                {
                    if let ty::Adt(def, _) = inner_ty.kind() { def.is_struct() } else { false }
                } else {
                    false
                };
                if !arg_ok {
                    tcx.sess.span_err(
                        tcx.def_span(instance.def_id()),
                        "The #[interp_step] function must accept a mutable reference to a struct",
                    );
                }

                if !mir.return_ty().is_unit() {
                    tcx.sess.span_err(
                        tcx.def_span(instance.def_id()),
                        "The #[interp_step] function must return unit",
                    );
                }

                flags |= ykpack::BodyFlags::INTERP_STEP;
            } else if tcx.sess.check_name(attr, sym::trace_debug) {
                flags |= ykpack::BodyFlags::TRACE_DEBUG;
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

        // There will be at least as many locals in the SIR as there are in the MIR.
        let local_decls = Vec::with_capacity(mir.local_decls.len());
        let symbol_name = String::from(&*tcx.symbol_name(*instance).name);

        let crate_name = tcx.crate_name(instance.def_id().krate).as_str();
        if crate_name == "core" || crate_name == "alloc" {
            flags |= ykpack::BodyFlags::DO_NOT_TRACE;
        }
        let var_map: FxHashMap<mir::Local, ykpack::IPlace> = FxHashMap::default();

        let mut this = Self {
            instance: instance.clone(),
            mir,
            func: ykpack::Body { symbol_name, blocks, flags, local_decls, num_args: mir.arg_count },
            var_map,
            next_sir_local: 0,
            tcx,
        };

        // Allocate return local and args in their anchored positions.
        for idx in 0..=mir.arg_count {
            let ml = mir::Local::from_usize(idx);
            let sirty =
                this.lower_ty_and_layout(bx, &this.mono_layout_of(bx, this.mir.local_decls[ml].ty));
            this.func.local_decls.push(ykpack::LocalDecl { ty: sirty, referenced: false });
            this.var_map.insert(
                ml,
                ykpack::IPlace::Val {
                    local: ykpack::Local(u32::try_from(idx).unwrap()),
                    off: 0,
                    ty: sirty,
                },
            );
            this.next_sir_local += 1;
        }
        this
    }

    /// Returns the IPlace corresponding with MIR local `ml`. A new IPlace is constructed if we've
    /// never seen this MIR local before.
    fn sir_local<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        ml: &mir::Local,
    ) -> ykpack::IPlace {
        let ret = if let Some(ip) = self.var_map.get(ml) {
            ip.clone()
        } else {
            let sirty = self
                .lower_ty_and_layout(bx, &self.mono_layout_of(bx, self.mir.local_decls[*ml].ty));
            let nl = self.new_sir_local(sirty);
            self.var_map.insert(*ml, nl.clone());
            nl
        };
        ret
    }

    /// Returns a zero-offset IPlace for a new SIR local.
    fn new_sir_local(&mut self, sirty: ykpack::TypeId) -> ykpack::IPlace {
        let idx = self.next_sir_local;
        self.next_sir_local += 1;
        self.func.local_decls.push(ykpack::LocalDecl { ty: sirty, referenced: false });
        ykpack::IPlace::Val { local: ykpack::Local(idx), off: 0, ty: sirty }
    }

    /// Tells the tracer codegen that the local `l` is referenced, and that is should be allocated
    /// directly to the stack and not a register. You can't reference registers.
    fn notify_referenced(&mut self, l: ykpack::Local) {
        let idx = usize::try_from(l.0).unwrap();
        let slot = self.func.local_decls.get_mut(idx).unwrap();
        slot.referenced = true;
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

    pub fn set_term_switchint<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        discr: &mir::Operand<'tcx>,
        targets: &mir::SwitchTargets,
    ) {
        let mut values = Vec::new();
        let mut target_bbs = Vec::new();
        for (v, t) in targets.iter() {
            values.push(ykpack::SerU128::new(v));
            target_bbs.push(t.as_u32());
        }
        let new_term = ykpack::Terminator::SwitchInt {
            discr: self.lower_operand(bx, bb, discr),
            values,
            target_bbs,
            otherwise_bb: targets.otherwise().as_u32(),
        };
        self.set_terminator(bb, new_term);
    }

    pub fn set_term_assert<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: mir::BasicBlock,
        cond: &mir::Operand<'tcx>,
        expected: bool,
        target_bb: mir::BasicBlock,
    ) {
        let bb = bb.as_u32();
        let cond_ip = self.lower_operand(bx, bb, cond);
        let term =
            ykpack::Terminator::Assert { cond: cond_ip, expected, target_bb: target_bb.as_u32() };
        self.set_terminator(bb, term);
    }

    /// Converts a MIR statement to SIR, appending the result to `bb`.
    pub fn lower_statement<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        stmt: &mir::Statement<'tcx>,
    ) {
        match stmt.kind {
            mir::StatementKind::Assign(box (ref place, ref rvalue)) => {
                self.lower_assign_stmt(bx, bb, place, rvalue)
            }
            // We compute our own liveness in Yorick, so these are ignored.
            mir::StatementKind::StorageLive(_) | mir::StatementKind::StorageDead(_) => {}
            _ => self.push_stmt(bb, ykpack::Statement::Unimplemented(format!("{:?}", stmt))),
        }
    }

    fn lower_assign_stmt<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        lvalue: &mir::Place<'tcx>,
        rvalue: &mir::Rvalue<'tcx>,
    ) {
        let dest_ty = lvalue.ty(self.mir, self.tcx).ty;
        let rhs = self.lower_rvalue(bx, bb, dest_ty, rvalue);

        // FIXME optimisation.
        // If the store can't affect any state observable from outside the function, then don't
        // emit a store, but instead just update the mapping. This will remove many unnecessary
        // stores and also act as a kind of constant propagation.
        let lhs = self.lower_place(bx, bb, lvalue);
        self.push_stmt(bb, ykpack::Statement::Store(lhs, rhs));
    }

    pub fn lower_operand<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        operand: &mir::Operand<'tcx>,
    ) -> ykpack::IPlace {
        match operand {
            mir::Operand::Copy(place) | mir::Operand::Move(place) => {
                self.lower_place(bx, bb, place)
            }
            mir::Operand::Constant(cst) => self.lower_constant(bx, cst),
        }
    }

    fn lower_cast_misc<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        op: &mir::Operand<'tcx>,
        ty: Ty<'tcx>,
    ) -> ykpack::IPlace {
        let lop = self.lower_operand(bx, bb, op);

        // The ty we are casting to is equivalent to dest_ty.
        let ty = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, ty));
        let dest_ip = self.new_sir_local(ty);
        let stmt = ykpack::Statement::Cast(dest_ip.clone(), lop);
        self.push_stmt(bb, stmt);
        dest_ip
    }

    fn lower_rvalue<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        dest_ty: Ty<'tcx>,
        rvalue: &mir::Rvalue<'tcx>,
    ) -> ykpack::IPlace {
        match rvalue {
            mir::Rvalue::Use(opnd) => self.lower_operand(bx, bb, opnd),
            mir::Rvalue::Ref(_, _, p) => self.lower_ref(bx, bb, dest_ty, p),
            mir::Rvalue::BinaryOp(op, opnd1, opnd2) => {
                self.lower_binop(bx, bb, dest_ty, *op, opnd1, opnd2, false)
            }
            mir::Rvalue::CheckedBinaryOp(op, opnd1, opnd2) => {
                self.lower_binop(bx, bb, dest_ty, *op, opnd1, opnd2, true)
            }
            mir::Rvalue::Cast(mir::CastKind::Misc, op, ty) => self.lower_cast_misc(bx, bb, op, ty),
            _ => ykpack::IPlace::Unimplemented(with_no_trimmed_paths(|| {
                format!("unimplemented rvalue: {:?}", rvalue)
            })),
        }
    }

    fn monomorphize<T>(&self, value: &T) -> T
    where
        T: TypeFoldable<'tcx> + Copy,
    {
        self.instance.subst_mir_and_normalize_erasing_regions(
            self.tcx,
            ty::ParamEnv::reveal_all(),
            *value,
        )
    }

    /// Wrapper for bx.layout_of() which ensures the type is first monomorphised.
    fn mono_layout_of<Bx: BuilderMethods<'a, 'tcx>>(
        &self,
        bx: &Bx,
        t: Ty<'tcx>,
    ) -> TyAndLayout<'tcx> {
        bx.layout_of(self.monomorphize(&t))
    }

    /// Apply an offset to an IPlace.
    fn offset_iplace<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        mut ip: ykpack::IPlace,
        add: ykpack::OffT,
        mirty: Ty<'tcx>,
    ) -> ykpack::IPlace {
        match &mut ip {
            ykpack::IPlace::Val { off, ty, .. } => {
                *off += add;
                *ty = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, mirty));
                ip
            }
            ykpack::IPlace::Indirect { off, ty, .. } => {
                *off += add;
                *ty = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, mirty));
                ip
            }
            ykpack::IPlace::Const { .. } => {
                ykpack::IPlace::Unimplemented("offset_iplace on a constant".to_owned())
            }
            ykpack::IPlace::Unimplemented(_) => ip,
        }
    }

    pub fn lower_place<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        place: &mir::Place<'_>,
    ) -> ykpack::IPlace {
        // We start with the base local and project away from it.
        let mut cur_iplace = self.sir_local(bx, &place.local);
        let mut cur_mirty = self.monomorphize(&self.mir.local_decls[place.local].ty);

        // Loop over the projection chain, updating cur_iplace as we go.
        for pj in place.projection {
            let next_mirty = match pj {
                mir::ProjectionElem::Field(f, _) => {
                    let fi = f.as_usize();
                    match cur_mirty.kind() {
                        ty::Adt(def, _) => {
                            if def.is_struct() {
                                let ty_lay = self.mono_layout_of(bx, cur_mirty);
                                let st_lay = ty_lay.for_variant(bx, VariantIdx::from_u32(0));
                                if let FieldsShape::Arbitrary { offsets, .. } = &st_lay.fields {
                                    let new_mirty = st_lay.field(bx, fi).ty;
                                    cur_iplace = self.offset_iplace(
                                        bx,
                                        cur_iplace,
                                        offsets[fi].bytes().try_into().unwrap(),
                                        new_mirty,
                                    );
                                    new_mirty
                                } else {
                                    return ykpack::IPlace::Unimplemented(format!(
                                        "struct field shape: {:?}",
                                        st_lay.fields
                                    ));
                                }
                            } else if def.is_enum() {
                                return ykpack::IPlace::Unimplemented(format!(
                                    "enum_projection: {:?}",
                                    def
                                ));
                            } else {
                                return ykpack::IPlace::Unimplemented(format!("adt: {:?}", def));
                            }
                        }
                        ty::Tuple(..) => {
                            let tup_lay = self.mono_layout_of(bx, cur_mirty);
                            match &tup_lay.fields {
                                FieldsShape::Arbitrary { offsets, .. } => {
                                    let new_mirty = tup_lay.field(bx, fi).ty;
                                    cur_iplace = self.offset_iplace(
                                        bx,
                                        cur_iplace,
                                        offsets[fi].bytes().try_into().unwrap(),
                                        new_mirty,
                                    );
                                    new_mirty
                                }
                                _ => {
                                    return ykpack::IPlace::Unimplemented(format!(
                                        "tuple field shape: {:?}",
                                        tup_lay.fields
                                    ));
                                }
                            }
                        }
                        _ => {
                            return ykpack::IPlace::Unimplemented(format!(
                                "field access on: {:?}",
                                cur_mirty
                            ));
                        }
                    }
                }
                mir::ProjectionElem::Index(idx) => {
                    if let ty::Array(elem_ty, ..) = cur_mirty.kind() {
                        let arr_lay = self.mono_layout_of(bx, cur_mirty);
                        let elem_size = match &arr_lay.fields {
                            FieldsShape::Array { stride, .. } => {
                                u32::try_from(stride.bytes_usize()).unwrap()
                            }
                            _ => unreachable!(),
                        };

                        let dest_ty =
                            self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, elem_ty));
                        let dest = self.new_sir_local(dest_ty);
                        let idx_ip = self.sir_local(bx, &idx);
                        let stmt = ykpack::Statement::DynOffs {
                            dest: dest.clone(),
                            base: cur_iplace.clone(),
                            idx: idx_ip,
                            scale: elem_size,
                        };
                        self.push_stmt(bb, stmt);
                        cur_iplace = dest.to_indirect(dest_ty);
                        elem_ty
                    } else {
                        return ykpack::IPlace::Unimplemented(format!("index on {:?}", cur_mirty));
                    }
                }
                mir::ProjectionElem::Deref => {
                    if let ty::Ref(_, ty, _) = cur_mirty.kind() {
                        if let ykpack::IPlace::Indirect { ty: dty, .. } = cur_iplace {
                            // We are dereffing an already indirect place, so we emit an
                            // intermediate store to strip away one level of indirection.
                            let dest = self.new_sir_local(dty);
                            let deref = ykpack::Statement::Store(dest.clone(), cur_iplace.clone());
                            self.push_stmt(bb, deref);
                            cur_iplace = dest;
                        }

                        if let Some(l) = cur_iplace.local() {
                            self.notify_referenced(l);
                        }

                        let tyid = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, ty));
                        cur_iplace = cur_iplace.to_indirect(tyid);
                        ty
                    } else {
                        return ykpack::IPlace::Unimplemented(format!("deref non-ref"));
                    }
                }
                _ => return ykpack::IPlace::Unimplemented(format!("projection: {:?}", pj)),
            };
            cur_mirty = self.monomorphize(&next_mirty);
        }
        cur_iplace
    }

    fn lower_constant<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        constant: &mir::Constant<'tcx>,
    ) -> ykpack::IPlace {
        match constant.literal.val {
            ty::ConstKind::Value(mir::interpret::ConstValue::Scalar(s)) => {
                let val = self.lower_scalar(bx, constant.literal.ty, s);
                let ty =
                    self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, constant.literal.ty));
                ykpack::IPlace::Const { val, ty }
            }
            _ => ykpack::IPlace::Unimplemented(with_no_trimmed_paths(|| {
                format!("unimplemented constant: {:?}", constant)
            })),
        }
    }

    fn lower_scalar<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        ty: Ty<'tcx>,
        s: mir::interpret::Scalar,
    ) -> ykpack::Constant {
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
            ty::Tuple(_) => {
                // FIXME for now just the unit tuple. Need to implement arbitrary scalar tuples.
                if ty.is_unit() {
                    let tyid = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, ty));
                    ykpack::Constant::Tuple(tyid)
                } else {
                    ykpack::Constant::Unimplemented(format!(
                        "unimplemented scalar: {:?}",
                        ty.kind()
                    ))
                }
            }
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

    fn lower_binop<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        dest_ty: Ty<'tcx>,
        op: mir::BinOp,
        opnd1: &mir::Operand<'tcx>,
        opnd2: &mir::Operand<'tcx>,
        checked: bool,
    ) -> ykpack::IPlace {
        let op = binop_lowerings!(
            op, Add, Sub, Mul, Div, Rem, BitXor, BitAnd, BitOr, Shl, Shr, Eq, Lt, Le, Ne, Ge, Gt,
            Offset
        );
        let opnd1 = self.lower_operand(bx, bb, opnd1);
        let opnd2 = self.lower_operand(bx, bb, opnd2);

        if checked {
            debug_assert!(CHECKABLE_BINOPS.contains(&op));
        }

        let ty = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, dest_ty));
        let dest_ip = self.new_sir_local(ty);
        let stmt = ykpack::Statement::BinaryOp { dest: dest_ip.clone(), op, opnd1, opnd2, checked };
        self.push_stmt(bb, stmt);
        dest_ip
    }

    fn lower_bool(&self, s: mir::interpret::Scalar) -> ykpack::Constant {
        match s.to_bool() {
            Ok(val) => ykpack::Constant::Bool(val),
            Err(e) => panic!("Could not lower scalar (bool) to u8: {}", e),
        }
    }

    fn lower_ref<Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        bb: ykpack::BasicBlockIndex,
        dest_ty: Ty<'tcx>,
        place: &mir::Place<'tcx>,
    ) -> ykpack::IPlace {
        let ty = self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, dest_ty));
        let dest_ip = self.new_sir_local(ty);
        let src_ip = self.lower_place(bx, bb, place);
        let mkref = ykpack::Statement::MkRef(dest_ip.clone(), src_ip.clone());
        if let Some(src_local) = src_ip.local() {
            self.notify_referenced(src_local);
        }
        self.push_stmt(bb, mkref);
        dest_ip
    }

    fn lower_ty_and_layout<'a, Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        ty_layout: &TyAndLayout<'tcx>,
    ) -> ykpack::TypeId {
        let sir_ty = match ty_layout.ty.kind() {
            ty::Int(si) => self.lower_signed_int_ty(*si),
            ty::Uint(ui) => self.lower_unsigned_int_ty(*ui),
            ty::Adt(adt_def, ..) => self.lower_adt_ty(bx, adt_def, &ty_layout),
            ty::Array(elem_ty, len) => {
                let align = usize::try_from(ty_layout.layout.align.abi.bytes()).unwrap();
                let size = usize::try_from(ty_layout.layout.size.bytes()).unwrap();
                ykpack::Ty::Array {
                    elem_ty: self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, elem_ty)),
                    len: usize::try_from(len.eval_usize(self.tcx, ty::ParamEnv::reveal_all()))
                        .unwrap(),
                    size_align: ykpack::SizeAndAlign {
                        size: size.try_into().unwrap(),
                        align: align.try_into().unwrap(),
                    },
                }
            }
            ty::Slice(typ) => {
                ykpack::Ty::Slice(self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, typ)))
            }
            ty::Ref(_, typ, _) => {
                ykpack::Ty::Ref(self.lower_ty_and_layout(bx, &self.mono_layout_of(bx, typ)))
            }
            ty::Bool => ykpack::Ty::Bool,
            ty::Char => ykpack::Ty::Char,
            ty::Tuple(..) => self.lower_tuple_ty(bx, ty_layout),
            _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
        };
        bx.cx().define_sir_type(sir_ty)
    }

    fn lower_signed_int_ty(&mut self, si: IntTy) -> ykpack::Ty {
        match si {
            IntTy::Isize => ykpack::Ty::SignedInt(ykpack::SignedIntTy::Isize),
            IntTy::I8 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I8),
            IntTy::I16 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I16),
            IntTy::I32 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I32),
            IntTy::I64 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I64),
            IntTy::I128 => ykpack::Ty::SignedInt(ykpack::SignedIntTy::I128),
        }
    }

    fn lower_unsigned_int_ty(&mut self, ui: UintTy) -> ykpack::Ty {
        match ui {
            UintTy::Usize => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::Usize),
            UintTy::U8 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U8),
            UintTy::U16 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U16),
            UintTy::U32 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U32),
            UintTy::U64 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U64),
            UintTy::U128 => ykpack::Ty::UnsignedInt(ykpack::UnsignedIntTy::U128),
        }
    }

    fn lower_tuple_ty<'a, Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
        bx: &Bx,
        ty_layout: &TyAndLayout<'tcx>,
    ) -> ykpack::Ty {
        let align = i32::try_from(ty_layout.layout.align.abi.bytes()).unwrap();
        let size = i32::try_from(ty_layout.layout.size.bytes()).unwrap();

        match &ty_layout.fields {
            FieldsShape::Arbitrary { offsets, .. } => {
                let mut sir_offsets = Vec::new();
                let mut sir_tys = Vec::new();
                for (idx, off) in offsets.iter().enumerate() {
                    sir_tys.push(self.lower_ty_and_layout(bx, &ty_layout.field(bx, idx)));
                    sir_offsets.push(off.bytes().try_into().unwrap());
                }

                ykpack::Ty::Tuple(ykpack::TupleTy {
                    fields: ykpack::Fields { offsets: sir_offsets, tys: sir_tys },
                    size_align: ykpack::SizeAndAlign { size, align },
                })
            }
            _ => ykpack::Ty::Unimplemented(format!("{:?}", ty_layout)),
        }
    }

    fn lower_adt_ty<'a, Bx: BuilderMethods<'a, 'tcx>>(
        &mut self,
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
                    for (idx, off) in offsets.iter().enumerate() {
                        sir_tys.push(self.lower_ty_and_layout(bx, &struct_layout.field(bx, idx)));
                        sir_offsets.push(off.bytes().try_into().unwrap());
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

pub mod labels {
    use object::{Object, ObjectSection};
    use std::{
        convert::TryFrom,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    /// Splits a Yorick mapping label name into its constituent fields.
    fn split_label_name(s: &str) -> (String, u32) {
        let data: Vec<&str> = s.split(':').collect();
        debug_assert!(data.len() == 3);
        let sym = data[1].to_owned();
        let bb_idx = data[2].parse::<u32>().unwrap();
        (sym, bb_idx)
    }

    /// Add a Yorick label section to the specified executable.
    pub fn add_yk_label_section(exe_path: &Path) {
        let labels = extract_dwarf_labels(exe_path).unwrap();
        let mut tempf = tempfile::NamedTempFile::new().unwrap();
        bincode::serialize_into(&mut tempf, &labels).unwrap();
        add_section(exe_path, tempf.path());
    }

    /// Copies the bytes in `sec_data_path` into a new Yorick label section of an executable.
    fn add_section(exe_path: &Path, sec_data_path: &Path) {
        let mut out_path = PathBuf::from(exe_path);
        out_path.set_extension("with_labels");
        Command::new("objcopy")
            .args(&[
                "--add-section",
                &format!("{}={}", ykpack::YKLABELS_SECTION, sec_data_path.to_str().unwrap()),
                "--set-section-flags",
                &format!("{}=contents,alloc,readonly", ykpack::YKLABELS_SECTION),
                exe_path.to_str().unwrap(),
                out_path.to_str().unwrap(),
            ])
            .output()
            .expect("failed to insert labels section");
        std::fs::rename(out_path, exe_path).unwrap();
    }

    /// Walks the DWARF tree of the specified executable and extracts Yorick location mapping
    /// labels. The labels are returned in a vector sorted by the file offset in the executable.
    fn extract_dwarf_labels(exe_filename: &Path) -> Result<Vec<ykpack::SirLabel>, gimli::Error> {
        let file = fs::File::open(exe_filename).unwrap();
        let mmap = unsafe { memmap::Mmap::map(&file).unwrap() };
        let object = object::File::parse(&*mmap).unwrap();
        let endian = if object.is_little_endian() {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };
        let loader = |id: gimli::SectionId| -> Result<&[u8], gimli::Error> {
            Ok(object
                .section_by_name(id.name())
                .map(|sec| sec.data().expect("failed to decompress section"))
                .unwrap_or(&[] as &[u8]))
        };
        let sup_loader = |_| Ok(&[] as &[u8]);
        let dwarf_cow = gimli::Dwarf::load(&loader, &sup_loader)?;
        let borrow_section: &dyn for<'a> Fn(
            &&'a [u8],
        )
            -> gimli::EndianSlice<'a, gimli::RunTimeEndian> =
            &|section| gimli::EndianSlice::new(section, endian);
        let dwarf = dwarf_cow.borrow(&borrow_section);
        let mut iter = dwarf.units();
        let mut subaddr = None;
        let mut labels = Vec::new();
        while let Some(header) = iter.next()? {
            let unit = dwarf.unit(header)?;
            let mut entries = unit.entries();
            while let Some((_, entry)) = entries.next_dfs()? {
                if entry.tag() == gimli::DW_TAG_subprogram {
                    if let Some(_name) = entry.attr_value(gimli::DW_AT_linkage_name)? {
                        if let Some(lowpc) = entry.attr_value(gimli::DW_AT_low_pc)? {
                            if let gimli::AttributeValue::Addr(v) = lowpc {
                                // We can not accurately insert labels at the beginning of
                                // functions, because the label is offset by the function headers.
                                // We thus simply remember the subprogram's address so we can later
                                // assign it to the first block (ending with '_0') of this
                                // subprogram.
                                subaddr = Some(v as u64);
                            } else {
                                panic!("Error reading dwarf information. Expected type 'Addr'.")
                            }
                        }
                    }
                } else if entry.tag() == gimli::DW_TAG_label {
                    if let Some(name) = entry.attr_value(gimli::DW_AT_name)? {
                        if let Some(es) = name.string_value(&dwarf.debug_str) {
                            let s = es.to_string()?;
                            if s.starts_with("__YK_") {
                                if let Some(lowpc) = entry.attr_value(gimli::DW_AT_low_pc)? {
                                    if subaddr.is_some() && s.ends_with("_0") {
                                        // This is the first block of the subprogram. Assign its
                                        // label to the subprogram's address.
                                        let (fsym, bb) = split_label_name(s);
                                        labels.push(ykpack::SirLabel {
                                            off: usize::try_from(subaddr.unwrap()).unwrap(),
                                            symbol_name: fsym,
                                            bb,
                                        });
                                        subaddr = None;
                                    } else {
                                        let (fsym, bb) = split_label_name(s);
                                        if let gimli::AttributeValue::Addr(v) = lowpc {
                                            labels.push(ykpack::SirLabel {
                                                off: usize::try_from(v as u64).unwrap(),
                                                symbol_name: fsym,
                                                bb,
                                            });
                                        } else {
                                            panic!(
                                                "Error reading dwarf information. Expected type 'Addr'."
                                            );
                                        }
                                    }
                                } else {
                                    // Ignore labels that have no address.
                                }
                            }
                        }
                    }
                }
            }
        }

        labels.sort_by_key(|l| l.off);
        Ok(labels)
    }
}
