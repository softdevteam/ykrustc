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
// bb2:
// ...
//     Assign(Local(42), Unimplemented)
// ...
//     term: SwitchInt { target_bbs: [4, 3] }
// bb3:
//     Assign(Local(45), Unimplemented)
//     term: Goto { target_bb: 4 }
// bb4:
//     Assign(Local(46), Phi([Local(42), Local(45)]))
// ...
// [End TIR for main]
