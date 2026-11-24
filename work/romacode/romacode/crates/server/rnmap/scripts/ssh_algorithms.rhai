// @description Parse SSH banner to extract supported algorithm lists
// @category safe
// @scope port

if proto != "tcp" {
    return "";
}

let banner = tcp_connect(host_ip, port);
if !starts_with(banner, "SSH-") {
    return "(not ssh)";
}

// The SSH banner is: SSH-2.0-<software_version>[_<comment>]
// Some servers append algorithm hints after the banner
let nl = index_of(banner, "\n");
let first_line = if nl > 0 { banner[..nl] } else { banner };

// Extract the software version string
let parts = split(first_line, "-");
if length(parts) < 3 {
    return first_line;
}

let ver_and_comment = parts[2];
// Split on space to get version vs extra
let sp = index_of(ver_and_comment, " ");
let ver = if sp > 0 { ver_and_comment[..sp] } else { ver_and_comment };

// Try to extract algorithm hints from the full banner (some servers send them)
let lower = to_lower(banner);
let mut algos = "";

if contains(lower, "curve25519-sha256") { algos = algos + "curve25519 "; }
if contains(lower, "ecdh-sha2-nistp256") { algos = algos + "ecdh256 "; }
if contains(lower, "ecdh-sha2-nistp384") { algos = algos + "ecdh384 "; }
if contains(lower, "aes128-gcm") { algos = algos + "aes128gcm "; }
if contains(lower, "aes256-gcm") { algos = algos + "aes256gcm "; }
if contains(lower, "chacha20-poly1305") { algos = algos + "chacha20 "; }
if contains(lower, "rsa-sha2-256") { algos = algos + "rsa-sha2-256 "; }
if contains(lower, "ed25519") { algos = algos + "ed25519 "; }

let result = "ssh_version=" + ver;
if algos != "" {
    result + " algos=[" + trim(algos) + "]"
} else {
    result
}
