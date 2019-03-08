// compile-flags: -O

#![crate_type = "lib"]
#![no_trace]

// CHECK: Function Attrs: norecurse nounwind
pub extern fn foo() {}
