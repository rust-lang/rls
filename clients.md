# Implementing clients

A short guide to implementing RLS support in your favourite editor.

Typically you will need to implement an extension or plugin in whatever format
and language your editor requires. There are two main cases - where there is
existing LSP support and where there is not. The former case is much easier,
luckily many editors now support the LSP either natively or with an extension.

If there is LSP support then you can get a pretty good 'out of the box' experience with the RLS - you'll get key features like code completion and renaming. However, it is a sub-optimal user experience. Compared to full support in an editor, you miss out on:

* discoverability and ease of setup
  - no easy to install Rust extension
  - nowhere to track bugs
  - user has to manage the RLS (installation, update, location, etc.)
* UX rough edges
  - e.g., no spinner to indicate RLS is working
* bugs
  - there are likely bugs in both the editor's client implementation and the RLS which are not found in other use cases
  - or mismatches in under-specified parts of the LSP
  - testing is required to find such issues
* extensions to the basic LSP protocol
  - the RLS supports additional refactorings and searches as extensions to the base LSP


## Preliminaries

Check the [tracking issue](https://github.com/rust-lang/rls/issues/87)
to see if support already exists or is in development. If not, comment there to
let us know you are starting work. If you would like, open an issue dedicated to
your editor, if one doesn't exist already. You should glance at
[issues with the clients label](https://github.com/rust-lang/rls/issues?q=is%3Aopen+is%3Aissue+label%3Aclients).

If there are things that can be fixed on the RLS side, please submit a PR or
file an issue.

Find out about the editor's extension ecosystem - get in touch with the
community, find out if there is LSP support, find support channels, etc.


## Where there is existing LSP support

If your editor has LSP support, then getting up and running is pretty easy. You
need a way to run the RLS and point the editor's LSP client at it. Hopefully
that is only a few lines of code. The next step is to ensure that the RLS gets
re-started after a crash - the LSP client may or may not do this automatically
(VSCode will do this five times before stopping).

Once you have this basic support in place, the hard work begins:

* Implement [extensions to the protocol](https://github.com/rust-lang/rls/blob/master/contributing.md#extensions-to-the-language-server-protocol)
* Client-side configuration.
  - You'll need to send the `workspace/didChangeConfiguration` notification when
    configuration changes.
  - For the config options, see [config.rs](https://github.com/rust-lang/rls/blob/master/src/config.rs#L99-L117)
* Check for and install the RLS
  - you should use Rustup
  - you should check if the RLS (`rls`) is installed, and if not, install it and the `rust-analysis` and `rust-src` components
  - you should provide a way to update the RLS component
* Client-side features
  - e.g., code snippets, build tasks, syntax highlighting
* Testing
* Ensure integration with existing Rust features
  - e.g., syntax highlighting
  - ideally users should only need one extension
* 'Marketing'
  - because we want people to actually use the extension
  - documentation - users need to know how to install and use the extension
  - keep us informed about status so we can advertise it appropriately
  - keep the RLS website updated
  - submit the extension to the editor package manager or marketplace


## Where there is no LSP support

If your editor has no existing LSP support, you'll need to do all the above plus
implement (parts of) the LSP. This is a fair amount of work, but probably not as
bad as it sounds. The LSP is a fairly simple JSON over stdio protocol. The
interesting bit is tying the client end of the protocol to functionality in your
editor.


### Required message support

The RLS currently requires support for the following messages. Note that we
often don't use anywhere near all the options, so even with this subset, you
don't need to implement everything.

Notifications:

* `exit`
* `initialized`
* `textDocument/didOpen`
* `textDocument/didChange`
* `textDocument/didSave`
* `workspace/didChangeConfiguration`
* `workspace/didChangeWatchedFiles`
* `cancel`

Requests:

* `shutdown`
* `initialize`
* `textDocument/definition`
* `textDocument/references`
* `textDocument/completion`
* `completionItem/resolve`
* `textDocument/rename`
* `textDocument/documentHighlight`
* `workspace/executeCommand`
* `textDocument/codeAction`
* `textDocument/documentSymbol`
* `textDocument/formatting`
* `textDocument/rangeFormatting`
* `textDocument/hover`
* `workspace/symbol`

From Server to client:

* `workspace/applyEdit`
* `client/registerCapability`
* `client/unregisterCapability`

The RLS also uses some [custom messages](https://github.com/rust-lang/rls/blob/master/contributing.md#extensions-to-the-language-server-protocol).


## Resources

* [LSP spec](https://microsoft.github.io/language-server-protocol/specification)
* [contributing.md](contributing.md) - overview of the RLS and how to build, test, etc.
* [VSCode reference implementation](https://github.com/rust-lang/rls-vscode) - an example of what client support looks like
* [Tracking issue](https://github.com/rust-lang/rls/issues/87)


## Getting help

We're happy to help however we can. The best way to get help is either to
leave a comment on an issue in this repo, or to ping me (nrc) in #rust-dev-tools
on IRC.
