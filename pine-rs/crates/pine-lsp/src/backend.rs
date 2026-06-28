//! tower-lsp Backend. Stores a live [`Document`] per open file and applies
//! INCREMENTAL edits via tree-sitter `InputEdit` (P6), so features read an
//! already-parsed tree instead of re-parsing per request.

use std::collections::HashMap;

use pine_core::Document;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::features;

pub struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, Document>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                        legend: features::semantic_token_legend(),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        range: None,
                        work_done_progress_options: Default::default(),
                    }),
                ),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "pine-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let diagnostics = {
            let mut docs = self.docs.lock().await;
            let doc = Document::parse(params.text_document.text)
                .unwrap_or_else(|| Document::parse("").expect("empty doc parses"));
            let diagnostics = features::all_diagnostics(&doc);
            docs.insert(uri.clone(), doc);
            diagnostics
        };
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let diagnostics = {
            let mut docs = self.docs.lock().await;
            let Some(doc) = docs.get_mut(&uri) else {
                return;
            };
            for change in params.content_changes {
                match change.range {
                    // Incremental edit: convert the UTF-16 LSP range to byte
                    // offsets *against the current doc*, then splice + reparse.
                    Some(range) => {
                        let start = doc.offset_at(range.start.line, range.start.character);
                        let end = doc.offset_at(range.end.line, range.end.character);
                        doc.apply_edit(start, end, &change.text);
                    }
                    // Whole-document replacement.
                    None => {
                        if let Some(fresh) = Document::parse(change.text) {
                            *doc = fresh;
                        }
                    }
                }
            }
            features::all_diagnostics(doc)
        };
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.lock().await.remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).and_then(|d| features::hover_at(d, pos)))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        // Cross-file member completion resolves `/// @source` libs relative to
        // the open document's directory. Non-`file:` URLs (e.g. `untitled:`)
        // yield Err -> None, degrading gracefully to builtin-only completion.
        let path = uri.to_file_path().ok();
        let base_dir = path.as_deref().and_then(|p| p.parent());
        let docs = self.docs.lock().await;
        let items = docs
            .get(&uri)
            .map(|d| features::completions_at(d, pos, base_dir))
            .unwrap_or_default();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).and_then(|d| features::signature_help(d, pos)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri.clone();
        let pos = params.text_document_position_params.position;
        // Cross-file go-to-definition resolves `/// @source` libs relative to the
        // open document's directory, exactly like completion. Non-`file:` URLs
        // yield Err -> None, degrading to same-file goto only.
        let path = uri.to_file_path().ok();
        let base_dir = path.as_deref().and_then(|p| p.parent());
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .and_then(|d| features::goto_definition(d, pos, uri.clone(), base_dir)))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| features::references(d, pos, uri.clone())))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .map(|d| DocumentSymbolResponse::Nested(features::document_symbols(d))))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let docs = self.docs.lock().await;
        let mut out = Vec::new();
        for (uri, doc) in docs.iter() {
            for sym in features::document_symbols(doc) {
                if !query.is_empty() && !sym.name.to_lowercase().contains(&query) {
                    continue;
                }
                #[allow(deprecated)]
                out.push(SymbolInformation {
                    name: sym.name,
                    kind: sym.kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: sym.range,
                    },
                    container_name: None,
                });
            }
        }
        Ok(Some(out))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let pos = params.position;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).and_then(|d| features::prepare_rename(d, pos)))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .and_then(|d| features::rename(d, pos, new_name, uri.clone())))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| features::folding_ranges(d)))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| features::inlay_hints(d)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .map(|d| SemanticTokensResult::Tokens(features::semantic_tokens(d))))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).and_then(|d| features::format_document(d)))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        Ok(Some(features::code_actions(&params.context.diagnostics, uri)))
    }
}
