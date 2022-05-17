// Small functions of utility
use std::rc::Rc;
use std::{cmp, error, fmt, path};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use crate::core::SearchType::{self, ExactMatch, StartsWith};
use crate::core::{BytePos, ByteRange, Location, LocationExt, RawSource, Session, SessionExt};

#[cfg(unix)]
pub const PATH_SEP: char = ':';
#[cfg(windows)]
pub const PATH_SEP: char = ';';

#[inline]
pub(crate) fn is_pattern_char(c: char) -> bool {
    c.is_alphanumeric() || c.is_whitespace() || (c == '_') || (c == ':') || (c == '.')
}

#[inline]
pub(crate) fn is_search_expr_char(c: char) -> bool {
    c.is_alphanumeric() || (c == '_') || (c == ':') || (c == '.')
}

#[inline]
pub(crate) fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || (c == '_') || (c == '!')
}

#[inline(always)]
pub(crate) fn is_whitespace_byte(b: u8) -> bool {
    b == b' ' || b == b'\r' || b == b'\n' || b == b'\t'
}

/// Searches for `needle` as a standalone identifier in `haystack`. To be considered a match,
/// the `needle` must occur either at the beginning of `haystack` or after a non-identifier
/// character.
pub fn txt_matches(stype: SearchType, needle: &str, haystack: &str) -> bool {
    txt_matches_with_pos(stype, needle, haystack).is_some()
}

pub fn txt_matches_with_pos(stype: SearchType, needle: &str, haystack: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    match stype {
        ExactMatch => {
            let n_len = needle.len();
            let h_len = haystack.len();
            for (n, _) in haystack.match_indices(needle) {
                if (n == 0 || !is_ident_char(char_before(haystack, n)))
                    && (n + n_len == h_len || !is_ident_char(char_at(haystack, n + n_len)))
                {
                    return Some(n);
                }
            }
        }
        StartsWith => {
            for (n, _) in haystack.match_indices(needle) {
                if n == 0 || !is_ident_char(char_before(haystack, n)) {
                    return Some(n);
                }
            }
        }
    }
    None
}

pub fn symbol_matches(stype: SearchType, searchstr: &str, candidate: &str) -> bool {
    match stype {
        ExactMatch => searchstr == candidate,
        StartsWith => candidate.starts_with(searchstr),
    }
}

pub fn find_closure(src: &str) -> Option<(ByteRange, ByteRange)> {
    let (pipe_range, _) = closure_valid_arg_scope(src)?;
    let mut chars = src
        .chars()
        .enumerate()
        .skip(pipe_range.end.0)
        .skip_while(|(_, c)| c.is_whitespace());
    let (start, start_char) = chars
        .next()
        .map(|(i, c)| (if c == '{' { i + 1 } else { i }, c))?;

    let mut clevel = if start_char == '{' { 1 } else { 0 };
    let mut plevel = 0;

    let mut last = None;
    for (i, current) in chars {
        match current {
            '{' => clevel += 1,
            '(' => plevel += 1,
            '}' => {
                clevel -= 1;
                if (clevel == 0 && start_char == '{') || (clevel == -1) {
                    last = Some(i);
                    break;
                }
            }
            ';' => {
                if start_char != '{' {
                    last = Some(i);
                    break;
                }
            }
            ')' => {
                plevel -= 1;
                if plevel == 0 {
                    last = Some(i + 1);
                }
                if plevel == -1 {
                    last = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    if let Some(last) = last {
        Some((pipe_range, ByteRange::new(BytePos(start), BytePos(last))))
    } else {
        None
    }
}

#[test]
fn test_find_closure() {
    let src = "|a, b, c| something()";
    let src2 = "|a, b, c| { something() }";
    let src3 = "let a = |a, b, c|something();";
    let src4 = "let a = |a, b, c| something().second().third();";
    let src5 = "| x: i32 | y.map(|z| z~)";
    let src6 = "| x: i32 | Struct { x };";
    let src7 = "y.map(| x: i32 | y.map(|z| z) )";
    let src8 = "|z| z)";
    let src9 = "let p = |z| something() + 5;";
    let get_range = |a, b| ByteRange::new(BytePos(a as usize), BytePos(b as usize));
    let find = |src: &str, a, off1: i32, b, off2: i32| {
        get_range(
            src.find(a).unwrap() as i32 + off1,
            src.rfind(b).unwrap() as i32 + 1 + off2,
        )
    };
    let get_pipe = |src| find(src, '|', 0, '|', 0);

    assert_eq!(
        Some((get_pipe(src), find(src, 's', 0, ')', 0))),
        find_closure(src)
    );
    assert_eq!(
        Some((get_pipe(src2), find(src2, '{', 1, '}', -1))),
        find_closure(src2)
    );
    assert_eq!(
        Some((get_pipe(src3), find(src3, 's', 0, ')', 0))),
        find_closure(src3)
    );
    assert_eq!(
        Some((get_pipe(src4), find(src4, 's', 0, ')', 0))),
        find_closure(src4)
    );
    assert_eq!(
        Some((find(src5, '|', 0, 'y', -2), find(src5, 'y', 0, ')', 0))),
        find_closure(src5)
    );
    assert_eq!(
        Some((get_pipe(src6), find(src6, 'S', 0, ';', -1))),
        find_closure(src6)
    );
    assert_eq!(
        Some((find(src7, '|', 0, 'y', -2), find(src7, '2', 4, ')', 0))),
        find_closure(src7)
    );
    assert_eq!(
        Some((get_pipe(src8), find(src8, ' ', 1, ')', 0))),
        find_closure(src8)
    );
    assert_eq!(
        Some((get_pipe(src9), find(src9, 's', 0, '5', 0))),
        find_closure(src9)
    );
}

/// Try to valid if the given scope contains a valid closure arg scope.
pub fn closure_valid_arg_scope(scope_src: &str) -> Option<(ByteRange, &str)> {
    // Try to find the left and right pipe, if one or both are not present, this is not a valid
    // closure definition
    let left_pipe = scope_src.find('|')?;
    let candidate = &scope_src[left_pipe..];
    let mut brace_level = 0;
    for (i, c) in candidate.chars().skip(1).enumerate() {
        match c {
            '{' => brace_level += 1,
            '}' => brace_level -= 1,
            '|' => {
                let right_pipe = left_pipe + 1 + i;
                // now we find right |
                if brace_level == 0 {
                    let range = ByteRange::new(left_pipe, right_pipe + 1);
                    return Some((range, &scope_src[range.to_range()]));
                }
                break;
            }
            ';' => break,
            _ => {}
        }
        if brace_level < 0 {
            break;
        }
    }
    None
}

#[test]
fn test_closure_valid_arg_scope() {
    let valid = r#"
    let a = |int, int| int * int;
"#;
    assert_eq!(
        closure_valid_arg_scope(valid),
        Some((ByteRange::new(BytePos(13), BytePos(23)), "|int, int|"))
    );

    let confusing = r#"
    match a {
        EnumA::A => match b {
            EnumB::A(u) | EnumB::B(u) => println!("u: {}", u),
        },
        EnumA::B => match b {
            EnumB::A(u) | EnumB::B(u) => println!("u: {}", u),
        },
    }
"#;
    assert_eq!(closure_valid_arg_scope(confusing), None);
}

#[test]
fn txt_matches_matches_stuff() {
    assert_eq!(true, txt_matches(ExactMatch, "Vec", "Vec"));
    assert_eq!(true, txt_matches(ExactMatch, "Vec", "use Vec"));
    assert_eq!(false, txt_matches(ExactMatch, "Vec", "use Vecä"));

    assert_eq!(true, txt_matches(StartsWith, "Vec", "Vector"));
    assert_eq!(true, txt_matches(StartsWith, "Vec", "use Vector"));
    assert_eq!(true, txt_matches(StartsWith, "Vec", "use Vec"));
    assert_eq!(false, txt_matches(StartsWith, "Vec", "use äVector"));
}

#[test]
fn txt_matches_matches_methods() {
    assert_eq!(true, txt_matches(StartsWith, "do_st", "fn do_stuff"));
    assert_eq!(true, txt_matches(StartsWith, "do_st", "pub fn do_stuff"));
    assert_eq!(
        true,
        txt_matches(StartsWith, "do_st", "pub(crate) fn do_stuff")
    );
    assert_eq!(
        true,
        txt_matches(StartsWith, "do_st", "pub(in codegen) fn do_stuff")
    );
}

/// Given a string and index, return span of identifier
///
/// `pos` is coerced to be within `s`. Note that `expand_ident` only backtracks.
/// If the provided `pos` is in the middle of an identifier, the returned
/// `(start, end)` will have `end` = `pos`.
///
/// # Examples
///
/// ```
/// extern crate racer;
///
/// let src = "let x = this_is_an_identifier;";
/// let pos = racer::Location::from(29);
/// let path = "lib.rs";
///
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, None);
///
/// session.cache_file_contents(path, src);
///
/// let expanded = racer::expand_ident(path, pos, &session).unwrap();
/// assert_eq!("this_is_an_identifier", expanded.ident());
/// ```
pub fn expand_ident<P, C>(filepath: P, cursor: C, session: &Session<'_>) -> Option<ExpandedIdent>
where
    P: AsRef<path::Path>,
    C: Into<Location>,
{
    let cursor = cursor.into();
    let indexed_source = session.load_raw_file(filepath.as_ref());
    let (start, pos) = {
        let s = &indexed_source.code[..];
        let pos = match cursor.to_point(&indexed_source) {
            Some(pos) => pos,
            None => {
                debug!("Failed to convert cursor to point");
                return None;
            }
        };

        // TODO: Would this better be an assertion ? Why are out-of-bound values getting here ?
        // They are coming from the command-line, question is, if they should be handled beforehand
        // clamp pos into allowed range
        let pos = cmp::min(s.len().into(), pos);
        let sb = &s[..pos.0];
        let mut start = pos;

        // backtrack to find start of word
        for (i, c) in sb.char_indices().rev() {
            if !is_ident_char(c) {
                break;
            }
            start = i.into();
        }

        (start, pos)
    };

    Some(ExpandedIdent {
        src: indexed_source,
        start,
        pos,
    })
}

pub struct ExpandedIdent {
    src: Rc<RawSource>,
    start: BytePos,
    pos: BytePos,
}

impl ExpandedIdent {
    pub fn ident(&self) -> &str {
        &self.src.code[self.start.0..self.pos.0]
    }

    pub fn start(&self) -> BytePos {
        self.start
    }

    pub fn pos(&self) -> BytePos {
        self.pos
    }
}

pub fn find_ident_end(s: &str, pos: BytePos) -> BytePos {
    // find end of word
    let sa = &s[pos.0..];
    for (i, c) in sa.char_indices() {
        if !is_ident_char(c) {
            return pos + i.into();
        }
    }
    s.len().into()
}

#[cfg(test)]
mod test_find_ident_end {
    use super::{find_ident_end, BytePos};
    fn find_ident_end_(s: &str, pos: usize) -> usize {
        find_ident_end(s, BytePos(pos)).0
    }
    #[test]
    fn ascii() {
        assert_eq!(5, find_ident_end_("ident", 0));
        assert_eq!(6, find_ident_end_("(ident)", 1));
        assert_eq!(17, find_ident_end_("let an_identifier = 100;", 4));
    }
    #[test]
    fn unicode() {
        assert_eq!(7, find_ident_end_("num_µs", 0));
        assert_eq!(10, find_ident_end_("ends_in_µ", 0));
    }
}

fn char_before(src: &str, i: usize) -> char {
    let mut prev = '\0';
    for (ii, ch) in src.char_indices() {
        if ii >= i {
            return prev;
        }
        prev = ch;
    }
    prev
}

#[test]
fn test_char_before() {
    assert_eq!('ä', char_before("täst", 3));
    assert_eq!('ä', char_before("täst", 2));
    assert_eq!('s', char_before("täst", 4));
    assert_eq!('t', char_before("täst", 100));
}

pub fn char_at(src: &str, i: usize) -> char {
    src[i..].chars().next().unwrap()
}

/// Error type returned from validate_rust_src_path()
#[derive(Debug, PartialEq)]
pub enum RustSrcPathError {
    Missing,
    DoesNotExist(path::PathBuf),
    NotRustSourceTree(path::PathBuf),
}

impl error::Error for RustSrcPathError {}

impl fmt::Display for RustSrcPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RustSrcPathError::Missing => write!(
                f,
                "RUST_SRC_PATH environment variable must be set to \
                 point to the src directory of a rust checkout. \
                 E.g. \"/home/foouser/src/rust/library\"  (or  \"/home/foouser/src/rust/src\" in older toolchains)"
            ),
            RustSrcPathError::DoesNotExist(ref path) => write!(
                f,
                "racer can't find the directory pointed to by the \
                 RUST_SRC_PATH variable \"{:?}\". Try using an \
                 absolute fully qualified path and make sure it \
                 points to the src directory of a rust checkout - \
                 e.g. \"/home/foouser/src/rust/library\" (or  \"/home/foouser/src/rust/src\" in older toolchains).",
                path
            ),
            RustSrcPathError::NotRustSourceTree(ref path) => write!(
                f,
                "Unable to find libstd under RUST_SRC_PATH. N.B. \
                 RUST_SRC_PATH variable needs to point to the *src* \
                 directory inside a rust checkout e.g. \
                 \"/home/foouser/src/rust/library\" (or  \"/home/foouser/src/rust/src\" in older toolchains). \
                 Current value \"{:?}\"",
                path
            ),
        }
    }
}

fn check_rust_sysroot() -> Option<path::PathBuf> {
    use std::process::Command;
    let mut cmd = Command::new("rustc");
    cmd.arg("--print").arg("sysroot");

    if let Ok(output) = cmd.output() {
        if let Ok(s) = String::from_utf8(output.stdout) {
            let sysroot = path::Path::new(s.trim());
            // See if the toolchain is sufficiently new, after the libstd
            // has been internally reorganized
            let srcpath = sysroot.join("lib/rustlib/src/rust/library");
            if srcpath.exists() {
                return Some(srcpath);
            }
            let srcpath = sysroot.join("lib/rustlib/src/rust/src");
            if srcpath.exists() {
                return Some(srcpath);
            }
        }
    }
    None
}

/// Get the path for Rust standard library source code.
/// Checks first the paths in the `RUST_SRC_PATH` environment variable.
///
/// If the environment variable is _not_ set, it checks the rust sys
/// root for the `rust-src` component.
///
/// If that isn't available, checks `/usr/local/src/rust/src` and
/// `/usr/src/rust/src` as default values.
///
/// If the Rust standard library source code cannot be found, returns
/// `Err(racer::RustSrcPathError::Missing)`.
///
/// If the path in `RUST_SRC_PATH` or the path in rust sys root is invalid,
/// returns a corresponding error. If a valid path is found, returns that path.
///
/// # Examples
///
/// ```
/// extern crate racer;
///
/// match racer::get_rust_src_path() {
///     Ok(_path) => {
///         // RUST_SRC_PATH is valid
///     },
///     Err(racer::RustSrcPathError::Missing) => {
///         // path is not set
///     },
///     Err(racer::RustSrcPathError::DoesNotExist(_path)) => {
///         // provided path doesnt point to valid file
///     },
///     Err(racer::RustSrcPathError::NotRustSourceTree(_path)) => {
///         // provided path doesn't have rustc src
///     }
/// }
/// ```
pub fn get_rust_src_path() -> Result<path::PathBuf, RustSrcPathError> {
    use std::env;

    debug!("Getting rust source path. Trying env var RUST_SRC_PATH.");

    if let Ok(ref srcpaths) = env::var("RUST_SRC_PATH") {
        if !srcpaths.is_empty() {
            if let Some(path) = srcpaths.split(PATH_SEP).next() {
                return validate_rust_src_path(path::PathBuf::from(path));
            }
        }
    };

    debug!("Nope. Trying rustc --print sysroot and appending lib/rustlib/src/rust/{{src, library}} to that.");

    if let Some(path) = check_rust_sysroot() {
        return validate_rust_src_path(path);
    };

    debug!("Nope. Trying default paths: /usr/local/src/rust/src and /usr/src/rust/src");

    let default_paths = ["/usr/local/src/rust/src", "/usr/src/rust/src"];

    for path in &default_paths {
        if let Ok(path) = validate_rust_src_path(path::PathBuf::from(path)) {
            return Ok(path);
        }
    }

    warn!("Rust stdlib source path not found!");

    Err(RustSrcPathError::Missing)
}

fn validate_rust_src_path(path: path::PathBuf) -> Result<path::PathBuf, RustSrcPathError> {
    if !path.exists() {
        return Err(RustSrcPathError::DoesNotExist(path));
    }
    // Historically, the Rust standard library was distributed under "libstd"
    // but was later renamed to "std" when the library was moved under "library/"
    // in https://github.com/rust-lang/rust/pull/73265.
    if path.join("libstd").exists() || path.join("std").join("src").exists() {
        Ok(path)
    } else {
        Err(RustSrcPathError::NotRustSourceTree(path.join("libstd")))
    }
}

#[cfg(test)]
lazy_static! {
    static ref TEST_SEMAPHORE: ::std::sync::Mutex<()> = Default::default();
}

#[test]
fn test_get_rust_src_path_env_ok() {
    use std::env;

    let _guard = TEST_SEMAPHORE.lock().unwrap();

    let original = env::var_os("RUST_SRC_PATH");
    if env::var_os("RUST_SRC_PATH").is_none() {
        env::set_var("RUST_SRC_PATH", check_rust_sysroot().unwrap());
    }
    let result = get_rust_src_path();

    match original {
        Some(path) => env::set_var("RUST_SRC_PATH", path),
        None => env::remove_var("RUST_SRC_PATH"),
    }
    assert!(result.is_ok());
}

#[test]
fn test_get_rust_src_path_does_not_exist() {
    use std::env;

    let _guard = TEST_SEMAPHORE.lock().unwrap();

    let original = env::var_os("RUST_SRC_PATH");
    env::set_var("RUST_SRC_PATH", "test_path");
    let result = get_rust_src_path();

    match original {
        Some(path) => env::set_var("RUST_SRC_PATH", path),
        None => env::remove_var("RUST_SRC_PATH"),
    }

    assert_eq!(
        Err(RustSrcPathError::DoesNotExist(path::PathBuf::from(
            "test_path"
        ))),
        result
    );
}

#[test]
fn test_get_rust_src_path_not_rust_source_tree() {
    use std::env;

    let _guard = TEST_SEMAPHORE.lock().unwrap();

    let original = env::var_os("RUST_SRC_PATH");

    env::set_var("RUST_SRC_PATH", "/");

    let result = get_rust_src_path();

    match original {
        Some(path) => env::set_var("RUST_SRC_PATH", path),
        None => env::remove_var("RUST_SRC_PATH"),
    }

    assert_eq!(
        Err(RustSrcPathError::NotRustSourceTree(path::PathBuf::from(
            "/libstd"
        ))),
        result
    );
}

#[test]
fn test_get_rust_src_path_missing() {
    use std::env;

    let _guard = TEST_SEMAPHORE.lock().unwrap();

    let path = env::var_os("PATH").unwrap();
    let original = env::var_os("RUST_SRC_PATH");

    env::remove_var("RUST_SRC_PATH");
    env::remove_var("PATH");

    let result = get_rust_src_path();

    env::set_var("PATH", path);
    match original {
        Some(path) => env::set_var("RUST_SRC_PATH", path),
        None => env::remove_var("RUST_SRC_PATH"),
    }

    assert_eq!(Err(RustSrcPathError::Missing), result);
}

#[test]
fn test_get_rust_src_path_rustup_ok() {
    use std::env;

    let _guard = TEST_SEMAPHORE.lock().unwrap();
    let original = env::var_os("RUST_SRC_PATH");
    env::remove_var("RUST_SRC_PATH");

    let result = get_rust_src_path();

    match original {
        Some(path) => env::set_var("RUST_SRC_PATH", path),
        None => env::remove_var("RUST_SRC_PATH"),
    }

    match result {
        Ok(_) => (),
        Err(_) => panic!(
            "Couldn't get the path via rustup! \
             Rustup and the component rust-src needs to be installed for this test to pass!"
        ),
    }
}

/// An immutable stack implemented as a linked list backed by a thread's stack.
// TODO: this implementation is fast, but if we want to run racer in multiple threads,
// we have to rewrite it using std::sync::Arc.
pub struct StackLinkedListNode<'stack, T>(Option<StackLinkedListNodeData<'stack, T>>);

struct StackLinkedListNodeData<'stack, T> {
    item: T,
    previous: &'stack StackLinkedListNode<'stack, T>,
}

impl<'stack, T> StackLinkedListNode<'stack, T> {
    /// Returns an empty node.
    pub fn empty() -> Self {
        StackLinkedListNode(None)
    }
    /// Pushes a new node on the stack. Returns the new node.
    pub fn push(&'stack self, item: T) -> Self {
        StackLinkedListNode(Some(StackLinkedListNodeData {
            item,
            previous: self,
        }))
    }
}

impl<'stack, T: PartialEq> StackLinkedListNode<'stack, T> {
    /// Check if the stack contains the specified item.
    /// Returns `true` if the item is found, or `false` if it's not found.
    pub fn contains(&self, item: &T) -> bool {
        let mut current = self;
        while let StackLinkedListNode(Some(StackLinkedListNodeData {
            item: ref current_item,
            previous,
        })) = *current
        {
            if current_item == item {
                return true;
            }
            current = previous;
        }
        false
    }
}

// don't use other than strip_visibilities or strip_unsafe
fn strip_word_impl(src: &str, allow_paren: bool) -> Option<BytePos> {
    let mut level = 0;
    for (i, &b) in src.as_bytes().into_iter().enumerate() {
        match b {
            b'(' if allow_paren => level += 1,
            b')' if allow_paren => level -= 1,
            _ if level >= 1 => (),
            // stop on the first thing that isn't whitespace
            _ if !is_whitespace_byte(b) => {
                if i == 0 {
                    break;
                }
                return Some(BytePos(i));
            }
            _ => continue,
        }
    }
    None
}

/// remove pub(crate), crate
pub(crate) fn strip_visibility(src: &str) -> Option<BytePos> {
    if src.starts_with("pub") {
        Some(strip_word_impl(&src[3..], true)? + BytePos(3))
    } else if src.starts_with("crate") {
        Some(strip_word_impl(&src[5..], false)? + BytePos(5))
    } else {
        None
    }
}

/// remove `unsafe` or other keywords
pub(crate) fn strip_word(src: &str, word: &str) -> Option<BytePos> {
    if src.starts_with(word) {
        let len = word.len();
        Some(strip_word_impl(&src[len..], false)? + BytePos(len))
    } else {
        None
    }
}

/// remove words
pub(crate) fn strip_words(src: &str, words: &[&str]) -> BytePos {
    let mut start = BytePos::ZERO;
    for word in words {
        start += strip_word(&src[start.0..], word).unwrap_or(BytePos::ZERO);
    }
    start
}

#[test]
fn test_strip_words() {
    assert_eq!(
        strip_words("const  unsafe  fn", &["const", "unsafe"]),
        BytePos(15)
    );
    assert_eq!(strip_words("unsafe  fn", &["const", "unsafe"]), BytePos(8));
    assert_eq!(strip_words("const   fn", &["const", "unsafe"]), BytePos(8));
    assert_eq!(strip_words("fn", &["const", "unsafe"]), BytePos(0));
}

/// Removes `pub(...)` from the start of a blob so that other code
/// can assess the struct/trait/fn without worrying about restricted
/// visibility.
pub(crate) fn trim_visibility(blob: &str) -> &str {
    if let Some(start) = strip_visibility(blob) {
        &blob[start.0..]
    } else {
        blob
    }
}

#[test]
fn test_trim_visibility() {
    assert_eq!(trim_visibility("pub fn"), "fn");
    assert_eq!(trim_visibility("pub(crate)   struct"), "struct");
    assert_eq!(trim_visibility("pub (in super)  const fn"), "const fn");
}

/// Checks if the completion point is in a function declaration by looking
/// to see if the second-to-last word is `fn`.
pub fn in_fn_name(line_before_point: &str) -> bool {
    // Determine if the cursor is sitting in the whitespace after typing `fn ` before
    // typing a name.
    let has_started_name = !line_before_point.ends_with(|c: char| c.is_whitespace());

    let mut words = line_before_point.split_whitespace().rev();

    // Make sure we haven't finished the name and started generics or arguments
    if has_started_name {
        if let Some(ident) = words.next() {
            if ident.chars().any(|c| !is_ident_char(c)) {
                return false;
            }
        }
    }

    words.next().map(|word| word == "fn").unwrap_or_default()
}

#[test]
fn test_in_fn_name() {
    assert!(in_fn_name("fn foo"));
    assert!(in_fn_name(" fn  foo"));
    assert!(in_fn_name("fn "));
    assert!(!in_fn_name("fn foo(b"));
    assert!(!in_fn_name("fn"));
}

/// calculate hash of string
pub fn calculate_str_hash(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[macro_export]
macro_rules! try_continue {
    ($res: expr) => {
        match ::std::ops::Try::branch($res) {
            ::std::ops::ControlFlow::Continue(o) => o,
            ::std::ops::ControlFlow::Break(_) => continue,
        }
    };
}

#[macro_export]
macro_rules! try_vec {
    ($res: expr) => {
        match ::std::ops::Try::branch($res) {
            ::std::ops::ControlFlow::Continue(o) => o,
            ::std::ops::ControlFlow::Break(_) => return Vec::new(),
        }
    };
}

pub(crate) fn gen_tuple_fields(u: usize) -> impl Iterator<Item = &'static str> {
    const NUM: [&'static str; 16] = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15",
    ];
    NUM.iter().take(::std::cmp::min(u, 16)).map(|x| *x)
}
