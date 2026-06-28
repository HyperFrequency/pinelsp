//! tower-lsp Backend. Stores a live [`Document`] per open file and applies
//! INCREMENTAL edits via tree-sitter `InputEdit` (P6), so features read an
//! already-parsed tree instead of re-parsing per request.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::SystemTime;

use pine_core::Document;
use pine_core::imports::{ImportResolution, import_table, resolve_imports};
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, error, info, warn};

use crate::features;

/// A cached, OWNED projection of one document's resolved imports.
///
/// `ResolvedImports<'a>` borrows the `ImportTable`, so it cannot be stored
/// directly (self-referential lifetime trap). Instead we store the owned
/// `(effective_namespace, ImportResolution)` pairs — `ImportResolution` is
/// `Clone` and owns its data (canonical `path: PathBuf`, `symbols: Vec<...>`).
///
/// `key` is a sorted vector of each `/// @source` lib's path + mtime. A stale
/// lib edit changes its mtime -> key mismatch -> re-resolve; a lib file
/// appearing/disappearing flips its sentinel (`None` mtime) entry, also
/// invalidating.
struct CachedImports {
    key: Vec<(PathBuf, Option<SystemTime>)>,
    resolved: Vec<(String, ImportResolution)>,
}

pub struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, Document>>,
    /// Per-document cache of resolved imports, keyed on each @source lib's
    /// path+mtime. Invalidated on did_close alongside the document.
    import_cache: Mutex<HashMap<Url, CachedImports>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
            import_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve `doc`'s imports against `base_dir`, serving a cached owned
    /// projection when no referenced `/// @source` lib has changed.
    ///
    /// The cache key is the sorted set of (lib path, mtime) for every import
    /// entry's `@source`. Entries whose lib is missing/unreadable contribute a
    /// `None` mtime sentinel, so a lib file appearing or disappearing changes
    /// the key. On a key match we skip both the filesystem read and the re-parse
    /// done by `resolve_imports`; on a mismatch we re-resolve and store the new
    /// owned projection. With `base_dir == None` we cannot resolve `@source`
    /// libs at all, so we return an empty projection without touching the cache
    /// (the untitled/in-memory graceful-degrade case — no fs access).
    ///
    /// Note: mtime granularity is coarse on some filesystems (~1s); two edits to
    /// a lib within the same second could share an mtime and briefly serve stale
    /// members. Acceptable for an editor cache; not worth content-hashing here.
    async fn resolved_imports(
        &self,
        uri: &Url,
        doc: &Document,
        base_dir: Option<&std::path::Path>,
    ) -> Vec<(String, ImportResolution)> {
        let Some(base_dir) = base_dir else {
            return Vec::new();
        };
        let mut cache = self.import_cache.lock().await;
        resolve_imports_cached(&mut cache, uri, doc, base_dir)
    }
}

/// Build the cache key for `doc`'s imports under `base_dir`: a sorted vector of
/// each `/// @source` lib's joined path + mtime. Missing/unreadable libs get a
/// `None` mtime sentinel so a lib appearing/disappearing changes the key.
///
/// Paths are joined lexically (NOT canonicalized) — keying only needs to track
/// the same files' existence + mtime; `resolve_imports` owns the path-safety
/// contract.
fn import_cache_key(
    doc: &Document,
    base_dir: &std::path::Path,
) -> Vec<(PathBuf, Option<SystemTime>)> {
    let table = import_table(doc);
    let mut key: Vec<(PathBuf, Option<SystemTime>)> = table
        .entries()
        .iter()
        .filter_map(|entry| entry.source.as_deref())
        .map(|source| {
            let path = base_dir.join(source);
            let mtime = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
            (path, mtime)
        })
        .collect();
    key.sort();
    key
}

/// Core cache logic, split out so it can be unit-tested without a `Client`.
/// Serves the cached owned projection on a key match (no fs read, no re-parse);
/// otherwise re-resolves once, stores, and returns the new projection.
fn resolve_imports_cached(
    cache: &mut HashMap<Url, CachedImports>,
    uri: &Url,
    doc: &Document,
    base_dir: &std::path::Path,
) -> Vec<(String, ImportResolution)> {
    let key = import_cache_key(doc, base_dir);

    if let Some(cached) = cache.get(uri)
        && cached.key == key
    {
        debug!(uri = %uri, "import cache hit");
        return cached.resolved.clone();
    }

    debug!(uri = %uri, "import cache miss; resolving");
    let table = import_table(doc);
    let resolved_borrowed = resolve_imports(&table, base_dir);
    let resolved: Vec<(String, ImportResolution)> = resolved_borrowed
        .entries()
        .iter()
        .map(|resolved| {
            (
                resolved.entry.effective_namespace().to_string(),
                resolved.resolution.clone(),
            )
        })
        .collect();

    cache.insert(
        uri.clone(),
        CachedImports {
            key,
            resolved: resolved.clone(),
        },
    );
    resolved
}

/// Run a synchronous feature computation under `catch_unwind`, returning
/// `T::default()` (empty Vec / `None`) if it panics so a single bad request
/// cannot kill the server. `AssertUnwindSafe` is required because `&Document`
/// and the closures aren't `UnwindSafe`; this is sound here because on panic we
/// discard the (possibly-broken) result and return a default — we never observe
/// a broken invariant. The panic itself is also logged via the global hook.
fn guard<T: Default>(label: &str, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            error!(label, "handler panicked; returning default");
            T::default()
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        info!("initialize");
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
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: features::semantic_token_legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("pine-lsp ready");
        self.client
            .log_message(MessageType::INFO, "pine-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        info!("shutdown");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        debug!(uri = %uri, "did_open");
        let diagnostics = {
            let mut docs = self.docs.lock().await;
            let doc = Document::parse(params.text_document.text).unwrap_or_else(|| {
                warn!(uri = %uri, "document failed to parse; falling back to empty doc");
                Document::parse("").expect("empty doc parses")
            });
            let diagnostics = guard("all_diagnostics", || features::all_diagnostics(&doc));
            docs.insert(uri.clone(), doc);
            diagnostics
        };
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        debug!(uri = %uri, "did_change");
        let diagnostics = {
            let mut docs = self.docs.lock().await;
            let Some(doc) = docs.get_mut(&uri) else {
                warn!(uri = %uri, "did_change for unknown document; ignoring");
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
                        } else {
                            warn!(uri = %uri, "whole-document replacement failed to parse; keeping prior doc");
                        }
                    }
                }
            }
            guard("all_diagnostics", || features::all_diagnostics(doc))
        };
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        debug!(uri = %uri, "did_close");
        self.docs.lock().await.remove(&uri);
        // Drop the import cache for this doc too, so a closed-then-reopened file
        // re-resolves from scratch.
        self.import_cache.lock().await.remove(&uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .and_then(|d| guard("hover_at", || features::hover_at(d, pos))))
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
        let items = match docs.get(&uri) {
            Some(doc) => {
                // Cache the per-keystroke import resolution so we don't re-read
                // and re-parse every lib on every completion request.
                let resolved = self.resolved_imports(&uri, doc, base_dir).await;
                guard("completions_at", || {
                    features::completions_at_cached(doc, pos, &resolved)
                })
            }
            None => Vec::new(),
        };
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .and_then(|d| guard("signature_help", || features::signature_help(d, pos))))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let pos = params.text_document_position_params.position;
        // Cross-file go-to-definition resolves `/// @source` libs relative to the
        // open document's directory, exactly like completion. Non-`file:` URLs
        // yield Err -> None, degrading to same-file goto only.
        let path = uri.to_file_path().ok();
        let base_dir = path.as_deref().and_then(|p| p.parent());
        let docs = self.docs.lock().await;
        let response = match docs.get(&uri) {
            Some(doc) => {
                let resolved = self.resolved_imports(&uri, doc, base_dir).await;
                let uri_for_goto = uri.clone();
                guard("goto_definition", || {
                    features::goto_definition_cached(doc, pos, uri_for_goto, &resolved)
                })
            }
            None => None,
        };
        Ok(response)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| {
            let uri_for_refs = uri.clone();
            guard("references", || features::references(d, pos, uri_for_refs))
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| {
            DocumentSymbolResponse::Nested(guard("document_symbols", || {
                features::document_symbols(d)
            }))
        }))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let docs = self.docs.lock().await;
        let mut out = Vec::new();
        for (uri, doc) in docs.iter() {
            for sym in guard("document_symbols", || features::document_symbols(doc)) {
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
        Ok(docs
            .get(&uri)
            .and_then(|d| guard("prepare_rename", || features::prepare_rename(d, pos))))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).and_then(|d| {
            let uri_for_rename = uri.clone();
            let new_name = new_name.clone();
            guard("rename", || {
                features::rename(d, pos, new_name, uri_for_rename)
            })
        }))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .map(|d| guard("folding_ranges", || features::folding_ranges(d))))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .map(|d| guard("inlay_hints", || features::inlay_hints(d))))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs.get(&uri).map(|d| {
            SemanticTokensResult::Tokens(guard("semantic_tokens", || features::semantic_tokens(d)))
        }))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().await;
        Ok(docs
            .get(&uri)
            .and_then(|d| guard("format_document", || features::format_document(d))))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        Ok(Some(guard("code_actions", || {
            features::code_actions(&params.context.diagnostics, uri.clone())
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- guard() panic isolation -----------------------------------------

    #[test]
    fn guard_returns_value_on_success() {
        let v: Vec<i32> = guard("ok_vec", || vec![1, 2, 3]);
        assert_eq!(v, vec![1, 2, 3]);
        let o: Option<i32> = guard("ok_opt", || Some(7));
        assert_eq!(o, Some(7));
    }

    #[test]
    fn guard_returns_default_on_panic_and_stays_alive() {
        // A feature fn that panics (the kind of crafted-input panic the LSP
        // handlers must survive) yields the Default instead of unwinding into
        // the async runtime / killing the server.
        let v: Vec<i32> = guard("boom_vec", || panic!("crafted panic"));
        assert!(v.is_empty(), "panicking handler must return empty Vec");
        let o: Option<i32> = guard("boom_opt", || panic!("crafted panic"));
        assert!(o.is_none(), "panicking handler must return None");

        // Server is still usable afterwards: a subsequent call works normally.
        let after: Vec<i32> = guard("after", || vec![42]);
        assert_eq!(after, vec![42], "guard must not corrupt later calls");
    }

    // ---- import-resolution cache -----------------------------------------

    /// Unique temp dir for one test, cleaned up on drop.
    struct TempLibDir {
        path: PathBuf,
    }
    impl TempLibDir {
        fn new(tag: &str) -> Self {
            let counter = {
                static N: AtomicUsize = AtomicUsize::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            };
            let path = std::env::temp_dir().join(format!(
                "pine-lsp-cache-test-{tag}-{}-{counter}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.path.join(name), contents).expect("write lib");
        }
    }
    impl Drop for TempLibDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn doc_importing(source: &str) -> Document {
        // Aliasless import -> effective namespace is the lib name `Lib`.
        let src = format!("//@version=6\n/// @source {source}\nimport User/Lib/1 as L\n");
        Document::parse(src).expect("doc parses")
    }

    fn fake_uri() -> Url {
        Url::parse("file:///cache_test/main.pine").expect("uri")
    }

    fn member_names(resolved: &[(String, ImportResolution)], namespace: &str) -> Vec<String> {
        resolved
            .iter()
            .find(|(ns, _)| ns == namespace)
            .and_then(|(_, res)| match res {
                ImportResolution::Resolved { symbols, .. } => {
                    Some(symbols.iter().map(|s| s.name.clone()).collect())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn cache_hit_serves_stored_projection_without_reresolving() {
        let dir = TempLibDir::new("hit");
        dir.write("lib.pine", "//@version=6\nexport one() =>\n    1\n");
        let doc = doc_importing("./lib.pine");
        let uri = fake_uri();
        let mut cache: HashMap<Url, CachedImports> = HashMap::new();

        // First call populates the cache from the real lib.
        let first = resolve_imports_cached(&mut cache, &uri, &doc, &dir.path);
        assert_eq!(member_names(&first, "L"), vec!["one".to_string()]);

        // Plant a DELIBERATELY-WRONG resolved payload under the *current* key.
        // If the next call serves it back, the cache-hit path ran (no re-resolve
        // — a re-resolve would re-read the lib and return `one`, not `sentinel`).
        let key = import_cache_key(&doc, &dir.path);
        cache.insert(
            uri.clone(),
            CachedImports {
                key,
                resolved: vec![(
                    "L".to_string(),
                    ImportResolution::Resolved {
                        path: dir.path.join("lib.pine"),
                        symbols: vec![],
                    },
                )],
            },
        );
        let second = resolve_imports_cached(&mut cache, &uri, &doc, &dir.path);
        assert!(
            member_names(&second, "L").is_empty(),
            "cache hit must serve the stored (planted-empty) projection, not re-resolve"
        );
    }

    #[test]
    fn cache_invalidates_when_lib_mtime_changes() {
        let dir = TempLibDir::new("mtime");
        dir.write("lib.pine", "//@version=6\nexport one() =>\n    1\n");
        let doc = doc_importing("./lib.pine");
        let uri = fake_uri();
        let mut cache: HashMap<Url, CachedImports> = HashMap::new();

        let first = resolve_imports_cached(&mut cache, &uri, &doc, &dir.path);
        assert_eq!(member_names(&first, "L"), vec!["one".to_string()]);

        // Simulate a stale edit: rewrite the lib with a new export, and force a
        // key mismatch by planting a cache entry whose mtime sentinel differs
        // (deterministic — avoids depending on coarse filesystem mtime ticks).
        dir.write("lib.pine", "//@version=6\nexport two() =>\n    2\n");
        let stale_key = vec![(dir.path.join("lib.pine"), Some(SystemTime::UNIX_EPOCH))];
        cache.insert(
            uri.clone(),
            CachedImports {
                key: stale_key,
                resolved: first.clone(),
            },
        );

        let second = resolve_imports_cached(&mut cache, &uri, &doc, &dir.path);
        assert_eq!(
            member_names(&second, "L"),
            vec!["two".to_string()],
            "a changed lib (key mismatch) must re-resolve and reflect new contents"
        );
    }

    #[test]
    fn cache_key_changes_when_source_file_created_or_deleted() {
        let dir = TempLibDir::new("sentinel");
        let doc = doc_importing("./lib.pine");

        // File absent -> sentinel `None` mtime.
        let key_absent = import_cache_key(&doc, &dir.path);
        assert_eq!(key_absent.len(), 1);
        assert!(
            key_absent[0].1.is_none(),
            "missing @source lib must key on a None mtime sentinel"
        );

        // File created -> key gains a real mtime, so it differs from absent.
        dir.write("lib.pine", "//@version=6\nexport one() =>\n    1\n");
        let key_present = import_cache_key(&doc, &dir.path);
        assert!(
            key_present[0].1.is_some(),
            "present @source lib must key on a real mtime"
        );
        assert_ne!(
            key_absent, key_present,
            "creating the @source file must change the cache key (invalidate)"
        );

        // File deleted -> key returns to the sentinel form.
        std::fs::remove_file(dir.path.join("lib.pine")).expect("remove lib");
        let key_deleted = import_cache_key(&doc, &dir.path);
        assert_eq!(
            key_absent, key_deleted,
            "deleting the @source file must change the key back to the sentinel"
        );
    }

    #[test]
    fn no_base_dir_keys_empty_and_no_fs_touch() {
        // base_dir=None path is handled in `resolved_imports`; here we assert the
        // cache key for a doc with no @source entries is empty (no fs access).
        let doc = Document::parse("//@version=6\nimport TV/Strategy/2\n".to_string())
            .expect("doc parses");
        let key = import_cache_key(&doc, std::path::Path::new("/nonexistent"));
        assert!(
            key.is_empty(),
            "an import with no @source contributes nothing to the key"
        );
    }
}
