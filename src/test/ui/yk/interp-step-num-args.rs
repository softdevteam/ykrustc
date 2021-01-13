// build-fail
// dont-check-compiler-stderr
// compile-flags: -C tracer=hw -C opt-level=0

fn main() {
    f1();
}

#[interp_step]
fn f1() {} //~ ERROR The #[interp_step] function must accept only one argument
