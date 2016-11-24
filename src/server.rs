// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use analysis::AnalysisHost;
use vfs::Vfs;

use build::*;
use lsp_data::*;
use actions::ActionHandler;

use std::io::{self};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::path::PathBuf;

use rust_lsp::lsp::LSPEndpoint;
use rust_lsp::lsp::LanguageServerHandling;
use rust_lsp::lsp::LSCompletable;
use rust_lsp::lsp_transport::LSPMessageReader;
use rust_lsp::lsp_transport::LSPMessageWriter;

use rust_lsp::jsonrpc::Endpoint;
use rust_lsp::jsonrpc::MethodCompletable;
use rust_lsp::jsonrpc::method_types::MethodError;
use rust_lsp::jsonrpc::service_util::MessageWriter;
use rust_lsp::jsonrpc::service_util::MessageReader;
use rust_lsp::util::core::GResult;


pub struct LsService {
    shut_down: AtomicBool,
    handler: Arc<ActionHandler>,
    output : Endpoint,
}

impl LsService {
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               build_queue: Arc<BuildQueue>,
               output: Endpoint)
               -> LsService {
        LsService {
            shut_down: AtomicBool::new(false),
            handler: Arc::new(ActionHandler::new(analysis, vfs, build_queue)),
            output: output,
        }
    }
    
    pub fn run<T : MessageReader>(self, msg_reader: &mut T) {
        let ep_out = self.output.clone();
        LSPEndpoint::run_server(msg_reader, ep_out, self);
    }
    
    fn init(&self, init: InitializeParams, completable: MethodCompletable<InitializeResult, InitializeError>) {
        
        let result = InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncKind::Incremental),
                hover_provider: Some(true),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    trigger_characters: vec![".".to_string()],
                }),
                // TODO
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec![]),
                }),
                definition_provider: Some(true),
                references_provider: Some(true),
                document_highlight_provider: Some(true),
                document_symbol_provider: Some(true),
                workspace_symbol_provider: Some(true),
                code_action_provider: Some(false),
                // TODO maybe?
                code_lens_provider: None,
                document_formatting_provider: Some(true),
                document_range_formatting_provider: Some(true),
                document_on_type_formatting_provider: None, // TODO: review this, maybe add?
                rename_provider: Some(true),
            }
        };
        
        completable.complete(Ok(result));
        
        let output = self.output.clone();
        let handler = self.handler.clone();
        let root_path = init.root_path.map(|str| PathBuf::from(str));
        
        thread::spawn(move || {
            if let Some(root_path) = root_path {
                handler.init(root_path, output);
            }
        });
    }

    pub fn error_not_available<DATA>(data : DATA) -> MethodError<DATA> {
        let msg = "Functionality not implemented.".to_string();
        MethodError::<DATA> { code : 1, message : msg, data : data }
    }
    
}

// TODO Cancel support
impl LanguageServerHandling for LsService {
    
    fn initialize(&mut self, init: InitializeParams, completable: MethodCompletable<InitializeResult, InitializeError>) {
        trace!("command(init): {:?}\n", init);
        self.init(init, completable);
    }
    fn shutdown(&mut self, _params: (), completable: LSCompletable<()>) {
        trace!("shutting down...\n");
        self.shut_down.store(true, Ordering::SeqCst);
        completable.complete(Ok(()))
    }
    fn exit(&mut self, _: ()) {
    }
    
    fn workspace_change_configuration(&mut self, _: DidChangeConfigurationParams) {
        // TODO handle me
    }
    
    fn did_open_text_document(&mut self, _: DidOpenTextDocumentParams) {
        // TODO handle me
    }
    
    fn did_change_text_document(&mut self, params: DidChangeTextDocumentParams) {
        trace!("notification(change): {:?}\n", params);
        self.handler.on_change(params, self.output.clone())
    }
    
    fn did_close_text_document(&mut self, _: DidCloseTextDocumentParams) {
        // TODO handle me
    }
    
    fn did_save_text_document(&mut self, _: DidSaveTextDocumentParams) {
        // TODO handle me
    }
    
    fn did_change_watched_files(&mut self, _: DidChangeWatchedFilesParams) {
        // TODO handle me
    }
    
    fn completion(&mut self, params: TextDocumentPositionParams, completable: LSCompletable<CompletionList>) {
        trace!("command(complete): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.complete(params));
        });
    }
    fn resolve_completion_item(&mut self, params: CompletionItem, completable: LSCompletable<CompletionItem>) {
        trace!("command(complete): {:?}\n", params);
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Ok(params));
        });
    }
    fn hover(&mut self, params: TextDocumentPositionParams, completable: LSCompletable<Hover>) {
        trace!("command(hover): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.hover(params));
        });
    }
    fn signature_help(&mut self, _params: TextDocumentPositionParams, completable: LSCompletable<SignatureHelp>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn goto_definition(&mut self, params: TextDocumentPositionParams, completable: LSCompletable<Vec<Location>>) {
        trace!("command(goto): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.goto_def(params));
        });
    }
    fn references(&mut self, params: ReferenceParams, completable: LSCompletable<Vec<Location>>) {
        trace!("command(find_all_refs): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.find_all_refs(params));
        });
    }
    fn document_highlight(&mut self, params: TextDocumentPositionParams, completable: LSCompletable<Vec<DocumentHighlight>>) {
        trace!("command(highlight): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.highlight(params));
        });
    }
    fn document_symbols(&mut self, params: DocumentSymbolParams, completable: LSCompletable<Vec<SymbolInformation>>) {
        trace!("command(goto): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.symbols(params));
        });
    }
    fn workspace_symbols(&mut self, _params: WorkspaceSymbolParams, completable: LSCompletable<Vec<SymbolInformation>>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn code_action(&mut self, _params: CodeActionParams, completable: LSCompletable<Vec<Command>>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn code_lens(&mut self, _params: CodeLensParams, completable: LSCompletable<Vec<CodeLens>>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn code_lens_resolve(&mut self, _params: CodeLens, completable: LSCompletable<CodeLens>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn document_link(&mut self, _params: DocumentLinkParams, completable: LSCompletable<Vec<DocumentLink>>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
           // FIXME todo
           completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn document_link_resolve(&mut self, _params: DocumentLink, completable: LSCompletable<DocumentLink>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
           // FIXME todo
           completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn formatting(&mut self, params: DocumentFormattingParams, completable: LSCompletable<Vec<TextEdit>>) {
        // FIXME take account of options.
        trace!("command(reformat): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.reformat(params.text_document));
        });
    }
    fn range_formatting(&mut self, params: DocumentRangeFormattingParams, completable: LSCompletable<Vec<TextEdit>>) {
        // FIXME reformats the whole file, not just a range.
        // FIXME take account of options.
        trace!("command(reformat range): {:?}", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.reformat(params.text_document));
        });
    }
    fn on_type_formatting(&mut self, _params: DocumentOnTypeFormattingParams, completable: LSCompletable<Vec<TextEdit>>) {
        let _handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(Err(Self::error_not_available(())));
        });
    }
    fn rename(&mut self, params: RenameParams, completable: LSCompletable<WorkspaceEdit>) {
        trace!("command(rename): {:?}\n", params);
        let handler = self.handler.clone();
        thread::spawn(move || {
            completable.complete(handler.rename(params));
        });
    }
}

struct StdioMsgReader {
}

impl MessageReader for StdioMsgReader {
    fn read_next(&mut self) -> GResult<String> {
        let stdin = io::stdin();
        let input: &mut io::BufRead = &mut stdin.lock(); 
        let msg_result = LSPMessageReader(input).read_next();
        
        match msg_result {
            Ok(ref ok) => { 
                debug!("Read message: {}\n", ok);
            } 
            Err(ref error) => { 
                info!("Error reading message: {}\n", error);
            }
        };
        msg_result
    }
}

struct StdioOutput {
}

impl MessageWriter for StdioOutput {
    fn write_message(&mut self, msg: &str) -> GResult<()> {
        debug!("Writing message: {}\n", msg);
        LSPMessageWriter(io::stdout()).write_message(msg)
    }
}

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>, build_queue: Arc<BuildQueue>) {
    
    debug!("\nLanguage Server Starting up\n");
    
    let msg_writer_provider = || StdioOutput { };
    
    let output = LSPEndpoint::create_lsp_output(msg_writer_provider);
    
    let service = LsService::new(analysis,
                                 vfs,
                                 build_queue,
                                 output.clone(),
    );
    let mut reader = StdioMsgReader { };
    LsService::run(service, &mut reader);
    debug!("\nServer shutting down.\n");
}
