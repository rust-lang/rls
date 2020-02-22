/// This is a macro
#[macro_export]
macro_rules! test_hover {
    () => {
        ()
    };
}
fn main() {
    test_hover!();
}
/// MORE DOCS
fn more() {}
