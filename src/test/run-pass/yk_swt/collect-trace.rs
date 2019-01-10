// Copyright 2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(yk_swt)]

use std::yk_swt::{start_tracing, stop_tracing};

pub fn main() {
    start_tracing();
    let _ = work();
    let trace = stop_tracing();
    assert!(!trace.is_empty());
}

#[inline(never)]
fn work() -> u64{
    let mut res = 100;
    for i in 0..10 {
        res += res / 2 + i;
    }
    res
}
