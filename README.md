[![Build Status](https://travis-ci.org/rust-lang-nursery/rls.svg?branch=master)](https://travis-ci.org/rust-lang-nursery/rls) [![Build status](https://ci.appveyor.com/api/projects/status/cxfejvsqnnc1oygs?svg=true)](https://ci.appveyor.com/project/jonathandturner/rls-x6grn)
[![Nightly status](https://img.shields.io/badge/dynamic/json.svg?label=rls-preview%20(Windows)&url=https%3A%2F%2Fraw.githubusercontent.com%2Frust-lang-nursery%2Frust-toolstate%2Fmaster%2F_data%2Flatest.json&query=%24%5B%3F(%40.tool%3D%3D%22rls%22)%5D.windows&colorB=lightgrey)](https://rust-lang-nursery.github.io/rust-toolstate/)
[![Nightly status](https://img.shields.io/badge/dynamic/json.svg?label=rls-preview%20(Linux)&url=https%3A%2F%2Fraw.githubusercontent.com%2Frust-lang-nursery%2Frust-toolstate%2Fmaster%2F_data%2Flatest.json&query=%24%5B%3F(%40.tool%3D%3D%22rls%22)%5D.linux&colorB=lightgrey)](https://rust-lang-nursery.github.io/rust-toolstate/)



# Rust Language Server (RLS)

The RLS provides a server that runs in the background, providing IDEs,
editors, and other tools with information about Rust programs. It supports
functionality such as 'goto definition', symbol search, reformatting, and code
completion, and enables renaming and refactorings.

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
[reference implementation of an RLS frontend](https://github.com/rust-lang-nursery/rls-vscode)
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
rustup component add rls-preview rust-analysis rust-src
```

#### Note (nightly only)
Sometimes the `rls-preview` component is not included in a nightly build due to
certain issues. To see if the component is included in a particular build and
what to do if it's not, check [#641](https://github.com/rust-lang-nursery/rls/issues/641).


## Running

The RLS is built to work with many IDEs and editors, we mostly use
VSCode to test the RLS. The easiest way is to use the [published extension](https://github.com/rust-lang-nursery/rls-vscode).

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
method.

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
* `wait_to_build` (`u64`, defaults to `1500`) time in milliseconds between
  receiving a change notification and starting build
* `all_targets` (`bool`, defaults to `true`) checks the project as if you were
  running `cargo check --all-targets`. I.e., check all targets and integration
  tests too
* `use_crate_blacklist` (`bool`, defaults to `true`) if disabled, also indexes
  data from the [blacklisted](https://github.com/nrc/rls-blacklist/blob/master/src/lib.rs) crates
* `build_on_save` (`bool`, defaults to `false`) toggles whether the RLS should
  perform continuous analysis or only after a file is saved
* `features` (`[String]`, defaults to empty) list of Cargo features to enable
* `all_features` (`bool`, defaults to `false`) enables all Cargo features
* `no_default_features` (`bool`, defaults to `false`) disables default Cargo
  features
* `racer_completion` (`bool`, defaults to `true`) enables code completion using
  racer (which is, at the moment, our only code completion backend). Also enables
  hover tooltips to fall back to racer when save-analysis data is unavailable.
* `clippy_preference` (`String`, defaults to `"opt-in"`) controls eagerness of clippy
  diagnostics when available. Valid values are _(case-insensitive)_:
  - `"off"` Disable clippy lints.
  - `"opt-in"` Clippy lints are shown when crates specify `#![warn(clippy)]`.
  - `"on"` Clippy lints enabled for all crates in workspace.

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
* `show_hover_context` show additional context in hover tooltips when available.
  This is often the local variable declaration. When set to false the content is
  only availabe when holding the `ctrl` key in some editors.


## Troubleshooting

For tips on debugging and troubleshooting, see [debugging.md](debugging.md).


## Contributing

You can look in the [contributing.md](https://github.com/rust-lang-nursery/rls/blob/master/contributing.md)
in this repo to learn more about contributing to this project.

If you want to implement RLS support in an editor, see [clients.md](clients.md).
