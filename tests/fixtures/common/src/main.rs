struct Bar {
    x: u64,
}

#[test]
pub fn test_fn() {
    let bar = Bar { x: 4 };
    println!("bar: {}", bar.x);
}

pub fn main() {
    let world = "world";
    println!("Hello, {}!", world);

    let bar2 = Bar { x: 5 };
    println!("bar2: {}", bar2.x);
}
