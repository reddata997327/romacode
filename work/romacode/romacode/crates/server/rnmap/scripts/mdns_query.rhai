// @description Send mDNS query for _services._dns-sd._udp.local
// @category safe
// @scope port

// mdns_query.rhai — send mDNS query for _services._dns-sd._udp.local
// Works on UDP port 5353.
let target = host_ip;
let port_num = port;

if proto != "udp" {
    return "";
}

// DNS query: ID=0, flags=0x0100 (standard query, RD), QD=1
// QNAME=_services._dns-sd._udp.local, QTYPE=12 (PTR), QCLASS=1 (IN)
let q = "\x00\x00\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x09_services\x07_dns-sd\x04_udp\x05local\x00\x00\x0c\x00\x01";
let resp = udp_send_recv(target, port_num, q);
if resp == "" {
    return "(no mDNS response)";
}

// A DNS response has QR bit (0x80) set in flags byte 2
if length(resp) >= 12 && (resp[2] & 0x80) != 0 {
    let answers = (resp[6] as int) * 256 + (resp[7] as int);
    return "mdns: " + answers + " answer(s)";
}
"(not mdns)"
