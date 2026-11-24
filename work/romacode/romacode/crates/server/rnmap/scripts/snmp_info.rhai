// @description SNMPv1 GetRequest for sysDescr.0
// @category safe
// @scope port

// snmp_info.rhai — send SNMPv1 GetRequest for sysDescr.0
// Works on UDP port 161.
let target = host_ip;
let port_num = port;

if proto != "udp" {
    return "";
}

// BER-encoded SNMPv1 GetRequest for OID 1.3.6.1.2.1.1.1.0 (sysDescr.0)
// Community = "public"
let probe = "\x30\x26\x02\x01\x00\x04\x06\x70\x75\x62\x6c\x69\x63\xa0\x19\x02\x01\x01\x02\x01\x00\x02\x01\x00\x30\x0d\x30\x0b\x06\x07\x2b\x06\x01\x02\x01\x01\x01\x00\x05\x00";
let resp = udp_send_recv(target, port_num, probe);
if resp == "" {
    return "(no snmp response)";
}

// SNMP responses start with 0x30 (sequence) and contain the sysDescr octet string
// We can't easily parse BER from Rhai, so just report the response length and
// try to extract any printable substring.
let lower = to_lower(resp);
let n = length(resp);
// Find printable run >= 4 chars
let mut i = 0;
let mut out = "";
while i < n {
    let c = resp[i];
    if c >= 32 && c < 127 {
        let mut j = i;
        while j < n {
            let cj = resp[j];
            if cj < 32 || cj >= 127 { break; }
            j = j + 1;
        }
        if j - i >= 4 {
            out = resp[i..j];
            break;
        }
        i = j;
    } else {
        i = i + 1;
    }
}
if out == "" {
    return "snmp: " + n + " bytes (no printable)";
}
"snmp sysDescr: " + out
