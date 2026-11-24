// @description Enumerate SMB shares via SMB1 negotiate + tree connect
// @category intrusive
// @scope port

// Send SMB1 negotiate to detect SMB version, then report
if proto != "tcp" {
    return "";
}

let nbu = "\x00\x00\x00\x86";
let smbh = "\xff\x53\x4d\x42\x72\x00\x00\x00\x00\x18\x53\xc8\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
let dialect = "\x02\x00PC NETWORK PROGRAM 1.0\x00";
let payload = nbu + smbh + dialect;

let resp = tcp_send_recv(host_ip, port, payload);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no smb response)";
}

if length(resp) >= 4 {
    let b0 = resp[0];
    let b1 = resp[1];
    let b2 = resp[2];
    let b3 = resp[3];
    if b0 == 0xff && b1 == 'S' as int && b2 == 'M' as int && b3 == 'B' as int {
        // SMB1 response — check if it negotiated SMB2 via dialect index
        if length(resp) > 65 {
            let dialect_idx = (resp[65] as int);
            return "smb1 (negotiated dialect index=" + dialect_idx + ")";
        }
        return "smb1 (negotiated)";
    }
    if b0 == 0xfe && b1 == 'S' as int && b2 == 'M' as int && b3 == 'B' as int {
        return "smb2 (negotiated)";
    }
}
"(not smb)"
