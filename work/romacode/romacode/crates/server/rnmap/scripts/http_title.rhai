// @description Extract <title> from HTTP response
// @category safe
// @scope port

// http_title.rhai — extract <title> from HTTP response
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let resp = http_get(target, port_num, "/");
if resp == "" || starts_with(resp, "ERR:") {
    return "";
}

// find <title> ... </title>
let lower = to_lower(resp);
let idx = index_of(lower, "<title>");
if idx < 0 {
    return "(no title)";
}
let start = idx + 7;
let end = index_of(lower[start..], "</title>");
if end < 0 {
    return "(malformed)";
}
let title = resp[start..start + end];
let trimmed = trim(title);
trimmed
