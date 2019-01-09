// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// The software trace recorder function.
/// The `AddYkSWTCalls` MIR pass injects a call this for every MIR block. The call is done
/// indirectly via a wrapper in libcore.
#[cfg_attr(not(stage0), lang="yk_swt_rec_loc")]
#[allow(unused_variables,dead_code)]
#[cfg_attr(not(stage0), no_trace)]
#[cfg(not(test))]
fn rec_loc(crate_hash: u64, def_idx: u32, bb_idx: u32) {
    // Not implemented.
}
