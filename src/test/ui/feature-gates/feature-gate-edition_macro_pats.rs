// Feature gate test for `edition_macro_pats` feature.

macro_rules! foo {
    ($x:pat2015) => {}; //~ERROR `pat2015` and `pat2021` are unstable
    ($x:pat2021) => {}; //~ERROR `pat2015` and `pat2021` are unstable
}

fn main() {}
