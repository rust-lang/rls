# rls-blacklist

Some crates produce large amounts of analysis data, to the point of slowing
down or crashing the RLS. This crate contains a list of crates to exclude from
analysis in the RLS.

See also the [related section in the RLS debugging documentation][debug].

[debug]: https://github.com/rust-lang-nursery/rls/blob/master/debugging.md#crates-with-large-data-files
