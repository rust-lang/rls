#![allow(dead_code, unused_imports)]

extern crate fnv;

// pub mod test_tooltip_01;
// pub mod test_tooltip_mod;
// pub mod test_tooltip_mod_use;
// pub mod test_tooltip_std;
pub mod macro_doc_comment;

/// Test Docs NOT MACRO
fn test() {
    test_hover!();
}
