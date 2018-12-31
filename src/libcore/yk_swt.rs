// Copyright 2018 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// The software trace recorder function.
/// This is a weak language item, it actually resides in libstd. It has to be weak to allow libcore
/// to call up to libstd (libstd is not a dependency of libcore).
extern "Rust" {
    #[cfg_attr(not(stage0), lang="yk_swt_rec_loc")]
    fn yk_swt_rec_loc(crate_hash: u64, def_idx: u32, bb: u32);
}

/// Wrapper lang item to call the above wrapper function.
/// This has to be a lang item too, as a MIR terminator cannot call a weak language item directly.
#[allow(dead_code)] // Used only indirectly in a MIR pass.
#[cfg_attr(not(stage0), lang="yk_swt_rec_loc_wrap")]
#[cfg_attr(not(stage0), no_trace)]
fn yk_swt_rec_loc_wrap(crate_hash: u64, def_idx: u32, bb: u32) {
    unsafe { yk_swt_rec_loc(crate_hash, def_idx, bb) };
}

