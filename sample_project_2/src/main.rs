use std::sync::*;

struct Foo {
    x: u32,
    y: u32
}

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
    let d = String::new();

    let e = Foo { x: 3, y: 4 };
    

    fn bar() {
    }
}
