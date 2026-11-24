//! Embedded Rhai service-probe scripts shipped with the vendored rnmap
//! distribution (see `crates/server/rnmap/README.md` for provenance).
//! One table entry per script, embedded verbatim via `include_bytes!`.

/// (script name, contents) for every script in the vendored distribution.
pub const SCRIPTS: &[(&str, &[u8])] = &[
    ("banner_grab.rhai", include_bytes!("../../rnmap/scripts/banner_grab.rhai")),
    ("dns_version.rhai", include_bytes!("../../rnmap/scripts/dns_version.rhai")),
    ("http_headers.rhai", include_bytes!("../../rnmap/scripts/http_headers.rhai")),
    ("http_methods.rhai", include_bytes!("../../rnmap/scripts/http_methods.rhai")),
    ("http_robots.rhai", include_bytes!("../../rnmap/scripts/http_robots.rhai")),
    ("http_security_headers.rhai", include_bytes!("../../rnmap/scripts/http_security_headers.rhai")),
    ("http_server_status.rhai", include_bytes!("../../rnmap/scripts/http_server_status.rhai")),
    ("http_title.rhai", include_bytes!("../../rnmap/scripts/http_title.rhai")),
    ("mdns_query.rhai", include_bytes!("../../rnmap/scripts/mdns_query.rhai")),
    ("mongodb_probe.rhai", include_bytes!("../../rnmap/scripts/mongodb_probe.rhai")),
    ("mysql_probe.rhai", include_bytes!("../../rnmap/scripts/mysql_probe.rhai")),
    ("netbios_name.rhai", include_bytes!("../../rnmap/scripts/netbios_name.rhai")),
    ("ntp_version.rhai", include_bytes!("../../rnmap/scripts/ntp_version.rhai")),
    ("postgres_probe.rhai", include_bytes!("../../rnmap/scripts/postgres_probe.rhai")),
    ("rdp_probe.rhai", include_bytes!("../../rnmap/scripts/rdp_probe.rhai")),
    ("redis_info.rhai", include_bytes!("../../rnmap/scripts/redis_info.rhai")),
    ("rpc_dump.rhai", include_bytes!("../../rnmap/scripts/rpc_dump.rhai")),
    ("smb_enumerate.rhai", include_bytes!("../../rnmap/scripts/smb_enumerate.rhai")),
    ("smb_negotiate.rhai", include_bytes!("../../rnmap/scripts/smb_negotiate.rhai")),
    ("snmp_info.rhai", include_bytes!("../../rnmap/scripts/snmp_info.rhai")),
    ("ssh_algorithms.rhai", include_bytes!("../../rnmap/scripts/ssh_algorithms.rhai")),
    ("ssh_version.rhai", include_bytes!("../../rnmap/scripts/ssh_version.rhai")),
    ("tls_cert_info.rhai", include_bytes!("../../rnmap/scripts/tls_cert_info.rhai")),
    ("vnc_banner.rhai", include_bytes!("../../rnmap/scripts/vnc_banner.rhai")),
];

/// Total vendored script count (used by tests and the installer).
pub const SCRIPT_COUNT: usize = 24;