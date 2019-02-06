# rls-rustc

A simple shim around rustc to allow using save-analysis with a stable toolchain

## Building and running

`cargo build` or `cargo run`

You probably want to use `--release`

## Support

File an issue or ping nrc in #rust-dev-tools

## Implementation

The compiler has an extensible driver interface. The main API is the `CompilerCalls`
trait. A tool can emulate the compiler, but adjust operation by implementing
that trait. This shim does exactly that, using nearly all the defaults, but
setting some properties that are useful for tools. These are usually only
available by using a nightly toolchain, but by using this shim, can be used on
stable.

In the future we might want to make the properties we set configurable.
