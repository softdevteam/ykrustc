// build-fail
// dont-check-compiler-stderr
// compile-flags: -C tracer=hw -C opt-level=0

fn main() {
    f1(5);
}

#[interp_step]
fn f1(_io: u8) -> bool {
    //~^ ERROR The #[interp_step] function must accept a mutable reference to a struct
    true
}
