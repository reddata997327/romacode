// @description Send PostgreSQL startup probe, detect rejection
// @category intrusive
// @scope port

// postgres_probe.rhai — send PostgreSQL startup probe and detect rejection
// An unauthenticated startup gets an ErrorResponse from a real Postgres server.
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// Build a minimal startup message:
//   [4 bytes length][4 bytes protocol = 196608][user\x00root\x00\x00]
let payload = "\x00\x00\x00\x29\x00\x03\x00\x00user\x00root\x00\x00";
let resp = tcp_send_recv(target, port_num, payload);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

// Postgres ErrorResponse starts with 'E' then 4 bytes length
if length(resp) >= 5 {
    let tag = resp[0];
    if tag == 'E' as int || tag == 69 {
        // find "SFATAL" or "C" fields — just return first 200 chars
        let cut = if length(resp) > 200 { 200 } else { length(resp) };
        return "postgres (rejected): " + resp[..cut];
    }
    if tag == 'R' as int || tag == 82 {
        return "(postgres: auth requested)";
    }
}
"(not postgres)"
