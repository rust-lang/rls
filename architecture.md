# Rust Language Server (RLS)

## Preface
In addition to the document below, it's also worth reading @nrc's blog post [How the RLS works](https://www.ncameron.org/blog/how-the-rls-works/). While some bits have changed, the gist of it stays the same.

However, we'll try to explain more in-depth here about how RLS obtains the underlying data to drive its indexing features as the context for the upcoming IDE planning and discussion at the 2019 Rust All-Hands.

It is assumed that the reader has read the [rust-analyzer](https://github.com/rust-analyzer/rust-analyzer/blob/e0d8c86563b72e5414cf10fe16da5e88201447e2/guide.md) guide as it covers a lot of common ground.

## High-level overview

At the time of writing, at the highest level RLS compiles your package/workspace (similar to `cargo check`) and reuses resulting `rustc` internal data structures to power its indexing features.

When initialized, (unless overriden by custom build command) RLS `cargo check`s the current project and collects inter-crate [1] dependency graph along with exact crate compilation invocations, which is used later to run the compiler again itself (but in-process).

In-process compilation runs return populated internal data structures (`rls_data::Analysis`), which are further lowered and cross-referenced to expose a fairly low-level indexing API (`rls_analysis::Analysis`) to finally be consumed by the Rust Language Server in order to answer relevant LSP queries.

[1] *crate* is a single unit of compilation as compiled by `rustc`. For example, Cargo package with bin+lib has *two* crates (sometimes called *targets* by Cargo).

## Information flow (in-depth)
The current flow is as follows:
```
rustc -> librustc_save_analysis -> rls_data -> rls_analysis -> rls
```

### librustc_save_analysis

Firstly, the Rust compiler contains a special `librustc_save_analysis` crate, which contains the necessary logic to dump the current knowledge about the currently compiled crate. The main entry point is [`process_crate`](https://github.com/rust-lang/rust/blob/7164a9f151a56316a382d8bc2b15ccf373e129ca/src/librustc_save_analysis/lib.rs#L1119), which walks the post-expansion AST and [saves](https://github.com/rust-lang/rust/blob/7164a9f151a56316a382d8bc2b15ccf373e129ca/src/librustc_save_analysis/lib.rs#L1146) the collected knowledge either by [dumping to a JSON file](https://github.com/rust-lang/rust/blob/7164a9f151a56316a382d8bc2b15ccf373e129ca/src/librustc_save_analysis/lib.rs#L1074-L1090) or by [calling back with resulting data structure](https://github.com/rust-lang/rust/blob/7164a9f151a56316a382d8bc2b15ccf373e129ca/src/librustc_save_analysis/lib.rs#L1092-L1117).

### rls_data

As mentioned previously, the returned data structure is [`rls_data::Analysis`](https://github.com/rust-dev-tools/rls-data/blob/9edbe8b4947c10ef670c4723be375c6944cab640/src/lib.rs#L30-L48) inside the [`rls_data`](https://github.com/rust-dev-tools/rls-data) crate.

### rls_analysis

### rls

## Build scheduling

## I/O

### *Actually* building

### VFS