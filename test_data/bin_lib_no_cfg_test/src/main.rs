extern crate bin_lib_no_cfg_test;

fn main() {
    let a = bin_lib_no_cfg_test::LibStruct {};
    let test = bin_lib_no_cfg_test::LibCfgTestStruct { };
    println!("Hello, world!");
}
