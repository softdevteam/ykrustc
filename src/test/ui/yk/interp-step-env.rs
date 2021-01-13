// build-fail
// dont-check-compiler-stderr
// compile-flags: -C tracer=hw -C opt-level=0
#![feature(stmt_expr_attributes)]

struct IO(u8);

fn main() {
    let y = 6;
    let f1 = #[interp_step] |io: &mut IO| {
        //~^ ERROR The #[interp_step] function must not capture from its environment
        io.0 = y
    };
    f1(&mut IO(0));
}
