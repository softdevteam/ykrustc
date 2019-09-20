// Copyright 2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(yk_swt)]
#![feature(libc)]
#![feature(test)]
#![feature(rustc_private)]

extern crate core;
extern crate libc;
extern crate test;

use core::yk_swt::{start_tracing, stop_tracing, invalidate_trace};
use test::black_box;

// Check that invalidating a trace works.
fn main() {
    start_tracing();
    black_box(work());
    invalidate_trace();
    assert!(stop_tracing().is_none());
}

#[inline(never)]
fn work() -> u64{
    let mut res = 2000;
    for _ in 0..100 {
        res -= 1;
    }
    res
}
