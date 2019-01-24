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

extern crate core;
extern crate libc;
extern crate test;

use core::yk_swt::{start_tracing, stop_tracing};
use test::black_box;

pub fn main() {
    start_tracing();
    black_box(work());
    let trace = stop_tracing().unwrap();

    let len = trace.len();
    // The default capacity of the trace buffer is 1024. We want to be sure we've tested the case
    // where it had to be reallocated beyond its starting capacity.
    assert!(len > 1024);

    for idx in 0..len {
        trace.loc(idx); // All indices are in bounds, so should not panic.
    }

    unsafe { libc::free(trace.buf() as *mut libc::c_void) };
}

#[inline(never)]
fn work() -> u64{
    let mut res = 100;
    for i in 0..3000 {
        res += res / 2 + i;
    }
    res
}
