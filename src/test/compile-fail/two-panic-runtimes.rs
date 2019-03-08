// error-pattern:cannot link together two panic runtimes: panic_runtime_unwind and panic_runtime_unwind2
// ignore-tidy-linelength
// aux-build:panic-runtime-unwind.rs
// aux-build:panic-runtime-unwind2.rs
// aux-build:panic-runtime-lang-items.rs

#![no_std]
#![feature(lang_items)]

extern crate panic_runtime_unwind;
extern crate panic_runtime_unwind2;
extern crate panic_runtime_lang_items;

fn main() {}

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
