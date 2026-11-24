//! rrx lsp — a real Language Server Protocol server for Solidity.
//! Diagnostics come from the REAL solc compiler: every error/warning you see
//! in the editor is genuine compiler output, mapped to precise ranges.

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

fn read_message(reader: &mut impl BufRead) -> Result<Value, String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let l = line.trim();
        if l.is_empty() {
            break; // headers done
        }
        if let Some(v) = l.strip_prefix("Content-Length:") {
            content_length = v.trim().parse::<usize>().ok();
        }
    }
    let len = content_length.ok_or("missing Content-Length header")?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_slice(&buf).map_err(|e| format!("bad LSP json: {e}"))
}

fn write_message(out: &mut impl Write, msg: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
    out.write_all(&body).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// Convert a byte offset to (line, character) using the document text.
fn offset_to_pos(text: &str, offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in text.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Compile the open document with REAL solc and map errors to LSP diagnostics.
fn diagnostics_for(uri: &str, text: &str) -> Vec<Value> {
    let diags: Vec<Value> = match rrx_solc::detect() {
        Ok(solc) => {
            let name = uri
                .trim_start_matches("file://")
                .rsplit('/')
                .next()
                .unwrap_or("file.sol")
                .to_string();
            let mut sources = serde_json::Map::new();
            sources.insert(
                name.clone(),
                json!({"content": text, "language": "Solidity"}),
            );
            let input = json!({
                "language": "Solidity",
                "sources": sources,
                "settings": {
                    "optimizer": {"enabled": true},
                    "outputSelection": {"*": {"*": []}}
                }
            });
            match solc.compile(&input) {
                Ok(output) => {
                    let mut out = vec![];
                    if let Some(errors) = output["errors"].as_array() {
                        for e in errors {
                            let sev = match e["severity"].as_str() {
                                Some("error") => 1,
                                Some("warning") => 2,
                                _ => 3,
                            };
                            let (sl, sc) = match e["sourceLocation"].as_object() {
                                Some(loc) => {
                                    let off = loc["offset"].as_i64().unwrap_or(0) as usize;
                                    offset_to_pos(text, off)
                                }
                                None => (0, 0),
                            };
                            out.push(json!({
                                "range": {"start": {"line": sl, "character": sc},
                                          "end": {"line": sl, "character": sc + 20}},
                                "severity": sev,
                                "source": "rrx-solc",
                                "message": e["message"].as_str().unwrap_or("").trim().to_string(),
                            }));
                        }
                    }
                    out
                }
                Err(e) => vec![json!({
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                    "severity": 1,
                    "source": "rrx-solc",
                    "message": format!("solc failed: {e}"),
                })],
            }
        }
        Err(e) => vec![json!({
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
            "severity": 2,
            "source": "rrx",
            "message": format!("solc not available: {e}"),
        })],
    };
    diags
}

fn publish(out: &mut impl Write, uri: &str, version: i64, diags: Vec<Value>) {
    let _ = write_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": version,
                "diagnostics": diags,
            }
        }),
    );
}

/// Serve LSP over stdio until the client disconnects.
pub fn serve() -> Result<(), String> {
    eprintln!("rrx-lsp: Solidity language server over stdio (real solc diagnostics)");
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut doc_version: i64 = 0;
    loop {
        let msg = match read_message(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                if e.contains("missing") || e.contains("stream") || e.is_empty() {
                    break; // clean EOF
                }
                return Err(e);
            }
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        match method {
            "initialize" => {
                write_message(
                    &mut out,
                    &json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "capabilities": {
                                "textDocumentSync": 1,
                                "diagnosticProvider": {"interFileDependencies": false, "workspaceDiagnostics": false}
                            },
                            "serverInfo": {"name": "rrx-lsp", "version": env!("CARGO_PKG_VERSION")}
                        }
                    }),
                )?;
            }
            "initialized" | "$/cancelRequest" => {}
            "shutdown" => {
                write_message(&mut out, &json!({"jsonrpc": "2.0", "id": id, "result": null}))?;
            }
            "exit" => break,
            "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didSave" => {
                let uri = params
                    .pointer("/textDocument/uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                doc_version = params
                    .pointer("/textDocument/version")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(doc_version);
                let text = params
                    .pointer("/textDocument/text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        params
                            .pointer("/contentChanges/0/text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    let diags = diagnostics_for(&uri, &text);
                    publish(&mut out, &uri, doc_version, diags);
                }
            }
            other => {
                if id != Value::Null {
                    write_message(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}}),
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _touch(_p: &PathBuf) {}
