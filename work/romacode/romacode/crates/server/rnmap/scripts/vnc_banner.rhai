// @description Detect RFB greeting banner
// @category safe
// @scope port

// vnc_banner.rhai — VNC servers send "RFB 003.00x\n" greeting immediately
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

let banner = tcp_connect(target, port_num);
if starts_with(banner, "RFB ") {
    let nl = index_of(banner, "\n");
    if nl > 0 {
        return banner[..nl];
    }
    return banner;
}
"(not vnc)"
