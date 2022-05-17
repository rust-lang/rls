use crate::core::{BytePos, ByteRange};

/// Type of the string
#[derive(Clone, Copy, Debug)]
enum StrStyle {
    /// normal string starts with "
    Cooked,
    /// Raw(n) => raw string started with n #s
    Raw(usize),
}

#[derive(Clone, Copy)]
enum State {
    Code,
    Comment,
    CommentBlock,
    String(StrStyle),
    Char,
    Finished,
}

#[derive(Clone, Copy)]
pub struct CodeIndicesIter<'a> {
    src: &'a str,
    pos: BytePos,
    state: State,
}

impl<'a> Iterator for CodeIndicesIter<'a> {
    type Item = ByteRange;

    fn next(&mut self) -> Option<ByteRange> {
        match self.state {
            State::Code => Some(self.code()),
            State::Comment => Some(self.comment()),
            State::CommentBlock => Some(self.comment_block()),
            State::String(style) => Some(self.string(style)),
            State::Char => Some(self.char()),
            State::Finished => None,
        }
    }
}

impl<'a> CodeIndicesIter<'a> {
    fn code(&mut self) -> ByteRange {
        let mut pos = self.pos;
        let start = match self.state {
            State::String(_) | State::Char => pos.decrement(), // include quote
            _ => pos,
        };
        let src_bytes = self.src.as_bytes();
        for &b in &src_bytes[pos.0..] {
            pos = pos.increment();
            match b {
                b'/' if src_bytes.len() > pos.0 => match src_bytes[pos.0] {
                    b'/' => {
                        self.state = State::Comment;
                        self.pos = pos.increment();
                        return ByteRange::new(start, pos.decrement());
                    }
                    b'*' => {
                        self.state = State::CommentBlock;
                        self.pos = pos.increment();
                        return ByteRange::new(start, pos.decrement());
                    }
                    _ => {}
                },
                b'"' => {
                    // "
                    let str_type = self.detect_str_type(pos);
                    self.state = State::String(str_type);
                    self.pos = pos;
                    return ByteRange::new(start, pos); // include dblquotes
                }
                b'\'' => {
                    // single quotes are also used for lifetimes, so we need to
                    // be confident that this is not a lifetime.
                    // Look for backslash starting the escape, or a closing quote:
                    if src_bytes.len() > pos.increment().0
                        && (src_bytes[pos.0] == b'\\' || src_bytes[pos.increment().0] == b'\'')
                    {
                        self.state = State::Char;
                        self.pos = pos;
                        return ByteRange::new(start, pos); // include single quote
                    }
                }
                _ => {}
            }
        }

        self.state = State::Finished;
        ByteRange::new(start, self.src.len().into())
    }

    fn comment(&mut self) -> ByteRange {
        let mut pos = self.pos;
        let src_bytes = self.src.as_bytes();
        for &b in &src_bytes[pos.0..] {
            pos = pos.increment();
            if b == b'\n' {
                if pos.0 + 2 <= src_bytes.len() && src_bytes[pos.0..pos.0 + 2] == [b'/', b'/'] {
                    continue;
                }
                break;
            }
        }
        self.pos = pos;
        self.code()
    }

    fn comment_block(&mut self) -> ByteRange {
        let mut nesting_level = 0usize;
        let mut prev = b' ';
        let mut pos = self.pos;
        for &b in &self.src.as_bytes()[pos.0..] {
            pos = pos.increment();
            match b {
                b'/' if prev == b'*' => {
                    prev = b' ';
                    if nesting_level == 0 {
                        break;
                    } else {
                        nesting_level -= 1;
                    }
                }
                b'*' if prev == b'/' => {
                    prev = b' ';
                    nesting_level += 1;
                }
                _ => {
                    prev = b;
                }
            }
        }
        self.pos = pos;
        self.code()
    }

    fn string(&mut self, str_type: StrStyle) -> ByteRange {
        let src_bytes = self.src.as_bytes();
        let mut pos = self.pos;
        match str_type {
            StrStyle::Raw(level) => {
                // raw string (e.g. br#"\"#)
                #[derive(Debug)]
                enum SharpState {
                    Sharp {
                        // number of preceding #s
                        num_sharps: usize,
                        // Position of last "
                        quote_pos: BytePos,
                    },
                    None, // No preceding "##...
                }
                let mut cur_state = SharpState::None;
                let mut end_was_found = false;
                // detect corresponding end(if start is r##", "##) greedily
                for (i, &b) in src_bytes[self.pos.0..].iter().enumerate() {
                    match cur_state {
                        SharpState::Sharp {
                            num_sharps,
                            quote_pos,
                        } => {
                            cur_state = match b {
                                b'#' => SharpState::Sharp {
                                    num_sharps: num_sharps + 1,
                                    quote_pos,
                                },
                                b'"' => SharpState::Sharp {
                                    num_sharps: 0,
                                    quote_pos: BytePos(i),
                                },
                                _ => SharpState::None,
                            }
                        }
                        SharpState::None => {
                            if b == b'"' {
                                cur_state = SharpState::Sharp {
                                    num_sharps: 0,
                                    quote_pos: BytePos(i),
                                };
                            }
                        }
                    }
                    if let SharpState::Sharp {
                        num_sharps,
                        quote_pos,
                    } = cur_state
                    {
                        if num_sharps == level {
                            end_was_found = true;
                            pos += quote_pos.increment();
                            break;
                        }
                    }
                }
                if !end_was_found {
                    pos = src_bytes.len().into();
                }
            }
            StrStyle::Cooked => {
                let mut is_not_escaped = true;
                for &b in &src_bytes[pos.0..] {
                    pos = pos.increment();
                    match b {
                        b'"' if is_not_escaped => {
                            break;
                        } // "
                        b'\\' => {
                            is_not_escaped = !is_not_escaped;
                        }
                        _ => {
                            is_not_escaped = true;
                        }
                    }
                }
            }
        };
        self.pos = pos;
        self.code()
    }

    fn char(&mut self) -> ByteRange {
        let mut is_not_escaped = true;
        let mut pos = self.pos;
        for &b in &self.src.as_bytes()[pos.0..] {
            pos = pos.increment();
            match b {
                b'\'' if is_not_escaped => {
                    break;
                }
                b'\\' => {
                    is_not_escaped = !is_not_escaped;
                }
                _ => {
                    is_not_escaped = true;
                }
            }
        }
        self.pos = pos;
        self.code()
    }

    fn detect_str_type(&self, pos: BytePos) -> StrStyle {
        let src_bytes = self.src.as_bytes();
        let mut sharp = 0;
        if pos == BytePos::ZERO {
            return StrStyle::Cooked;
        }
        // now pos is at one byte after ", so we have to start at pos - 2
        for &b in src_bytes[..pos.decrement().0].iter().rev() {
            match b {
                b'#' => sharp += 1,
                b'r' => return StrStyle::Raw(sharp),
                _ => return StrStyle::Cooked,
            }
        }
        StrStyle::Cooked
    }
}

/// Returns indices of chunks of code (minus comments and string contents)
pub fn code_chunks(src: &str) -> CodeIndicesIter<'_> {
    CodeIndicesIter {
        src,
        state: State::Code,
        pos: BytePos::ZERO,
    }
}

#[cfg(test)]
mod code_indices_iter_test {
    use super::*;
    use crate::testutils::{rejustify, slice};

    #[test]
    fn removes_a_comment() {
        let src = &rejustify(
            "
    this is some code // this is a comment
    some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!("some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_consecutive_comments() {
        let src = &rejustify(
            "
    this is some code // this is a comment
    // this is more comment
    // another comment
    some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!("some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_string_contents() {
        let src = &rejustify(
            "
    this is some code \"this is a string\" more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code \"", slice(src, it.next().unwrap()));
        assert_eq!("\" more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_char_contents() {
        let src = &rejustify(
            "
    this is some code \'\"\' more code \'\\x00\' and \'\\\'\' that\'s it
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code \'", slice(src, it.next().unwrap()));
        assert_eq!("\' more code \'", slice(src, it.next().unwrap()));
        assert_eq!("\' and \'", slice(src, it.next().unwrap()));
        assert_eq!("\' that\'s it", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_string_contents_with_a_comment_in_it() {
        let src = &rejustify(
            "
    this is some code \"string with a // fake comment \" more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code \"", slice(src, it.next().unwrap()));
        assert_eq!("\" more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_a_comment_with_a_dbl_quote_in_it() {
        let src = &rejustify(
            "
    this is some code // comment with \" double quote
    some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!("some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_multiline_comment() {
        let src = &rejustify(
            "
    this is some code /* this is a
    \"multiline\" comment */some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!("some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn handles_nesting_of_block_comments() {
        let src = &rejustify(
            "
    this is some code /* nested /* block */ comment */ some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!(" some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn handles_documentation_block_comments_nested_into_block_comments() {
        let src = &rejustify(
            "
    this is some code /* nested /** documentation block */ comment */ some more code
    ",
        );
        let mut it = code_chunks(src);
        assert_eq!("this is some code ", slice(src, it.next().unwrap()));
        assert_eq!(" some more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_string_with_escaped_dblquote_in_it() {
        let src = &rejustify(
            "
    this is some code \"string with a \\\" escaped dblquote fake comment \" more code
    ",
        );

        let mut it = code_chunks(src);
        assert_eq!("this is some code \"", slice(src, it.next().unwrap()));
        assert_eq!("\" more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_raw_string_with_dangling_escape_in_it() {
        let src = &rejustify(
            "
    this is some code br\" escaped dblquote raw string \\\" more code
    ",
        );

        let mut it = code_chunks(src);
        assert_eq!("this is some code br\"", slice(src, it.next().unwrap()));
        assert_eq!("\" more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn removes_string_with_escaped_slash_before_dblquote_in_it() {
        let src = &rejustify("
    this is some code \"string with an escaped slash, so dbl quote does end the string after all \\\\\" more code
    ");

        let mut it = code_chunks(src);
        assert_eq!("this is some code \"", slice(src, it.next().unwrap()));
        assert_eq!("\" more code", slice(src, it.next().unwrap()));
    }

    #[test]
    fn handles_tricky_bit_from_str_rs() {
        let src = &rejustify(
            "
        before(\"\\\\\'\\\\\\\"\\\\\\\\\");
        more_code(\" skip me \")
    ",
        );

        for range in code_chunks(src) {
            let range = || range.to_range();
            println!("BLOB |{}|", &src[range()]);
            if src[range()].contains("skip me") {
                panic!("{}", &src[range()]);
            }
        }
    }

    #[test]
    fn removes_nested_rawstr() {
        let src = &rejustify(
            r####"
    this is some code br###""" r##""##"### more code
    "####,
        );

        let mut it = code_chunks(src);
        assert_eq!("this is some code br###\"", slice(src, it.next().unwrap()));
        assert_eq!("\"### more code", slice(src, it.next().unwrap()));
    }

}
