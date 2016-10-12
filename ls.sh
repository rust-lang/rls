cd testls-client
export RUSTC="/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2/bin/rustc"
export SYS_ROOT="/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2"
export RUST_BACKTRACE=1
cargo build && code
