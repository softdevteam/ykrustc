// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Custom CFG serialiser for Yorick.
/// At the time of writing no crate using `proc_macro` can be used in-compiler, otherwise we'd have
/// used Serde.

use rustc::ty::TyCtxt;

use rustc::hir::def_id::DefId;
use rustc::mir::{Mir, TerminatorKind, Operand, Constant, BasicBlock};
use rustc::ty::{TyS, TyKind, Const, LazyConst};
use rustc::util::nodemap::DefIdSet;
use std::path::PathBuf;
use std::fs::File;
use rustc_yk_link::YkExtraLinkObject;
use std::fs;
use byteorder::{NativeEndian, WriteBytesExt};

// Edge kinds.
const GOTO: u8 = 0;
const SWITCHINT: u8 = 1;
const RESUME: u8 = 2;
const ABORT: u8 = 3;
const RETURN: u8 = 4;
const UNREACHABLE: u8 = 5;
const DROP_NO_UNWIND: u8 = 6;
const DROP_WITH_UNWIND: u8 = 7;
const DROP_AND_REPLACE_NO_UNWIND: u8 = 8;
const DROP_AND_REPLACE_WITH_UNWIND: u8 = 9;
const CALL_NO_CLEANUP: u8 = 10;
const CALL_WITH_CLEANUP: u8 = 11;
const CALL_UNKNOWN_NO_CLEANUP: u8 = 12;
const CALL_UNKNOWN_WITH_CLEANUP: u8 = 13;
const ASSERT_NO_CLEANUP: u8 = 14;
const ASSERT_WITH_CLEANUP: u8 = 15;
const YIELD_NO_DROP: u8 = 16;
const YIELD_WITH_DROP: u8 = 17;
const GENERATOR_DROP: u8 = 18;
const FALSE_EDGES: u8 = 19;
const FALSE_UNWIND: u8 = 20;
const NO_MIR: u8 = 254;
const SENTINAL: u8 = 255;

const MIR_CFG_SECTION_NAME: &'static str = ".yk_mir_cfg";
const SECTION_VERSION: u16 = 0;

/// Serialises the control flow for the given `DefId`s into a ELF object file and returns a handle
/// for linking.
pub fn emit_mir_cfg_section<'a, 'tcx, 'gcx>(
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, def_ids: &DefIdSet, exe_filename: PathBuf)
    -> YkExtraLinkObject {

    // Serialise the MIR into a file whose name is derived from the output binary. The filename
    // must be the same between builds of the same binary for the reproducible build tests to pass.
    let mut mir_path: String = exe_filename.to_str().unwrap().to_owned();
    mir_path.push_str(".ykcfg");
    let mut fh = File::create(&mir_path).unwrap();

    // Write a version field for sanity checking when deserialising.
    fh.write_u16::<NativeEndian>(SECTION_VERSION).unwrap();

    // To satisfy the reproducible build tests, the CFG must be written out in a deterministic
    // order, thus we sort the `DefId`s first.
    let mut sorted_def_ids: Vec<&DefId> = def_ids.iter().collect();
    sorted_def_ids.sort();

    for def_id in sorted_def_ids {
        if tcx.is_mir_available(*def_id) {
            process_mir(&mut fh, tcx, def_id, tcx.optimized_mir(*def_id));
        } else {
            fh.write_u8(NO_MIR).unwrap();
            fh.write_u64::<NativeEndian>(tcx.crate_hash(def_id.krate).as_u64()).unwrap();
            fh.write_u32::<NativeEndian>(def_id.index.as_raw_u32()).unwrap();
        }
    }

    // Write end-of-section sentinal.
    fh.write_u8(SENTINAL).unwrap();

    // Now graft it into an object file.
    let path = PathBuf::from(mir_path);
    let ret = YkExtraLinkObject::new(&path, MIR_CFG_SECTION_NAME);
    fs::remove_file(path).unwrap();

    ret
}

/// For each block in the given MIR write out one CFG edge record.
fn process_mir(fh: &mut File, tcx: &TyCtxt, def_id: &DefId, mir: &Mir) {
    for (bb, maybe_bb_data) in mir.basic_blocks().iter_enumerated() {
        let bb_data = maybe_bb_data.terminator.as_ref().unwrap();
        match bb_data.kind {
            TerminatorKind::Goto{target: target_bb} => {
                write_rec_header(fh, tcx, GOTO, def_id, bb);
                fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
            },
            TerminatorKind::SwitchInt{ref targets, ..} => {
                write_rec_header(fh, tcx, SWITCHINT, def_id, bb);

                if cfg!(target_pointer_width = "64") {
                    fh.write_u64::<NativeEndian>(targets.len() as u64).unwrap();
                } else {
                    panic!("unknown pointer width");
                }

                for target_bb in targets {
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                }
            },
            TerminatorKind::Resume => write_rec_header(fh, tcx, RESUME, def_id, bb),
            TerminatorKind::Abort => write_rec_header(fh, tcx, ABORT, def_id, bb),
            TerminatorKind::Return => write_rec_header(fh, tcx, RETURN, def_id, bb),
            TerminatorKind::Unreachable => write_rec_header(fh, tcx, UNREACHABLE, def_id, bb),
            TerminatorKind::Drop{target: target_bb, unwind: opt_unwind_bb, ..} => {
                if let Some(unwind_bb) = opt_unwind_bb {
                    write_rec_header(fh, tcx, DROP_WITH_UNWIND, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                    fh.write_u32::<NativeEndian>(unwind_bb.index() as u32).unwrap();
                } else {
                    write_rec_header(fh, tcx, DROP_NO_UNWIND, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                }
            },
            TerminatorKind::DropAndReplace{target: target_bb, unwind: opt_unwind_bb, ..} => {
                if let Some(unwind_bb) = opt_unwind_bb {
                    write_rec_header(fh, tcx, DROP_AND_REPLACE_WITH_UNWIND, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                    fh.write_u32::<NativeEndian>(unwind_bb.index() as u32).unwrap();
                } else {
                    write_rec_header(fh, tcx, DROP_AND_REPLACE_NO_UNWIND, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                }
            },
            TerminatorKind::Call{ref func, cleanup: opt_cleanup_bb, ..} => {
                if let Operand::Constant(box Constant {
                    literal: LazyConst::Evaluated(Const {
                        ty: &TyS {
                            sty: TyKind::FnDef(target_def_id, _substs), ..
                        }, ..
                    }), ..
                }, ..) = func {
                    // A statically known call target.
                    if opt_cleanup_bb.is_some() {
                        write_rec_header(fh, tcx, CALL_WITH_CLEANUP, def_id, bb);
                    } else {
                        write_rec_header(fh, tcx, CALL_NO_CLEANUP, def_id, bb);
                    }

                    fh.write_u64::<NativeEndian>(tcx.crate_hash(
                        target_def_id.krate).as_u64()).unwrap();
                    fh.write_u32::<NativeEndian>(target_def_id.index.as_raw_u32()).unwrap();

                    if let Some(cleanup_bb) = opt_cleanup_bb {
                        fh.write_u32::<NativeEndian>(cleanup_bb.index() as u32).unwrap();
                    }
                } else {
                    // It's a kind of call that we can't statically know the target of.
                    if let Some(cleanup_bb) = opt_cleanup_bb {
                        write_rec_header(fh, tcx, CALL_UNKNOWN_WITH_CLEANUP, def_id, bb);
                        fh.write_u32::<NativeEndian>(cleanup_bb.index() as u32).unwrap();
                    } else {
                        write_rec_header(fh, tcx, CALL_UNKNOWN_NO_CLEANUP, def_id, bb);
                    }
                }
            },
            TerminatorKind::Assert{target: target_bb, cleanup: opt_cleanup_bb, ..} => {
                if let Some(cleanup_bb) = opt_cleanup_bb {
                    write_rec_header(fh, tcx, ASSERT_WITH_CLEANUP, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                    fh.write_u32::<NativeEndian>(cleanup_bb.index() as u32).unwrap();
                } else {
                    write_rec_header(fh, tcx, ASSERT_NO_CLEANUP, def_id, bb);
                    fh.write_u32::<NativeEndian>(target_bb.index() as u32).unwrap();
                }
            },
            TerminatorKind::Yield{resume: resume_bb, drop: opt_drop_bb, ..} => {
                if let Some(drop_bb) = opt_drop_bb {
                    write_rec_header(fh, tcx, YIELD_WITH_DROP, def_id, bb);
                    fh.write_u32::<NativeEndian>(resume_bb.index() as u32).unwrap();
                    fh.write_u32::<NativeEndian>(drop_bb.index() as u32).unwrap();
                } else {
                    write_rec_header(fh, tcx, YIELD_NO_DROP, def_id, bb);
                    fh.write_u32::<NativeEndian>(resume_bb.index() as u32).unwrap();
                }
            },
            TerminatorKind::GeneratorDrop => write_rec_header(fh, tcx, GENERATOR_DROP, def_id, bb),
            TerminatorKind::FalseEdges{real_target: real_target_bb, ..} => {
                // Fake edges not considered.
                write_rec_header(fh, tcx, FALSE_EDGES, def_id, bb);
                fh.write_u32::<NativeEndian>(real_target_bb.index() as u32).unwrap();
            },
            TerminatorKind::FalseUnwind{real_target: real_target_bb, ..} => {
                // Fake edges not considered.
                write_rec_header(fh, tcx, FALSE_UNWIND, def_id, bb);
                fh.write_u32::<NativeEndian>(real_target_bb.index() as u32).unwrap();
            },
        }
    }
}

/// Writes the "header" of a record, which is common to all record types.
fn write_rec_header(fh: &mut File, tcx: &TyCtxt, kind: u8, def_id: &DefId, bb: BasicBlock) {
    fh.write_u8(kind).unwrap();
    fh.write_u64::<NativeEndian>(tcx.crate_hash(def_id.krate).as_u64()).unwrap();
    fh.write_u32::<NativeEndian>(def_id.index.as_raw_u32()).unwrap();
    fh.write_u32::<NativeEndian>(bb.index() as u32).unwrap();
}
