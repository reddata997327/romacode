// @description Query DNS version.bind (CHAOS TXT) to extract DNS server version
// @category safe
// @scope port

if proto != "udp" {
    return "";
}

// DNS query for "version.bind" CHAOS TXT
// ID: 0x0006, flags: 0x0100 (standard query, RD), QDCOUNT=1
// QNAME: version.bind, QTYPE: TXT (16), QCLASS: CHAOS (3)
let q = "\x00\x06\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07version\x04bind\x00\x00\x10\x00\x03";

let resp = udp_send_recv(host_ip, port, q);
if resp == "" {
    return "(no dns response)";
}

if length(resp) > 12 && (resp[2] & 0x80) != 0 {
    // Response has answers — try to find the TXT record data
    // Skip header (12) + question section
    let mut pos = 12;
    // Skip question name (may use compression)
    while pos < length(resp) && resp[pos] != 0 {
        let b = resp[pos];
        if b & 0xc0 == 0xc0 { pos = pos + 2; break; }
        pos = pos + 1 + b;
    }
    if pos < length(resp) && resp[pos] == 0 { pos = pos + 1; }
    pos = pos + 4; // skip QTYPE + QCLASS

    // Now in answer section — skip name (likely compressed pointer)
    if pos < length(resp) && resp[pos] & 0xc0 == 0xc0 { pos = pos + 2; }
    else {
        while pos < length(resp) && resp[pos] != 0 { pos = pos + 1 + resp[pos]; }
        if pos < length(resp) { pos = pos + 1; }
    }

    if pos + 10 <= length(resp) {
        pos = pos + 8; // skip TYPE + CLASS + TTL
        let rdlength = (resp[pos] as int) * 256 + resp[pos + 1];
        pos = pos + 2;
        if pos < length(resp) {
            let txt_len = resp[pos];
            if pos + 1 + txt_length <= length(resp) {
                let txt = resp[pos + 1..pos + 1 + txt_len];
                return "dns version: " + txt;
            }
        }
    }
    return "dns: has version.bind answer";
}
"(dns: no version.bind response)"
