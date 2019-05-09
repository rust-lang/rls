
use crate::actions::InitActionContext;
use crate::lsp_data::*;
use lazy_static::lazy_static;
use log::error;
use regex::Regex;


use rls_vfs::FileContents;
use std::collections::HashMap;
fn offset_to_position(text: &str, offset: usize) -> Option<Position> {
    if offset > text.len() {
        return None;
    }
    let mut line = 0u64;
    let mut character = 0u64;
    for (count, c) in text.chars().enumerate() {
        if count >= offset {
            return Some(Position::new(line, character));
        }
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    None
}

struct Namespace {
    members: HashMap<String, String>,
    subspaces: HashMap<String, Namespace>,
    glob: bool,
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{")?;
        for (origin, alias) in self.members.iter() {
            if origin == alias {
                write!(f, "{}, ", origin)?;
            } else {
                write!(f, "{} as {}, ", origin, alias)?;
            }
        }
        for (name, namespace) in self.subspaces.iter() {
            write!(f, "{}::{}, ", name, namespace)?;
        }
        if self.glob {
            write!(f, "*")?;
        }
        write!(f, "}}")?;
        Ok(())
    }
}

impl Namespace {
    fn new() -> Self {
        Namespace {
            members: HashMap::<String, String>::new(),
            subspaces: HashMap::<String, Namespace>::new(),
            glob: false,
        }
    }

    fn get_subspace(&mut self, key: String) -> &mut Namespace {
        match self.subspaces.get_mut(&key) {
            Some(_subspace) => {}
            None => {
                let subspace = Namespace::new();
                self.subspaces.insert(key.clone(), subspace);
            }
        };
        self.subspaces.get_mut(&key).unwrap()
    }

    fn parse(&mut self, value: String) {
        let value = value.trim().to_string();
        if value.starts_with("{") {
            let mut bracket_counter = 0;
            let mut start = 1;
            for (i, character) in value.chars().enumerate() {
                match character {
                    '{' => {
                        bracket_counter += 1;
                    }
                    '}' => {
                        bracket_counter -= 1;
                        if bracket_counter == 0 {
                            self.parse(value[start..i].to_string());
                        }
                    }
                    ',' => {
                        if bracket_counter == 1 {
                            self.parse(value[start..i].to_string());
                            start = i + 1;
                        }
                    }
                    _ => {}
                };
            }
        } else if let Some(position) = value.chars().position(|character| character == ':') {
            let prefix = value[0..position].to_string();
            let suffix = value[position + 2..value.len()].to_string();
            self.get_subspace(prefix).parse(suffix);
        } else if value == "*" {
            self.glob = true;
        } else {
            lazy_static! {
                static ref AS_REGEX: Regex =
                    Regex::new(r"([A-Za-z0-9_]+)\s+as\s+([A-Za-z0-9_]+)").unwrap();
            }
            match AS_REGEX.captures(&value) {
                Some(capture) => {
                    self.members.insert(capture[1].to_string(), capture[2].to_string());
                }
                None => {
                    self.members.insert(value.clone(), value.clone());
                }
            };
        }
    }

    fn recurse_simplify(&self, typename: String) -> Option<String> {
        match typename.chars().position(|character| character == ':') {
            Some(position) => {
                let prefix = typename[0..position].to_string();
                let suffix = typename[position + 2..typename.len()].to_string();
                if let Some(subspace) = self.subspaces.get(&prefix) {
                    if let Some(simplified) = subspace.recurse_simplify(suffix.clone()) {
                        return Some(simplified);
                    }
                }
                if let Some(simplified_prefix) = self.members.get(&prefix) {
                    return Some((simplified_prefix.to_string() + "::" + &suffix).to_string());
                }
                if self.glob {
                    return Some((prefix + "::" + &suffix).to_string());
                }
                None
            }
            None => {
                if let Some(simplified) = self.members.get(&typename) {
                    return Some(simplified.to_string());
                }
                if self.glob {
                    return Some(typename);
                }
                None
            }
        }
    }

    fn simplify(&self, typename: String) -> String {
        match self.recurse_simplify(typename.clone()) {
            Some(simplified) => simplified,
            None => typename,
        }
    }
}

fn parse_uses(text: &str) -> Namespace {
    lazy_static! {
        static ref USE_REGEX: Regex = Regex::new(r"use ([^;]+);").unwrap();
    }
    let mut root = Namespace::new();
    for capture in USE_REGEX.captures_iter(text) {
        root.parse(capture[1].to_string());
    }
    root
}

fn simplify_typename(typename: String, root: &Namespace) -> String {
    lazy_static! {
        static ref SUB_TYPE: Regex = Regex::new(r"(['a-zA-Z:_]+)").unwrap();
    }
    let mut simplified = "".to_string();
    let mut captures = SUB_TYPE.captures_iter(&typename);
    if let Some(mut latest) = captures.nth(0).unwrap().get(0) {
        let subtype = latest;
        simplified += &typename[0..subtype.start()].to_string();
        simplified += &root.simplify(subtype.as_str().to_string());
        for subtype in captures {
            if let Some(subtype) = subtype.get(0) {
                let start = latest.end();
                simplified += &typename[start..subtype.start()].to_string();
                simplified += &root.simplify(subtype.as_str().to_string());
                latest = subtype;
            }
        }
        simplified += &typename[latest.end()..typename.len()];
    }
    simplified
}

#[test]
fn name_shortening_test() {
    let uses = "use std::vec::Vec;\nuse std::collections::*; use std::collections::HashSet as HashSetAlias; use std::sync::mpsc; use futures::{Futures,\nStream, mpsc::channel};";
    let root = parse_uses(uses);
    println!("Namespace Description:");
    println!("{}", root);
    println!();
    let test_cases = [
        ("std::vec::Vec", "Vec"),
        ("std::collections::HashMap", "HashMap"),
        ("std::sync::Mutex", "std::sync::Mutex"),
        ("futures::Futures", "Futures"),
        ("std::sync::mpsc::channel", "mpsc::channel"),
        ("futures::mpsc::channel", "channel"),
        ("futures::Stream", "Stream"),
        ("std::collections::HashSet", "HashSetAlias"),
    ];
    for case in test_cases.iter() {
        println!("{} -> {}", case.0, root.simplify(case.0.to_string()));
        assert_eq!(root.simplify(case.0.to_string()), case.1);
    }
}

const MULTIPLE_DECLARATIONS_REGEX: &'static str = r"(&?(mut\s+)?\w+(\s*:\s*\w+)?\s*,?\s*)+";

fn collect_declarations(text: &str) -> Vec<(Position, bool)> {
    lazy_static! {
        static ref LET_REGEX: Regex = Regex::new(r"let(\s+mut)?\s+(\w+)[ :=]").unwrap();
        static ref TUPLE_UNPACKING: Regex = Regex::new(
            &(r"(let\s+|for\s+|if let[^=]+)(\(".to_string() + MULTIPLE_DECLARATIONS_REGEX + r"\))")
        )
        .unwrap();
        static ref MATCH_CASE: Regex =
            Regex::new(&(r"\(".to_string() + MULTIPLE_DECLARATIONS_REGEX + r"\)[)\s]*=>")).unwrap();
        static ref CLOSURE_PARAMETERS: Regex =
            Regex::new(&(r"\|".to_string() + MULTIPLE_DECLARATIONS_REGEX + r"\|")).unwrap();
        static ref INNER_DECLARATION: Regex = Regex::new(r"&?\s*(mut\s+)?\w").unwrap();
    }


    let mut declarations = Vec::<(Position, bool)>::new();
    for capture in LET_REGEX.find_iter(text) {
        let offset = capture.end() - 1;
        if let Some(position) = offset_to_position(text, offset.clone()) {
            declarations.push((position, capture.as_str().contains("mut ")));
        }
    }

    let mut packed_declarations: Vec<regex::Match<'_>> = TUPLE_UNPACKING
        .captures_iter(text)
        .map(|capture| capture.get(2))
        .filter_map(|option| option)
        .collect();
    packed_declarations.extend(MATCH_CASE.find_iter(text));
    packed_declarations.extend(CLOSURE_PARAMETERS.find_iter(text));

    for matched in packed_declarations.iter() {
        let mut start = 0;
        let substring = matched.as_str();
        for (count, character) in substring.chars().enumerate() {
            if character == ',' {
                if let Some(capture) = INNER_DECLARATION.find(&substring[start..count]) {
                    let offset = matched.start() + capture.end() + start;
                    if let Some(position) = offset_to_position(text, offset) {
                        declarations.push((position, capture.as_str().starts_with("mut ")));
                    }
                }
                start = count + 1;
            }
        }
        if let Some(capture) = INNER_DECLARATION.find(&substring[start..substring.len()]) {
            let offset = matched.start() + capture.end() + start;
            if let Some(position) = offset_to_position(text, offset) {
                declarations.push((position, capture.as_str().starts_with("mut ")));
            }
        }
    }
    declarations
}

#[test]
fn declaration_collection() {
    let text = r"
    let panda = 8;
    let (_panda) = Vec::<u8>::new();
    let (test, t, te) = (0u8, 1u32, 'a');
    let closure = |hello: String, there: u8| {};";
    for (declaration, mutability) in collect_declarations(&text).iter() {
        println!(
            "{}{}, {}",
            if *mutability { "mut " } else { "" },
            declaration.line,
            declaration.character
        );
    }
}

pub fn collect_declaration_typings(
    ctx: &InitActionContext,
    params: &TextDocumentIdentifier,
) -> Vec<CodeLens> {
    let analysis = &ctx.analysis;
    let file = parse_file_path(&params.uri).unwrap();
    let mut ret = Vec::new();
    let text = match ctx.vfs.load_file(&file) {
        Ok(FileContents::Text(text)) => text,
        Ok(FileContents::Binary(_)) => return ret,
        Err(e) => {
            error!("failed to collect run actions: {}", e);
            return ret;
        }
    };
    if !text.contains("let") {
        return ret;
    }
    let declarations = collect_declarations(&text);
    let root = parse_uses(&text);
    for (position, mutable) in declarations.iter() {
        let span_position = Position::new(position.line, position.character - 1);
        let span = ctx.convert_pos_to_span(file.clone(), span_position);
        let var_name = {
            if let Ok(name) = analysis.show_name(&span) {
                name + ": "
            } else {
                "".to_string()
            }
        };
        let lens_start = *position;
        let mut lens_end = *position;
        lens_end.character += 1;
        let lens_range = Range::new(lens_start, lens_end);
        match analysis.show_type(&span) {
            Ok(typename) => {
                let typename = if typename.contains("[closure") {
                    "closure".to_string()
                } else {
                    simplify_typename(typename.replace("'<empty>", "'_"), &root)
                };
                ret.push(CodeLens {range: lens_range, command: Some(Command {
                    title: var_name + if *mutable { "mut " } else { "" } + &typename,
                    command: "".to_string(),
                    arguments: None,
                }), data:  None})
            }
            Err(e) => {
                let command = "".to_string();
                if !ctx.analysis_ready() {
                    let title = var_name
                        + &format!("Waiting for index. Edit or switch tab to refresh.");
                    ret.push(CodeLens {
                        range: lens_range,
                        command: Some(Command{title, command, arguments: None}),
                        data: None
                    })
                } else {
                    #[cfg(debug_assertions)]
                    {
                        let title = format!("Err({}) at ({}, {})", e, position.line+1, position.character+1);
                        ret.push(CodeLens {
                            range: lens_range,
                            command: Some(Command{title, command, arguments: None}),
                            data: None
                        })
                    }
                }
            },
        }
    }
    ret
}
