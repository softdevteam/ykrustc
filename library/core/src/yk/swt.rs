// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use super::SirLoc;

/// The software trace recorder function.
/// This is implemented in C so that: the `yk_swt_calls` MIR pass doesn't see inside.
#[allow(dead_code)] // Used only indirectly in a MIR pass.
#[cfg_attr(not(bootstrap), lang = "yk_swt_rec_loc")]
#[cfg_attr(not(bootstrap), no_sw_trace)]
#[cfg(not(test))]
fn yk_swt_rec_loc(crate_hash: u64, def_idx: u32, bb_idx: u32) {
    extern "C" {
        fn yk_swt_rec_loc_impl(crate_hash: u64, def_idx: u32, bb_idx: u32);
    }
    /// SAFETY: Calls C.
    unsafe {
        yk_swt_rec_loc_impl(crate_hash, def_idx, bb_idx);
    }
}

/// Start software tracing on the current thread. The current thread must not already be tracing.
#[cfg_attr(not(bootstrap), no_sw_trace)]
pub fn start_tracing() {
    extern "C" {
        fn yk_swt_start_tracing_impl();
    }
    /// SAFETY: Calls C.
    unsafe {
        yk_swt_start_tracing_impl();
    }
}

/// Stop software tracing and on success return a tuple containing a pointer to the raw trace
/// buffer, and the number of items inside. Returns `None` if an error occurred. The current thread
/// must already be tracing.
#[cfg_attr(not(bootstrap), no_sw_trace)]
pub fn stop_tracing() -> Option<(*mut SirLoc, usize)> {
    let len: usize = 0;

    extern "C" {
        fn yk_swt_stop_tracing_impl(ret_len: &usize) -> *mut SirLoc;
    }
    /// SAFETY: Calls C.
    let buf = unsafe { yk_swt_stop_tracing_impl(&len) };

    if buf.is_null() { None } else { Some((buf, len)) }
}
