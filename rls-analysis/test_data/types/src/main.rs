struct Foo {
    f: u32,
}

fn main() {
    let x = Foo { f: 42 };
    let _: Foo = x;
}

fn foo(x: Foo) -> Foo {
    let test_binding = true;
    const TEST_CONST: bool = true;
    static TEST_STATIC: u32 = 16;
    panic!();
}

mod test_module {
    type TestType = u32;
}

union TestUnion {
    f1: u32
}

trait TestTrait {
    fn test_method(&self);
}

enum FooEnum {
    TupleVariant,
    StructVariant { x: u8 },
}
