// Copyright 2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// error-pattern: thread 'main' panicked at 'tracing was already started for this thread!'

#![feature(yk_swt)]

use std::yk_swt::{start_tracing, stop_tracing};

pub fn main() {
    start_tracing();
    start_tracing(); // Can't call a second time without a stop_tracing() first.
}
