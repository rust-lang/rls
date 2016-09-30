# Rust Language Service (RLS)

**This project is in the early stages of development, it is not yet ready for real
use. It will probably eat your laundry.**

The RLS is provides a service that runs in the background, providing IDEs,
editors, and other tools with information about Rust programs. It supports
functionality such as 'goto definition', symbol search, reformatting, and code
completion, and enables renaming and refactorings.

The RLS gets its source data from the compiler and from
[Racer](https://github.com/phildawes/racer). Where possible it uses data from
the compiler which is precise and complete. Where its not possible, (for example
for code completion and where building is too slow), it uses Racer.

Since the Rust compiler does not yet support end-to-end incremental compilation,
we can't offer a perfect experience. However, by optimising our use of the
compiler and falling back to Racer, we can offer a pretty good experience for
small to medium sized crates. As the RLS and compiler evolve, we'll offer a
better experience for larger and larger crates.

The RLS is designed to be frontend-independent. We hope it will be widely
adopted by different editors and IDEs. To seed development, we provide a
[reference implementation of an RLS frontend](https://github.com/jonathandturner/rustls_vscode)
for [Visual Studio Code](https://code.visualstudio.com/).


## Building

Since the RLS is closely linked to the compiler and is in active development,
you'll need a recent nightly compiler to build it.

Use `cargo build` to build.


## Testing

YOLO! (https://github.com/jonathandturner/rustls/issues/11, https://github.com/jonathandturner/rustls/issues/12)


## Running

To run the RLS, you need to specify the sysroot as an environment variable (this
should become unnecessary in the future). This is the route directory of your
Rust installation. You can find the sysroot with `rustc --print sysroot`.

If you have installed Rust directly it will probably be `/usr/local`; if you are using
a home-made compiler, it will be something like `~/rust/x86_64-unknown-linux-gnu/stage2`;
with Rustup it will change depending on the version of Rust being used, it
should be something like `~/multirust/toolchain/nightly-x86_64-unknown-linux-gnu`.

Run with:

```
SYS_ROOT=/usr/local cargo run
```

To run with VSCode, you'll need a recent version of that
[installed](https://code.visualstudio.com/download).

You'll then need a copy of our [VSCode plugin](https://github.com/jonathandturner/rustls_vscode).
Assuming you'll be doing this for development, you probably don't want to
install that plugin, but just open it in VSCode and then run it (F5).

It should all just work! You might need to make an edit and save before some of
the features kick in (which is a bug - https://github.com/jonathandturner/rustls_vscode/issues/3).

To work with the RLS, your project must be buildable using `cargo build`. If you
use syntax extensions or build scripts, it is likely things will go wrong.


## Standard library support

Getting the RLS to work with the standard libraries takes a little more work, we
hope to address this in the future for a more ergonomic solution (https://github.com/jonathandturner/rustls/issues/9).

The way it works is that when the libraries are built, the compiler can emit all
the data that the RLS needs. This can be read by the RLS on startup and used to
provide things like type on hover without having access to the source code for
the libraries.

The compiler gives every definition an id, and the RLS matches up these ids. In
order for the RLS to work, the id of a identifier used in the IDE and the id of
its declaration in a library must match exactly. Since ids are very unstable,
the data used by the RLS for libraries must match exactly with the crate that
your source code links with.

You need to generate the above data for the standard libraries if you want the
RLS to know about them. Furthermore, you must do so for the exact version of the
libraries which your code uses. The easiest (but certainly not the quickest) way
to do this is to build the compiler and libraries from source, and use these
libraries with your code.

In your Rust directory, you want to run the following:

```
# Or whatever -j you usually use.
RUSTFLAGS_STAGE2='-Zsave-analysis-api' make -j6
```

Then go get a coffee, possibly from a cafe on the other side of town if you have
a slower machine.

If all goes well, you should have a bunch of JSON data in a directory like
`~/rust/x86_64-unknown-linux-gnu/stage2/lib/rustlib/x86_64-unknown-linux-gnu/lib/save-analysis`.
You need to copy all those files (should be around 16) into `libs/save-analysis`
in the root of your project directory (i.e., next to `src` and `target`).

Finally, to run the RLS you'll need to set things up to use the newly built
compiler, something like:

```
export RUSTC="/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2/bin/rustc"
export SYS_ROOT="/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2"
cargo run
```

Yeah, sorry, it's quite the process, like I said we should be able to do better
than this...

You'll also need to use the above script to run the RLS if you're making changes
to the compiler which affect the RLS.


## Implementation overview

TODO

* goals/constraints
* compiler/racer
    - in-process compiler
* communication with IDEs
* other crates
    - https://github.com/phildawes/racer
    - https://github.com/nrc/rls-analysis
    - https://github.com/nrc/rls-vfs
* modules

## Contributing

The RLS is open source and we'd love you to contribute to the project. Testing,
reporting issues, writing documentation, writing tests, writing code, and
implementing clients are all extremely valuable.

Here is the list of known [issues](https://github.com/jonathandturner/rustls/issues).
These are [good issues to start on](https://github.com/jonathandturner/rustls/issues?q=is%3Aopen+is%3Aissue+label%3Aeasy).

We're happy to help however we can. The best way to get help is either to
leave a comment on an issue in this repo, or to ping us (nrc or jntrnr) in #rust-tools
on IRC.

We'd love for existing and new tools to use the RLS. If that sounds interesting
please get in touch by filing an issue or on IRC.
