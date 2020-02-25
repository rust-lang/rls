/// This is a macro
#[macro_export]
macro_rules! test_hover {
    () => {
        let _ = String::default();
    };
}
fn main() {
    test_hover!();
}
