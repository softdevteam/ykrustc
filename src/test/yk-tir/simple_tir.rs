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
// [Begin TIR for dummy]
// ...
// [End TIR for dummy]
