// compile-flags:-C panic=abort

#![feature(lang_items)]
#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    loop {}
}

#[lang = "panic_impl"]
fn panic2(info: &PanicInfo) -> ! { //~ ERROR duplicate lang item found: `panic_impl`.
    loop {}
}

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
