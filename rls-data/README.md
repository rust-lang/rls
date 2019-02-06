# RLS-data

Data structures used by the RLS and the Rust compiler.

These are used by the save-analysis functionality in the compiler
(`rustc -Zsave-analysis`). In that use, the compiler translates info in its
internal data structures to these data structures then serialises them as JSON.
Clients (such as the RLS) can use this crate when deserialising.

The data can also be passed directly from compiler to client if the compiler is
used as a library.
