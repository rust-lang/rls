use std::iter::{Fuse, Iterator};

use crate::core::{BytePos, ByteRange};
use crate::scopes;
use crate::util::is_whitespace_byte;

/// An iterator which iterates statements.
/// e.g. for "let a = 5; let b = 4;" it returns "let a = 5;" and then "let b = 4;"
/// This iterator only works for comment-masked source codes.
pub struct StmtIndicesIter<'a> {
    src: &'a str,
    pos: BytePos,
    end: BytePos,
}

impl<'a> Iterator for StmtIndicesIter<'a> {
    type Item = ByteRange;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let src_bytes = self.src.as_bytes();
        let mut enddelim = b';';
        let mut bracelevel = 0isize;
        let mut parenlevel = 0isize;
        let mut bracketlevel = 0isize;
        let mut pos = self.pos;
        for &b in &src_bytes[pos.0..self.end.0] {
            match b {
                b' ' | b'\r' | b'\n' | b'\t' => {
                    pos += BytePos(1);
                }
                _ => {
                    break;
                }
            }
        }
        let start = pos;
        // test attribute   #[foo = bar]
        if pos < self.end && src_bytes[pos.0] == b'#' {
            enddelim = b']'
        };
        // iterate through the chunk, looking for stmt end
        for &b in &src_bytes[pos.0..self.end.0] {
            pos += BytePos(1);
            match b {
                b'(' => {
                    parenlevel += 1;
                }
                b')' => {
                    parenlevel -= 1;
                }
                b'[' => {
                    bracketlevel += 1;
                }
                b']' => {
                    bracketlevel -= 1;
                }
                b'{' => {
                    // if we are top level and stmt is not a 'use' or 'let' then
                    // closebrace finishes the stmt
                    if bracelevel == 0
                        && parenlevel == 0
                        && !(is_a_use_stmt(src_bytes, start, pos)
                            || is_a_let_stmt(src_bytes, start, pos))
                    {
                        enddelim = b'}';
                    }
                    bracelevel += 1;
                }
                b'}' => {
                    // have we reached the end of the scope?
                    if bracelevel == 0 {
                        self.pos = pos;
                        return None;
                    }
                    bracelevel -= 1;
                }
                b'!' => {
                    // macro if followed by at least one space or (
                    // FIXME: test with boolean 'not' expression
                    if parenlevel == 0 && bracelevel == 0 && pos < self.end && (pos - start).0 > 1 {
                        match src_bytes[pos.0] {
                            b' ' | b'\r' | b'\n' | b'\t' | b'(' => {
                                enddelim = b')';
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if parenlevel < 0
                || bracelevel < 0
                || bracketlevel < 0
                || (enddelim == b && bracelevel == 0 && parenlevel == 0 && bracketlevel == 0)
            {
                self.pos = pos;
                return Some(ByteRange::new(start, pos));
            }
        }
        if start < self.end {
            self.pos = pos;
            return Some(ByteRange::new(start, self.end));
        }
        None
    }
}

fn is_a_use_stmt(src_bytes: &[u8], start: BytePos, pos: BytePos) -> bool {
    let src = unsafe { ::std::str::from_utf8_unchecked(&src_bytes[start.0..pos.0]) };
    scopes::use_stmt_start(&src).is_some()
}

fn is_a_let_stmt(src_bytes: &[u8], start: BytePos, pos: BytePos) -> bool {
    pos.0 > 3
        && &src_bytes[start.0..start.0 + 3] == b"let"
        && is_whitespace_byte(src_bytes[start.0 + 3])
}

impl<'a> StmtIndicesIter<'a> {
    pub fn from_parts(src: &str) -> Fuse<StmtIndicesIter<'_>> {
        StmtIndicesIter {
            src,
            pos: BytePos::ZERO,
            end: BytePos(src.len()),
        }
        .fuse()
    }
}

#[cfg(test)]
mod test {
    use std::iter::Fuse;

    use crate::codecleaner;
    use crate::testutils::{rejustify, slice};

    use super::*;

    fn iter_stmts(src: &str) -> Fuse<StmtIndicesIter<'_>> {
        let idx: Vec<_> = codecleaner::code_chunks(&src).collect();
        let code = scopes::mask_comments(src, &idx);
        let code: &'static str = Box::leak(code.into_boxed_str());
        StmtIndicesIter::from_parts(code)
    }

    #[test]
    fn iterates_single_use_stmts() {
        let src = rejustify(
            "
            use std::Foo; // a comment
            use std::Bar;
        ",
        );

        let mut it = iter_stmts(src.as_ref());
        assert_eq!("use std::Foo;", slice(&src, it.next().unwrap()));
        assert_eq!("use std::Bar;", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_array_stmts() {
        let src = rejustify(
            "
            let a: [i32; 2] = [1, 2];
            let b = [[0], [1], [2]];
            let c = ([1, 2, 3])[1];
        ",
        );

        let mut it = iter_stmts(src.as_ref());
        assert_eq!("let a: [i32; 2] = [1, 2];", slice(&src, it.next().unwrap()));
        assert_eq!("let b = [[0], [1], [2]];", slice(&src, it.next().unwrap()));
        assert_eq!("let c = ([1, 2, 3])[1];", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_use_stmt_over_two_lines() {
        let src = rejustify(
            "
        use std::{Foo,
                  Bar}; // a comment
        ",
        );
        let mut it = iter_stmts(src.as_ref());
        assert_eq!(
            "use std::{Foo,
              Bar};",
            slice(&src, it.next().unwrap())
        );
    }

    #[test]
    fn iterates_use_stmt_without_the_prefix() {
        let src = rejustify(
            "
        pub use {Foo,
                 Bar}; // this is also legit apparently
        ",
        );
        let mut it = iter_stmts(src.as_ref());
        assert_eq!(
            "pub use {Foo,
             Bar};",
            slice(&src, it.next().unwrap())
        );
    }

    #[test]
    fn iterates_while_stmt() {
        let src = rejustify(
            "
            while self.pos < 3 { }
        ",
        );
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("while self.pos < 3 { }", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_lambda_arg() {
        let src = rejustify(
            "
            myfn(|n|{});
        ",
        );
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("myfn(|n|{});", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_macro() {
        let src = "
        mod foo;
        macro_rules! otry(
            ($e:expr) => (match $e { Some(e) => e, None => return })
        )
        mod bar;
        ";
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("mod foo;", slice(&src, it.next().unwrap()));
        assert_eq!(
            "macro_rules! otry(
            ($e:expr) => (match $e { Some(e) => e, None => return })
        )",
            slice(&src, it.next().unwrap())
        );
        assert_eq!("mod bar;", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_macro_invocation() {
        let src = "
            mod foo;
            local_data_key!(local_stdout: Box<Writer + Send>)  // no ';'
            mod bar;
        ";
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("mod foo;", slice(&src, it.next().unwrap()));
        assert_eq!(
            "local_data_key!(local_stdout: Box<Writer + Send>)",
            slice(&src, it.next().unwrap())
        );
        assert_eq!("mod bar;", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_if_else_stmt() {
        let src = "
            if self.pos < 3 { } else { }
        ";
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("if self.pos < 3 { }", slice(&src, it.next().unwrap()));
        assert_eq!("else { }", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_inner_scope() {
        let src = &"
        while(self.pos < 3 {
            let a = 35;
            return a + 35;  // should iterate this
        }
        {
            b = foo;       // but not this
        }
        "[29..];

        let mut it = iter_stmts(src.as_ref());

        assert_eq!("let a = 35;", slice(&src, it.next().unwrap()));
        assert_eq!("return a + 35;", slice(&src, it.next().unwrap()));
        assert_eq!(None, it.next());
    }

    #[test]
    fn iterates_module_attribute() {
        let src = rejustify(
            "
            #![license = \"BSD\"]
            #[test]
        ",
        );
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("#![license = \"BSD\"]", slice(&src, it.next().unwrap()));
        assert_eq!("#[test]", slice(&src, it.next().unwrap()));
    }

    #[test]
    fn iterates_half_open_subscope_if_is_the_last_thing() {
        let src = "
            let something = 35;
            while self.pos < 3 {
            let a = 35;
            return a + 35;  // should iterate this
        ";

        let mut it = iter_stmts(src.as_ref());
        assert_eq!("let something = 35;", slice(&src, it.next().unwrap()));
        assert_eq!(
            "while self.pos < 3 {
            let a = 35;
            return a + 35;  // should iterate this
        ",
            slice(&src, it.next().unwrap())
        );
    }

    #[test]
    fn iterates_ndarray() {
        let src = "
            let a = [[f64; 5]; 5];
            pub struct Matrix44f(pub [[f64; 4]; 4]);
        ";
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("let a = [[f64; 5]; 5];", slice(&src, it.next().unwrap()));
        assert_eq!(
            "pub struct Matrix44f(pub [[f64; 4]; 4]);",
            slice(&src, it.next().unwrap())
        );
    }

    #[test]
    #[ignore]
    fn iterates_for_struct() {
        let src = "
            let a = 5;
            for St { a, b } in iter() {
                let b = a;
            }
            while let St { a, b } = iter().next() {

            }
            if let St(a) = hoge() {

            }
        ";
        let mut it = iter_stmts(src.as_ref());
        assert_eq!("let a = 5;", slice(&src, it.next().unwrap()));
        assert_eq!(
            r"for St { a, b } in iter() {
                let b = a;
            }",
            slice(&src, it.next().unwrap())
        );
    }
}
