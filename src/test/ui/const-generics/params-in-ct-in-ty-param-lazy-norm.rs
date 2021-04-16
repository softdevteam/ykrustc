// revisions: full min

#![cfg_attr(full, feature(const_generics))]
#![cfg_attr(full, allow(incomplete_features))]

struct Foo<T, U = [u8; std::mem::size_of::<T>()]>(T, U);
//[full]~^ ERROR constant values inside of type parameter defaults
//[min]~^^ ERROR generic parameters may not be used in const operations

// FIXME(const_generics_defaults): We still don't know how to deal with type defaults.
struct Bar<T = [u8; N], const N: usize>(T);
//~^ ERROR constant values inside of type parameter defaults
//~| ERROR generic parameters with a default

fn main() {}
