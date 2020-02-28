/// This is a macro
macro_rules! test_hover {
    () => {
        let _ = String::default();
    };
}

/// This is a macro Item
macro_rules! test_hover_item {
    () => {
        struct Foo;
    };
}

/// This is a macro Expr
macro_rules! test_hover_expr {
    () => {
        if true {}
    };
}

pub fn main() {
    test_hover!();
    test_hover_item!();
    test_hover_expr!();
}
