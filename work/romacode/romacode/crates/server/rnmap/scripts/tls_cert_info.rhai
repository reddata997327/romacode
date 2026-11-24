// @description Send TLS ClientHello, detect ServerHello version
// @category safe
// @scope port

// tls_cert_info.rhai — attempt a minimal TLS ClientHello and report server hello
// Pure detection: we send a ClientHello and check if the response starts with 0x16 (Handshake)
// and 0x01 (ServerHello). Full cert parsing is left to the agent.
let target = host_ip;
let port_num = port;

if proto != "tcp" {
    return "";
}

// Minimal TLS 1.0 ClientHello:
// record layer: 0x16 (handshake), 0x03 0x01 (TLS 1.0), length 0x0027
// handshake: 0x01 (ClientHello), length 0x000023, version 0x03 0x01
// random (32 bytes), session_id_len=0, cipher_suites (4 bytes: TLS_RSA_WITH_AES_128_CBC_SHA + TLS_EMPTY_RENEGOTIATION_INFO),
// compression_methods (1 byte: 0x00)
let client_hello = "\x16\x03\x01\x00\x27\x01\x00\x00\x23\x03\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x2f\x00\xff\x01\x00";
let resp = tcp_send_recv(target, port_num, client_hello);
if resp == "" || starts_with(resp, "ERR:") {
    return "(no tls response)";
}

// TLS ServerHello record starts with 0x16 (Handshake), 0x03 0x0X (version)
if length(resp) >= 6 {
    let b0 = resp[0];
    let b5 = resp[5];
    if b0 == 0x16 && b5 == 0x02 {
        // ServerHello — pick out the version
        let v_hi = resp[9];
        let v_lo = resp[10];
        let v = if v_hi == 3 && v_lo == 1 { "TLS 1.0" }
                else if v_hi == 3 && v_lo == 2 { "TLS 1.1" }
                else if v_hi == 3 && v_lo == 3 { "TLS 1.2" }
                else if v_hi == 3 && v_lo == 4 { "TLS 1.3" }
                else { "TLS unknown" };
        return "tls serverhello: " + v;
    }
    // TLS 1.3 might respond with 0x17 (ApplicationData) right away
    if b0 == 0x17 {
        return "tls serverhello: TLS 1.3 (encrypted)";
    }
    // Alert record 0x15
    if b0 == 0x15 {
        return "tls alert (rejected)";
    }
}
"(not tls)"
