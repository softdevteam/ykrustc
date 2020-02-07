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

const SYM_MAX: usize = 128;

/// A SIR basic block location.
/// Note that this cannot implement Debug, as we have an array of length >32 inside.
#[allow(missing_debug_implementations)]
#[repr(C)]
pub struct SirLoc {
    /// The name of the binary-level symbol.
    /// This has to be stored as a fixed-size array, as we have no String in libcore.
    symbol_name: [u8; SYM_MAX],
    /// The basic block index.
    bb_idx: u32,
}

impl SirLoc {
    /// Creates a new SirLoc.
    pub fn new(symbol_name: [u8; SYM_MAX], bb_idx: u32) -> SirLoc {
        SirLoc {
            symbol_name,
            bb_idx
        }
    }

    /// Returns the binary-level symbol name of the location.
    pub fn symbol_name(&self) -> [u8; SYM_MAX] {
        self.symbol_name
    }

    /// Returns the basic block index of the location.
    pub fn bb_idx(&self) -> u32 {
        self.bb_idx
    }
}
