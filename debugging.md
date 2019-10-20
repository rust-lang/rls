# Debugging and Troubleshooting

Some tips and advice for debugging issues with the RLS. Many of these issues are
specific to the Visual Studio Code extension.

Where we mention settings, below, we usually mean Visual Studio Code's settings.
These can be set per-user and per-project, and can be found in the `File >
Preferences > Settings` menu.


## Common problems

### Missing Rustup

The only external component that the VSCode extension requires is Rustup. It
will install everything else (RLS, even Rust) itself.

You can install Rustup from [rustup.rs](https://www.rustup.rs/). The extension
should warn you if it is not present. See the extension section below for more
issues.


### Missing RLS component

### stable, beta toolchains

You might see an error like `toolchain 'stable-x86_64-unknown-linux-gnu' does not contain component 'rls' for target 'x86_64-unknown-linux-gnu'`,
however we guarantee for stable and beta toolchains to contain the `rls` component.
This might be [rustup.rs issue](https://github.com/rust-lang/rustup.rs/issues/1626).
Plese submit additional information to above issue if you'd like.
If you face this case, you may have to reinstall the toolchain.

```
$ rustup uninstall stable
$ rustup install stable
$ rustup component add rls
```

### nightly toolchain

You might see an error like `toolchain 'nightly-x86_64-unknown-linux-gnu' does not contain component 'rls' for target 'x86_64-unknown-linux-gnu'`.

This is due to a nightly release missing the RLS component. That
happens occasionally when the RLS cannot be built with the current compiler. To
work around this issue you can use an RLS from the beta or stable channels, wait
for a new nightly which does contain the RLS component, or use an older nightly
which includes the RLS component. To do the latter follow [these
instructions](https://github.com/rust-lang/rls-vscode/issues/181#issue-269383659),
then avoid `rustup update`.

### Out of date components

Run `rustup update` from the command line to make sure Rust, the RLS, and
associated data are all up to date.


### Project has both a library and a binary

The RLS can currently only work with one target at a time. By default, the RLS
works with the binary, if there is only one. You can build the library by
setting `rust.build_lib` to `true` (this is often most useful). If you have
multiple binaries, you can specify one to work with using `rust.build_bin`.

Auto-detection for some of this should be in the next release.


### Opening a Rust file outside of a project

Before opening a Rust file, the RLS needs to be aware of the whole crate. That
means you need to have opened the folder containing `Cargo.toml` in VSCode
before opening a Rust file (which triggers loading the Rust extension).


### Information for paths

If you have a path such as `foo::bar::baz`, the RLS only has information for the
last segment of the path. That means you can only 'goto def' or get type
information on hover for `baz`, not for `foo` or `bar`. This is a limitation in
the Rust compiler, but should be addressed at some point.


### Tests, examples

The RLS currently only works with the main part of a crate. It does not work
with the tests or examples folders.

The RLS can give information about unit tests, you need to set `rust.cfg_test`
to `true` (note that this will cause a lot of 'unused code' warnings, which is
why it is off by default).


### Stale data

Stale data can often trip up or slow down the RLS. It can be  worth running `cargo
clean` and/or deleting the entire `target` directory for your crate. You'll need
to restart the extension after doing this to get a proper rebuild.

It is also possible (but rarer) that Rustup gets into a bad place with stale
data. You can reinstall rustup and/or delete its cache (in `~/.rustup`) to try
and solve this.


### Deprecated environment variables

If you were using early versions of the RLS and extension, you might have used
`RLS_PATH` or `RLS_ROOT` env vars. These can cause issues now, so remove them
from your environment (this won't be necessary with the next release of the
extension).


## Extension issues

We recommend using our [VSCode extension](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust).
Note that there are other VSCode extensions.

Ensure the extension and VSCode is up to date. You can check the latest version
of VSCode on [their website](https://code.visualstudio.com/). You can see your
version in the `Help > About` menu item. VSCode should tell you if it or your
extension are not up to date.

The extension must be able to run Rustup. If Rustup is installed, it must be in
your PATH. Note that if you run VSCode from an icon or launcher, the PATH may
not be the same as from a terminal. You can check this issue by running VSCode
from your terminal (`code` should work).


## Project issues

It can be useful to determine if a problem is with your environment or with a
project. Try running the VSCode extension with a very simple project. Use `cargo
new foo --bin` to create a new project called `foo`, open the `foo` folder in
VSCode. Add a local variable and a use of it, see if the RLS gives you the type
of the variable on hover and if you can jump to its definition.

If the above works, then you probably have project issues. If it
doesn't, then there is a problem with the environment.

If a project has a lot of dependencies, initial indexing might take a long time.
In general, initial indexing should take about the same time as a full compile of
the project (usually a little less time, but that depends).

If the primary crate of a project is large, it probably won't work well with the
RLS (too slow). Exactly what 'large' means here will depend on how fast your
machine is and how tolerant of latency you are.

Projects with Cargo workspaces will not work (for now).

Projects with non-Cargo build systems will not work (you *might* be able to make
this work with some effort, talk to nrc on Discord).

Rarely, there are problems with the RLS's build model. You can try running
`cargo check` on the command line to emulate the build model outside of the IDE.


### Crates with large data files

Some crates can have surprisingly large data files. Large data files can slow
down the RLS to the point of crashing (or appearing to crash). Check the json
files in the `target/rls/deps/save-analysis` directory. Anything over 1mb is
suspicious. You can test if this is important by deleting the json file(s) and
restarting the extension (you'd have to do this every time you do a full build,
for example after `cargo clean` or updating the toolchain).

If you find such large data files, please report an issue on this repo. We can
try to optimise the data, or blacklist the crate.


## Racer vs compiler issues

The RLS uses Racer for code completion, and the compiler for everything else
(such as type on hover). If you are getting code completion options but not type
on hover, etc., then there is probably an issue with the RLS getting data from
the compiler. If you have type on hover, but poor code completion, then it is
probably a Racer issue.

Racer and the rest of the RLS use different data sources for indexing the
standard libraries. If you have Racer problems with the standard libraries, then
it is worth checking the `rust-source` component. If Racer is working, but other
things are not, it is worth checking the `rust-analysis` component (both
components are delivered by Rustup).


## Logging

When using VSCode extension, you can view error messages and logging in the
Output window, under View > Output, in the 'Rust Language Server' channel
that can be selected in the dropdown menu on the right of the panel.

To see more info in the logs, set `RUST_LOG=rls=debug` in your environment. You
can also set `RUST_LOG=rls_analysis=debug` to see logging specific to the
data analysis. In general, these will be printed to the standard error stream
of the server.

If you are seeing crashes in the logs, you can get a backtrace by setting
`RUST_BACKTRACE=1`.

You can also dump to a file by setting `rust-client.logToFile` to `true` in the
VSCode extension. The file will be in the project root; each time you start the
extension, you'll get a new file.

You can get more info about VSCode and the extension itself by running VSCode
with `--verbose`. However, I have only rarely found this to be useful. You can
also use VSCode's debugger to debug the extension. This can be useful if the
extension hangs.

It might be useful to find the `rls` process and attach a debugger to it.
However, with an optimised build and no debug symbols, this is not likely to be
useful.


## Library issues

If you get an error like `error while loading shared libraries` while starting
up the RLS, you should try the following:

On Linux:

```
export LD_LIBRARY_PATH=$(rustc --print sysroot)/lib:$LD_LIBRARY_PATH
```

On MacOS (this might only work if SIP is disabled (depending on how you run the
RLS), you could modify the environment in the client):

```
export DYLD_LIBRARY_PATH=$(rustc --print sysroot)/lib:$DYLD_LIBRARY_PATH
```

(This should not happen if you are using Rustup, only if building and running
from source).


## Getting more help

Please feel free to [open an issue](https://github.com/rust-lang/rls/issues/new)
to discuss any problem.

If you use Discord, you can ask in #dev-tools on the Rust lang server. You can
ping nrc.
