//! Serialised Intermediate Representation (SIR).
//!
//! SIR is built in-memory during code-generation (in rustc_codegen_ssa), and finally placed
//! into an ELF section at link time.

use crate::ty::{Instance, TyCtxt};
use rustc::ty::Ty;
use rustc::{mir, ty};
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_session::config::OutputType;
use rustc_span::sym;
use std::cell::RefCell;
use std::convert::TryFrom;
use std::default::Default;
use std::io;
use syntax::ast;
use ykpack;

const BUILD_SCRIPT_CRATE: &str = "build_script_build";

/// A collection of in-memory SIR data structures to be serialised.
/// Each codegen unit builds one instance of this which is then merged into a "global" instance
/// when the unit completes.
#[derive(Default)]
pub struct Sir {
    pub funcs: RefCell<Vec<ykpack::Body>>,
}

impl Sir {
    /// Returns `true` if we should collect SIR for the current crate.
    pub fn is_required(tcx: TyCtxt<'_>) -> bool {
        (tcx.sess.opts.cg.tracer.encode_sir()
            || tcx.sess.opts.output_types.contains_key(&OutputType::YkSir))
            && tcx.crate_name(LOCAL_CRATE).as_str() != BUILD_SCRIPT_CRATE
    }

    /// Returns true if there is nothing inside.
    pub fn is_empty(&self) -> bool {
        self.funcs.borrow().len() == 0
    }

    /// Merges the SIR in `other` into `self`, consuming `other`.
    pub fn update(&self, other: Self) {
        self.funcs.borrow_mut().extend(other.funcs.into_inner());
    }

    /// Writes a textual representation of the SIR to `w`. Used for `--emit yk-sir`.
    pub fn dump(&self, tcx: TyCtxt<'_>, w: &mut dyn io::Write) -> Result<(), io::Error> {
        for f in tcx.sir.funcs.borrow().iter() {
            writeln!(w, "{}", f)?;
        }
        Ok(())
    }
}

/// A structure for building the SIR of a function.
pub struct SirFuncCx {
    pub func: ykpack::Body,
}

impl SirFuncCx {
    pub fn new<'tcx>(tcx: TyCtxt<'tcx>, instance: &Instance<'tcx>, num_blocks: usize) -> Self {
        let symbol_name = format!("new__{}", tcx.symbol_name(*instance).name.as_str());

        let mut flags = 0;
        for attr in tcx.get_attrs(instance.def_id()).iter() {
            if attr.check_name(sym::trace_head) {
                flags |= ykpack::bodyflags::TRACE_HEAD;
            } else if attr.check_name(sym::trace_tail) {
                flags |= ykpack::bodyflags::TRACE_TAIL;
            }
        }

        // Since there's a one-to-one mapping between MIR and SIR blocks, we know how many SIR
        // blocks we will need and can allocate empty SIR blocks ahead of time.
        let blocks = vec![
            ykpack::BasicBlock {
                stmts: Default::default(),
                term: ykpack::Terminator::Unreachable,
            };
            num_blocks
        ];

        Self { func: ykpack::Body { symbol_name, blocks, flags } }
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
    pub fn codegen_statement(&mut self, bb: ykpack::BasicBlockIndex, stmt: &mir::Statement<'_>) {
        match stmt.kind {
            mir::StatementKind::Assign(box (ref place, ref rvalue)) => {
                let assign = self.lower_assign_stmt(place, rvalue);
                self.push_stmt(bb, assign);
            }
            mir::StatementKind::StorageLive(..) | mir::StatementKind::StorageDead(..) => {
                // Ignore. Tracer doesn't need to know about these.
            }
            _ => self.push_stmt(bb, ykpack::Statement::Unimplemented(format!("{:?}", stmt))),
        }
    }

    fn lower_assign_stmt(
        &self,
        lvalue: &mir::Place<'_>,
        rvalue: &mir::Rvalue<'_>,
    ) -> ykpack::Statement {
        match self.lower_place(lvalue) {
            Ok(lhs) => {
                let rhs = self.lower_rvalue(rvalue);
                ykpack::Statement::Assign(ykpack::Place::from(lhs), rhs)
            }
            Err(_) => ykpack::Statement::Unimplemented(format!("Assign({:?})", lvalue)),
        }
    }

    fn lower_rvalue(&self, rvalue: &mir::Rvalue<'_>) -> ykpack::Rvalue {
        match rvalue {
            mir::Rvalue::Use(mir::Operand::Copy(place))
            | mir::Rvalue::Use(mir::Operand::Move(place)) => match self.lower_place(place) {
                Ok(p) => ykpack::Rvalue::Use(ykpack::Operand::from(p)),
                Err(_) => ykpack::Rvalue::Unimplemented(format!("Use({:?})", place).to_string()),
            },
            mir::Rvalue::Use(mir::Operand::Constant(box constant)) => {
                match self.lower_constant(constant) {
                    Ok(c) => ykpack::Rvalue::Use(ykpack::Operand::Constant(c)),
                    Err(_) => {
                        ykpack::Rvalue::Unimplemented(format!("Use({:?})", constant).to_string())
                    }
                }
            }
            _ => ykpack::Rvalue::Unimplemented(format!("{:?}", rvalue).to_string()),
        }
    }

    fn lower_place(&self, place: &mir::Place<'_>) -> Result<ykpack::Local, ()> {
        if place.projection.len() == 0 {
            Ok(self.lower_local(place.local))
        } else {
            // For now we are not dealing with projections.
            Err(())
        }
    }

    fn lower_local(&self, local: mir::Local) -> ykpack::Local {
        // For the lowering of `Local`s we currently assume a 1:1 mapping from MIR to SIR. If this
        // mapping turns out to be impossible or impractial, this is the place to change it.
        ykpack::Local(local.as_u32())
    }

    fn lower_constant(&self, constant: &mir::Constant<'_>) -> Result<ykpack::Constant, ()> {
        match constant.literal.val {
            ty::ConstKind::Value(mir::interpret::ConstValue::Scalar(s)) => {
                self.lower_scalar(constant.literal.ty, s)
            }
            _ => Err(()),
        }
    }

    fn lower_scalar(&self, ty: Ty<'_>, s: mir::interpret::Scalar) -> Result<ykpack::Constant, ()> {
        match ty.kind {
            ty::Uint(uint) => self
                .lower_uint(uint, s)
                .map(|i| ykpack::Constant::Int(ykpack::ConstantInt::UnsignedInt(i))),
            _ => Err(()),
        }
    }

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
            _ => Err(()),
        }
    }
}
