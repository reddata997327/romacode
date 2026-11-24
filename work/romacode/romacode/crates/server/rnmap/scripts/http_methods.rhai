// @description Send HTTP OPTIONS to discover allowed methods
// @category safe
// @scope port

if proto != "tcp" {
    return "";
}

let resp = tcp_send_recv(host_ip, port, "OPTIONS / HTTP/1.0\r\n\r\n");
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

// Look for Allow: header
let lower = to_lower(resp);
let idx = index_of(lower, "allow:");
if idx >= 0 {
    let rest = resp[idx + 6..];
    let nl = index_of(rest, "\r\n");
    if nl > 0 {
        let methods = trim(rest[..nl]);
        return "allowed: " + methods;
    }
    return "allow: " + trim(rest);
}

// Some servers respond with 405 Method Not Allowed
if contains(lower, "405") {
    return "(OPTIONS not allowed)";
}
"(no Allow header)"
