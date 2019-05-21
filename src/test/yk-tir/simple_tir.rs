use std::env;

#[no_trace]
fn main() {
    let x = env::args().count();
    let mut res = 42;

    if x > 4 {
        res = 100;
    }

    println!("res: {}", res);
}

// END RUST SOURCE
// [Begin TIR for main]
// ...
// $1: t0 = $2: t0
// ...
// [End TIR for main]
