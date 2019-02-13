//! benchmark which uses libstd/path.rs
//! make sure rust-src installed before running this bench

#![feature(test)]
extern crate rls_span;
extern crate rls_vfs;
extern crate test;

use rls_span::{Column, Position, Row, Span};
use rls_vfs::{Change, VfsSpan};
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

struct EmptyUserData;
type Vfs = rls_vfs::Vfs<EmptyUserData>;

fn std_path() -> PathBuf {
    let mut cmd = Command::new("rustc");
    cmd.args(&["--print", "sysroot"]);
    let op = cmd.output().unwrap();
    let sysroot = Path::new(::std::str::from_utf8(&op.stdout).unwrap().trim());
    sysroot.join("lib/rustlib/src/rust/src").to_owned()
}

fn add_file(vfs: &Vfs, path: &Path) {
    let mut buf = String::new();
    let mut file = fs::File::open(path).unwrap();
    file.read_to_string(&mut buf).unwrap();
    let change = Change::AddFile { file: path.to_owned(), text: buf };
    vfs.on_changes(&[change]).unwrap();
}

fn make_change_(path: &Path, start_line: usize, interval: usize) -> Change {
    const LEN: usize = 10;
    let txt = unsafe { std::str::from_utf8_unchecked(&[b' '; 100]) };
    let start =
        Position::new(Row::new_zero_indexed(start_line as u32), Column::new_zero_indexed(0));
    let end = Position::new(
        Row::new_zero_indexed((start_line + interval) as u32),
        Column::new_zero_indexed(0),
    );
    let buf = (0..LEN).map(|_| txt.to_owned() + "\n").collect::<String>();
    Change::ReplaceText {
        span: VfsSpan::from_usv(Span::from_positions(start, end, path), None),
        text: buf,
    }
}

fn make_replace(path: &Path, start_line: usize) -> Change {
    make_change_(path, start_line, 10)
}

fn make_insertion(path: &Path, start_line: usize) -> Change {
    make_change_(path, start_line, 1)
}

fn prepare() -> (Vfs, PathBuf) {
    let vfs = Vfs::new();
    // path.rs is very long(about 4100 lines) so let's use it
    let lib = std_path().join("libstd/path.rs");
    println!("{:?}", lib);
    add_file(&vfs, &lib);
    (vfs, lib)
}

#[bench]
fn replace_front(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_replace(&lib, 0);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}

#[bench]
fn replace_mid(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_replace(&lib, 2000);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}

#[bench]
fn replace_tale(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_replace(&lib, 4000);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}

#[bench]
fn insert_front(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_insertion(&lib, 0);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}

#[bench]
fn insert_mid(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_insertion(&lib, 2000);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}

#[bench]
fn insert_tale(b: &mut test::Bencher) {
    let (vfs, lib) = prepare();
    b.iter(|| {
        for _ in 0..10 {
            let change = make_insertion(&lib, 4000);
            vfs.on_changes(&[change]).unwrap();
        }
    })
}
