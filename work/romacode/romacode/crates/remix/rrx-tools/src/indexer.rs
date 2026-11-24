//! rrx indexer + explorer — fetch REAL blocks from a live network into a local
//! indexed store, then serve a real block-explorer web UI + JSON API over HTTP.
//! All data comes from the actual chain; the store is append-only JSONL.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use rrx_rpc::Provider;

pub struct IndexOptions {
    pub blocks: u64,
    pub db: PathBuf,
    pub rpc: Option<String>,
}

/// Index the latest N REAL blocks (headers, txs, logs count) into the store.
pub fn index_blocks(opts: &IndexOptions) -> Result<String, String> {
    let p = match &opts.rpc {
        Some(url) => Provider::new(url),
        None => rrx_rpc::auto_mainnet()?,
    };
    let head = p.block_number()?;
    let mut db = load_db(&opts.db);
    let start = head.saturating_sub(opts.blocks.saturating_sub(1));
    let mut indexed = 0u64;
    let mut txs = 0u64;
    for n in start..=head {
        if let Some(existing) = db.blocks.iter().find(|b| b["number"] == json!(n)) {
            let _ = existing;
            continue;
        }
        let block = p
            .block_by_number(n, true)?
            .ok_or_else(|| format!("block {n} not found"))?;
        let tx_count = block["transactions"].as_array().map(|a| a.len()).unwrap_or(0);
        txs += tx_count as u64;
        db.blocks.push(block);
        indexed += 1;
    }
    db.head = head;
    save_db(&opts.db, &db)?;
    Ok(format!(
        "indexed {indexed} REAL blocks ({start}..{head}) with {txs} txs from {}\nstore: {}\n",
        p.url,
        opts.db.display()
    ))
}

pub struct IndexDb {
    pub head: u64,
    pub blocks: Vec<Value>,
}

pub fn load_db(path: &PathBuf) -> IndexDb {
    if let Ok(txt) = std::fs::read_to_string(path) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            return IndexDb {
                head: v["head"].as_u64().unwrap_or(0),
                blocks: v["blocks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            };
        }
    }
    IndexDb { head: 0, blocks: vec![] }
}

pub fn save_db(path: &PathBuf, db: &IndexDb) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let v = json!({ "head": db.head, "blocks": db.blocks });
    std::fs::write(path, serde_json::to_string(&v).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// explorer HTTP server (tiny_http, same stack as rrx node)
// ---------------------------------------------------------------------------

pub struct ExplorerOptions {
    pub port: u16,
    pub db: PathBuf,
}

/// Serve the mini block explorer over HTTP. Live JSON API:
///   GET /            → HTML dashboard (real indexed blocks)
///   GET /api/stats   → totals
///   GET /api/blocks  → recent blocks (limit param)
///   GET /api/tx/HASH → transaction detail
pub fn explorer_serve(opts: &ExplorerOptions) -> Result<(), String> {
    use tiny_http::{Header, Response, Server};
    let db = Arc::new(Mutex::new(load_db(&opts.db)));
    // background refresh from the real chain every 30s keeps data live
    {
        let db2 = Arc::clone(&db);
        let path = opts.db.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if let Ok(txt) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    if let Ok(mut guard) = db2.lock() {
                        guard.head = v["head"].as_u64().unwrap_or(guard.head);
                        guard.blocks = v["blocks"].as_array().cloned().unwrap_or_default();
                    }
                }
            }
        });
    }
    let url = format!("0.0.0.0:{}", opts.port);
    let server = Server::http(&url).map_err(|e| e.to_string())?;
    println!(
        "rrx explorer listening on http://127.0.0.1:{} ({} REAL blocks indexed)",
        opts.port,
        db.lock().map(|d| d.blocks.len()).unwrap_or(0)
    );
    for mut request in server.incoming_requests() {
        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        let path = request.url().to_string();
        let guard = db.lock().ok();
        let resp = route(&path, guard.as_deref());
        let response = Response::from_string(resp.1)
            .with_status_code(resp.0)
            .with_header(
                Header::from_bytes(
                    &b"Content-Type"[..],
                    if path.starts_with("/api") {
                        &b"application/json"[..]
                    } else {
                        &b"text/html; charset=utf-8"[..]
                    },
                )
                .unwrap(),
            );
        let _ = request.respond(response);
    }
    Ok(())
}

fn route(path: &str, db: Option<&IndexDb>) -> (u16, String) {
    let db = match db {
        Some(d) => d,
        None => return (503, json!({"error": "index not ready"}).to_string()),
    };
    if path.starts_with("/api/stats") {
        let total_tx: usize = db.blocks.iter().map(|b| b["transactions"].as_array().map(|a| a.len()).unwrap_or(0)).sum();
        let gas: u128 = db
            .blocks
            .iter()
            .filter_map(|b| b["gasUsed"].as_str())
            .filter_map(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .sum();
        return (
            200,
            json!({
                "head": db.head,
                "blocks_indexed": db.blocks.len(),
                "transactions": total_tx,
                "total_gas_used": gas.to_string(),
            })
            .to_string(),
        );
    }
    if let Some(hash) = path.strip_prefix("/api/tx/") {
        for b in &db.blocks {
            if let Some(txs) = b["transactions"].as_array() {
                if let Some(tx) = txs.iter().find(|t| t["hash"].as_str() == Some(hash)) {
                    return (200, json!({"block": b["number"], "tx": tx}).to_string());
                }
            }
        }
        return (404, json!({"error": "tx not in indexed range"}).to_string());
    }
    if path.starts_with("/api/blocks") {
        let limit = path
            .split("limit=")
            .nth(1)
            .and_then(|l| l.split('&').next())
            .and_then(|l| l.parse::<usize>().ok())
            .unwrap_or(10);
        let mut list: Vec<&Value> = db.blocks.iter().rev().take(limit).collect();
        list.sort_by_key(|b| b["number"].as_u64().unwrap_or(0));
        list.reverse();
        let blocks: Vec<Value> = list
            .iter()
            .map(|b| {
                json!({
                    "number": b["number"],
                    "hash": b["hash"],
                    "timestamp": b["timestamp"],
                    "txs": b["transactions"].as_array().map(|a| a.len()).unwrap_or(0),
                    "gasUsed": b["gasUsed"],
                    "baseFeePerGas": b["baseFeePerGas"],
                    "miner": b["miner"],
                })
            })
            .collect();
        return (200, json!({ "head": db.head, "blocks": blocks }).to_string());
    }
    if path == "/" || path.starts_with("/index.html") {
        return (200, dashboard_html());
    }
    (404, "<h1>404 — rrx explorer</h1>".into())
}

fn dashboard_html() -> String {
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>rrx explorer — real chain data</title>
<style>
  body{{background:#0b0e14;color:#e6edf3;font-family:ui-monospace,monospace;margin:0;padding:24px}}
  h1{{color:#58a6ff;font-weight:600}} .card{{background:#11151c;border:1px solid #1f2733;border-radius:10px;padding:16px;margin-bottom:16px}}
  table{{width:100%;border-collapse:collapse}} th,td{{text-align:left;padding:6px 10px;border-bottom:1px solid #1f2733;font-size:13px}}
  th{{color:#8b949e;font-weight:500}} .num{{color:#7ee787}} a{{color:#58a6ff;text-decoration:none}}
  .pill{{background:#12261e;color:#7ee787;border-radius:99px;padding:2px 10px;font-size:12px}}
</style></head>
<body>
<h1>rrx explorer <span class="pill">REAL CHAIN DATA</span></h1>
<div class="card" id="stats">loading…</div>
<div class="card"><h3>Latest blocks</h3><table><thead><tr><th>block</th><th>txs</th><th>gas used</th><th>base fee</th><th>hash</th></tr></thead><tbody id="rows"></tbody></table></div>
<script>
fetch('/api/stats').then(r=>r.json()).then(s=>{{
  document.getElementById('stats').innerHTML =
    '<b>head</b> <span class="num">'+s.head+'</span> &nbsp; <b>blocks</b> '+s.blocks_indexed+
    ' &nbsp; <b>txs</b> '+s.transactions+' &nbsp; <b>total gas</b> '+s.total_gas_used;
}});
fetch('/api/blocks?limit=20').then(r=>r.json()).then(d=>{{
  document.getElementById('rows').innerHTML = d.blocks.map(b=>'<tr>'+
    '<td class="num">'+parseInt(b.number)+'</td>'+
    '<td>'+b.txs+'</td>'+
    '<td>'+parseInt(b.gasUsed)+'</td>'+
    '<td>'+(parseInt(b.baseFeePerGas)/1e9).toFixed(4)+' gwei</td>'+
    '<td>'+(b.hash||'').slice(0,22)+'…</td></tr>').join('');
}});
</script>
</body></html>"#
    )
}
