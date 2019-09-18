#![crate_type = "lib"]
#![no_sw_trace]

pub mod a {
    #[inline(always)]
    pub fn foo() {
    }

    pub fn bar() {
    }
}

#[no_mangle]
pub fn bar() {
    a::foo();
}
