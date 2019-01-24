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

// Collect two traces sequentially in the same thread.
pub fn main() {
    start_tracing();
    black_box(work1());
    let trace1 = stop_tracing().unwrap();

    start_tracing();
    black_box(work2());
    let trace2 = stop_tracing().unwrap();

    assert!(trace1.len() > trace2.len());

    unsafe { libc::free(trace1.buf() as *mut libc::c_void) };
    unsafe { libc::free(trace2.buf() as *mut libc::c_void) };
}

#[inline(never)]
fn work1() -> u64{
    let mut res = 100;
    for _ in 0..9000 {
        res += 1;
    }
    res
}

#[inline(never)]
fn work2() -> u64{
    let mut res = 6000;
    for _ in 0..3000 {
        res -= 1;
    }
    res
}
