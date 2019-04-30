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
//     term: SwitchInt { target_bbs: [4, 3] }
// bb3:
//     Assign(Base(Local(0)), Use(Constant(SignedInt(I32(100)))))
//     term: Goto { target_bb: 4 }
// bb4:
// ...
// [End TIR for main]
