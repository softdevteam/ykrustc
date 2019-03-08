#![crate_type = "bin"]
#![feature(lang_items)]
#![no_main]
#![no_std]

use core::alloc::Layout;
use core::panic::PanicInfo;

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}

#[lang = "eh_personality"]
fn eh() {}

#[lang = "oom"]
fn oom(_: Layout) -> ! {
    loop {}
}
