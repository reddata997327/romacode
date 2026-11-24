// @description Check HTTP security headers (HSTS, CSP, X-Frame-Options, X-Content-Type-Options)
// @category safe
// @scope port

if proto != "tcp" {
    return "";
}

let resp = http_get(host_ip, port, "/");
if resp == "" || starts_with(resp, "ERR:") {
    return "(no response)";
}

let lower = to_lower(resp);
let mut findings = "";

// Strict-Transport-Security (HSTS)
if contains(lower, "strict-transport-security") {
    findings = findings + "HSTS ";
}

// Content-Security-Policy
if contains(lower, "content-security-policy") {
    findings = findings + "CSP ";
}

// X-Frame-Options
if contains(lower, "x-frame-options") {
    findings = findings + "XFO ";
}

// X-Content-Type-Options
if contains(lower, "x-content-type-options") {
    findings = findings + "XCTO ";
}

// Referrer-Policy
if contains(lower, "referrer-policy") {
    findings = findings + "Referrer ";
}

// Permissions-Policy
if contains(lower, "permissions-policy") {
    findings = findings + "Perms ";
}

if findings == "" {
    return "(no security headers found)";
}
trim(findings)
