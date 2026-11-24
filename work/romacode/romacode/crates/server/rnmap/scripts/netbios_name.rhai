// @description Query NetBIOS name service (port 137 UDP) for host names
// @category safe
// @scope port

if proto != "udp" {
    return "";
}

// NetBIOS Name Service (NBNS) Node Status Request
// Transaction ID: 0x1337, flags: 0x0010 (broadcast, recursion desired)
// QDCOUNT=0, ANCOUNT=0, NSCOUNT=0, ARCOUNT=1
// Question: "*" (wildcard) type NBSTAT (0x21) class IN (0x01)
let query = "\x13\x37\x00\x10\x00\x00\x00\x00\x00\x00\x00\x01\x20\x43\x4b\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x00\x00\x21\x00\x01";

let resp = udp_send_recv(host_ip, port, query);
if resp == "" {
    return "(no netbios response)";
}

if length(resp) >= 57 {
    // The response contains the number of names at offset 56
    let num_names = resp[56];
    let mut names = "";
    let mut i = 0;
    let mut off = 57;
    while i < num_names && off + 18 <= length(resp) {
        let name = resp[off..off + 15];
        let trimmed = trim(name);
        if i > 0 { names = names + ", "; }
        names = names + trimmed;
        i = i + 1;
        off = off + 18;
    }
    if names != "" {
        return "netbios: " + num_names + " names: " + names;
    }
    return "netbios: " + num_names + " name(s)";
}
"(not netbios)"
