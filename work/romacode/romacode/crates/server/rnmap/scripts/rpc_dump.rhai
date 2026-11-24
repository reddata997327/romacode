// @description Dump RPC programs via RPCBIND (portmap) v2 CALLIT
// @category intrusive
// @scope port

if proto != "tcp" {
    return "";
}

// RPCBIND v2 DUMP request
// XID: 0x00000001, message type: CALL (0), RPC version: 2,
// program: 100000 (rpcbind), version: 2, procedure: 4 (DUMP)
let rpc = "\x00\x00\x00\x28\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x02\x00\x01\x86\xa0\x00\x00\x00\x02\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
// Add FRAG header for TCP: last fragment + length
let frag = "\x80\x00\x00\x28";
let payload = frag + rpc[4..];

let resp = tcp_send_recv(host_ip, port, payload);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no rpc response)";
}

if length(resp) >= 8 {
    // Check if response is an ACCEPTED reply
    let rtype = resp[8];
    if rtype == 1 {
        let prog_count = (resp[24] as int) * 256 + resp[25];
        return "rpcbind: ~" + prog_count + " program(s) listed";
    }
}
"(not rpcbind or no programs)"
