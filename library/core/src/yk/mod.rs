// Copyright 2018-2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Software Tracing FIXME broken and rotted.
//pub mod swt;

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
        SirLoc { symbol_name, bb_idx }
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

/// This is a special function used to indicate the trace inputs to the compiler.
/// `tup` is a tuple of the trace inputs. The local variable number of the returned tuple is stored
/// in SIR for consumption at runtime.
#[cfg_attr(not(bootstrap), lang = "yk_trace_inputs")]
pub fn trace_inputs<T>(tup: T) -> T {
    tup
}
