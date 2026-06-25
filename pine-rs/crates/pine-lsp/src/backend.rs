//! tower-lsp Backend. P2 wires document sync + diagnostics, hover, and
//! completion to the pure functions in [`crate::features`]. Definition,
//! references, rename, and document/workspace symbols are the next P2 increment;
//! the semantic checker lands in P3.
//!
//! Document sync is FULL for now (re-parse per change); true incremental reparse
//! via tree-sitter `InputEdit` is P6. Parsing is cheap (sub-ms), so the Backend
//! stores source text and re-parses per request rather than holding a `Tree`.

use std::collections::HashMap;

use pine_core::Document;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::features;

pub struct Backend {
    client: Client,
    /// uri -> current source text (FULL sync).
    docs: Mutex<HashMap<Url, String>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
        }
    }

    async fn refresh(&self, uri: Url, text: &str) {
        let diagnostics = Document::parse(text)
            .map(|d| features::all_diagnostics(&d))
            .unwrap_or_default();
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
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
        let text = params.text_document.text;
        self.docs.lock().await.insert(uri.clone(), text.clone());
        self.refresh(uri, &text).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        // FULL sync: the final change event carries the whole document.
        if let Some(change) = params.content_changes.pop() {
            let uri = params.text_document.uri.clone();
            self.docs.lock().await.insert(uri.clone(), change.text.clone());
            self.refresh(uri, &change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.lock().await.remove(&params.text_document.uri);
        // Clear diagnostics for the closed file.
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .and_then(|d| features::hover_at(&d, pos)))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        let items = text
            .and_then(Document::parse)
            .map(|d| features::completions_at(&d, pos))
            .unwrap_or_default();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .and_then(|d| features::signature_help(&d, pos)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri.clone();
        let pos = params.text_document_position_params.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .and_then(|d| features::goto_definition(&d, pos, uri)))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .map(|d| features::references(&d, pos, uri)))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .map(|d| DocumentSymbolResponse::Nested(features::document_symbols(&d))))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let docs = self.docs.lock().await;
        let mut out = Vec::new();
        for (uri, text) in docs.iter() {
            let Some(doc) = Document::parse(text.clone()) else {
                continue;
            };
            for sym in features::document_symbols(&doc) {
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
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .and_then(|d| features::prepare_rename(&d, pos)))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let text = self.docs.lock().await.get(&uri).cloned();
        Ok(text
            .and_then(Document::parse)
            .and_then(|d| features::rename(&d, pos, new_name, uri)))
    }
}
