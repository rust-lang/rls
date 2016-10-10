use std::sync::*;

extern crate sample_project;

/// A struct which does things
///
/// * one thing
/// * another thing
struct Foo {
    x: u32,
    y: u32,
}


// mod sub_mod;

fn foo_maker() -> Foo {
    Foo { x: 3, y: 4 }
}

fn main() {
    let mut bar = 421;
    let f = bar;
    let g = &mut bar;
    let foo = f;
    let _ = foo + 2;
    println!("Hello world! {} {}", foo, g);

    let a = Arc::new(42);
    let b = Once::new();
    // let c = sub_mod::foo();
    let d = String::new();

    let e = Foo { x: 3, y: 4 };

    let v: Vec<i32> = Vec::new();

    let x = foo_maker();

    fn bar(x: i32) {
        let y = x;
    }


    let a = sample_project::Request::new();
}
