extern crate zero;

use std::sync::*;

use zero::Pod;

struct Foo;

unsafe impl Pod for Foo {}
 
mod sub_mod;

fn main() {
    let mut bar = 42;
    let f = &mut bar;
    let g = &mut bar;
    let foo = 42;
    let _ = foo + 2;
    println!("Hello world! {}", foo);

    let a = Arc::new(42);
    let b = Once::new();

    let c = sub_mod::foo();

    fn bar() {
    }
}
