// @description Send NTP v3 request to extract server version and stratum
// @category safe
// @scope port

if proto != "udp" {
    return "";
}

// NTP v3 client request: LI=0, VN=3, Mode=3 (client), 48 bytes total
let mut req = "\x1b";
let mut i = 0;
while i < 47 {
    req = req + "\x00";
    i = i + 1;
}

let resp = udp_send_recv(host_ip, port, req);
if resp == "" {
    return "(no ntp response)";
}

if length(resp) >= 48 {
    // First byte: LI(2) VN(3) Mode(3)
    let b0 = resp[0];
    let mode = b0 & 0x07;
    let vn = (b0 >> 3) & 0x07;
    let li = (b0 >> 6) & 0x03;

    // Stratum is at offset 1
    let stratum = resp[1];

    // Reference ID at offset 12
    let ref_id = "";
    if length(resp) >= 16 {
        ref_id = format(resp[12]) + "." + format(resp[13]) + "." + format(resp[14]) + "." + format(resp[15]);
    }

    let li_str = if li == 0 { "none" } else if li == 1 { "last-minute-61" } else if li == 2 { "last-minute-59" } else { "alarm" };
    return "ntp v" + vn + " stratum=" + stratum + " li=" + li_str + " ref_id=" + ref_id;
}
"(not ntp)"
