// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module converts MIR into Yorick SIR (Serialised IR).
//! Note that we preserve the MIR block structure when lowering to SIR.
//!
//! Note that SIR assumes the abort panic strategy.
//!
//! Serialisation itself is performed by an external library: ykpack.

use rustc::ty::{TyCtxt, TyS, Const, TyKind, Ty, Instance};
use syntax::ast::{UintTy, IntTy};
use rustc::hir::def_id::DefId;
use rustc::mir::{
    Body, Local, BasicBlockData, Statement, StatementKind, Place, PlaceBase, Rvalue, Operand,
    Terminator, TerminatorKind, Constant, BinOp
};
use rustc::mir::interpret::{ConstValue, Scalar};
use rustc::util::nodemap::DefIdSet;
use std::path::PathBuf;
use std::fs::File;
use rustc_yk_link::YkExtraLinkObject;
use std::fs;
use std::io::Write;
use std::error::Error;
use std::mem::size_of;
use rustc_data_structures::indexed_vec::IndexVec;
use ykpack;
use rustc::ty::fold::TypeFoldable;

const SECTION_NAME: &'static str = ".yk_sir";
const TMP_EXT: &'static str = ".yk_sir.tmp";

/// Describes how to output MIR.
pub enum SirMode {
    /// Write MIR into an object file for linkage. The inner path should be the path to the main
    /// executable (from this we generate a filename for the resulting object).
    Default(PathBuf),
    /// Write MIR in textual form the specified path.
    TextDump(PathBuf),
}

/// A conversion context holds the state needed to perform the SIR lowering.
struct ConvCx<'a, 'tcx, 'gcx> {
    /// The compiler's god struct. Needed for queries etc.
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>,
    /// Monotonically increasing number used to give SIR variables a unique ID.
    next_sir_var: ykpack::LocalIndex,
    /// A mapping from MIR variables to SIR variables.
    var_map: IndexVec<Local, Option<ykpack::Local>>,
    /// The MIR we are lowering.
    mir: &'a Body<'tcx>,
    /// The DefId of the above MIR.
    def_id: DefId,
}

impl<'a, 'tcx, 'gcx> ConvCx<'a, 'tcx, 'gcx> {
    fn new(tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_id: DefId, mir: &'a Body<'tcx>) -> Self {
        Self {
            tcx,
            next_sir_var: 0,
            var_map: IndexVec::new(),
            mir,
            def_id,
        }
    }

    /// Returns a guaranteed unique SIR variable index.
    fn new_sir_var(&mut self) -> ykpack::LocalIndex {
        let var_idx = self.next_sir_var;
        self.next_sir_var += 1;
        var_idx
    }

    /// Get the SIR variable for the specified MIR variable, creating a fresh variable if needed.
    fn sir_var(&mut self, local: Local) -> ykpack::Local {
        let local_u32 = local.as_u32();

        // Resize the backing Vec if necessary.
        // Vector indices are `usize`, but variable indices are `u32`, so converting from a
        // variable index to a vector index is always safe if a `usize` can express all `u32`s.
        assert!(size_of::<usize>() >= size_of::<u32>());
        if self.var_map.len() <= local_u32 as usize {
            self.var_map.resize(local_u32.checked_add(1).unwrap() as usize, None);
        }

        self.var_map[local].unwrap_or_else(|| {
            let idx = self.new_sir_var();
            let ty = 0; // FIXME notimplemented.
            let sir_local = ykpack::Local::new(idx, ty);
            self.var_map[local] = Some(sir_local);
            sir_local
        })
    }

    /// Entry point for the lowering process.
    fn lower(&mut self) -> ykpack::Body {
        let dps = self.tcx.def_path_str(self.def_id);
        ykpack::Body::new(self.lower_def_id(&self.def_id.to_owned()),
            dps, self.mir.basic_blocks().iter().map(|b| self.lower_block(b)).collect())
    }

    fn lower_def_id(&mut self, def_id: &DefId) -> ykpack::DefId {
        lower_def_id(self.tcx, def_id)
    }

    fn lower_block(&mut self, blk: &BasicBlockData<'tcx>) -> ykpack::BasicBlock {
        let term = match self.lower_terminator(blk.terminator()) {
            Ok(t) => t,
            _ => ykpack::Terminator::Unimplemented(format!("{:?}", blk.terminator())),
        };
        ykpack::BasicBlock::new(
            blk.statements.iter().map(|s| self.lower_stmt(s)).flatten().collect(), term)
    }

    fn lower_terminator(&mut self, term: &Terminator<'tcx>) -> Result<ykpack::Terminator, ()> {
        match term.kind {
            TerminatorKind::Goto{target: target_bb} =>
                Ok(ykpack::Terminator::Goto(u32::from(target_bb))),
            TerminatorKind::SwitchInt{ref discr, ref values, ref targets, ..} => {
                match self.lower_operand(discr) {
                    Ok(ykpack::Operand::Local(local)) => {
                        let mut target_bbs: Vec<ykpack::BasicBlockIndex> =
                            targets.iter().map(|bb| u32::from(*bb)).collect();
                        // In the `SwitchInt` MIR terminator the last block index in the targets
                        // list is the block to jump to if the discriminant matches none of the
                        // values. In SIR, we use a dedicated field to avoid confusion.
                        let otherwise_bb = target_bbs.pop().expect("no otherwise block");
                        Ok(ykpack::Terminator::SwitchInt{
                            local,
                            values: values.iter().map(|u| ykpack::SerU128::new(*u)).collect(),
                            target_bbs,
                            otherwise_bb,
                        })
                    },
                    _ => Err(()), // FIXME
                }
            },
            // Since SIR uses the abort panic strategy, Resume and Abort are redundant.
            TerminatorKind::Resume | TerminatorKind::Abort => Err(()),
            TerminatorKind::Return => Ok(ykpack::Terminator::Return),
            TerminatorKind::Unreachable => Ok(ykpack::Terminator::Unreachable),
            TerminatorKind::Drop{target: target_bb, ref location, ..} =>
                Ok(ykpack::Terminator::Drop{
                    location: self.lower_place(location)?,
                    target_bb: u32::from(target_bb),
                }),
            TerminatorKind::DropAndReplace{target: target_bb, ref location, ref value, ..} =>
                Ok(ykpack::Terminator::DropAndReplace{
                    location: self.lower_place(location)?,
                    value: self.lower_operand(value)?,
                    target_bb: u32::from(target_bb),
                }),
            TerminatorKind::Call{ref func, ref destination, .. } => {
                let ser_oper = if let Operand::Constant(box Constant {
                    literal: Const {
                        ty: &TyS {
                            sty: TyKind::FnDef(ref target_def_id, ref substs), ..
                        }, ..
                    }, ..
                }, ..) = func {
                    // A statically known call target.
                    let inst = Instance::new(*target_def_id, substs);
                    let sym_name = match substs.needs_subst() {
                        // If the instance isn't full instantiated, then it has no symbol name.
                        true => None,
                        false => Some(self.tcx.symbol_name(inst).as_str().get().to_owned()),
                    };
                    ykpack::CallOperand::Fn(self.lower_def_id(target_def_id), sym_name)
                } else {
                    // FIXME -- implement other callables.
                    ykpack::CallOperand::Unknown
                };

                let ret_bb = destination.as_ref().map(|(_, bb)| u32::from(*bb));
                Ok(ykpack::Terminator::Call{
                    operand: ser_oper,
                    ret_bb: ret_bb,
                })
            },
            TerminatorKind::Assert{target: target_bb, ref cond, expected, ..} => {
                let local = match self.lower_operand(cond)? {
                    ykpack::Operand::Local(l) => l,
                    // Constant assertions will have been optimised out.
                    ykpack::Operand::Constant(_) => panic!("constant assertion"),
                };
                Ok(ykpack::Terminator::Assert{
                    cond: local,
                    expected,
                    target_bb: u32::from(target_bb),
                })
            },
            // We will never see these MIR terminators, as they are not present at code-gen time.
            TerminatorKind::Yield{..} => panic!("Tried to lower a Yield terminator"),
            TerminatorKind::GeneratorDrop => panic!("Tried to lower a GeneratorDrop terminator"),
            TerminatorKind::FalseEdges{..} => panic!("Tried to lower a FalseEdges terminator"),
            TerminatorKind::FalseUnwind{..} => panic!("Tried to lower a FalseUnwind terminator"),
        }
    }

    fn lower_stmt(&mut self, stmt: &Statement<'_>) -> Vec<ykpack::Statement> {
        let unimpl_stmt = |stmt| {
            vec![ykpack::Statement::Unimplemented(format!("{:?}", stmt))]
        };

        match stmt.kind {
            StatementKind::Assign(ref place, ref rval) => {
                match self.lower_assign_stmt(place, rval) {
                    Ok(t_st) => vec![t_st],
                    _ => unimpl_stmt(stmt),
                }
            },
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => Vec::new(),
            _ => unimpl_stmt(stmt),
        }
    }

    fn lower_assign_stmt(&mut self, place: &Place<'_>, rval: &Rvalue<'_>)
        -> Result<ykpack::Statement, ()>
    {
        Ok(ykpack::Statement::Assign(self.lower_place(place)?, self.lower_rval(rval)?))
    }

    // FIXME No possibility of error once everything is implemented.
    fn lower_place(&mut self, place: &Place<'_>) -> Result<ykpack::Local, ()> {
        match place {
            Place::Base(PlaceBase::Local(l)) => Ok(self.lower_local(*l)),
            _  => Err(()),
        }
    }

    // FIXME No possibility of error once everything is implemented.
    fn lower_rval(&mut self, rval: &Rvalue<'_>) -> Result<ykpack::Rvalue, ()> {
        match rval {
            Rvalue::Use(ref oper) => {
                match self.lower_operand(oper) {
                    Ok(ykpack::Operand::Local(l)) => Ok(ykpack::Rvalue::Local(l)),
                    Ok(ykpack::Operand::Constant(c)) => Ok(ykpack::Rvalue::Constant(c)),
                    _ => Err(()),
                }
            },
            Rvalue::BinaryOp(bin_op, o1, o2) =>
                Ok(ykpack::Rvalue::BinaryOp(self.lower_binary_op(*bin_op), self.lower_operand(o1)?,
                    self.lower_operand(o2)?)),
            _ => Err(()),
        }
    }

    fn lower_binary_op(&mut self, oper: BinOp) -> ykpack::BinOp {
        match oper {
            BinOp::Add => ykpack::BinOp::Add,
            BinOp::Sub => ykpack::BinOp::Sub,
            BinOp::Mul => ykpack::BinOp::Mul,
            BinOp::Div => ykpack::BinOp::Div,
            BinOp::Rem => ykpack::BinOp::Rem,
            BinOp::BitXor => ykpack::BinOp::BitXor,
            BinOp::BitAnd => ykpack::BinOp::BitAnd,
            BinOp::BitOr => ykpack::BinOp::BitOr,
            BinOp::Shl => ykpack::BinOp::Shl,
            BinOp::Shr => ykpack::BinOp::Shr,
            BinOp::Eq => ykpack::BinOp::Eq,
            BinOp::Lt => ykpack::BinOp::Lt,
            BinOp::Le => ykpack::BinOp::Le,
            BinOp::Ne => ykpack::BinOp::Ne,
            BinOp::Ge => ykpack::BinOp::Ge,
            BinOp::Gt => ykpack::BinOp::Gt,
            BinOp::Offset => ykpack::BinOp::Offset,
        }
    }

    fn lower_operand(&mut self, oper: &Operand<'_>) -> Result<ykpack::Operand, ()> {
        match oper {
            Operand::Copy(ref place) | Operand::Move(ref place) =>
                Ok(ykpack::Operand::Local(self.lower_place(place)?)),
            Operand::Constant(ref cnst) =>
                Ok(ykpack::Operand::Constant(self.lower_constant(cnst)?)),
        }
    }

    fn lower_constant(&mut self, cnst: &Constant<'_>) -> Result<ykpack::Constant, ()> {
        self.lower_const(cnst.literal)
    }

    fn lower_const(&mut self, cnst: &Const<'_>) -> Result<ykpack::Constant, ()> {
        match cnst.val {
            ConstValue::Scalar(ref s) => Ok(self.lower_scalar(cnst.ty, s)?),
            _ => Err(()),
        }
    }

    fn lower_scalar(&mut self, ty: Ty<'_>, sclr: &Scalar) -> Result<ykpack::Constant, ()> {
        match ty.sty {
            TyKind::Uint(t) => Ok(ykpack::Constant::Int(self.lower_uint(t, sclr))),
            TyKind::Int(t) => Ok(ykpack::Constant::Int(self.lower_int(t, sclr))),
            _ => Err(()), // FIXME Not implemented.
        }
    }

    fn lower_uint(&mut self, typ: UintTy, sclr: &Scalar) -> ykpack::ConstantInt {
        match sclr {
            Scalar::Raw{data, ..} => {
                // Here `size` is a u8, so upcasting is always OK.
                match typ {
                    UintTy::Usize => ykpack::ConstantInt::usize_from_bits(*data),
                    UintTy::U8 => ykpack::ConstantInt::u8_from_bits(*data),
                    UintTy::U16 => ykpack::ConstantInt::u16_from_bits(*data),
                    UintTy::U32 => ykpack::ConstantInt::u32_from_bits(*data),
                    UintTy::U64 => ykpack::ConstantInt::u64_from_bits(*data),
                    UintTy::U128 => ykpack::ConstantInt::u128_from_bits(*data),
                }
            },
            _ => panic!("non-raw scalar encountered in lowering unsigned int"),
        }
    }

    fn lower_int(&mut self, typ: IntTy, sclr: &Scalar) -> ykpack::ConstantInt {
        match sclr {
            Scalar::Raw{data, ..} => {
                // Here `size` is a u8, so upcasting is always OK.
                match typ {
                    IntTy::Isize => ykpack::ConstantInt::isize_from_bits(*data),
                    IntTy::I8 => ykpack::ConstantInt::i8_from_bits(*data),
                    IntTy::I16 => ykpack::ConstantInt::i16_from_bits(*data),
                    IntTy::I32 => ykpack::ConstantInt::i32_from_bits(*data),
                    IntTy::I64 => ykpack::ConstantInt::i64_from_bits(*data),
                    IntTy::I128 => ykpack::ConstantInt::i128_from_bits(*data),
                }
            },
            _ => panic!("non-raw scalar encountered in lowering signed int"),
        }
    }

    fn lower_local(&mut self, local: Local) -> ykpack::Local {
        self.sir_var(local)
    }
}

/// Writes SIR to file for the specified DefIds, possibly returning a linkable ELF object.
pub fn generate_sir<'a, 'tcx, 'gcx>(
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_ids: &DefIdSet, mode: SirMode)
    -> Result<Option<YkExtraLinkObject>, Box<dyn Error>>
{
    let sir_path = do_generate_sir(tcx, def_ids, &mode)?;
    match mode {
        SirMode::Default(_) => {
            // In this case the file at `sir_path` is a raw binary file which we use to make an
            // object file for linkage.
            let obj = YkExtraLinkObject::new(&sir_path, SECTION_NAME);
            // Now we have our object, we can remove the temp file. It's not the end of the world
            // if we can't remove it, so we allow this to fail.
            fs::remove_file(sir_path).ok();
            Ok(Some(obj))
        },
        SirMode::TextDump(_) => {
            // In this case we have no object to link, and we keep the file at `sir_path` around,
            // as this is the text dump the user asked for.
            Ok(None)
        }
    }
}

fn do_generate_sir<'a, 'tcx, 'gcx>(
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_ids: &DefIdSet, mode: &SirMode)
    -> Result<PathBuf, Box<dyn Error>>
{
    let (sir_path, mut default_file, textdump_file) = match mode {
        SirMode::Default(exe_path) => {
            // The default mode of operation dumps SIR in binary format to a temporary file, which
            // is later converted into an ELF object. Note that the temporary file name must be the
            // same between builds for the reproducible build tests to pass.
            let mut sir_path = exe_path.clone();
            sir_path.set_extension(TMP_EXT);
            let file = File::create(&sir_path)?;
            (sir_path, Some(file), None)
        },
        SirMode::TextDump(dump_path) => {
            // In text dump mode we just write lines to a file and we don't need an encoder.
            let file = File::create(&dump_path)?;
            (dump_path.clone(), None, Some(file))
        },
    };

    let mut enc = match default_file {
        Some(ref mut f) => Some(ykpack::Encoder::from(f)),
        _ => None,
    };

    // To satisfy the reproducible build tests, the CFG must be written out in a deterministic
    // order, thus we sort the `DefId`s first.
    let mut sorted_def_ids: Vec<&DefId> = def_ids.iter().collect();
    sorted_def_ids.sort();

    for def_id in sorted_def_ids {
        if tcx.is_mir_available(*def_id) {
            let mir = tcx.optimized_mir(*def_id);
            let mut ccx = ConvCx::new(tcx, *def_id, mir);
            let pack = ccx.lower();

            if let Some(ref mut e) = enc {
                e.serialise(ykpack::Pack::Body(pack))?;
            } else {
                write!(textdump_file.as_ref().unwrap(), "{}", pack)?;
            }
        }

        if let Some(ref mut e) = enc {
            e.serialise(ykpack::Pack::Debug(ykpack::SirDebug::new(
                lower_def_id(tcx, def_id), tcx.def_path_str(*def_id))))?;
        }
    }

    if let Some(e) = enc {
        // Now finalise the encoder and convert the resulting blob file into an object file for
        // linkage into the main binary. Once we've converted, we no longer need the original file.
        e.done()?;
    }

    Ok(sir_path)
}

fn lower_def_id(tcx: &TyCtxt<'_, '_, '_>, &def_id: &DefId) -> ykpack::DefId {
    ykpack::DefId {
        crate_hash: tcx.crate_hash(def_id.krate).as_u64(),
        def_idx: def_id.index.as_u32(),
    }
}
