// End-to-end smoke for the remix integration: binary discovery, the doctor,
// the chain-write gate, an offline command, live mainnet reads and the daemon
// refusal. Runs against the REAL network — run it manually:
//   cargo run -p romacode-server --example remix_smoke
// (needs an rrx binary reachable via the discovery order, see remix_status).
use romacode_server::config::Config;
use romacode_server::remix;
use std::sync::atomic::AtomicBool;

fn main() {
    let cfg = Config::default();
    let cancel = AtomicBool::new(false);
    println!("=== discovery: {:?}", remix::ensure_binary(&cfg));
    println!("=== doctor ===\n{}", remix::status_execute(&cfg, &serde_json::json!({})).unwrap());
    println!("=== chain write refusal (flag off) ===");
    let r = remix::execute(&cfg, &serde_json::json!({"command":"send","args":["--to","vitalik.eth","--value","0.001"]}), &cancel);
    println!("{}", r.unwrap_err());
    println!("=== utils keccak (offline) ===");
    let r = remix::execute(&cfg, &serde_json::json!({"command":"utils","args":["keccak","romacode-remix"]}), &cancel);
    println!("{}", r.unwrap());
    println!("=== live mainnet chain (--json) ===");
    let r = remix::execute(&cfg, &serde_json::json!({"command":"chain","args":[],"timeout_secs":60}), &cancel);
    println!("{}", r.unwrap_or_else(|e| format!("ERR: {e}")));
    println!("=== live account vitalik.eth ===");
    let r = remix::execute(&cfg, &serde_json::json!({"command":"account","args":["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"],"timeout_secs":60}), &cancel);
    println!("{}", r.unwrap_or_else(|e| format!("ERR: {e}")));
    println!("=== daemon refusal ===");
    let r = remix::execute(&cfg, &serde_json::json!({"command":"explorer"}), &cancel);
    println!("{}", r.unwrap_err());
}
