// build-fail
// dont-check-compiler-stderr
// compile-flags: -C tracer=hw -C opt-level=0

struct IO();

fn main() {
    f1(&mut IO());
}

#[interp_step]
fn f1(_io: &mut IO) -> u8 { //~ ERROR: The #[interp_step] function must return unit
    0
}
