// Copyright 2019 King's College London.
// Created by the Software Development Team <http://soft-dev.org/>.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate cc;

fn main() {
    let mut c_build = cc::Build::new();

    c_build.file("yk_swt_impl.c");
    c_build.compile("yk_swt_impl");
    c_build.flag("-std=c11");
    c_build.warnings(true);
    c_build.extra_warnings(true);

    println!("cargo:rerun-if-changed=yk_swt_impl.c");
}
