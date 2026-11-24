// @description Passive TCP banner grab
// @category safe
// @scope port

// banner_grab.rhai — passive banner grab on TCP port
// Runs at port scope. Returns the raw banner text or empty string.

let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let banner = tcp_connect(target, port_num);
if banner == "" {
    return "(no banner)";
}
banner
