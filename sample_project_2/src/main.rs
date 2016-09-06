extern crate zero;

use std::sync::*;

use zero::Pod;

struct Foo;

unsafe impl Pod for Foo {}
 
mod sub_mod;

fn main() {
    let mut bar = 42;
    let f = bar;
    let g = &mut bar;
    let foo = f;
    let _ = foo + 2;
    println!("Hello world! {} {}", foo, g);

    let a = Arc::new(42);
    let b = Once::new();

    let c = sub_mod::foo();

    fn bar() {
    }
}
