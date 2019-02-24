/// Foo enum
#[derive(Copy, Clone)]
pub enum Foo {
    /// Bar variant
    Bar,
    /// Baz variant
    Baz
}

/// Bar struct
pub struct Bar<T> {
    /// The first field
    field_1: Tuple,
    /// The second field
    field_2: T,
    /// The third field
    field_3: Foo,
}

impl<T> Bar<T> {
    /// Create a new Bar
    fn new(one: Tuple, two: T, three: Foo) -> Bar<T> {
        Bar {
            field_1: one,
            field_2: two,
            field_3: three,
        }
    }
}

/// Tuple struct
pub struct Tuple(pub u32, f32);

/// Bar function
/// 
/// # Examples
/// 
/// ```no_run,ignore
/// # extern crate does_not_exist;
/// 
/// use does_not_exist::other;
/// 
/// let foo = bar(1.0);
/// other(foo);
/// ```
fn bar<T>(thing: T) -> Bar<T> {
    Bar {
        field_1: Tuple(1, 3.0),
        field_2: thing,
        field_3: Foo::Bar,
    }
}

impl<T> Bar<T> {
    /// Foo method
    fn foo(&mut self, foo: Foo) -> Foo {
        let other = self.field_3;
        self.field_3 = foo;
        other
    }

    /// Bar method
    fn bar(&mut self, thing: T) -> Bar<T> where T: Copy {
        self.field_2 = thing;
        bar(self.field_2)
    }

    /// Other method
    fn other(&self, tuple: Tuple) -> Bar<f32> {
        Bar::new(Tuple(3, 1.0), tuple.1, Foo::Bar)
    }
}

fn foo() {
    let mut bar = Bar::new(Tuple(3, 1.0), 2.0, Foo::Bar);
    bar.bar(4.0);
    bar.foo(Foo::Baz);
    bar.other(Tuple(4, 5.0));
}

trait Baz {
    /// Foo other type
    type Foo: Other;

    fn foo() -> Self::Foo;

}

/// The other trait
trait Other {}

/// The constant FOO
const FOO: &'static str = "FOO";

/// The static BAR
static BAR: u32 = 123;

pub fn print_foo() {
    println!("{}", FOO);
}

pub fn print_bar() {
    println!("{}", BAR);
}