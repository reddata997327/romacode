//! LangChain-native integration — the vendored `langchain-rust` crate wired
//! into the brain as a first-class engine.
//!
//! Three pieces live here:
//!
//! 1. **Engine** — `run_langchain` drives langchain's `AgentExecutor` (the
//!    plan → tools → answer loop) with a `ChatOpenAI` model built from the
//!    resolved provider. Every tool the executor sees is a *romacode tool*:
//!    the executor calls them through the same [`Security`] gate the native
//!    engine uses, so approvals, Plan mode and deny rules behave identically.
//!    A callback bridge streams tool activity and per-step text to clients.
//!
//! 2. **Memory** — the session transcript is preloaded into langchain's
//!    `ConversationBufferMemory`, and `WindowBufferMemory` is available for
//!    long sessions (`agent.engine = "langchain"`, `memory.window` in a
//!    future config rev; buffer is the default, matching the native loop).
//!
//! 3. **RAG** — `rag_index` / `rag_search` tools back onto langchain's
//!    `RecursiveCharacterTextSplitter`, `OpenAIEmbeddings` and
//!    `InMemoryVectorStore`. Indexing reads files directly from disk (the
//!    permission policy ships fully open — nothing gates file reads).

use crate::agent::{system_prompt_for_mode, Emit};
use crate::config::{Config, Resolved};
use crate::security::{Decision, Security};
use crate::session::Mode;
use async_trait::async_trait;
use langchain_rust::agents::tools::{tool_fn, Tool};
use langchain_rust::agents::AgentExecutor;
use langchain_rust::callbacks::CallbackHandler;
use langchain_rust::errors::LangchainError;
use langchain_rust::llms::openai::ChatOpenAI;
use langchain_rust::memory::{ConversationBufferMemory, Memory};
use langchain_rust::messages::{Message as LcMessage, ToolSpec};
use langchain_rust::text_splitter::{Document as LcDocument, RecursiveCharacterTextSplitter};
use langchain_rust::vectorstores::InMemoryVectorStore;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};

// ---------------------------------------------------------------------------
// 1. The engine.
// ---------------------------------------------------------------------------

/// Callback bridge: langchain executor events → wire events on the bus.
#[derive(Clone)]
struct BridgeHandler {
    emit: Emit,
    last_tool_id: Arc<Mutex<String>>,
    counter: Arc<AtomicU64>,
}

impl BridgeHandler {
    fn new(emit: Emit) -> Self {
        Self {
            emit,
            last_tool_id: Arc::new(Mutex::new(String::new())),
            counter: Arc::new(AtomicU64::new(1)),
        }
    }
}

#[async_trait]
impl CallbackHandler for BridgeHandler {
    async fn on_llm_start(&self, _model: &str, _messages: &[LcMessage]) {}

    async fn on_llm_end(&self, _model: &str, output: &str) {
        // Each model step's text lands as one delta; clients append it to the
        // current assistant block exactly like a native-engine stream chunk.
        if !output.trim().is_empty() {
            self.emit.text(output);
        }
    }

    async fn on_tool_start(&self, tool: &str, args: &serde_json::Value) {
        let id = format!("lc-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        if let Ok(mut g) = self.last_tool_id.lock() {
            *g = id.clone();
        }
        self.emit.tool_start(&id, tool, args);
    }

    async fn on_tool_end(&self, _tool: &str, result: &str) {
        let id = self
            .last_tool_id
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        let extra = result
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>();
        self.emit.tool_end(
            &id,
            true,
            if extra.is_empty() { None } else { Some(extra) },
            None,
        );
    }

    async fn on_agent_step(&self, iteration: usize) {
        if iteration > 1 {
            self.emit.notice(format!("langchain agent step {iteration}"));
        }
    }

    async fn on_agent_finish(&self, _output: &str) {}
}

/// Build the langchain `ChatOpenAI` from the resolved provider.
fn build_model(cfg: &Config, r: &Resolved) -> Result<Arc<ChatOpenAI>, String> {
    let mut b = ChatOpenAI::builder()
        .model(&r.model)
        .base_url(&r.base_url);
    // `0 = no request timeout` (the documented default). The old
    // `.max(1)` turned the shipped default into a ONE-SECOND total
    // timeout on every model call, so the langchain engine could not
    // complete a single turn. `timeout_secs` bounds non-streaming calls
    // only; SSE streams are unbounded either way.
    if cfg.network.request_timeout_secs > 0 {
        b = b.timeout_secs(cfg.network.request_timeout_secs as u64);
    }
    if let Some(k) = &r.api_key {
        b = b.api_key(k);
    }
    // Profile-driven sampling: the model's official rules (modeldata) bound
    // temperature the same way the native loop clamps it — an Anthropic-wire
    // model must never receive 2.0, a reasoning model never any temperature.
    let profile = romacode_shared::modeldata::profile_for(&r.id, &r.model);
    if let Some(t) = cfg.model_params.temperature {
        let clamped = match profile.params.temperature {
            Some(spec) if !spec.supported => None,
            Some(spec) if spec.max >= spec.min && spec.max > 0.0 => {
                Some(t.clamp(spec.min, spec.max))
            }
            _ => Some(t.clamp(0.0, 2.0)),
        };
        if let Some(t) = clamped {
            b = b.temperature(t as f64);
        }
    }
    let max_out = if profile.max_output_tokens > 0 {
        cfg.max_tokens.min(profile.max_output_tokens)
    } else {
        cfg.max_tokens
    };
    b = b.max_tokens(max_out.min(u32::MAX as u32) as u32);
    let m = b.build().map_err(|e| format!("langchain model: {e}"))?;
    Ok(Arc::new(m))
}

/// Every romacode tool as a langchain `Tool`: schema from the registry,
/// executor = security gate → tools::execute. The gate decides Deny/Ask/Run
/// exactly like the native loop, so approvals and Plan mode are identical.
fn romacode_tools(
    cfg: &Config,
    mode: Mode,
    emit: Emit,
    security: Arc<Security>,
    android: Option<&crate::android::AndroidAgent>,
    cancel: Arc<AtomicBool>,
) -> Vec<Tool> {
    let android = android.cloned();
    crate::tools::definitions(cfg, false)
        .into_iter()
        .map(|def| {
            let name = def["name"].as_str().unwrap_or_default().to_string();
            let description = def["description"].as_str().unwrap_or_default().to_string();
            let parameters = def
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            let spec = ToolSpec {
                name: name.clone(),
                description,
                parameters,
            };
            let emit = emit.clone();
            let security = security.clone();
            let cfg = cfg.clone();
            let cancel = cancel.clone();
            let android = android.clone();
            let m = mode;
            tool_fn(
                name.clone(),
                spec.description.clone(),
                spec.parameters.clone(),
                move |args| {
                    let emit = emit.clone();
                    let security = security.clone();
                    let cfg = cfg.clone();
                    let cancel = cancel.clone();
                    let android = android.clone();
                    let name = name.clone();
                    let mode = m;
                    async move {
                        // THE gate — identical semantics to the native engine.
                        let fail =
                            |msg: String| LangchainError::ToolExecution(name.clone(), msg);
                        match security.authorize(&cfg, mode, &name, &args) {
                            Decision::Deny(reason) => Err(fail(reason)),
                            Decision::Run => {
                                crate::tools::execute(&cfg, &name, &args, android.as_ref(), &cancel)
                                    .map_err(fail)
                            }
                            Decision::Ask { request_id, effect } => {
                                // Same contract as the native loop: register
                                // first (so an instant Allow can never race the
                                // registry), then broadcast the real
                                // ApprovalRequest event for the screens.
                                let rx = security.register(&request_id);
                                emit.approval_request(&request_id, &name, &effect);
                                let mut answered: Option<bool> = None;
                                loop {
                                    if cancel.load(Ordering::SeqCst) {
                                        break;
                                    }
                                    match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                                        Ok(v) => {
                                            answered = Some(v);
                                            break;
                                        }
                                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                        Err(_) => break,
                                    }
                                }
                                match answered {
                                    Some(true) => crate::tools::execute(
                                        &cfg,
                                        &name,
                                        &args,
                                        android.as_ref(),
                                        &cancel,
                                    )
                                    .map_err(fail),
                                    Some(false) => {
                                        Err(fail("denied by user; action not run".into()))
                                    }
                                    None => Err(fail(
                                        "cancelled while awaiting approval; action not run".into(),
                                    )),
                                }
                            }
                        }
                    }
                },
            )
        })
        .collect()
}

/// Run the whole turn on the langchain engine. Returns Ok(final_answer) or a
/// provider/engine error (both are already streamed to clients; the error is
/// additionally reported in chat by the caller).
#[allow(clippy::too_many_arguments)]
pub fn run_langchain(
    cfg: &Config,
    r: &Resolved,
    history: &[(&'static str, String)],
    mode: Mode,
    emit: &Emit,
    security: &Arc<Security>,
    android: Option<&crate::android::AndroidAgent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let model = build_model(cfg, r)?;

    // Transcript → langchain memory (preloaded so multi-turn context works
    // exactly like the native engine's plain-text history). The final user
    // entry is this turn's input and is passed to `invoke` instead, so it is
    // not duplicated.
    let input = history
        .iter()
        .rev()
        .find(|(role, _)| *role == "user")
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    let mut preload_len = history.len();
    if let Some(pos) = history
        .iter()
        .rposition(|(role, _)| *role == "user")
    {
        if history[pos].1 == input {
            preload_len = pos;
        }
    }
    let mut memory = ConversationBufferMemory::new();
    for (role, text) in &history[..preload_len] {
        match *role {
            "assistant" => memory.push(LcMessage::ai(text.clone())),
            // Tool summaries ride along as human turns — the same compromise
            // the native loop makes for providers without native tool roles.
            _ => memory.push(LcMessage::human(text.clone())),
        }
    }
    let memory: Box<dyn Memory> = Box::new(memory);

    let tools = romacode_tools(
        cfg,
        mode,
        emit.clone(),
        security.clone(),
        android,
        cancel.clone(),
    );
    let mut executor = AgentExecutor::builder(model)
        .tools(tools)
        .memory(memory)
        .system_prompt(system_prompt_for_mode(cfg, mode))
        .max_iterations(cfg.agent.max_tool_rounds as usize)
        .callbacks(langchain_rust::callbacks::CallbackManager::new().add(Arc::new(
            BridgeHandler::new(emit.clone()),
        )))
        .build();

    // The user message for this turn was already pushed by the api layer
    // into `history`; the executor needs just the new input.
    let input = history
        .iter()
        .rev()
        .find(|(role, _)| *role == "user")
        .map(|(_, t)| t.clone())
        .unwrap_or_default();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("langchain runtime: {e}"))?;

    let answer = rt.block_on(async {
        if cancel.load(Ordering::SeqCst) {
            return Err(LangchainError::Other("cancelled".into()));
        }
        executor.invoke(&input).await
    });

    match answer {
        Ok(_text) => {
            // The final answer (and each step's text) already reached clients
            // through the callback bridge; usage comes from the model when
            // the provider reports it (native engine path).
            Ok(())
        }
        Err(e) => Err(format!("langchain engine: {e}")),
    }
}

// ---------------------------------------------------------------------------
// 3. RAG — index + search over project files.
// ---------------------------------------------------------------------------

struct RagState {
    store: InMemoryVectorStore,
    embedder: Arc<dyn langchain_rust::embeddings::Embeddings>,
    label: String,
}

static RAG: OnceLock<Mutex<Option<RagState>>> = OnceLock::new();

fn rag_slot() -> &'static Mutex<Option<RagState>> {
    RAG.get_or_init(|| Mutex::new(None))
}

fn embedder_from(cfg: &Config) -> Result<(Arc<dyn langchain_rust::embeddings::Embeddings>, String), String> {
    let r = cfg.resolve()?;
    let model = std::env::var("ROMACODE_EMBEDDINGS_MODEL")
        .unwrap_or_else(|_| "text-embedding-3-small".to_string());
    let mut b = langchain_rust::embeddings::openai::OpenAIEmbeddings::builder()
        .model(model.clone())
        .base_url(&r.base_url);
    // 0 = none (same contract as the native engine); never clamp to 1s.
    if cfg.network.request_timeout_secs > 0 {
        b = b.timeout_secs(cfg.network.request_timeout_secs as u64);
    }
    if let Some(k) = &r.api_key {
        b = b.api_key(k);
    }
    let e = b.build().map_err(|err| format!("embeddings: {err}"))?;
    Ok((Arc::new(e), format!("{} @ {}", model, r.name)))
}

/// Walk `root` for files matching a `**`/`*` glob (project-relative, `/`
/// separators). Skips VCS/build dirs, respects the file cap and per-file size.
fn walk_glob(root: &std::path::Path, pattern: &str, max_files: usize) -> Vec<std::path::PathBuf> {
    use crate::security::glob;
    fn rec(
        dir: &std::path::Path,
        rel: &str,
        pattern: &str,
        out: &mut Vec<std::path::PathBuf>,
        budget: usize,
    ) {
        if out.len() >= budget {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= budget {
                return;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".hg" | ".svn" | "__pycache__") {
                    continue;
                }
                let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                rec(&entry.path(), &child_rel, pattern, out, budget);
            } else if ft.is_file() {
                let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if glob(pattern, &child_rel, true) {
                    out.push(entry.path());
                }
            }
        }
    }
    let mut out = Vec::new();
    rec(root, "", pattern, &mut out, max_files.max(1));
    out.sort();
    out
}

const RAG_CHUNK_CHARS: usize = 1200;
const RAG_MAX_FILE_BYTES: u64 = 512 * 1024;

/// Dispatch a RAG tool call. Files are read directly from disk (the
/// permission policy ships fully open — nothing gates file reads).
pub fn rag_execute(cfg: &Config, name: &str, input: &Value) -> Result<String, String> {
    match name {
        "rag_index" => rag_index(cfg, input),
        "rag_search" => rag_search(input),
        other => Err(format!("unknown rag tool {other}")),
    }
}

fn rag_index(cfg: &Config, input: &Value) -> Result<String, String> {
    let pattern = input["glob"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing string glob")?;
    let max_files = input["max_files"].as_u64().unwrap_or(200).min(2_000) as usize;
    let root = crate::security::root()?;

    let files = walk_glob(&root, pattern, max_files);
    if files.is_empty() {
        return Err(format!("no files match '{pattern}' under {}", root.display()));
    }

    let (embedder, label) = embedder_from(cfg)?;
    let splitter = RecursiveCharacterTextSplitter::new(RAG_CHUNK_CHARS, 150);
    let store = InMemoryVectorStore::new();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("langchain runtime: {e}"))?;

    let mut indexed_files = 0usize;
    let mut chunks = 0usize;
    let mut skipped = 0usize;
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.display().to_string());
        // Binary-ish guard: skip files containing a NUL in the first 8 KiB.
        let meta = match std::fs::metadata(path) {
            Ok(m) if m.len() <= RAG_MAX_FILE_BYTES => m,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let _ = meta;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if bytes.iter().take(8192).any(|&b| b == 0) {
            skipped += 1;
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let pieces = splitter.split(&text);
        if pieces.is_empty() {
            continue;
        }
        let docs: Vec<LcDocument> = pieces
            .into_iter()
            .map(|c| LcDocument::with_source(c, rel.clone()))
            .collect();
        let added = rt
            .block_on(store.add_documents(&docs, embedder.as_ref()))
            .map_err(|e| format!("embed {rel}: {e}"))?;
        chunks += added;
        indexed_files += 1;
    }
    if indexed_files == 0 {
        return Err("nothing indexable matched (all skipped as binary/oversized/unreadable)".into());
    }

    *rag_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(RagState {
        store,
        embedder,
        label: label.clone(),
    });

    Ok(format!(
        "Indexed {chunks} chunks from {indexed_files} files (skipped {skipped}) with {label}. Use rag_search to query."
    ))
}

fn rag_search(input: &Value) -> Result<String, String> {
    let query = input["query"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing string query")?;
    let k = input["k"].as_u64().unwrap_or(6).clamp(1, 20) as usize;

    let state = rag_slot().lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = state.as_ref() else {
        return Err("the vector store is empty — call rag_index first".into());
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("langchain runtime: {e}"))?;
    let vector = rt
        .block_on(async { st.embedder.embed_query(query).await })
        .map_err(|e| format!("embed query: {e}"))?;
    let hits = st
        .store
        .similarity_search_by_vector(&vector, k)
        .map_err(|e| format!("search: {e}"))?;
    if hits.is_empty() {
        return Ok("No matches. Index something with rag_index first.".into());
    }
    let mut out = String::from("Semantic matches (score = cosine similarity):\n");
    for (i, (doc, score)) in hits.iter().enumerate() {
        let src = doc
            .metadata
            .get("source")
            .map(String::as_str)
            .unwrap_or("(unknown)");
        let snippet: String = doc.page_content.chars().take(600).collect();
        out.push_str(&format!("\n{}. [{score:.3}] {src}\n{snippet}\n", i + 1));
    }
    Ok(out)
}

/// Current store size, for the welcome payload / status lines.
pub fn rag_stats() -> Option<(usize, String)> {
    let state = rag_slot().lock().ok()?;
    let st = state.as_ref()?;
    Some((st.store.len().ok()?, st.label.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentMode;

    #[test]
    fn walk_glob_matches_relative_patterns() {
        let dir = std::env::temp_dir().join(format!("romacode-rag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.join("src/deep/b.rs"), "fn b() {}").unwrap();
        std::fs::write(dir.join("README.md"), "# x").unwrap();
        let hits = walk_glob(&dir, "src/**/*.rs", 100);
        assert_eq!(hits.len(), 2, "{hits:?}");
        let hits = walk_glob(&dir, "*.md", 100);
        assert_eq!(hits.len(), 1, "{hits:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rag_search_without_index_is_a_clean_error() {
        let e = rag_search(&json!({"query": "hello"}));
        assert!(e.is_err() && e.unwrap_err().contains("rag_index"));
    }

    #[test]
    fn tool_bridge_shapes_openai_specs() {
        let mut cfg = Config::default();
        cfg.agent.default_mode = AgentMode::Build;
        // definitions(cfg, false) must produce OpenAI function fragments.
        let defs = crate::tools::definitions(&cfg, false);
        let read = defs.iter().find(|d| d["function"]["name"] == "Read").unwrap();
        assert!(read["function"]["parameters"].is_object());
    }
}
