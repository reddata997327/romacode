// @description Extract SSH version banner string
// @category safe
// @scope port

// ssh_version.rhai — extract SSH banner version string
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let banner = tcp_connect(target, port_num);
if starts_with(banner, "SSH-") {
    let nl = index_of(banner, "\n");
    if nl > 0 {
        return banner[..nl];
    }
    return banner;
}
"(not ssh)"
