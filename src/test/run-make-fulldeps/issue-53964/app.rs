#![crate_type = "bin"]
#![no_main]
#![no_std]

#![deny(unused_extern_crates)]
#![feature(lang_items)]

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}

// `panic` provides a `panic_handler` so it shouldn't trip the `unused_extern_crates` lint
extern crate panic;
