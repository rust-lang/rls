use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CFG_RELEASE_CHANNEL");

    println!(
        "cargo:rustc-env=GIT_HASH={}",
        rustc_tools_util::get_commit_hash().unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=COMMIT_DATE={}",
        rustc_tools_util::get_commit_date().unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=FIXTURES_DIR={}",
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("tests")
            .join("fixtures")
            .display()
    );
}
