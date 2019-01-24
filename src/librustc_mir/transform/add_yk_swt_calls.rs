// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use rustc::ty::{self, TyCtxt, List};
use rustc::mir::{Operand, LocalDecl, Place, SourceInfo, BasicBlock, Local, BasicBlockData,
    TerminatorKind, Terminator, OUTERMOST_SOURCE_SCOPE, Constant, Mir};
use rustc_data_structures::indexed_vec::Idx;
use syntax_pos::DUMMY_SP;
use syntax::attr;
use transform::{MirPass, MirSource};
use rustc::hir;
use rustc::hir::def_id::{DefIndex, LOCAL_CRATE};
use rustc::hir::map::blocks::FnLikeNode;

/// A MIR transformation that, for each basic block, inserts a call to the software trace recorder.
/// The arguments to the calls (crate hash, DefId and block index) identify the position to be
/// inserted into a trace.
///
/// The transformation works by copying each original "user block" and replacing it with a new
/// block -- its "shadow" -- which calls the trace recorder function, before returning to the copy.
///
/// For example:
///
/// +----+   +----+           +-----+   +-----+   +----+   +-----+   +-----+   +----+
/// | B0 |-->| B1 |  Becomes: | B0' |-->| Rec |-->| B2 |-->| B1' |-->| Rec |-->| B3 |
/// +----+   +----+           +-----+   +-----+   +----+   +-----+   +-----+   +----+
///
/// Where:
///  * B0 and B1 are "user blocks" in the MIR before the transformation.
///  * B0' and B1' are "shadow blocks" of B0 and B1 respectively.
///  * B2 and B3 are copies of B0 and B1 respectively.
///  * 'Rec' is the trace recorder function.
///  * The block indices match the indices in the backing vector in the MIR.
///
/// The extra calls we insert mean that we have to allocate new local decls for the (unit) return
/// values: one new decl for each call.
pub struct AddYkSWTCalls(pub DefIndex);

impl MirPass for AddYkSWTCalls {
    fn run_pass<'a, 'tcx>(&self,
                          tcx: TyCtxt<'a, 'tcx, 'tcx>,
                          src: MirSource,
                          mir: &mut Mir<'tcx>) {
        if is_untraceable(tcx, src) {
            return;
        }

        let rec_fn_defid = tcx.get_lang_items(LOCAL_CRATE).yk_swt_rec_loc()
            .expect("couldn't find software trace recorder function");

        let unit_ty = tcx.mk_unit();
        let u32_ty = tcx.types.u32;
        let u64_ty = tcx.types.u64;

        let mut shadow_blks = Vec::new();
        let mut user_blks = Vec::new(); // Copies of the blocks we started with.
        let mut new_local_decls = Vec::new();

        let num_orig_local_decls = mir.local_decls.len();
        let local_crate_hash = tcx.crate_hash(LOCAL_CRATE).as_u64();

        for (bb, bb_data) in mir.basic_blocks().iter_enumerated() {
            // Copy the block.
            let new_blk = bb_data.clone();
            let new_blk_idx = BasicBlock::new(mir.basic_blocks().len() + user_blks.len());
            user_blks.push(new_blk);

            // Prepare to call the recorder function.
            let ret_val = LocalDecl::new_temp(unit_ty, DUMMY_SP);
            let ret_place = Place::Local(Local::new(num_orig_local_decls + new_local_decls.len()));
            new_local_decls.push(ret_val);

            let crate_hash_oper = Operand::Constant(box Constant {
                span: DUMMY_SP,
                ty: u64_ty,
                user_ty: None,
                literal: ty::Const::from_u64(tcx, local_crate_hash),
            });

            let def_idx_oper = Operand::Constant(box Constant {
                span: DUMMY_SP,
                ty: u32_ty,
                user_ty: None,
                literal: ty::Const::from_u32(tcx, self.0.as_raw_u32()),
            });

            let bb_oper = Operand::Constant(box Constant {
                span: DUMMY_SP,
                ty: u32_ty,
                user_ty: None,
                literal: ty::Const::from_u32(tcx, bb.index() as u32),
            });

            let rec_fn_oper = Operand::function_handle(tcx, rec_fn_defid,
                List::empty(), DUMMY_SP);

            let term_kind = TerminatorKind::Call {
                func: rec_fn_oper,
                args: vec![crate_hash_oper, def_idx_oper, bb_oper],
                destination: Some((ret_place, new_blk_idx)), // Return to the copied block.
                cleanup: None,
                from_hir_call: false,
            };

            // Build the replacement block with the new call terminator.
            let source_info = bb_data.terminator.clone().map(|t| t.source_info)
                .or(Some(SourceInfo { span: DUMMY_SP, scope: OUTERMOST_SOURCE_SCOPE })).unwrap();
            let replace_block = BasicBlockData {
                statements: vec![],
                terminator: Some(Terminator {
                    source_info,
                    kind: term_kind
                }),
                is_cleanup: false
            };
            shadow_blks.push(replace_block);
        }

        // Finally, commit our transformations.
        mir.basic_blocks_mut().extend(user_blks);
        mir.local_decls.extend(new_local_decls);
        for (bb, bb_data) in shadow_blks.drain(..).enumerate() {
            mir.basic_blocks_mut()[BasicBlock::new(bb)] = bb_data;
        }
    }
}

/// Given a `MirSource`, decides if it is possible for us to trace (and thus whether we should
/// transform) the MIR. Returns `true` if we cannot trace, otherwise `false`.
fn is_untraceable(tcx: TyCtxt<'a, 'tcx, 'tcx>, src: MirSource) -> bool {
    // Never annotate anything annotated with the `#[no_trace]` attribute. This is used on tests
    // where our pass would interfere and on the trace recorder to prevent infinite
    // recursion.
    //
    // "naked functions" can't be traced because their implementations manually implement
    // binary-level function epilogues and prologues, often using in-line assembler. We can't
    // automatically insert our calls into such code without breaking stuff.
    for attr in tcx.get_attrs(src.def_id).iter() {
        if attr.check_name("no_trace") {
            return true;
        }
        if attr.check_name("naked") {
            return true;
       }
    }

    // Similar to `#[no_trace]`, don't transform anything inside a crate marked `#![no_trace]`.
    for attr in tcx.hir.krate_attrs() {
        if attr.check_name("no_trace") {
            return true;
        }
    }

    // We can't call the software tracing function if the crate doesn't depend upon libcore because
    // that's where the entry point to the trace recorder function lives.
    if attr::contains_name(tcx.hir.krate_attrs(), "no_core") {
        return true;
    }

    // Attempting to transform libcompiler_builtins leads to an undefined reference to the trace
    // recorder wrapper `core::yk_swt::yk_swt_rec_loc_wrap`. It's not worth investigating, as this
    // crate only contains wrapped C and ASM code that we can't transform anyway.
    if tcx.is_compiler_builtins(LOCAL_CRATE) {
        return true;
    }

    // We can't transform promoted items, because they are `const`, and our trace recorder isn't.
    if let Some(_) = src.promoted {
        return true;
    }

    // For the same reason as above, regular const functions can't be transformed.
    let node_id = tcx.hir.as_local_node_id(src.def_id)
        .expect("Failed to get node id");
    if let Some(fn_like) = FnLikeNode::from_node(tcx.hir.get(node_id)) {
        fn_like.constness() == hir::Constness::Const
    } else {
        true
    }
}
