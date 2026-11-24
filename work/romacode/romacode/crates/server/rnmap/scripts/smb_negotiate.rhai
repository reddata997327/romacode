// @description SMB1/SMB2 negotiate protocol request
// @category intrusive
// @scope port

// smb_negotiate.rhai — send SMB1 negotiate protocol request
// Real SMB servers reply with a negotiate response.
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// SMB1 Negotiate Protocol Request, NetBIOS session service header
// \xffSMB header + 12 zero bytes + dialect "PC NETWORK PROGRAM 1.0"
// Length 134 = 0x86 in NetBIOS length (3-byte field).
let nbu = "\x00\x00\x00\x86";
let smbh = "\xff\x53\x4d\x42\x72\x00\x00\x00\x00\x18\x53\xc8\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
let dialect = "\x02\x00PC NETWORK PROGRAM 1.0\x00";
let payload = nbu + smbh + dialect;

let resp = tcp_send_recv(target, port_num, payload);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

// SMB1 response starts with \xffSMB or \xfeSMB (SMB2)
if length(resp) >= 4 {
    let b0 = resp[0];
    let b1 = resp[1];
    let b2 = resp[2];
    let b3 = resp[3];
    if b0 == 0xff && b1 == 'S' as int && b2 == 'M' as int && b3 == 'B' as int {
        return "smb1 (negotiated)";
    }
    if b0 == 0xfe && b1 == 'S' as int && b2 == 'M' as int && b3 == 'B' as int {
        return "smb2 (negotiated)";
    }
}
"(not smb)"
