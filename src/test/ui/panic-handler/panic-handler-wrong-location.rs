// compile-flags:-C panic=abort

#![no_std]
#![no_main]
#![feature(lang_items)]

#[panic_handler] //~ ERROR `panic_impl` language item must be applied to a function
#[no_mangle]
static X: u32 = 42;

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
