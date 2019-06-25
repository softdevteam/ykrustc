// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// A SIR basic block location.
/// FIXME: This shouldn't live here, as it will need to be shared across all tracing backends.
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

/// The software trace recorder function.
/// This is implemented in C so that: the `yk_swt_calls` MIR pass doesn't see inside.
#[allow(dead_code)] // Used only indirectly in a MIR pass.
#[cfg_attr(not(stage0), lang="yk_swt_rec_loc")]
#[cfg_attr(not(stage0), no_trace)]
#[cfg(not(test))]
fn yk_swt_rec_loc(crate_hash: u64, def_idx: u32, bb_idx: u32) {
    extern "C" { fn yk_swt_rec_loc_impl(crate_hash: u64, def_idx: u32, bb_idx: u32); }
    unsafe { yk_swt_rec_loc_impl(crate_hash, def_idx, bb_idx); }
}

/// Start software tracing on the current thread. The current thread must not already be tracing.
#[cfg_attr(not(stage0), no_trace)]
pub fn start_tracing() {
    extern "C" { fn yk_swt_start_tracing_impl(); }
    unsafe { yk_swt_start_tracing_impl(); }
}

/// Stop software tracing and on success return a tuple containing a pointer to the raw trace
/// buffer, and the number of items inside. Returns `None` if the trace was invalidated, or if an
/// error occurred. The current thread must already be tracing.
#[cfg_attr(not(stage0), no_trace)]
pub fn stop_tracing() -> Option<(*mut SirLoc, usize)> {
    let len: usize = 0;

    extern "C" { fn yk_swt_stop_tracing_impl(ret_len: &usize) -> *mut SirLoc; }
    let buf = unsafe { yk_swt_stop_tracing_impl(&len) };

    if buf.is_null() {
        None
    } else {
        Some((buf, len))
    }
}

/// Invalidate the software trace, if one is being collected.
#[cfg_attr(not(stage0), no_trace)]
pub fn invalidate_trace() {
    extern "C" { fn yk_swt_invalidate_trace_impl(); }
    unsafe { yk_swt_invalidate_trace_impl(); }
}
