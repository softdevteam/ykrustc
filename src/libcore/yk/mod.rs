// Copyright 2018-2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Software Tracing
pub mod swt;

/// A SIR basic block location.
#[repr(C)]
#[derive(Debug)]
pub struct SirLoc {
    /// Unique identifier for the crate.
    crate_hash: u64,
    /// The definition index.
    def_idx: u32,
    /// The basic block index.
    bb_idx: u32,
}

impl SirLoc {
    /// Creates a new SirLoc.
    pub fn new(crate_hash: u64, def_idx: u32, bb_idx: u32) -> SirLoc {
        SirLoc {
            crate_hash,
            def_idx,
            bb_idx
        }
    }

    /// Returns the crate hash of the location.
    pub fn crate_hash(&self) -> u64 {
        self.crate_hash
    }

    /// Returns the definition index of the location.
    pub fn def_idx(&self) -> u32 {
        self.def_idx
    }

    /// Returns the basic block index of the location.
    pub fn bb_idx(&self) -> u32 {
        self.bb_idx
    }
}
