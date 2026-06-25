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
            .map(|d| features::syntax_diagnostics(&d))
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
}
