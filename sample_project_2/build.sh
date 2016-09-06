export RUSTFLAGS="-Zsave-analysis -Zno-trans -Zcontinue-parse-after-error"
export PATH="/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2/bin:$PATH"
cargo build
