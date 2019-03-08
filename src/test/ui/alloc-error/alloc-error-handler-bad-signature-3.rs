// compile-flags:-C panic=abort

#![feature(alloc_error_handler, panic_handler, lang_items)]
#![no_std]
#![no_main]

struct Layout;

#[alloc_error_handler]
fn oom() -> ! { //~ ERROR function should have one argument
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
