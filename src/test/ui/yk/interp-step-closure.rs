// build-fail
// dont-check-compiler-stderr
// compile-flags: -C tracer=hw -C opt-level=0

#![feature(stmt_expr_attributes)]

struct Ctx;

fn main() {
    let step = #[interp_step] |ctx: &mut Ctx| {};
    //~^ #[interp_step] can only be applied to regular function definitions
    step(&mut Ctx);
}
