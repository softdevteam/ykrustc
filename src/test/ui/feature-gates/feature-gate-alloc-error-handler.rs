// compile-flags:-C panic=abort

#![no_std]
#![no_main]
#![feature(lang_items)]

use core::alloc::Layout;

#[alloc_error_handler] //~ ERROR #[alloc_error_handler] is an unstable feature (see issue #51540)
fn oom(info: Layout) -> ! {
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
