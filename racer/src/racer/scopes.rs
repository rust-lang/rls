use crate::ast_types::Path as RacerPath;
#[cfg(test)]
use crate::core::{self, Coordinate};
use crate::core::{BytePos, ByteRange, CompletionType, Namespace, RangedRawSrc, Src};

use crate::util::{self, char_at};
use std::iter::Iterator;
use std::path::{Path, PathBuf};
use std::str::from_utf8;

fn find_close<'a, A>(iter: A, open: u8, close: u8, level_end: u32) -> Option<BytePos>
where
    A: Iterator<Item = &'a u8>,
{
    let mut levels = 0u32;
    for (count, &b) in iter.enumerate() {
        if b == close {
            if levels == level_end {
                return Some(count.into());
            }
            if levels == 0 {
                return None;
            }
            levels -= 1;
        } else if b == open {
            levels += 1;
        }
    }
    None
}

// expected to use with
fn find_close_with_pos<'a>(
    iter: impl Iterator<Item = (usize, &'a u8)>,
    open: u8,
    close: u8,
    level_end: u32,
) -> Option<BytePos> {
    let mut levels = 0u32;
    for (pos, &c) in iter {
        if c == close {
            if levels == level_end {
                // +1 for compatibility with find_close
                return Some(BytePos(pos).increment());
            }
            if levels == 0 {
                return None;
            }
            levels -= 1;
        } else if c == open {
            levels += 1;
        }
    }
    None
}

pub fn find_closing_paren(src: &str, pos: BytePos) -> BytePos {
    find_close(src.as_bytes()[pos.0..].iter(), b'(', b')', 0)
        .map_or(src.len().into(), |count| pos + count)
}

pub fn find_closure_scope_start(
    src: Src<'_>,
    point: BytePos,
    parentheses_open_pos: BytePos,
) -> Option<BytePos> {
    let closing_paren_pos = find_closing_paren(&src[..], point - parentheses_open_pos);
    let src_between_parent = &src[..closing_paren_pos.0];
    util::closure_valid_arg_scope(src_between_parent).map(|_| parentheses_open_pos)
}

pub fn scope_start(src: Src<'_>, point: BytePos) -> BytePos {
    let src = src.change_length(point);
    let (mut clev, mut plev) = (0u32, 0u32);
    let mut iter = src[..].as_bytes().into_iter().enumerate().rev();
    for (pos, b) in &mut iter {
        match b {
            b'{' => {
                // !!! found { earlier than (
                if clev == 0 {
                    return BytePos(pos).increment();
                }
                clev -= 1;
            }
            b'}' => clev += 1,
            b'(' => {
                // !!! found ( earlier than {
                if plev == 0 {
                    if let Some(scope_pos) =
                        find_closure_scope_start(src, point, BytePos(pos).increment())
                    {
                        return scope_pos;
                    } else {
                        break;
                    }
                }
                plev -= 1;
            }
            b')' => plev += 1,
            _ => {}
        }
    }
    // fallback: return curly_parent_open_pos
    find_close_with_pos(iter, b'}', b'{', 0).unwrap_or(BytePos::ZERO)
}

pub fn find_stmt_start(msrc: Src<'_>, point: BytePos) -> Option<BytePos> {
    let scope_start = scope_start(msrc, point);
    find_stmt_start_given_scope(msrc, point, scope_start)
}

fn find_stmt_start_given_scope(
    msrc: Src<'_>,
    point: BytePos,
    scope_start: BytePos,
) -> Option<BytePos> {
    // Iterate the scope to find the start of the statement that surrounds the point.
    debug!(
        "[find_stmt_start] now we are in scope {:?} ~ {:?}",
        scope_start, point,
    );
    msrc.shift_start(scope_start)
        .iter_stmts()
        .map(|range| range.shift(scope_start))
        .find(|range| range.contains(point))
        .map(|range| range.start)
}

/// Finds a statement start or panics.
pub fn expect_stmt_start(msrc: Src<'_>, point: BytePos) -> BytePos {
    find_stmt_start(msrc, point).expect("Statement does not have a beginning")
}

pub fn get_local_module_path(msrc: Src<'_>, point: BytePos) -> Vec<String> {
    let mut v = Vec::new();
    get_local_module_path_(msrc, point, &mut v);
    v
}

fn get_local_module_path_(msrc: Src<'_>, point: BytePos, out: &mut Vec<String>) {
    for range in msrc.iter_stmts() {
        if range.contains_exclusive(point) {
            let blob = msrc.shift_range(range);
            let start = util::strip_visibility(&blob).unwrap_or(BytePos::ZERO);
            if !blob[start.0..].starts_with("mod") {
                continue;
            }
            if let Some(newstart) = blob[start.0 + 3..].find('{') {
                let newstart = newstart + start.0 + 4;
                out.push(blob[start.0 + 3..newstart - 1].trim().to_owned());
                get_local_module_path_(
                    blob.shift_start(newstart.into()),
                    point - range.start - newstart.into(),
                    out,
                );
            }
        }
    }
}

pub fn get_module_file_from_path(
    msrc: Src<'_>,
    point: BytePos,
    parentdir: &Path,
    raw_src: RangedRawSrc,
) -> Option<PathBuf> {
    let mut iter = msrc.iter_stmts();
    while let Some(range) = iter.next() {
        let blob = &raw_src[range.to_range()];
        let start = range.start;
        if blob.starts_with("#[path ") {
            if let Some(ByteRange {
                start: _,
                end: modend,
            }) = iter.next()
            {
                if start < point && modend > point {
                    let pathstart = blob.find('"')? + 1;
                    let pathend = blob[pathstart..].find('"').unwrap();
                    let path = &blob[pathstart..pathstart + pathend];
                    debug!("found a path attribute, path = |{}|", path);
                    let filepath = parentdir.join(path);
                    if filepath.exists() {
                        return Some(filepath);
                    }
                }
            }
        }
    }
    None
}

// TODO(kngwyu): this functions shouldn't be generic
pub fn find_impl_start(msrc: Src<'_>, point: BytePos, scopestart: BytePos) -> Option<BytePos> {
    let len = point - scopestart;
    msrc.shift_start(scopestart)
        .iter_stmts()
        .find(|range| range.end > len)
        .and_then(|range| {
            let blob = msrc.shift_start(scopestart + range.start);
            if blob.starts_with("impl") || util::trim_visibility(&blob[..]).starts_with("trait") {
                Some(scopestart + range.start)
            } else {
                let newstart = blob.find('{')? + 1;
                find_impl_start(msrc, point, scopestart + range.start + newstart.into())
            }
        })
}

#[test]
fn finds_subnested_module() {
    use crate::core;
    let src = "
    pub mod foo {
        pub mod bar {
            here
        }
    }";
    let raw_src = core::RawSource::new(src.to_owned());
    let src = core::MaskedSource::new(src);
    let point = raw_src.coords_to_point(&Coordinate::new(4, 12)).unwrap();
    let v = get_local_module_path(src.as_src(), point);
    assert_eq!("foo", &v[0][..]);
    assert_eq!("bar", &v[1][..]);

    let point = raw_src.coords_to_point(&Coordinate::new(3, 8)).unwrap();
    let v = get_local_module_path(src.as_src(), point);
    assert_eq!("foo", &v[0][..]);
}

// TODO: This function can't handle use_nested_groups
pub fn split_into_context_and_completion(s: &str) -> (&str, &str, CompletionType) {
    match s
        .char_indices()
        .rev()
        .find(|&(_, c)| !util::is_ident_char(c))
    {
        Some((i, c)) => match c {
            '.' => (&s[..i], &s[(i + 1)..], CompletionType::Field),
            ':' if s.len() > 1 => (&s[..(i - 1)], &s[(i + 1)..], CompletionType::Path),
            _ => (&s[..(i + 1)], &s[(i + 1)..], CompletionType::Path),
        },
        None => ("", s, CompletionType::Path),
    }
}

/// search in reverse for the start of the current expression
/// allow . and :: to be surrounded by white chars to enable multi line call chains
pub fn get_start_of_search_expr(src: &str, point: BytePos) -> BytePos {
    #[derive(Debug)]
    enum State {
        /// In parentheses; the value inside identifies depth.
        Paren(usize),
        /// in bracket
        Bracket(usize),
        /// In a string
        StringLiteral,
        /// In char
        CharLiteral,
        StartsWithDot,
        MustEndsWithDot(usize),
        StartsWithCol(usize),
        None,
        Result(usize),
    }
    let mut ws_ok = State::None;
    for (i, c) in src.as_bytes()[..point.0].iter().enumerate().rev() {
        ws_ok = match (*c, ws_ok) {
            (b'(', State::None) => State::Result(i + 1),
            (b'(', State::Paren(1)) => State::None,
            (b'(', State::Paren(lev)) => State::Paren(lev - 1),
            (b')', State::Paren(lev)) => State::Paren(lev + 1),
            (b')', State::None) | (b')', State::StartsWithDot) => State::Paren(1),
            (b'[', State::None) => State::Result(i + 1),
            (b'[', State::Bracket(1)) => State::None,
            (b'[', State::Bracket(lev)) => State::Bracket(lev - 1),
            (b']', State::Bracket(lev)) => State::Bracket(lev + 1),
            (b']', State::StartsWithDot) => State::Bracket(1),
            (b'.', State::None) => State::StartsWithDot,
            (b'.', State::StartsWithDot) => State::Result(i + 2),
            (b'.', State::MustEndsWithDot(_)) => State::None,
            (b':', State::MustEndsWithDot(index)) => State::StartsWithCol(index),
            (b':', State::StartsWithCol(_)) => State::None,
            (b'"', State::None) | (b'"', State::StartsWithDot) => State::StringLiteral,
            (b'"', State::StringLiteral) => State::None,
            (b'?', State::StartsWithDot) => State::None,
            (b'\'', State::None) | (b'\'', State::StartsWithDot) => State::CharLiteral,
            (b'\'', State::StringLiteral) => State::StringLiteral,
            (b'\'', State::CharLiteral) => State::None,
            (_, State::CharLiteral) => State::CharLiteral,
            (_, State::StringLiteral) => State::StringLiteral,
            (_, State::StartsWithCol(index)) => State::Result(index),
            (_, State::None) if char_at(src, i).is_whitespace() => State::MustEndsWithDot(i + 1),
            (_, State::MustEndsWithDot(index)) if char_at(src, i).is_whitespace() => {
                State::MustEndsWithDot(index)
            }
            (_, State::StartsWithDot) if char_at(src, i).is_whitespace() => State::StartsWithDot,
            (_, State::MustEndsWithDot(index)) => State::Result(index),
            (_, State::None) if !util::is_search_expr_char(char_at(src, i)) => State::Result(i + 1),
            (_, State::None) => State::None,
            (_, s @ State::Paren(_)) => s,
            (_, s @ State::Bracket(_)) => s,
            (_, State::StartsWithDot) if util::is_search_expr_char(char_at(src, i)) => State::None,
            (_, State::StartsWithDot) => State::Result(i + 1),
            (_, State::Result(_)) => unreachable!(),
        };
        if let State::Result(index) = ws_ok {
            return index.into();
        }
    }
    BytePos::ZERO
}

pub fn get_start_of_pattern(src: &str, point: BytePos) -> BytePos {
    let mut levels = 0u32;
    for (i, &b) in src[..point.0].as_bytes().into_iter().enumerate().rev() {
        match b {
            b'(' => {
                if levels == 0 {
                    return BytePos(i).increment();
                }
                levels -= 1;
            }
            b')' => {
                levels += 1;
            }
            _ => {
                if levels == 0 && !util::is_pattern_char(b as char) {
                    return BytePos(i).increment();
                }
            }
        }
    }
    BytePos::ZERO
}

#[cfg(test)]
mod test_get_start_of_pattern {
    use super::{get_start_of_pattern, BytePos};
    fn get_start_of_pattern_(s: &str, u: usize) -> usize {
        get_start_of_pattern(s, BytePos(u)).0
    }
    #[test]
    fn handles_variant() {
        assert_eq!(4, get_start_of_pattern_("foo, Some(a) =>", 13));
    }

    #[test]
    fn handles_variant2() {
        assert_eq!(
            4,
            get_start_of_pattern_("bla, ast::PatTup(ref tuple_elements) => {", 36)
        );
    }
}

pub fn expand_search_expr(msrc: &str, point: BytePos) -> ByteRange {
    let start = get_start_of_search_expr(msrc, point);
    ByteRange::new(start, util::find_ident_end(msrc, point))
}

#[cfg(test)]
mod test_expand_seacrh_expr {
    use super::{expand_search_expr, BytePos};
    fn expand_search_expr_(s: &str, u: usize) -> (usize, usize) {
        let res = expand_search_expr(s, BytePos(u));
        (res.start.0, res.end.0)
    }
    #[test]
    fn finds_ident() {
        assert_eq!((0, 7), expand_search_expr_("foo.bar", 5))
    }

    #[test]
    fn ignores_bang_at_start() {
        assert_eq!((1, 4), expand_search_expr_("!foo", 1))
    }

    #[test]
    fn handles_chained_calls() {
        assert_eq!((0, 20), expand_search_expr_("yeah::blah.foo().bar", 18))
    }

    #[test]
    fn handles_inline_closures() {
        assert_eq!(
            (0, 29),
            expand_search_expr_("yeah::blah.foo(|x:foo|{}).bar", 27)
        )
    }
    #[test]
    fn handles_a_function_arg() {
        assert_eq!(
            (5, 25),
            expand_search_expr_("myfn(foo::new().baz().com)", 23)
        )
    }

    #[test]
    fn handles_macros() {
        assert_eq!((0, 9), expand_search_expr_("my_macro!()", 8))
    }

    #[test]
    fn handles_pos_at_end_of_search_str() {
        assert_eq!((0, 7), expand_search_expr_("foo.bar", 7))
    }

    #[test]
    fn handles_type_definition() {
        assert_eq!((4, 7), expand_search_expr_("x : foo", 7))
    }

    #[test]
    fn handles_ws_before_dot() {
        assert_eq!((0, 8), expand_search_expr_("foo .bar", 7))
    }

    #[test]
    fn handles_ws_after_dot() {
        assert_eq!((0, 8), expand_search_expr_("foo. bar", 7))
    }

    #[test]
    fn handles_ws_dot() {
        assert_eq!((0, 13), expand_search_expr_("foo. bar .foo", 12))
    }

    #[test]
    fn handles_let() {
        assert_eq!((8, 11), expand_search_expr_("let b = foo", 10))
    }

    #[test]
    fn handles_double_dot() {
        assert_eq!((2, 5), expand_search_expr_("..foo", 4))
    }
}

fn fill_gaps(buffer: &str, result: &mut String, start: usize, prev: usize) {
    for _ in 0..((start - prev) / buffer.len()) {
        result.push_str(buffer);
    }
    result.push_str(&buffer[..((start - prev) % buffer.len())]);
}

pub fn mask_comments(src: &str, chunks: &[ByteRange]) -> String {
    let mut result = String::with_capacity(src.len());
    let buf_byte = &[b' '; 128];
    let buffer = from_utf8(buf_byte).unwrap();
    let mut prev = BytePos::ZERO;
    for range in chunks {
        fill_gaps(buffer, &mut result, range.start.0, prev.0);
        result.push_str(&src[range.to_range()]);
        prev = range.end;
    }

    // Fill up if the comment was at the end
    if src.len() > prev.0 {
        fill_gaps(buffer, &mut result, src.len(), prev.0);
    }
    assert_eq!(src.len(), result.len());
    result
}

pub fn mask_sub_scopes(src: &str) -> String {
    let mut result = String::with_capacity(src.len());
    let buf_byte = [b' '; 128];
    let buffer = from_utf8(&buf_byte).unwrap();
    let mut levels = 0i32;
    let mut start = 0usize;
    let mut pos = 0usize;

    for &b in src.as_bytes() {
        pos += 1;
        match b {
            b'{' => {
                if levels == 0 {
                    result.push_str(&src[start..(pos)]);
                    start = pos + 1;
                }
                levels += 1;
            }
            b'}' => {
                if levels == 1 {
                    fill_gaps(buffer, &mut result, pos, start);
                    result.push_str("}");
                    start = pos;
                }
                levels -= 1;
            }
            b'\n' if levels > 0 => {
                fill_gaps(buffer, &mut result, pos, start);
                result.push('\n');
                start = pos + 1;
            }
            _ => {}
        }
    }
    if start > pos {
        start = pos;
    }
    if levels > 0 {
        fill_gaps(buffer, &mut result, pos, start);
    } else {
        result.push_str(&src[start..pos]);
    }
    result
}

pub fn end_of_next_scope(src: &str) -> Option<BytePos> {
    find_close(src.as_bytes().iter(), b'{', b'}', 1)
}

#[test]
fn test_scope_start() {
    let src = String::from(
        "
fn myfn() {
    let a = 3;
    print(a);
}
",
    );
    let src = core::MaskedSource::new(&src);
    let raw_src = core::RawSource::new(src.to_string());
    let point = raw_src.coords_to_point(&Coordinate::new(4, 10)).unwrap();
    let start = scope_start(src.as_src(), point);
    assert_eq!(start, BytePos(12));
}

#[test]
fn test_scope_start_handles_sub_scopes() {
    let src = String::from(
        "
fn myfn() {
    let a = 3;
    {
      let b = 4;
    }
    print(a);
}
",
    );
    let src = core::MaskedSource::new(&src);
    let raw_src = core::RawSource::new(src.to_string());
    let point = raw_src.coords_to_point(&Coordinate::new(7, 10)).unwrap();
    let start = scope_start(src.as_src(), point);
    assert_eq!(start, BytePos(12));
}

#[test]
fn masks_out_comments() {
    let src = String::from(
        "
this is some code
this is a line // with a comment
some more
",
    );
    let raw = core::RawSource::new(src.to_string());
    let src = core::MaskedSource::new(&src);
    assert!(src.len() == raw.len());
    // characters at the start are the same
    assert!(src.as_bytes()[5] == raw.as_bytes()[5]);
    // characters in the comments are masked
    let commentoffset = raw.coords_to_point(&Coordinate::new(3, 23)).unwrap();
    assert!(char_at(&src, commentoffset.0) == ' ');
    assert!(src.as_bytes()[commentoffset.0] != raw.as_bytes()[commentoffset.0]);
    // characters afterwards are the same
    assert!(src.as_bytes()[src.len() - 3] == raw.as_bytes()[src.len() - 3]);
}

#[test]
fn finds_end_of_struct_scope() {
    let src = "
struct foo {
   a: usize,
   blah: ~str
}
Some other junk";

    let expected = "
struct foo {
   a: usize,
   blah: ~str
}";
    let end = end_of_next_scope(src).unwrap();
    assert_eq!(expected, &src[..=end.0]);
}

/// get start of path from use statements
/// e.g. get Some(16) from "pub(crate)  use a"
pub(crate) fn use_stmt_start(line_str: &str) -> Option<BytePos> {
    let use_start = util::strip_visibility(line_str).unwrap_or(BytePos::ZERO);
    util::strip_word(&line_str[use_start.0..], "use").map(|b| b + use_start)
}

pub(crate) fn is_extern_crate(line_str: &str) -> bool {
    let extern_start = util::strip_visibility(line_str).unwrap_or(BytePos::ZERO);
    if let Some(crate_start) = util::strip_word(&line_str[extern_start.0..], "extern") {
        let crate_str = &line_str[(extern_start + crate_start).0..];
        crate_str.starts_with("crate ")
    } else {
        false
    }
}

#[inline(always)]
fn next_use_item(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut i = bytes.len();
    let mut before = b' ';
    while i > 0 {
        i -= 1;
        let cur = bytes[i];
        if before == b':' && cur == b':' {
            return Some(i);
        }
        if cur == b',' {
            while i > 0 && bytes[i] != b'{' {
                i -= 1;
            }
        }
        before = cur;
    }
    None
}

/// get path from use statement, supposing completion point is end of expr
/// e.g. "use std::collections::{hash_map,  Hash" -> P["std", "collections", "Hash"]
pub(crate) fn construct_path_from_use_tree(expr: &str) -> RacerPath {
    let mut segments = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = bytes.len();
    let mut ident_end = Some(i - 1);
    while i > 0 {
        i -= 1;
        if util::is_ident_char(bytes[i] as char) {
            if ident_end.is_none() {
                ident_end = Some(i)
            }
        } else {
            if let Some(end) = ident_end {
                segments.push(&expr[i + 1..=end]);
                ident_end = None;
            }
            if let Some(point) = next_use_item(&expr[..=i]) {
                i = point;
                continue;
            }
            break;
        }
    }
    if let Some(end) = ident_end {
        segments.push(&expr[0..=end]);
    }
    segments.reverse();
    let is_global = expr.starts_with("::");
    RacerPath::from_vec(is_global, segments)
}

/// get current statement for completion context
pub(crate) fn get_current_stmt<'c>(src: Src<'c>, pos: BytePos) -> (BytePos, String) {
    let mut scopestart = scope_start(src, pos);
    // for use statement
    if scopestart > BytePos::ZERO && src[..scopestart.0].ends_with("::{") {
        if let Some(pos) = src[..pos.0].rfind("use") {
            scopestart = scope_start(src, pos.into());
        }
    }
    let linestart = find_stmt_start_given_scope(src, pos, scopestart).unwrap_or(scopestart);
    (
        linestart,
        (&src[linestart.0..pos.0])
            .trim()
            .rsplit(';')
            .next()
            .unwrap()
            .to_owned(),
    )
}

pub(crate) fn expr_to_path(expr: &str) -> (RacerPath, Namespace) {
    let is_global = expr.starts_with("::");
    let v: Vec<_> = (if is_global { &expr[2..] } else { expr })
        .split("::")
        .collect();
    let path = RacerPath::from_vec(is_global, v);
    let namespace = if path.len() == 1 {
        Namespace::Global | Namespace::Path
    } else {
        Namespace::Path
    };
    (path, namespace)
}

pub(crate) fn is_in_struct_ctor(
    src: Src<'_>,
    stmt_start: BytePos,
    pos: BytePos,
) -> Option<ByteRange> {
    const ALLOW_SYMBOL: [u8; 5] = [b'{', b'(', b'|', b';', b','];
    const ALLOW_KEYWORDS: [&'static str; 3] = ["let", "mut", "ref"];
    const INIHIBIT_KEYWORDS: [&'static str; 2] = ["unsafe", "async"];
    if stmt_start.0 <= 3 || src.as_bytes()[stmt_start.0 - 1] != b'{' || pos <= stmt_start {
        return None;
    }
    {
        for &b in src[stmt_start.0..pos.0].as_bytes().iter().rev() {
            match b {
                b',' => break,
                b':' => return None,
                _ => continue,
            }
        }
    }
    let src = &src[..stmt_start.0 - 1];
    #[derive(Clone, Copy, Debug)]
    enum State {
        Initial,
        Name(usize),
        End,
    }
    let mut state = State::Initial;
    let mut result = None;
    let bytes = src.as_bytes();
    for (i, b) in bytes.iter().enumerate().rev() {
        match (state, *b) {
            (State::Initial, b) if util::is_whitespace_byte(b) => continue,
            (State::Initial, b) if util::is_ident_char(b.into()) => state = State::Name(i),
            (State::Initial, _) => return None,
            (State::Name(_), b) if b == b':' || util::is_ident_char(b.into()) => continue,
            (State::Name(end), b) if util::is_whitespace_byte(b) => {
                result = Some(ByteRange::new(i + 1, end + 1));
                if INIHIBIT_KEYWORDS.contains(&&src[i + 1..=end]) {
                    return None;
                }
                state = State::End;
            }
            (State::Name(end), b) if ALLOW_SYMBOL.contains(&b) => {
                result = Some(ByteRange::new(i + 1, end + 1));
                break;
            }
            (State::End, b) if util::is_ident_char(b.into()) => {
                let bytes = &bytes[..=i];
                if !ALLOW_KEYWORDS.iter().any(|s| bytes.ends_with(s.as_bytes())) {
                    return None;
                } else {
                    break;
                }
            }
            (State::End, b) if util::is_whitespace_byte(b) => continue,
            (State::End, b) if ALLOW_SYMBOL.contains(&b) => break,
            (_, _) => return None,
        }
    }
    match state {
        State::Initial => None,
        State::Name(end) => {
            if INIHIBIT_KEYWORDS.contains(&&src[0..=end]) {
                None
            } else {
                Some(ByteRange::new(0, end + 1))
            }
        }
        State::End => result,
    }
}

#[cfg(test)]
mod use_tree_test {
    use super::*;
    #[test]
    fn test_use_stmt_start() {
        assert_eq!(use_stmt_start("pub(crate)   use   some::").unwrap().0, 19);
    }

    #[test]
    fn test_is_extern_crate() {
        assert!(is_extern_crate("extern crate "));
        assert!(is_extern_crate("pub extern crate abc"));
        assert!(!is_extern_crate("pub extern crat"));
    }
    #[test]
    fn test_construct_path_from_use_tree() {
        let get_path_idents = |s| {
            let s = construct_path_from_use_tree(s);
            s.segments
                .into_iter()
                .map(|seg| seg.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            get_path_idents("std::collections::HashMa"),
            vec!["std", "collections", "HashMa"],
        );
        assert_eq!(
            get_path_idents("std::{collections::{HashMap, hash_ma"),
            vec!["std", "collections", "hash_ma"],
        );
        assert_eq!(
            get_path_idents("std::{collections::{HashMap, "),
            vec!["std", "collections", ""],
        );
        assert_eq!(
            get_path_idents("std::collections::{"),
            vec!["std", "collections", ""],
        );
        assert_eq!(
            get_path_idents("std::{collections::HashMap, sync::Arc"),
            vec!["std", "sync", "Arc"],
        );
        assert_eq!(get_path_idents("{Str1, module::Str2, Str3"), vec!["Str3"],);
    }
}

#[cfg(test)]
mod ctor_test {
    use super::{is_in_struct_ctor, scope_start};
    use crate::core::{ByteRange, MaskedSource};
    fn check(src: &str) -> Option<ByteRange> {
        let source = MaskedSource::new(src);
        let point = src.find("~").unwrap();
        let scope_start = scope_start(source.as_src(), point.into());
        is_in_struct_ctor(source.as_src(), scope_start, point.into())
    }
    #[test]
    fn first_line() {
        let src = "
    struct UserData {
        name: String,
        id: usize,
    }
    fn main() {
        UserData {
            na~
        }
    }";
        assert!(check(src).is_some())
    }
    #[test]
    fn second_line() {
        let src = r#"
    fn main() {
        UserData {
            name: "ahkj".to_owned(), 
            i~d:
        }
    }"#;
        assert!(check(src).is_some())
    }
    #[test]
    fn tuple() {
        let src = r#"
    fn main() {
        let (a,
            UserData {
                name: "ahkj".to_owned(),
                i~d:
            }
        ) = f();
    }"#;
        assert!(check(src).is_some())
    }
    #[test]
    fn expr_pos() {
        let src = r#"
    fn main() {
        UserData {
            name: ~
        }
    }"#;
        assert!(check(src).is_none())
    }
    #[test]
    fn fnarg() {
        let src = r#"
        func(UserData {
            name~
        })
    "#;
        assert!(check(src).is_some())
    }
    #[test]
    fn closure() {
        let src = r#"
        let f = || UserData {
            name~
        };
    "#;
        assert!(check(src).is_some())
    }
    #[test]
    fn unsafe_() {
        let src = r#"
        unsafe {
            name~
        }
    "#;
        assert!(check(src).is_none())
    }
}
