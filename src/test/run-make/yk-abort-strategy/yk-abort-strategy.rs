fn main() {
    let s = String::from("hello");
    println!("{} {}", s, f(1));
}

#[inline(never)]
fn f(_a: usize) -> usize {
    panic!();
}
