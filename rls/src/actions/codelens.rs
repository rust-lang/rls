use crate::lsp_data::*;
use crate::actions::InitActionContext;
use rls_vfs::FileContents;
use regex::Regex;
use log::error;

fn offset_to_position(text: &str, offset: usize) -> Option<Position> {
    if offset > text.len() {
        return None;
    }
    let mut line = 0u64;
    let mut character = 0u64;
    let mut count = 0;
    for c in text.chars() {
        if count >= offset {
            return Some(Position::new(line, character));
        }
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
        count += 1;
    }
    None
}

pub fn collect_declaration_typings(ctx: &InitActionContext, params: &TextDocumentIdentifier) -> Vec<CodeLens> {
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

    let let_regex = Regex::new(r"let(\s+mut)?\s+([^( ]+)").unwrap();

    for capture in let_regex.find_iter(&text) {
        let offset = capture.end();
        if let Some(position) = offset_to_position(&text, offset.clone()) {
            let span_position = Position::new(position.line, position.character-1);
            let span = ctx.convert_pos_to_span(file.clone(), span_position);
            let typename: String = analysis.show_type(&span).unwrap_or("?".to_string());
            let command = Command {
                        title: {if capture.as_str().contains("mut") {":mut "} else {": "}}.to_string() + &typename,
                        command: "".to_string(),
                        arguments: None
            };
            let lens = CodeLens {range: Range::new(position, position), command: Some(command), data: None};
            ret.push(lens);
        }
    }
    
    ret
}