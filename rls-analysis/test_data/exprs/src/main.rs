struct Foo;

impl Foo {
    fn bar(&self) {
        let _ = self;
    }
}

pub extern "C" fn foo() {}

fn main() {
    foo();
}
