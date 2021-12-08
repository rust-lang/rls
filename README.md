[![Build Status](https://github.com/rust-lang/rls/workflows/CI/badge.svg?branch=master)](https://github.com/rust-lang/rls/actions?query=workflow%3ACI+branch%3Amaster)

# Rust Language Server (RLS)

The RLS provides a server that runs in the background, providing IDEs,
editors, and other tools with information about Rust programs. It supports
functionality such as 'goto definition', symbol search, reformatting, and code
completion, and enables renaming and refactorings.

A high-level overview of the architecture can be found [here](architecture.md).

The RLS gets its source data from the compiler and from
[Racer](https://github.com/racer-rust/racer). Where possible it uses data from
the compiler which is precise and complete. Where it is not possible, (for example
for code completion and where building is too slow), it uses Racer.

Since the Rust compiler does not yet support end-to-end incremental compilation,
we can't offer a perfect experience. However, by optimising our use of the
compiler and falling back to Racer, we can offer a pretty good experience for
small to medium sized crates. As the RLS and compiler evolve, we'll offer a
better experience for larger and larger crates.

The RLS is designed to be frontend-independent. We hope it will be widely
adopted by different editors and IDEs. To seed development, we provide a
[reference implementation of an RLS frontend](https://github.com/rust-lang/rls-vscode)
for [Visual Studio Code](https://code.visualstudio.com/).


## Setup

### Step 1: Install rustup

You can install [rustup](http://rustup.rs/) on many platforms. This will help us quickly install the
RLS and its dependencies.

If you already have rustup installed, update to ensure you have the latest
rustup and compiler:

```
rustup update
```


If you're going to use the VSCode extension, you can skip step 2.


### Step 2: Install the RLS

Once you have rustup installed, run the following commands:

```
rustup component add rls rust-analysis rust-src
```

### error: component 'rls' is unavailable for download (nightly)
The development of rustc's internals is quite fast paced. Downstream projects that rely on nightly internals, particularly clippy, can break fairly often because of this.

When such breakages occur the nightly release will be missing rls. This is a trade-off compared with the other option of just not publishing the night's release, but does avoid blocking the rust nightly releases for people that don't need clippy/rls.

To mitigate the issues we have:
* rustup will warn if the update is missing any components you currently have. This means you can no longer accidentally update to a no-rls release. Once rls is available again it'll update.
* rls, clippy are available on the stable channel. Meaning most developers installing for the first time should use stable.
* However, if you need latest nightly rls you can use https://rust-lang.github.io/rustup-components-history/ to find and install a dated nightly release ie `rustup install nightly-2018-12-06`.

Also see [#641](https://github.com/rust-lang/rls/issues/641).

## Running

The RLS is built to work with many IDEs and editors, we mostly use
VSCode to test the RLS. The easiest way is to use the [published extension](https://github.com/rust-lang/rls-vscode).

You'll know it's working when you see this in the status bar at the bottom, with
a spinning indicator:

`RLS: working ◐`

Once you see:

`RLS`

Then you have the full set of capabilities available to you.  You can goto def,
find all refs, rename, goto type, etc.  Completions are also available using the
heuristics that Racer provides.  As you type, your code will be checked and
error squiggles will be reported when errors occur.  You can hover these
squiggles to see the text of the error.

## Configuration

The RLS can be configured on a per-project basis; using the Visual
Studio Code extension this will be done via the workspace settings file
`settings.json`.

Other editors will have their own way of sending the
[workspace/DidChangeConfiguration](https://microsoft.github.io/language-server-protocol/specification#workspace_didChangeConfiguration)
method. Options are nested in the `rust` object, so your LSP client might send
`{"settings":{"rust":{"unstable_features":true}}}` as parameters.

Entries in this file will affect how the RLS operates and how it builds your
project.

Currently we accept the following options:

* `unstable_features` (`bool`, defaults to `false`) enables unstable features.
  Currently no option requires this flag.
* `sysroot` (`String`, defaults to `""`) if the given string is not empty, use
  the given path as the sysroot for all rustc invocations instead of trying to
  detect the sysroot automatically
* `target` (`String`, defaults to `""`) if the given string is not empty, use
  the given target triple for all rustc invocations
* `wait_to_build` (`u64`) overrides build debounce duration (ms). This is otherwise automatically
  inferred by the latest build duration.
* `all_targets` (`bool`, defaults to `true`) checks the project as if you were
  running `cargo check --all-targets`. I.e., check all targets and integration
  tests too
* `crate_blacklist` (`[String]`, defaults to [this list](https://github.com/rust-dev-tools/rls-blacklist/blob/master/src/lib.rs))
  allows to specify which crates should be skipped by the RLS.
  By default skips libraries that are of considerable size but which the user
  often may not be directly interested in, thus reducing the build latency.
* `build_on_save` (`bool`, defaults to `false`) toggles whether the RLS should
  perform continuous analysis or only after a file is saved
* `features` (`[String]`, defaults to empty) list of Cargo features to enable
* `all_features` (`bool`, defaults to `false`) enables all Cargo features
* `no_default_features` (`bool`, defaults to `false`) disables default Cargo
  features
* `racer_completion` (`bool`, defaults to `true`) enables code completion using
  racer (which is, at the moment, our only code completion backend). Also enables
  hover tooltips & go-to-definition to fall back to racer when save-analysis data is unavailable.
* `clippy_preference` (`String`, defaults to `"opt-in"`) controls eagerness of clippy
  diagnostics when available. Valid values are _(case-insensitive)_:
  - `"off"` Disable clippy lints.
  - `"on"` Display the same diagnostics as command-line clippy invoked with no arguments (`clippy::all` unless overridden).
  - `"opt-in"` Only display the lints [explicitly enabled in the code](https://github.com/rust-lang/rust-clippy#allowingdenying-lints). Start by adding `#![warn(clippy::all)]` to the root of each crate you want linted.

and the following unstable options:

* `build_lib` (`bool`, defaults to `false`) checks the project as if you passed
  the `--lib` argument to cargo. Mutually exclusive with, and preferred over,
  `build_bin`.
* `build_bin` (`String`, defaults to `""`) checks the project as if you passed
  `-- bin <build_bin>` argument to cargo. Mutually exclusive with `build_lib`.
* `cfg_test` (`bool`, defaults to `false`) checks the project as if you were
  running `cargo test` rather than `cargo build`. I.e., compiles (but does not
  run) test code.
* `full_docs` (`bool`, defaults to `false`) instructs rustc to populate the
  save-analysis data with full source documentation. When set to `false`, only the
  first paragraph is recorded. This option _currently_ has little to no effect on
  hover tooltips. The save-analysis docs are only used if source extraction fails.
  This option has no effect on the standard library.
* `show_hover_context` (`bool`, defaults to `true`) show additional context in
  hover tooltips when available. This is often the local variable declaration.
  When set to false the content is only available when holding the `ctrl` key in
  some editors.


## Troubleshooting

For tips on debugging and troubleshooting, see [debugging.md](debugging.md).


## Contributing

You can look in the [contributing.md](https://github.com/rust-lang/rls/blob/master/contributing.md)
in this repo to learn more about contributing to this project.

If you want to implement RLS support in an editor, see [clients.md](clients.md).
