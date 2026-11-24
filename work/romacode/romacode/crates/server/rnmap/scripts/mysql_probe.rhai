// @description Parse MySQL greeting packet
// @category intrusive
// @scope port

// mysql_probe.rhai — send MySQL greeting handshake probe
// On an open MySQL port, the server sends a greeting packet first.
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// MySQL server sends the greeting without us sending anything;
// tcp_connect() reads up to 1024 bytes of banner.
let banner = tcp_connect(target, port_num);

if length(banner) < 4 {
    return "(no greeting)";
}

// MySQL greeting packet format:
//   [3 bytes len][1 byte seq][1 byte protocol_version][NUL-term server version]
let len = (banner[0] as int) + ((banner[1] as int) << 8) + ((banner[2] as int) << 16);
if len < 6 || len > 1000 {
    return "(not mysql)";
}

// protocol version is at offset 4
let proto_v = banner[4];
// version string starts at offset 5, NUL-terminated
let v_start = 5;
let v_end = index_of(banner[v_start..], "\x00");
if v_end < 0 {
    return "(no version nul)";
}
let ver = banner[v_start..v_start + v_end];
"MySQL proto=" + proto_v + " version=" + ver
