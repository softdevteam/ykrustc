fn main() {
    println!("{}", f(None));
}

#[inline(never)]
fn f(a: Option<usize>) -> String {
    let s = String::from("hello");
    format!("{}{}", s, a.unwrap())
}
