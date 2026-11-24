// @description Send TPKT/COTP CR, detect RDP service
// @category safe
// @scope port

// rdp_probe.rhai — send RDP X.224 Connection Request, detect RDP service
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// TPKT header (4 bytes) + COTP CR header (7 bytes) + 8 bytes
// Standard RDP probe (nmap-style): TPKT version 3, length 19, COTP code 0xe0
let probe = "\x03\x00\x00\x13\x0e\xe0\x01\x00\x00\x00\x00\x00\x01\x00\x08\x00\x03\x00\x00";
let resp = tcp_send_recv(target, port_num, probe);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

// RDP server responds with TPKT (starts 0x03) + COTP CC (code 0xd0)
if length(resp) >= 6 {
    let b0 = resp[0];
    let b1 = resp[1];
    let b2 = resp[2];
    let b3 = resp[3];
    let b5 = resp[5];
    if b0 == 3 && b5 == 0xd0 {
        let len = ((b2 as int) << 8) + (b3 as int);
        return "rdp (tpkt len=" + len + ")";
    }
    // Some RDP servers respond with negotiation response 0x01 (TYPE_RDP_NEG_RSP)
    if b0 == 1 {
        return "rdp (negotiation response)";
    }
}
"(not rdp)"
