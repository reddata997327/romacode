// @description Send INFO server, extract redis_version
// @category intrusive
// @scope port

// redis_info.rhai — connect to Redis, send INFO, parse version line
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let info = tcp_send_recv(target, port_num, "INFO server\r\n");
if info == "" || starts_with(info, "ERR:") {
    return "(no redis)";
}

// Look for redis_version: line
let lower = to_lower(info);
let idx = index_of(lower, "redis_version:");
if idx < 0 {
    return "(not redis or no auth)";
}
let rest = info[idx + 14..];
let nl = index_of(rest, "\r\n");
if nl > 0 {
    let ver = rest[..nl];
    return "redis_version=" + ver;
}
"redis_version=" + rest
