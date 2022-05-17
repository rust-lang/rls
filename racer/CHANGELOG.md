Change Log
==========

All notable changes to this project will be documented in this file. This
project adheres to [Semantic Versioning](https://semver.org/).

# 2.1.37
- Bump rustc-ap-* version to 677.0
- Account for new standard library source directory layout

# 2.1.37
- Bump rustc-ap-* version to 671.0

# 2.1.36
- Bump rustc-ap-* version to 669.0

# 2.1.35
- Bump rustc-ap-* version to 664.0

# 2.1.34
- Bump rustc-ap-* version to 659.0
- Fix submodule search (#1107)

# 2.1.33
- Bump rustc-ap-* version to 654.0

# 2.1.32
- Bump rustc-ap-* version to 651.0

# 2.1.31
- Bump rustc-ap-* version to 642.0

# 2.1.30
- Support for union(#1086)

# 2.1.29
- Support async/await syntax(#1083, #1085)

# 2.1.28
- Update the version of rustc-ap-syntax

# 2.1.27
- Update the version of rustc-ap-syntax

# 2.1.26
- Update the version of rustc-ap-syntax

# 2.1.25
- Update the version of rustc-ap-syntax

# 2.1.24
- Rust 2018 (#1051)
- Update the version of rustc-ap-syntax

# 2.1.22
- Fix completion for `super::super::...`(#1053)

# 2.1.20, 2.1.21
- Fix completion in testdir for Rust 2018(#1022)
- Fix enum variant completion for pub(crate) enum(#1025)

# 2.1.18, 2.1.19
- Update rustc-ap-syntax

# 2.1.17, 2.1.18
- Fix doc comment parsing(#1010)

# 2.1.15. 2.1.16
- Handle CRLF correctly(#1007)

# 2.1.14
- Completion for binary operation(#976)

# 2.1.10, 2.1.11, 2.1.12, 2.1.13
- Completion for impl trait(#985, #986)
- Completion for use as(#988)

# 2.1.8, 2.1.9
- Completion for trait objects(#972)
- Completion for simple closure return types(#973)

# 2.1.7
- Lots of refactoring(#961, #963, #965)
- Add `is_use_statement` for RLS(#965)

# 2.1.6
- Completion based on impl<T: Bound> #948
- Fix for argument completion #943
- Trait bound in where clause #937

# 2.1.5
- migrate to cargo metadata #930

# 2.1.3
- Make cargo optional for RLS #910

## 2.1.2
- Fix bug around getting `use` context #906
- Update rustc-ap-syntax to fix build in current nightly #911

## 2.1.1
- Fix coordinate bug
- Get doc string for macro #905

## 2.1.0
- Support completions for stdlib macros #902
- Support extern "~"  block #895
- Support `crate_in_paths` #891
- Fix bug of getting completion context from `use` statement #886
- Handle const unsafe fn #879
- Limit recursion depth through glob imports #875
- Enable completion based on trait bound for function args #871
- Fix bug in search_closure_args #862
- Replace cargo.rs with cargo crate #855
- Migrate over to rustc_ap_syntax #854
- Make RUST_SRC_PATH optional #808
- Refactor based on clippy #860

## 2.0.14
- Cache generic impls #839
- Cache parsed TOML file and cargo crate roots #838
- Skip `pub` keyword as a temporary fix for #624 #850
- Remove complex generic type by impl trait #848
- Fix bug for array expression #841
- Support completion for enum variants without type annotation #825
- Fix bug for raw string #822

## 2.0.13
- Fix bug for finding the start of match statement #819

## 2.0.12
- Fix bug that broke completions in previous release #807

## 2.0.11

- Use `rustup` to find libstd path even when used as library #799

## 2.0.10

- Support resolving `use as` aliases declared in multi-element `use` statements #753
- Provide suggestions for global paths in more cases #765
- Suggestions imported via `use as` statements now return their in-scope alias as the match string #767
- Add new commands for converting between points and coordinates in files #776
- Return fewer duplicate suggestions #778
- Handle cases where mod names and trait methods collide, such as `fmt` #781

## 2.0.9

- Support completion after using try operator `?` #726
- Find methods on cooked string literals #728
- Fix bug caused by closure completions feature #734
- Find static methods on enums #737
- Find doc comments on named and indexed struct fields #739
- Find `pub(restricted)` items #748

## 2.0.8

- Fix bug finding definitions where impl contains bang #717
- Find definition for closures #697
- Resolve types for tuple struct fields #722
- Resolve types for let patterns #724
- Fix completions for reference fields #723

## 2.0.7

- Fix panic with macros called `impl*` #701
- Relax semver specs

## 2.0.6

- resolve Self (e.g. in-impl function calls like Self::myfunction())
- Fix stack overflow issue on unresolvable imports :tada: #698

## 2.0.5

- Chained completions on separate lines now work #686

## 2.0.4

- Fix for find-doc not always returning full doc string #675

## 2.0.3

- Fix for recursion in certain `use foo::{self, ..}` cases #669

## 2.0.2

- Internal fixes so we can publish on crates.io

## 2.0.1

- Syntex 0.52 #643

- Fix `racer --help` bug from 2.0 refactor #662

- Support short revision identifiers for git checkout paths #664

- Handle self resolution when using `use mymod::{self, Thing}` #665

- Fix type alias resolution #666

## 2.0

- Rework public API to hide many implementation details and allow the project to
  move forward without breaking changes.

- Many fixes that didn't make it into the changelog, but we're going to work on
  that in the future!

## 1.2

- Added basic 'daemon' mode, racer process can be kept running between
  invocations

- now uses clap to parse command line options

- Adds caching of file source and code indices

- Adds an alternative 'tabbed' mode where inputs and outputs can be tab
  separated for easier parsing

- emacs and vim support split out into their own git projects [emacs-racer] and
  [vim-racer], respectively.

- Fix issue resolving some `std::*` modules in latest rust source: (rust std lib
  implicitly imports core with `#![no_std]`)

- Searches multirust overrides when locating cargo src directories

## 1.0.0 2015-07-29

- First release

[vim-racer]: https://github.com/racer-rust/vim-racer
[emacs-racer]: https://github.com/racer-rust/emacs-racer
