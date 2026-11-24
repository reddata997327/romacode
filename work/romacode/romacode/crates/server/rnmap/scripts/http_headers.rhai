// @description Dump HTTP response headers
// @category safe
// @scope port

// http_headers.rhai — dump HTTP response headers
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let resp = http_get(target, port_num, "/");
if resp == "" {
    return "";
}

// headers end at first blank line
let hdr_end = index_of(resp, "\r\n\r\n");
if hdr_end < 0 {
    return resp;
}
resp[..hdr_end]
