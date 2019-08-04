# Changelog

## [Unreleased]
### Added
- Allow to override or disable default crate blacklist via new `crate_blacklist` setting
- Support both owned and borrowed blacklisted crate names in `rls-analysis`
- Publicly re-export `rls_analysis::raw::Crate`
### Changed
- Formatting project files now only needs project to parse and expand macros (and not type-check)
- Converted remaining crates `rls-*` to 2018 edition
### Removed
- Removed `use_crate_blacklist` setting in favour of `crate_blacklist`
## [Beta]
### Changed
- Fix spurious tests on slow disks by clearing `CARGO_TARGET_DIR` for tests
- Document `RUSTC_SHIM_ENV_VAR_NAME` purpose
- Disable `clear_env_rust_log` in CLI mode

### Fixed
- Fixed passing `--file-lines` to external Rustfmt for whole-file formatting requests ([#1497](https://github.com/rust-lang/rls/pull/1497))
- Fixed RLS when used together with Cargo pipelined build feature ([#1500](https://github.com/rust-lang/rls/pull/1500))

## [1.36.0]

### Changed
- Cleaned up and converted `rls-{analysis, span}` to 2018 edition
- Made `rls-{analysis, span}` use `serde` instead of `rustc_serialize ` by default
- Clarified how `clippy_preference` setting works in README

### Removed
- Removed support for obsolete `rustDocument/{beginBuild,diagnostics{Begin,End}}` LSP messages

### Fixed
- Fixed destructive formatting edits due to miscalculated newlines in diffs ([#1455](https://github.com/rust-lang/rls/pull/1455))

[Unreleased]: https://github.com/rust-lang/rls/compare/beta...HEAD
[Beta]: https://github.com/rust-lang/rls/compare/1.36.0...beta
[1.36.0]: https://github.com/rust-lang/rls/compare/1.35.0...1.36.0
