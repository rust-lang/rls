use std::sync::*;

extern crate sample_project;

/// A struct which does things
///
/// * one thing
/// * another thing
pub struct Foo {
    pub x: u32,
    pub y: u32,
}


// mod sub_mod;

pub fn foo_maker() -> Foo {
    Foo { x: 3, y: 4 }
}

pub fn main() {
    let mut bar = 42;
    let f = bar;
    let g = &mut bar;
    let foo = f;
    let _ = foo + 2;
    println!("Hello world! {} {}", foo, g);

    let _a = Arc::new(42);
    let _b = Once::new();
    // let c = sub_mod::foo();
    let _d = String::new();

    let _e = Foo { x: 3, y: 4 };

    let _v: Vec<i32> = Vec::new();

    let _x = foo_maker();

    fn bar(x: i32) {
        let y = x;
    }


    let a = sample_project::Request::new();
}
