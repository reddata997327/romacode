//! Parses the chisel `<remote>` spec.
//!
//! Syntax:
//!   <local-host>:<local-port>:<remote-host>:<remote-port>/<proto>
//!   R:<local-interface>:<local-port>:<remote-host>:<remote-port>/<proto>
//!   stdio:<remote-host>:<remote-port>   (client stdio -> remote)
//!
//! Defaults:
//!   local-host  = 0.0.0.0
//!   local-port  = remote-port
//!   remote-host = 127.0.0.1
//!   proto       = tcp

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn from_str(s: &str) -> Option<Proto> {
        match s.to_ascii_lowercase().as_str() {
            "tcp" => Some(Proto::Tcp),
            "udp" => Some(Proto::Udp),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub reverse: bool,
    pub local_host: String,  // "stdio" is special
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub proto: Proto,
}

impl Remote {
    pub fn parse(spec: &str) -> Result<Remote, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty remote".into());
        }
        let (reverse, rest) = if let Some(rest) = spec.strip_prefix("R:") {
            (true, rest)
        } else if spec == "R" {
            return Err("reverse remote without body".into());
        } else {
            (false, spec)
        };

        // split off protocol suffix
        let (rest, proto) = match rest.rsplit_once('/') {
            Some((l, p)) => (l, Proto::from_str(p).ok_or_else(|| format!("unknown proto: {}", p))?),
            None => (rest, Proto::Tcp),
        };

        let parts: Vec<&str> = rest.split(':').collect();
        let (local_host, local_port, remote_host, remote_port);

        match parts.len() {
            // "remote-port"  (forward, port-only)
            1 => {
                local_host = "0.0.0.0".into();
                remote_port = parse_port(parts[0])?;
                local_port = remote_port;
                remote_host = "127.0.0.1".into();
            }
            // "remote-host:remote-port" or "local-port:remote-port"
            2 => {
                if parts[0].parse::<u16>().is_ok() {
                    // first is a port number -> local-port:remote-port
                    local_host = "0.0.0.0".into();
                    local_port = parse_port(parts[0])?;
                    remote_host = "127.0.0.1".into();
                    remote_port = parse_port(parts[1])?;
                } else {
                    local_host = "0.0.0.0".into();
                    remote_port = parse_port(parts[1])?;
                    local_port = remote_port;
                    remote_host = parts[0].into();
                }
            }
            // "local-port:remote-host:remote-port"
            3 => {
                local_host = "0.0.0.0".into();
                local_port = parse_port(parts[0])?;
                remote_host = parts[1].into();
                remote_port = parse_port(parts[2])?;
            }
            // "local-host:local-port:remote-host:remote-port"
            4 => {
                local_host = parts[0].into();
                local_port = parse_port(parts[1])?;
                remote_host = parts[2].into();
                remote_port = parse_port(parts[3])?;
            }
            _ => return Err(format!("invalid remote spec: {}", spec)),
        }

        if remote_port == 0 {
            return Err("remote-port must be non-zero".into());
        }

        Ok(Remote {
            reverse,
            local_host,
            local_port,
            remote_host,
            remote_port,
            proto,
        })
    }

    pub fn summary(&self) -> String {
        let r = if self.reverse { "R:" } else { "" };
        format!(
            "{}{}:{}:{}:{}/{}",
            r,
            self.local_host,
            self.local_port,
            self.remote_host,
            self.remote_port,
            self.proto.as_str()
        )
    }
}

fn parse_port(s: &str) -> Result<u16, String> {
    s.parse::<u16>()
        .map_err(|_| format!("invalid port: {}", s))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn port_only() {
        let r = Remote::parse("3000").unwrap();
        assert!(!r.reverse);
        assert_eq!(r.local_port, 3000);
        assert_eq!(r.remote_port, 3000);
        assert_eq!(r.remote_host, "127.0.0.1");
        assert_eq!(r.local_host, "0.0.0.0");
    }
    #[test]
    fn host_port() {
        let r = Remote::parse("example.com:3000").unwrap();
        assert_eq!(r.remote_host, "example.com");
        assert_eq!(r.remote_port, 3000);
        assert_eq!(r.local_port, 3000);
    }
    #[test]
    fn full_forward() {
        let r = Remote::parse("192.168.0.5:3000:google.com:80").unwrap();
        assert_eq!(r.local_host, "192.168.0.5");
        assert_eq!(r.local_port, 3000);
        assert_eq!(r.remote_host, "google.com");
        assert_eq!(r.remote_port, 80);
    }
    #[test]
    fn reverse() {
        let r = Remote::parse("R:2222:localhost:22").unwrap();
        assert!(r.reverse);
        assert_eq!(r.local_port, 2222);
        assert_eq!(r.remote_port, 22);
        assert_eq!(r.remote_host, "localhost");
    }
    #[test]
    fn proto() {
        let r = Remote::parse("1.1.1.1:53/udp").unwrap();
        assert_eq!(r.proto, Proto::Udp);
        assert_eq!(r.remote_port, 53);
    }
}
