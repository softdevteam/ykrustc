// compile-flags: -C panic=abort
// no-prefer-dynamic

#![no_std]
#![crate_type = "staticlib"]
#![feature(panic_handler, alloc_error_handler, alloc, lang_items)]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[alloc_error_handler]
fn oom(_: core::alloc::Layout) -> ! {
    loop {}
}

extern crate alloc;

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
