// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module converts MIR into Yorick TIR (Tracing IR). TIR is more suitable for the run-time
//! tracer: TIR (unlike MIR) is in SSA form (but it does preserve MIR's block structure).
//!
//! The conversion happens in stages:
//!
//! 1) The MIR is lowered into an initial TIR.
//! 2) PHI nodes are inserted.
//! 3) Variables are renamed and we arrive at SSA TIR.
//! 4) The finalised SSA TIR is serialised using ykpack.

use rustc::ty::TyCtxt;

use rustc::hir::def_id::DefId;
use rustc::mir::{
    Mir, TerminatorKind, Operand, Constant, StatementKind, BasicBlock, BasicBlockData, Terminator,
    Place, Rvalue, Statement, Local, PlaceBase
};
use rustc::ty::{TyS, TyKind, Const, LazyConst};
use rustc::util::nodemap::DefIdSet;
use std::path::PathBuf;
use std::fs::File;
use rustc_yk_link::YkExtraLinkObject;
use std::fs;
use std::io::Write;
use std::error::Error;
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::convert::TryFrom;
use rustc_data_structures::bit_set::BitSet;
use rustc_data_structures::indexed_vec::{IndexVec, Idx};
use rustc_data_structures::graph::dominators::{Dominators, DominatorFrontiers};
use rustc_data_structures::graph::WithSuccessors;
use ykpack;
use ykpack::LocalIndex as TirLocal;
use ykpack::BasicBlockIndex as TirBasicBlockIndex;
use rustc_data_structures::fx::FxHashSet;

const SECTION_NAME: &'static str = ".yk_tir";
const TMP_EXT: &'static str = ".yk_tir.tmp";

/// The pre-SSA return value variable. In MIR, a return terminator implicitly returns variable
/// zero. We can't do this in TIR because TIR is in SSA form and the variable we return depends
/// upon which SSA variable reaches the terminator. So, during initial TIR lowering, we convert the
/// implicit MIR terminator to an explicit TIR terminator returning variable index zero. During SSA
/// conversion this is then re-written to the SSA variable that reaches the terminator.
static PRE_SSA_RET_VAR: TirLocal = 0;

/// Describes how to output MIR.
pub enum TirMode {
    /// Write MIR into an object file for linkage. The inner path should be the path to the main
    /// executable (from this we generate a filename for the resulting object).
    Default(PathBuf),
    /// Write MIR in textual form the specified path.
    TextDump(PathBuf),
}

/// A conversion context holds the state needed to perform the conversion to the intial TIR.
struct ConvCx<'a, 'tcx, 'gcx> {
    /// The compiler's god struct. Needed for queries etc.
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>,
    /// Maps TIR variables to their definition sites.
    def_sites: RefCell<Vec<BitSet<BasicBlock>>>,
    /// Maps each block to the variable it defines. This is what Appel calls `A_{orig}`.
    block_defines: RefCell<IndexVec<BasicBlock, FxHashSet<TirLocal>>>,
    /// Monotonically increasing number used to give TIR variables a unique ID.
    /// Note that 0 is reserved for `PRE_SSA_RET_VAR`.
    next_tir_var: Cell<TirLocal>,
    /// A mapping from MIR variables to TIR variables.
    var_map: RefCell<IndexVec<Local, Option<TirLocal>>>,
    /// The number of blocks in the MIR (and therefore in the TIR).
    num_blks: usize,
    /// The number of "predefined" variables at the entry point.
    num_predefs: u32,
    /// The MIR we are lowering.
    mir: &'a Mir<'tcx>,
}

impl<'a, 'tcx, 'gcx> ConvCx<'a, 'tcx, 'gcx> {
    fn new(tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, mir: &'a Mir<'tcx>) -> Self {
        let num_blks = mir.basic_blocks().len();

        Self {
            tcx,
            def_sites: RefCell::new(Vec::new()),
            block_defines: RefCell::new(IndexVec::from_elem_n(FxHashSet::default(), num_blks)),
            next_tir_var: Cell::new(0),
            var_map: RefCell::new(IndexVec::new()),
            num_blks: num_blks,
            num_predefs: 0,
            mir,
        }
    }

    /// Make a definition of all variables (before renaming) at the entry point. Call this
    /// immediately after constructing a `RenameCx`.
    ///
    /// From the Appel book:
    ///     "We consider the start node to contain an implicit definition of every variable,
    ///     either because the variable may be a formal parameter or to represent the notion of
    ///     `a ← uninitialized` without special cases"
    ///
    /// See also the insertion of SsaEntryDefs instructions elsewhere.
    fn predefine_variables(&mut self) {
        let mut return_var = vec![Local::new(0)];
        // It's important that the implicit MIR return variable is processed first. See the comment
        // above about PRE_SSA_RET_VAR.
        for v in return_var.drain(..)
            .chain(self.mir.args_iter())
            .chain(self.mir.vars_iter())
            .chain(self.mir.temps_iter())
        {
            self.push_def_site(BasicBlock::new(0), self.tir_var(v));
            self.num_predefs += 1;
        }
    }

    /// Returns a guaranteed unique TIR variable index.
    fn new_tir_var(&self) -> TirLocal {
        let var_idx = self.next_tir_var.get();
        self.next_tir_var.set(var_idx + 1);
        var_idx
    }

    /// Get the TIR variable for the specified MIR variable, creating a fresh variable if needed.
    fn tir_var(&self, local: Local) -> TirLocal {
        let local_u32 = local.as_u32();
        let mut var_map = self.var_map.borrow_mut();

        // Resize the backing Vec if necessary.
        // Vector indices are `usize`, but variable indices are `u32`, so converting from a
        // variable index to a vector index is always safe if a `usize` can express all `u32`s.
        assert!(size_of::<usize>() >= size_of::<u32>());
        if var_map.len() <= local_u32 as usize {
            var_map.resize(local_u32.checked_add(1).unwrap() as usize, None);
        }

        var_map[local].unwrap_or_else(|| {
            let var_idx = self.new_tir_var();
            var_map[local] = Some(var_idx);
            var_idx
        })
    }

    /// Finalise the conversion context, returning a tuple of:
    ///  - The definition sites.
    ///  - The block defines mapping.
    ///  - The next available TIR variable index.
    fn done(self) -> (Vec<BitSet<BasicBlock>>, IndexVec<BasicBlock, FxHashSet<TirLocal>>, u32) {
        (self.def_sites.into_inner(), self.block_defines.into_inner(),
            self.next_tir_var.into_inner())
    }

    /// Add `bb` as a definition site of the TIR variable `var`.
    fn push_def_site(&self, bb: BasicBlock, var: TirLocal) {
        let mut sites = self.def_sites.borrow_mut();
        // This conversion is safe because `var` was generated by `tir_var()` which guarantees that
        // a `u32` can fit in a `usize`.
        let var_usize = var as usize;
        if sites.len() <= var_usize {
            // By performing the checked addition on the original `u32` we ensure the indices in
            // `self.def_sites` are never outside of what a `u32` can express.
            sites.resize(var.checked_add(1).unwrap() as usize,
                BitSet::new_empty(self.num_blks));
        }
        sites[var_usize].insert(bb);

        // Also push into the inverse mapping (blocks to defined vars).
        self.block_defines.borrow_mut()[bb].insert(var);
    }
}

/// Writes TIR to file for the specified DefIds, possibly returning a linkable ELF object.
pub fn generate_tir<'a, 'tcx, 'gcx>(
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_ids: &DefIdSet, mode: TirMode)
    -> Result<Option<YkExtraLinkObject>, Box<dyn Error>>
{
    let tir_path = do_generate_tir(tcx, def_ids, &mode)?;
    match mode {
        TirMode::Default(_) => {
            // In this case the file at `tir_path` is a raw binary file which we use to make an
            // object file for linkage.
            let obj = YkExtraLinkObject::new(&tir_path, SECTION_NAME);
            // Now we have our object, we can remove the temp file. It's not the end of the world
            // if we can't remove it, so we allow this to fail.
            fs::remove_file(tir_path).ok();
            Ok(Some(obj))
        },
        TirMode::TextDump(_) => {
            // In this case we have no object to link, and we keep the file at `tir_path` around,
            // as this is the text dump the user asked for.
            Ok(None)
        }
    }
}

fn do_generate_tir<'a, 'tcx, 'gcx>(
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_ids: &DefIdSet, mode: &TirMode)
    -> Result<PathBuf, Box<dyn Error>>
{
    let (tir_path, mut default_file, textdump_file) = match mode {
        TirMode::Default(exe_path) => {
            // The default mode of operation dumps TIR in binary format to a temporary file, which
            // is later converted into an ELF object. Note that the temporary file name must be the
            // same between builds for the reproducible build tests to pass.
            let mut tir_path = exe_path.clone();
            tir_path.set_extension(TMP_EXT);
            let mut file = File::create(&tir_path)?;
            (tir_path, Some(file), None)
        },
        TirMode::TextDump(dump_path) => {
            // In text dump mode we just write lines to a file and we don't need an encoder.
            let mut file = File::create(&dump_path)?;
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
            let doms = mir.dominators();
            let mut ccx = ConvCx::new(tcx, mir);
            ccx.predefine_variables();

            let mut pack = (&ccx, def_id, tcx.optimized_mir(*def_id)).to_pack();
            {
                let ykpack::Pack::Mir(ykpack::Mir{ref mut blocks, ..}) = pack;
                let (def_sites, block_defines, next_tir_var) = ccx.done();
                insert_phis(blocks, &doms, mir, def_sites, block_defines);
                RenameCx::new(next_tir_var).rename_all(&doms, &mir, blocks);
            }

            if let Some(ref mut e) = enc {
                e.serialise(pack)?;
            } else {
                write!(textdump_file.as_ref().unwrap(), "{}", pack)?;
            }
        }
    }

    if let Some(e) = enc {
        // Now finalise the encoder and convert the resulting blob file into an object file for
        // linkage into the main binary. Once we've converted, we no longer need the original file.
        e.done()?;
    }

    Ok(tir_path)
}

/// Insert PHI nodes into the initial pre-SSA TIR pack.
///
/// Algorithm reference:
/// Bottom of p406 of 'Modern Compiler Implementation in Java (2nd ed.)' by Andrew Appel.
fn insert_phis(blocks: &mut Vec<ykpack::BasicBlock>, doms: &Dominators<BasicBlock>,
               mir: &Mir, mut def_sites: Vec<BitSet<BasicBlock>>,
               a_orig: IndexVec<BasicBlock, FxHashSet<TirLocal>>) {
    let df = DominatorFrontiers::new(mir, &doms);
    let num_tir_vars = def_sites.len();
    let num_tir_blks = a_orig.len();

    let mut a_phi: Vec<BitSet<TirLocal>> = Vec::with_capacity(num_tir_blks);
    a_phi.resize(num_tir_blks, BitSet::new_empty(num_tir_vars));

    // We don't need the elements of `def_sites` again past this point, so we can take them out
    // of `def_sites` with a draining iterator and mutate in-place.
    for (a, mut w) in def_sites.drain(..).enumerate() {
        while !w.is_empty() {
            let n = bitset_pop(&mut w);
            for y in df.frontier(n).iter() {
                let y_usize = y.index();
                // `def_sites` is guaranteed to only contain indices expressible by `u32`.
                let a_u32 = a as u32;
                if !a_phi[y_usize].contains(a_u32) {
                    a_phi[y_usize].insert(a_u32);
                    if !a_orig[y].contains(&a_u32) {
                        // The assertion in `tir_var()` has already checked the cast is safe.
                        insert_phi(&mut blocks[y_usize], a as u32, mir.predecessors_for(y).len());
                        w.insert(y);
                    }
                }
            }
        }
    }
}

fn insert_phi(block: &mut ykpack::BasicBlock, var: TirLocal, arity: usize) {
    let lhs = ykpack::Place::Local(var);
    let rhs_vars = (0..arity).map(|_| lhs.clone()).collect();
    let rhs = ykpack::Rvalue::Phi(rhs_vars);
    block.stmts.insert(0, ykpack::Statement::Assign(lhs, rhs));
}

/// A statement location.
#[derive(Clone, Debug)]
struct StmtLoc {
    /// The block containing the statement.
    bb: TirBasicBlockIndex,
    /// The statement index.
    si: usize,
}

/// SSA variable renaming. Algorithm reference:
/// Bottom of p408 of 'Modern Compiler Implementation in Java (2nd ed.)' by Andrew Appel.
struct RenameCx {
    /// A counter used to give new TIR variables a unique identifier.
    /// The original algorithm used one counter per original variable. This would mean storing each
    /// SSA variable as a (name, version) pair. For added efficiency, we use a single counter and
    /// represent our variables as plain old integers.
    count: TirLocal,
    /// Each variable has a stack of definitions.
    stack: Vec<Vec<TirLocal>>,
}

impl RenameCx {
    /// Make a new renaming context. To prevent variable naming clashes, the `next_fresh_var`
    /// argument should be one more than the last variable the previous step of the conversion
    /// created.
    fn new(next_fresh_var: u32) -> Self {
        // We start with space for the variables we know about so far. The vectors will grow as new
        // SSA variables are instantiated.
        let next_fresh_var_usize = usize::try_from(next_fresh_var).unwrap();
        let mut stack = Vec::with_capacity(next_fresh_var_usize);
        stack.resize(next_fresh_var_usize, Vec::new());
        Self {
            count: next_fresh_var,
            stack,
        }
    }

    /// Create a new SSA variable.
    fn fresh_var(&mut self) -> TirLocal {
        let ret = self.count;
        self.count = self.count.checked_add(1).unwrap();
        ret
    }

    /// Entry point for variable renaming.
    fn rename_all(mut self, doms: &Dominators<BasicBlock>, mir: &Mir,
        blks: &mut Vec<ykpack::BasicBlock>)
    {
        // We start renaming in the entry block and it ripples down the dominator tree.
        self.rename(doms, mir, blks, 0);
    }

    // FIXME rename variables in terminators.
    fn rename(&mut self, doms: &Dominators<BasicBlock>, mir: &Mir,
        blks: &mut Vec<ykpack::BasicBlock>, n: TirBasicBlockIndex)
    {
        let n_usize = n as usize;
        // We have to remember the variables whose stacks we must pop from when we come back from
        // recursion. These must be the variables *before* they were renamed.
        let mut pop_later = Vec::new();
        {
            let n_blk = &mut blks[n_usize];
            for st in n_blk.stmts.iter_mut() {
                if !st.is_phi() {
                    for x in st.uses_vars_mut().iter_mut() {
                        let i = self.stack[**x as usize].last().cloned().unwrap();
                        **x = i;
                    }
                }
                for a in st.defs_vars_mut().iter_mut() {
                    let i = self.fresh_var();
                    self.stack[**a as usize].push(i);
                    pop_later.push(**a);
                    **a = i;
                }
            }
        }

        let n_idx = BasicBlock::new(n_usize);
        for y in mir.successors(n_idx) {
            // "Suppose n is the jth predecessor of y".
            let j = mir.predecessors_for(y).iter().position(|b| b == &n_idx).unwrap();
            // "For each Phi function in y"
            for st in &mut blks[y.as_usize()].stmts {
                if let Some(ref mut a) = st.phi_arg_mut(j) {
                    // We only get here if `st` was a Phi.
                    let i = self.stack[**a as usize].last().cloned().unwrap();
                    **a = i;
                }
            }
        }

        for x in doms.immediately_dominates(n_idx) {
            self.rename(doms, mir, blks, x.as_u32());
        }

        for a in pop_later {
            self.stack[usize::try_from(a).unwrap()].pop();
         }
    }
}

/// The trait for converting MIR data structures into a bytecode packs.
trait ToPack<T> {
    fn to_pack(&mut self) -> T;
}

/// Mir -> Pack
impl<'tcx> ToPack<ykpack::Pack> for (&ConvCx<'_, 'tcx, '_>, &DefId, &Mir<'tcx>) {
    fn to_pack(&mut self) -> ykpack::Pack {
        let (ccx, def_id, mir) = self;

        let mut ser_blks = Vec::new();
        for (bb, bb_data) in mir.basic_blocks().iter_enumerated() {
            ser_blks.push((*ccx, bb, bb_data).to_pack());
        }

        let ser_def_id = ykpack::DefId::new(
            ccx.tcx.crate_hash(def_id.krate).as_u64(), def_id.index.as_raw_u32());

        ykpack::Pack::Mir(ykpack::Mir::new(ser_def_id, ccx.tcx.item_path_str(**def_id), ser_blks))
    }
}

/// DefId -> Pack
impl ToPack<ykpack::DefId> for (&ConvCx<'_, '_, '_>, &DefId) {
    fn to_pack(&mut self) -> ykpack::DefId {
        let (ccx, def_id) = self;
        ykpack::DefId {
            crate_hash: ccx.tcx.crate_hash(def_id.krate).as_u64(),
            def_idx: def_id.index.as_raw_u32(),
        }
    }
}

/// Terminator -> Pack
impl<'tcx> ToPack<ykpack::Terminator> for (&ConvCx<'_, 'tcx, '_>, &Terminator<'tcx>) {
    fn to_pack(&mut self) -> ykpack::Terminator {
        let (ccx, term) = self;

        match term.kind {
            TerminatorKind::Goto{target: target_bb}
            | TerminatorKind::FalseEdges{real_target: target_bb, ..}
            | TerminatorKind::FalseUnwind{real_target: target_bb, ..} =>
                ykpack::Terminator::Goto{target_bb: u32::from(target_bb)},
            TerminatorKind::SwitchInt{targets: ref target_bbs, ..} => {
                let target_bbs = target_bbs.iter().map(|bb| u32::from(*bb)).collect();
                ykpack::Terminator::SwitchInt{target_bbs}
            },
            TerminatorKind::Resume => ykpack::Terminator::Resume,
            TerminatorKind::Abort => ykpack::Terminator::Abort,
            TerminatorKind::Return => ykpack::Terminator::Return(PRE_SSA_RET_VAR),
            TerminatorKind::Unreachable => ykpack::Terminator::Unreachable,
            TerminatorKind::Drop{target: target_bb, unwind: unwind_bb, ..} =>
                ykpack::Terminator::Drop{
                    target_bb: u32::from(target_bb),
                    unwind_bb: unwind_bb.map(|bb| u32::from(bb)),
                },
            TerminatorKind::DropAndReplace{target: target_bb, unwind: unwind_bb, ..} =>
                ykpack::Terminator::DropAndReplace{
                    target_bb: u32::from(target_bb),
                    unwind_bb: unwind_bb.map(|bb| u32::from(bb)),
                },
            TerminatorKind::Call{ref func, cleanup: cleanup_bb, ref destination, .. } => {
                let ser_oper = if let Operand::Constant(box Constant {
                    literal: LazyConst::Evaluated(Const {
                        ty: &TyS {
                            sty: TyKind::FnDef(target_def_id, _substs), ..
                        }, ..
                    }), ..
                }, ..) = func {
                    // A statically known call target.
                    ykpack::CallOperand::Fn((*ccx, &target_def_id).to_pack())
                } else {
                    // FIXME -- implement other callables.
                    ykpack::CallOperand::Unknown
                };

                let ret_bb = destination.as_ref().map(|(_, bb)| u32::from(*bb));
                ykpack::Terminator::Call{
                    operand: ser_oper,
                    cleanup_bb: cleanup_bb.map(|bb| u32::from(bb)),
                    ret_bb: ret_bb,
                }
            },
            TerminatorKind::Assert{target: target_bb, cleanup: cleanup_bb, ..} =>
                ykpack::Terminator::Assert{
                    target_bb: u32::from(target_bb),
                    cleanup_bb: cleanup_bb.map(|bb| u32::from(bb)),
                },
            TerminatorKind::Yield{resume: resume_bb, drop: drop_bb, ..} =>
                ykpack::Terminator::Yield{
                    resume_bb: u32::from(resume_bb),
                    drop_bb: drop_bb.map(|bb| u32::from(bb)),
                },
            TerminatorKind::GeneratorDrop => ykpack::Terminator::GeneratorDrop,
        }
    }
}

/// BasicBlockData -> Pack
impl<'tcx> ToPack<ykpack::BasicBlock> for
    (&ConvCx<'_, 'tcx, '_>, BasicBlock, &BasicBlockData<'tcx>)
{
    fn to_pack(&mut self) -> ykpack::BasicBlock {
        let (ccx, bb, bb_data) = self;
        let mut ser_stmts = Vec::new();

        // If we are lowering the first block, we insert a special `SsaEntryDefs` instruction, which
        // tells the SSA algorithms to make an initial definition of every TIR variable.
        if bb.as_u32() == 0 {
            ser_stmts.push(ykpack::Statement::SsaEntryDefs((0..ccx.num_predefs).collect()));
        }

        ser_stmts.extend(bb_data.statements.iter().map(|stmt| (*ccx, *bb, stmt).to_pack()));
        ykpack::BasicBlock::new(ser_stmts, (*ccx, bb_data.terminator.as_ref().unwrap()).to_pack())
    }
}

/// Statement -> Pack
impl<'tcx> ToPack<ykpack::Statement> for (&ConvCx<'_, 'tcx, '_>, BasicBlock, &Statement<'tcx>) {
    fn to_pack(&mut self) -> ykpack::Statement {
        let (ccx, bb, ref stmt) = self;

        match stmt.kind {
            StatementKind::Assign(ref place, ref rval) => {
                let lhs = (*ccx, place).to_pack();
                let rhs = (*ccx, &**rval).to_pack();
                if let ykpack::Place::Local(tvar) = lhs {
                    ccx.push_def_site(*bb, tvar);
                }
                ykpack::Statement::Assign(lhs, rhs)
            },
            _ => ykpack::Statement::Unimplemented,
        }
    }
}

/// Place -> Pack
impl<'tcx> ToPack<ykpack::Place> for (&ConvCx<'_, 'tcx, '_>, &Place<'tcx>) {
    fn to_pack(&mut self) -> ykpack::Place {
        let (ccx, place) = self;

        match place {
            Place::Base(PlaceBase::Local(local)) => ykpack::Place::Local(ccx.tir_var(*local)),
            _ => ykpack::Place::Unimplemented, // FIXME
        }
    }
}

/// Rvalue -> Pack
impl<'tcx> ToPack<ykpack::Rvalue> for (&ConvCx<'_, 'tcx, '_>, &Rvalue<'tcx>) {
    fn to_pack(&mut self) -> ykpack::Rvalue {
        let (ccx, rval) = self;

        match *rval {
            Rvalue::Use(Operand::Move(place)) => ykpack::Rvalue::Place((*ccx, place).to_pack()),
            _ => ykpack::Rvalue::Unimplemented, // FIXME
        }
    }
}

/// At the time of writing, you can't pop from a `BitSet`.
fn bitset_pop<T>(s: &mut BitSet<T>) -> T where T: Eq + Idx + Clone {
    let e = s.iter().next().unwrap().clone();
    let removed = s.remove(e);
    debug_assert!(removed);
    e
}
