// @description Fetch /robots.txt and report first 500 bytes
// @category safe
// @scope port

// http_robots.rhai — fetch /robots.txt and report its contents (truncated)
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let resp = http_get(target, port_num, "/robots.txt");
if resp == "" || starts_with(resp, "ERR:") {
    return "(no robots.txt)";
}

// Keep just the headers + first 500 chars of body
let hdr_end = index_of(resp, "\r\n\r\n");
if hdr_end < 0 {
    return resp;
}
let body = resp[hdr_end + 4..];
let truncated = if length(body) > 500 { body[..500] } else { body };
truncated
