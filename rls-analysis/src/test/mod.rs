use crate::loader::SearchDirectory;
use crate::raw::DefKind;
use crate::{AnalysisHost, AnalysisLoader};

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, new)]
struct TestAnalysisLoader {
    path: PathBuf,
}

impl AnalysisLoader for TestAnalysisLoader {
    fn needs_hard_reload(&self, _path_prefix: &Path) -> bool {
        true
    }

    fn fresh_host(&self) -> AnalysisHost<Self> {
        AnalysisHost::new_with_loader(self.clone())
    }

    fn set_path_prefix(&mut self, _path_prefix: &Path) {}

    fn abs_path_prefix(&self) -> Option<PathBuf> {
        panic!();
    }

    fn search_directories(&self) -> Vec<SearchDirectory> {
        vec![SearchDirectory::new(self.path.clone(), None)]
    }
}

#[test]
fn doc_urls_resolve_correctly() {
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/rust-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/rust-analysis"), Path::new("test_data/rust-analysis"))
        .unwrap();

    fn assert_url_for_type<S: Into<Option<&'static str>>>(
        host: &AnalysisHost<TestAnalysisLoader>,
        type_: &str,
        qualname: S,
        url: &str,
    ) {
        let qualname = qualname.into();
        let ids = host.search_for_id(type_).unwrap();
        let defs: Vec<_> = ids
            .into_iter()
            .map(|id| host.get_def(id).unwrap())
            .filter(|def| qualname.is_none() || def.qualname == qualname.unwrap())
            .collect();
        trace!("{}: {:#?}", type_, defs);
        assert_eq!(defs.len(), 1);
        assert_eq!(host.doc_url(&defs[0].span), Ok(url.into()));
    }

    // FIXME This test cannot work for some values
    // Primitives like i64. i64 is shown with type mod but requires name "primitive".
    // All methods (instead of trait methods, see as_mut), seem to only be available for generic qualname
    // Unions like ManuallyDrop are not in the analysis file, just methods implemented for them or methods using them

    assert_url_for_type(
        &host,
        "MAIN_SEPARATOR",
        None,
        "https://doc.rust-lang.org/nightly/std/path/MAIN_SEPARATOR.v.html",
    );
    // the parent has a qualname which is not represented in the usage, the ip part
    assert_url_for_type(
        &host,
        "Ipv4Addr",
        None,
        "https://doc.rust-lang.org/nightly/std/net/ip/Ipv4Addr.t.html",
    );
    assert_url_for_type(
        &host,
        "VarError",
        None,
        "https://doc.rust-lang.org/nightly/std/env/VarError.t.html",
    );
    assert_url_for_type(
        &host,
        "NotPresent",
        None,
        "https://doc.rust-lang.org/nightly/std/env/VarError.t.html#NotPresent.v",
    );
    assert_url_for_type(
        &host,
        "Result",
        "std::thread::Result",
        "https://doc.rust-lang.org/nightly/std/thread/Result.t.html",
    );
    assert_url_for_type(
        &host,
        "args",
        "std::env::args",
        "https://doc.rust-lang.org/nightly/std/env/args.v.html",
    );
    assert_url_for_type(
        &host,
        "AsciiExt",
        None,
        "https://doc.rust-lang.org/nightly/std/ascii/AsciiExt.t.html",
    );
    assert_url_for_type(
        &host,
        "is_ascii",
        "std::ascii::AsciiExt::is_ascii",
        "https://doc.rust-lang.org/nightly/std/ascii/AsciiExt.t.html#is_ascii.v",
    );
    assert_url_for_type(
        &host,
        "status",
        "std::process::Output::status",
        "https://doc.rust-lang.org/nightly/std/process/Output.t.html#status.v",
    );
    assert_url_for_type(
        &host,
        "copy",
        "std::fs::copy",
        "https://doc.rust-lang.org/nightly/std/fs/copy.v.html",
    );
    // prelude and fs are both mod, but the parent once has a trailing / and once not
    assert_url_for_type(
        &host,
        "prelude",
        "std::io::prelude",
        "https://doc.rust-lang.org/nightly/std/io/prelude/",
    );
    assert_url_for_type(&host, "fs", "std::fs", "https://doc.rust-lang.org/nightly/std/fs/");
}

#[test]
fn smoke() {
    // Read in test data and lower it, check we don't crash.
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/rls-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/rls-analysis"), Path::new("test_data/rls-analysis")).unwrap();
}

#[test]
fn test_hello() {
    // Simple program, a somewhat thorough test that we have all the defs and refs we expect.
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/hello/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/hello"), Path::new("test_data/hello")).unwrap();

    let ids = host.search_for_id("print_hello").unwrap();
    assert_eq!(ids.len(), 1);
    let id = ids[0];
    let def = host.get_def(id).unwrap();
    assert_eq!(def.name, "print_hello");
    assert_eq!(def.kind, DefKind::Function);
    let refs = host.find_all_refs_by_id(id).unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 0);
    assert_eq!(refs[1].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[1].range.row_start.0, 6);
    let refs = host.search("print_hello").unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 0);
    assert_eq!(refs[1].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[1].range.row_start.0, 6);

    let ids = host.search_for_id("main").unwrap();
    assert_eq!(ids.len(), 1);
    let id = ids[0];
    let def = host.get_def(id).unwrap();
    assert_eq!(def.name, "main");
    assert_eq!(def.kind, DefKind::Function);
    let refs = host.find_all_refs_by_id(id).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 5);
    let refs = host.search("main").unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 5);

    let ids = host.search_for_id("name").unwrap();
    assert_eq!(ids.len(), 1);
    let id = ids[0];
    let def = host.get_def(id).unwrap();
    assert_eq!(def.name, "name");
    assert_eq!(def.kind, DefKind::Local);
    let refs = host.find_all_refs_by_id(id).unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 1);
    assert_eq!(refs[1].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[1].range.row_start.0, 2);
    let refs = host.search("name").unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[0].range.row_start.0, 1);
    assert_eq!(refs[1].file, Path::new("test_data/hello/src/main.rs"));
    assert_eq!(refs[1].range.row_start.0, 2);

    let defs = host.matching_defs("print_hello").unwrap();
    assert_eq!(defs.len(), 1);
    let hello_def = &defs[0];
    assert_eq!(hello_def.name, "print_hello");
    assert_eq!(hello_def.kind, DefKind::Function);
    assert_eq!(hello_def.span.range.row_start.0, 0);

    let defs = host.matching_defs("main").unwrap();
    assert_eq!(defs.len(), 1);
    let main_def = &defs[0];
    assert_eq!(main_def.name, "main");
    assert_eq!(main_def.kind, DefKind::Function);
    assert_eq!(main_def.span.range.row_start.0, 5);

    let defs = host.matching_defs("name").unwrap();
    assert_eq!(defs.len(), 1);
    let matching_def = &defs[0];
    assert_eq!(matching_def.name, "name");
    assert_eq!(matching_def.kind, DefKind::Local);
    assert_eq!(matching_def.span.range.row_start.0, 1);

    assert_eq!(host.matching_defs("goodbye").unwrap().len(), 0);
    assert_eq!(host.matching_defs("mäin").unwrap().len(), 0);

    let pri_matches = host.matching_defs("pri").unwrap();
    let print_hello_matches = host.matching_defs("print_hello").unwrap();
    assert_eq!(1, pri_matches.len());
    assert_eq!(1, print_hello_matches.len());
    let pri_f = &pri_matches[0];
    let print_hello_f = &print_hello_matches[0];
    assert_eq!(pri_f.name, print_hello_f.name);
    assert_eq!(pri_f.kind, print_hello_f.kind);

    let all_matches =
        host.matching_defs("").unwrap().iter().map(|d| d.name.to_owned()).collect::<HashSet<_>>();

    let expected_matches =
        ["main", "name", "print_hello"].iter().map(|&m| String::from(m)).collect::<HashSet<_>>();
    assert_eq!(all_matches, expected_matches);
}

// TODO
// check span functions
// check complex programs

#[test]
fn test_types() {
    fn assert_type(
        host: &AnalysisHost<TestAnalysisLoader>,
        name: &str,
        def_kind: DefKind,
        expect_lines: &[u32],
    ) {
        let ids = host.search_for_id(name).unwrap();
        println!("name: {}", name);
        assert_eq!(ids.len(), 1);

        let id = ids[0];
        let def = host.get_def(id).unwrap();
        assert_eq!(def.name, name);
        assert_eq!(def.kind, def_kind);

        let refs = host.find_all_refs_by_id(id).unwrap();
        assert_eq!(refs.len(), expect_lines.len());

        for (i, start) in expect_lines.iter().enumerate() {
            assert_eq!(refs[i].file, Path::new("test_data/types/src/main.rs"));
            assert_eq!(refs[i].range.row_start.0 + 1, *start);
        }
    }

    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/types/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/types"), Path::new("test_data/types")).unwrap();

    assert_type(&host, "Foo", DefKind::Struct, &[1, 6, 7, 10, 10]);
    assert_type(&host, "f", DefKind::Field, &[2, 6]);
    assert_type(&host, "main", DefKind::Function, &[5]);
    assert_type(&host, "test_binding", DefKind::Local, &[11]);
    assert_type(&host, "TEST_CONST", DefKind::Const, &[12]);
    assert_type(&host, "TEST_STATIC", DefKind::Static, &[13]);
    assert_type(&host, "test_module", DefKind::Mod, &[17]);
    assert_type(&host, "TestType", DefKind::Type, &[18]);
    assert_type(&host, "TestUnion", DefKind::Union, &[21]);
    assert_type(&host, "TestTrait", DefKind::Trait, &[25]);
    assert_type(&host, "test_method", DefKind::Method, &[26]);
    assert_type(&host, "FooEnum", DefKind::Enum, &[29]);
    assert_type(&host, "TupleVariant", DefKind::TupleVariant, &[30]);
    assert_type(&host, "StructVariant", DefKind::StructVariant, &[31]);

    let t_matches = host.matching_defs("t").unwrap();
    let t_names = t_matches.iter().map(|m| m.name.to_owned()).collect::<HashSet<_>>();
    let expected_t_names = [
        "TEST_CONST",
        "TEST_STATIC",
        "TestTrait",
        "TestType",
        "TestUnion",
        "TupleVariant",
        "test_binding",
        "test_method",
        "test_module",
    ]
    .iter()
    .map(|&n| String::from(n))
    .collect::<HashSet<_>>();

    assert_eq!(t_names, expected_t_names);

    let upper_matches = host.matching_defs("FOOENUM").unwrap();
    let lower_matches = host.matching_defs("fooenum").unwrap();
    assert_eq!(upper_matches[0].name, "FooEnum");
    assert_eq!(lower_matches[0].name, "FooEnum");
}

#[test]
fn test_child_count() {
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/types/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/types"), Path::new("test_data/types")).unwrap();

    let ids = host.search_for_id("Foo").unwrap();
    let id = ids[0];
    assert_eq!(host.for_each_child_def(id, |id, _| id).unwrap().len(), 1);
}

#[test]
fn test_self() {
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/exprs/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/exprs"), Path::new("test_data/exprs")).unwrap();

    let spans = host.search("self").unwrap();
    assert_eq!(spans.len(), 2);
    let def = host.goto_def(&spans[1]);
    assert_eq!(def.unwrap(), spans[0]);
}

#[test]
fn test_extern_fn() {
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/exprs/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/exprs"), Path::new("test_data/exprs")).unwrap();

    let spans = host.search("foo").unwrap();
    assert_eq!(spans.len(), 2);
    let def = host.goto_def(&spans[1]);
    assert_eq!(def.unwrap(), spans[0]);
}

#[test]
fn test_all_ref_unique() {
    let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
        Path::new("test_data/rename/save-analysis").to_owned(),
    ));
    host.reload(Path::new("test_data/rename"), Path::new("test_data/rename")).unwrap();

    let spans = host.search("bar").unwrap();
    assert_eq!(spans.len(), 4);
    let refs = host.find_all_refs(&spans[3], true, true);
    assert_eq!(refs.unwrap().len(), 0);

    let spans = host.search("qux").unwrap();
    assert_eq!(spans.len(), 3);
    let refs = host.find_all_refs(&spans[2], true, true);
    assert_eq!(refs.unwrap().len(), 3);
}
