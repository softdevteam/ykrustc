// compile-flags:-C panic=abort

#![no_std]
#![no_main]
#![feature(lang_items)]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(
    info: PanicInfo, //~ ERROR argument should be `&PanicInfo`
) -> () //~ ERROR return type should be `!`
{
}

#[lang = "yk_swt_rec_loc"]
fn yk_swt_rec_loc(_crate_hash: u64, _def_idx: u32, _bb: u32) {}
