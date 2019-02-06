mod a {
    pub fn bar() {}
    pub fn qux() {}
}

mod b {
    use a::qux;
    use a::bar as baz;

    pub fn foo() {
        qux();
        baz();
    }
}

fn main() {}
