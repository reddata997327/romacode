// @description Probe MongoDB wire protocol
// @category intrusive
// @scope port

// mongodb_probe.rhai — check if a port is MongoDB by sending isMaster command
// MongoDB uses binary BSON wire protocol; a minimal ismaster probe.
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// We can't easily build BSON from Rhai, so do a passive banner grab.
// MongoDB does NOT send a greeting, so tcp_connect returns empty.
// Instead send 4 bytes of garbage and see if the server closes vs replies with an error.
let banner = tcp_send_recv(target, port_num, "\x00\x00\x00\x00");
if banner == "" {
    return "(silent — likely mongodb or filtered)";
}

// If server replies with a structured error containing "mongo" or "$err" or "errmsg"
let lower = to_lower(banner);
if contains(lower, "mongo") || contains(lower, "$err") || contains(lower, "errmsg") || contains(lower, "ok") {
    return "(mongodb-like response)";
}
"(not mongodb)"
