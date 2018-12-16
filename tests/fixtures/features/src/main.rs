#[cfg(feature = "foo")]
pub struct Foo;

#[cfg(feature = "bar")]
pub struct Bar;

#[cfg(feature = "baz")]
pub struct Baz;

fn main() {
    Foo {};
    Bar {};
    Baz {};
}
