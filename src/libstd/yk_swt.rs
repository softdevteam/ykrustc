// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use ::cell::RefCell;
use ::fmt;

#[allow(missing_docs)]
/// A block location in the Rust MIR.
pub struct MirLoc {
    pub crate_hash: u64,
    pub def_idx: u32,
    pub bb_idx: u32,
}

impl fmt::Debug for MirLoc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "loc<{}, {}, {}>", self.crate_hash, self.def_idx, self.bb_idx)
    }
}

thread_local! {
    /// The software trace currently being collected (if any).
    /// When `Some`, a tracing is enabled, otherwise tracing is disabled.
    pub static TRACE: RefCell<Option<Vec<MirLoc>>> = RefCell::new(None);
}

/// Start software tracing.
#[cfg_attr(not(stage0), no_trace)]
pub fn start_tracing() {
    TRACE.with(|rc| {
        let mut trace_o = rc.borrow_mut();
        match *trace_o {
            Some(_) => panic!("tracing was already started for this thread!"),
            None => *trace_o = Some(Vec::new()),
        }
    });
}

// FIXME Anything used in `rec_loc` below cannot itself be traced, or we get infinite recursion. To
// work sround this, many crates are ignored by the software tracing MIR pass (see
// librustc_mir/transform/add_yk_swt_calls.rs). Consider re-implementing the trace recorder in C?

/// The software trace recorder function.
/// The `AddYkSWTCalls` MIR pass injects a call this for every MIR block. The call is done
/// indirectly via a wrapper in libcore.
#[cfg_attr(not(stage0), lang="yk_swt_rec_loc")]
#[allow(unused_variables,dead_code)]
#[cfg_attr(not(stage0), no_trace)]
#[cfg(not(test))]
fn rec_loc(crate_hash: u64, def_idx: u32, bb_idx: u32) {
    TRACE.with(|rc| {
        let mut trace_o = rc.borrow_mut();
        match trace_o.as_mut() {
            Some(trace) => trace.push(MirLoc{crate_hash, def_idx, bb_idx}),
            None => (), // Tracing is disabled, do nothing.
        }
    });
}

/// Stop tracing and return the trace.
#[cfg_attr(not(stage0), no_trace)]
pub fn stop_tracing() -> Vec<MirLoc> {
    TRACE.with(|rc| {
        let trace_o = rc.borrow_mut().take();
        if trace_o.is_none() {
            panic!("tracing not started on this thread");
        }
        trace_o.unwrap()
    })
}
