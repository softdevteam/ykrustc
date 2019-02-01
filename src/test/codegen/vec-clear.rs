// ignore-debug: the debug assertions get in the way
// compile-flags: -O
// ignore-test FIXME swt_ignore

#![crate_type = "lib"]

// CHECK-LABEL: @vec_clear
#[no_mangle]
pub fn vec_clear(x: &mut Vec<u32>) {
    // CHECK-NOT: load
    // CHECK-NOT: icmp
    x.clear()
}
