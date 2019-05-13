// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module converts MIR into Yorick TIR (Tracing IR).
//! Note that we preserve the MIR block structure when lowering to TIR.
//!
//! Serialisation itself is performed by an external library: ykpack.

// FIXME, just temporarily.
#![allow(unused_variables)]
#![allow(dead_code)]

use rustc::ty::TyCtxt;

use rustc::hir::def_id::DefId;
use rustc::mir::{Mir, Local};
use rustc::util::nodemap::DefIdSet;
use std::path::PathBuf;
use std::fs::File;
use rustc_yk_link::YkExtraLinkObject;
use std::fs;
use std::io::Write;
use std::error::Error;
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use rustc_data_structures::indexed_vec::IndexVec;
use ykpack;
use ykpack::LocalIndex as TirLocal;

const SECTION_NAME: &'static str = ".yk_tir";
const TMP_EXT: &'static str = ".yk_tir.tmp";

/// Describes how to output MIR.
pub enum TirMode {
    /// Write MIR into an object file for linkage. The inner path should be the path to the main
    /// executable (from this we generate a filename for the resulting object).
    Default(PathBuf),
    /// Write MIR in textual form the specified path.
    TextDump(PathBuf),
}

/// A conversion context holds the state needed to perform the TIR lowering.
struct ConvCx<'a, 'tcx, 'gcx> {
    /// The compiler's god struct. Needed for queries etc.
    tcx: &'a TyCtxt<'a, 'tcx, 'gcx>,
    /// Monotonically increasing number used to give TIR variables a unique ID.
    next_tir_var: Cell<TirLocal>,
    /// A mapping from MIR variables to TIR variables.
    var_map: RefCell<IndexVec<Local, Option<TirLocal>>>,
    /// The number of blocks in the MIR (and therefore in the TIR).
    num_blks: usize,
}

impl<'a, 'tcx, 'gcx> ConvCx<'a, 'tcx, 'gcx> {
    fn new(tcx: &'a TyCtxt<'a, 'tcx, 'gcx>, mir: &Mir<'tcx>) -> Self {
        let var_map = IndexVec::new();
        let num_blks = mir.basic_blocks().len();

        Self {
            tcx,
            next_tir_var: Cell::new(0),
            var_map: RefCell::new(var_map),
            num_blks: num_blks,
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
            let file = File::create(&tir_path)?;
            (tir_path, Some(file), None)
        },
        TirMode::TextDump(dump_path) => {
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
            // FIXME implement the lowering here. For now we are serialising dummy TIRs.
            let _ccx = ConvCx::new(tcx, mir);
            let pack = ykpack::Tir::new(ykpack::DefId{crate_hash: 0, def_idx: 0},
                                        String::from("dummy"), Vec::new());

            if let Some(ref mut e) = enc {
                e.serialise(ykpack::Pack::Tir(pack))?;
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
