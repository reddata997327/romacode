// @description Probe Apache mod_status /server-status endpoint
// @category safe
// @scope port

// http_server_status.rhai — try Apache mod_status endpoint /server-status
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let resp = http_get(target, port_num, "/server-status");
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

// Detect the status page by checking if body mentions "Apache Status" or "Server uptime"
let lower = to_lower(resp);
if contains(lower, "apache status") || contains(lower, "server uptime") {
    let hdr_end = index_of(resp, "\r\n\r\n");
    if hdr_end > 0 {
        let body = resp[hdr_end + 4..];
        let cut = if length(body) > 400 { 400 } else { length(body) };
        return "ENABLED: " + body[..cut];
    }
}
"(not enabled)"
