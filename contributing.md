# Contributing

This document provides information for developers who want to contribute to the
RLS or run it in a heavily customised configuration.

The RLS is open source and we'd love you to contribute to the project. Testing,
reporting issues, writing documentation, writing tests, writing code, and
implementing clients are all extremely valuable.

Here is the list of known [issues](https://github.com/rust-lang/rls/issues).
These are [good issues to start on](https://github.com/rust-lang/rls/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22).

A good resource on how RLS works can be found [here](architecture.md).

We're happy to help however we can. The best way to get help is either to
leave a comment on an issue in this repo, or to ping us (nrc) in #rust-dev-tools
on IRC.

We'd love for existing and new tools to use the RLS. If that sounds interesting
please get in touch by filing an issue or on IRC.

If you want to implement RLS support in an editor, see [clients.md](clients.md).

## Building

Note, you don't need to build the `rls` to use it. Instead, you can install
via `rustup`, which is the currently preferred method. See the
[readme](README.md) for more information.

### Step 1: Install build dependencies

On Linux, you will need [cmake](https://cmake.org/), [pkg-config](https://www.freedesktop.org/wiki/Software/pkg-config/)
and [zlib](http://zlib.net/):

- On Ubuntu run: `sudo apt-get install cmake pkg-config zlib1g-dev libssl-dev`
- On Fedora run: `sudo dnf install cmake pkgconfig zlib-devel openssl-devel`

On Windows, you will need to have [cmake](https://cmake.org/) installed.

### Step 2: Clone and build the RLS

Since the RLS is closely linked to the compiler and is in active development,
you'll need a recent nightly compiler to build it.

```
git clone https://github.com/rust-lang/rls.git
cd rls
cargo build --release
```

#### If RLS couldn't be built with clippy

Sometimes nightly toolchain changes break the `clippy_lints` dependency.
Since RLS depends on `clippy_lints` by default, those changes can also break RLS itself.
In this case, you can build RLS like this:

`cargo build --no-default-features` (disabling the clippy feature)

And sometimes git revision of `clippy` submodule in the Rust repo (https://github.com/rust-lang/rust/tree/master/src/tools) and `clippy_lints` dependency of RLS is different.
In this case, submit a PR here updating the `clippy_lints` dependency to the git revision pulled from the Rust tree.

### Step 3: Connect the RLS to your compiler

If you're using recent versions of rustup, you will also need to make sure that
the compiler's dynamic libraries are available for the RLS to load. You can see
where they  are using:

```
rustc --print sysroot
```

This will show you where the compiler keeps the dynamic libs. In Windows, this
will be  in the `bin` directory under this path. On other platforms, it will be
in the `lib` directory.

Next, you'll make the compiler available to the RLS:

#### Windows

On Windows, make sure this path (plus `bin`) is in your PATH.  For example:

```
set PATH=%PATH%;C:\Users\appveyor\.multirust\toolchains\nightly-i686-pc-windows-gnu\bin
```

#### Mac

For Mac, you need to set the DYLD_LIBRARY_PATH.  For example:

```
export DYLD_LIBRARY_PATH=$(rustc --print sysroot)/lib
```

#### Linux

For Linux, this path is called LD_LIBRARY_PATH.

```
export LD_LIBRARY_PATH=$(rustc --print sysroot)/lib
```

### Step 4: Download standard library metadata

Finally, we need to get the metadata for the standard library.  This lets
us get additional docs and types for all of `std`.  The command is currently only
supported on the nightly compilers, though we hope to remove this restriction in
the future.

```
rustup component add rust-analysis
```

If you've never set up Racer before, you may also need to follow the
[Racer configuration steps](https://github.com/racer-rust/racer#configuration)

## Running and testing

You can run the rls by hand with:

```
cargo run
```

Though more commonly, you'll use an IDE plugin to invoke it for you
(see [README.md](README.md) for details).

We recommend using https://github.com/rust-lang/rls-vscode in VSCode.
You can configure `rls-vscode` to use custom built binary by changing the
`rust-client.rlsPath` setting to a full path to the binary you want to use.

Anything the RLS writes to stderr is redirected to the output pane in
VSCode - select "Rust Language Server" from the drop down box ("Rust Language
Server" will only show up if there is any debugging output from RLS). Do not
write to stdout, that will cause LSP errors (this means you cannot
`println`). You can enable logging using
[RUST_LOG](https://docs.rs/env_logger/) environment variable
(e.g. `RUST_LOG=rls=debug code`). For adding your own, temporary logging you may
find the `eprintln` macro useful.

Test using `cargo test`.

Testing is unfortunately minimal. There is support for regression tests, but not
many actual tests exists yet. There is significant [work to do](https://github.com/rust-lang/rls/issues/12)
before we have a comprehensive testing story.

### CLI

You can run RLS in the command line mode which is useful for debugging and
testing, especially to narrow down a bug to either the RLS or a client.

You need to run it in the root directory of the project to be analyzed with the
`--cli` flag, e.g., `cargo run -- --cli`. This should initialize the RLS (which
will take some time for large projects) and then give you a `>` prompt. During
initialization RLS will print out a number of progress messages to the console
(that might hide the prompt) during which some of the commands may not work
properly. Look for the final message that will signal the end of the
initialization phase which will look something like:

```
{"jsonrpc":"2.0","method":"window/progress","params":{"done":true,"id":"progress_0","message":null,"percentage":null,"title":"Indexing"}}
```

Type `help` (or just `h`) to see the [commands available][CLI_COMMANDS]. Note
that the [positions][LSP_POSITION] in the requests and the responses are
_zero-based_ (contrary to what you'll normally see in the IDE line numbers).

[LSP_POSITION]: https://github.com/Microsoft/language-server-protocol/blob/gh-pages/specification.md#position

[CLI_COMMANDS]: https://github.com/rust-lang/rls/blob/6d99a32d888a427250ff06229b6030b7dc276eac/rls/src/cmd.rs#L390-L424

## Standard library support

The way it works is that when the libraries are built, the compiler can emit all
the data that the RLS needs. This can be read by the RLS on startup and used to
provide things like type on hover without having access to the source code for
the libraries.

The compiler gives every definition an id, and the RLS matches up these ids. In
order for the RLS to work, the id of a identifier used in the IDE and the id of
its declaration in a library must match exactly. Since ids are very unstable,
the data used by the RLS for libraries must match exactly with the crate that
your source code links with.

You need a version of the above data which exactly matches the standard
libraries you will use with your project. Rustup takes care of this for you and
is the preferred (and easiest) method for installing this data. If you want to
use the RLS with a Rust compiler/libraries you have built yourself, then you'll
need to take some extra steps.


### Install with rustup

You'll need to be using [rustup](https://www.rustup.rs/) to manage your Rust
compiler toolchains. The RLS does not yet support cross-compilation - your
compiler host and target must be exactly the same.

You must be using nightly (you need to be using nightly for the RLS to work at
the moment in any case). To install a nightly toolchain use `rustup install
nightly`. To switch to using that nightly toolchain by default use `rustup
default nightly`.

Add the RLS data component using `rustup component add rust-analysis`.

Everything should now work! You may need to restart the RLS.


### Build it yourself

When you build Rust, run it with a `RUSTC_SAVE_ANALYSIS=api` environment variable, e.g. with:

```
RUSTC_SAVE_ANALYSIS=api ./x.py build
```

When the build has finished, you should have a bunch of JSON data in a directory like
`~/rust1/build/x86_64-unknown-linux-gnu/stage1-std/x86_64-unknown-linux-gnu/release/deps/save-analysis`.

You need to copy all those files (should be around 16) into a new directory:
`~/rust1/build/x86_64-unknown-linux-gnu/stage2/lib/rustlib/x86_64-unknown-linux-gnu/analysis`
(assuming you are running the stage 2 compiler you just built. You'll need to
modify the root directory (`~/rust1` here) and the host triple
(`x86_64-unknown-linux-gnu` in both places)).


Finally, to run the RLS you'll need to set things up to use the newly built
compiler, something like:

```
export RUSTC="~/rust1/build/x86_64-unknown-linux-gnu/stage2/bin/rustc"
```

Either before you run the RLS, or before you run the IDE which will start the
RLS.


### Details

Rustup (or you, manually) will install the rls data (which is a bunch of json
files) into `$SYSROOT/lib/rustlib/$TARGET_TRIPLE/analysis`, where `$SYSROOT` is
your Rust sysroot, this can be found using `rustc --print=sysroot`.
`$TARGET_TRIPLE` is the triple which defines the compilation target. Since the
RLS currently does not support cross-compilation, this must match your host
triple. It will look something like `x86_64-unknown-linux-gnu`.

For example, on my system RLS data is installed at:

```
/home/ncameron/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/analysis
```

This data is only for the standard libraries, project-specific data is stored
inside your project's target directory.


## Implementation overview

The goal of the RLS project is to provide an awesome IDE experience *now*. That
means not waiting for incremental compilation support in the compiler. However,
Rust is a somewhat complex language to analyze and providing precise and
complete information about programs requires using the compiler.

The RLS has two data sources - the compiler and Racer. The compiler is always
right, and always precise. But can sometimes be too slow for IDEs. Racer is
nearly always fast, but can't handle some constructs (e.g., macros) or can only
handle them with limited precision (e.g., complex generic types).

The RLS tries to provide data using the compiler. It sets a time budget and
queries both the compiler and Racer. If the compiler completes within the time
budget, we use that data. If not, we use Racer's data.

We link both Racer and the compiler into the RLS, so we don't need to shell out
to either (though see notes on the build process below). We also customise our
use of the compiler (via standard APIs) so that we can read modified files
directly from memory without saving them to disk.

### Building

The RLS tracks changes to files, and keeps the changed file in memory (i.e., the
RLS does not need the IDE to save a file before providing data). These changed
files are tracked by the 'Virtual File System' (which is a bit of a grandiose
name for a pretty simple file cache at the moment, but I expect this area to
grow significantly in the future). The VFS is in a [separate
-crate](https://github.com/nrc/rls-vfs).

We want to start building before the user needs information (it would be too
slow to start a build when data is requested). However, we don't want to start a
build on every keystroke (this would be too heavy on user resources). Nor is
there any point starting multiple builds when we would throw away the data from
some of them. We therefore try to queue up and coalesce builds. This is further
documented in [src/build.rs](src/build.rs).

When we do start a build, we may also need to build dependent crates. We
therefore do a full `cargo build`. However, we do not compile the last crate
(the one the user is editing in the IDE). We only run Cargo to get a command
line to build that crate. Furthermore, we cache that command line, so for most
builds (where we don't need to build dependent crates, and where we can be
reasonably sure they haven't changed since a previous build) we don't run Cargo
at all.

The command line we got from Cargo, we chop up and feed to the in-process
compiler. We then collect error messages and analysis data in JSON format
(although this is inefficient and [should
change](https://github.com/rust-lang/rls/issues/25)).

### Analysis data

From the compiler, we get a serialized dump of its analysis data (from name
resolution and type checking). We combine data from all crates and the standard
libraries and combine this into an index for the whole project. We cross-
reference and store this data in HashMaps and use it to look up data for the
IDE.

Reading, processing, and storing the analysis data is handled by the
[rls-analysis crate](https://github.com/nrc/rls-analysis)

### Communicating with IDEs

The RLS communicates with IDEs via
the [Language Server protocol](https://github.com/Microsoft/language-server-protocol/blob/master/protocol.md).

The LS protocol uses JSON sent over stdin/stdout. The JSON is rather dynamic -
we can't make structs to easily map to many of the protocol objects. The client
sends commands and notifications to the RLS. Commands must get a reply,
notifications do not. Usually the structure of the reply is dictated by the
protocol spec. The RLS can also send notifications to the client. So for a long
running task (such as a build), the RLS will reply quickly to acknowledge the
request, then send a message later with the result of the task.

Associating requests with replies is done using an id which must be handled by
the RLS.


### Extensions to the Language Server Protocol

The RLS uses some custom extensions to the Language Server Protocol.
These are all sent from the RLS to an LSP client and are only used to
improve the user experience by showing progress indicators.

* `window/progress`: notification, `title: "Building"`. Sent before build starts.
* `window/progress`: notification with `title: "Building"`, repeated for each compile target.
  * When total amount of work is not known, has field `message` set to the current crate name.
  * When total amount of work is known, has field `percentage` set to how much of build has started.
* `window/progress`: notification, `title: "Building"`, `"done": true`. Sent when build ends.
* `window/progress`: notification, `title: "Indexing"`. Sent before analysis of build starts.
* ... standard LSP `publishDiagnostics`
* `window/progress`: notification, `title: "Indexing"`, `"done": true`. Sent when analysis ends.
