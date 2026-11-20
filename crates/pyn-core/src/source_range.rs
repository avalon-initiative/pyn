//! Network address ranges a credential may be used from: an address or a CIDR block, IPv4 or IPv6.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::error::{PynError, Result};

/// An address range in canonical text: a bare address stands for itself, a block has its host bits cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    base: IpAddr,
    prefix: u8,
}

fn invalid(text: &str) -> PynError {
    PynError::InvalidRequest(format!(
        "invalid source range {text:?}: use an IP address or a CIDR block such as 10.0.0.0/8"
    ))
}

fn mask(bits: u32, prefix: u8) -> u128 {
    let all = if bits == 32 {
        u128::from(u32::MAX)
    } else {
        u128::MAX
    };
    match prefix {
        0 => 0,
        p if u32::from(p) >= bits => all,
        p => all & !(all >> p),
    }
}

/// An IPv4-mapped IPv6 address is the IPv4 address it wraps.
pub fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn bits(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => u128::from(u32::from(v4)),
        IpAddr::V6(v6) => u128::from(v6),
    }
}

impl SourceRange {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p.parse::<u8>().map_err(|_| invalid(text))?)),
            None => (text, None),
        };
        let ip = unmap(addr.parse::<IpAddr>().map_err(|_| invalid(text))?);
        let width: u8 = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(width);
        if prefix > width {
            return Err(invalid(text));
        }
        let host = bits(ip) & mask(u32::from(width), prefix);
        let base = match ip {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::from(host as u32)),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::from(host)),
        };
        Ok(Self { base, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = unmap(ip);
        if ip.is_ipv4() != self.base.is_ipv4() {
            return false;
        }
        let width: u32 = if ip.is_ipv4() { 32 } else { 128 };
        bits(ip) & mask(width, self.prefix) == bits(self.base)
    }

    /// Canonical text: the bare address for a single host, `base/prefix` otherwise.
    pub fn canonical(&self) -> String {
        let single = self.prefix == if self.base.is_ipv4() { 32 } else { 128 };
        if single {
            self.base.to_string()
        } else {
            format!("{}/{}", self.base, self.prefix)
        }
    }
}

/// Parses and canonicalizes a list of ranges, dropping duplicates and keeping the first-seen order.
pub fn parse_all(texts: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in texts {
        let c = SourceRange::parse(t)?.canonical();
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

/// Whether `ip` is in any of the canonical `ranges`. An unreadable stored range matches nothing.
pub fn any_contains(ranges: &[String], ip: IpAddr) -> bool {
    ranges
        .iter()
        .filter_map(|r| SourceRange::parse(r).ok())
        .any(|r| r.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_block_contains_its_hosts_and_nothing_else() {
        let r = SourceRange::parse("10.1.2.3/8").unwrap();
        assert_eq!(r.canonical(), "10.0.0.0/8");
        assert!(r.contains(ip("10.255.0.1")));
        assert!(!r.contains(ip("11.0.0.1")));
        assert!(!r.contains(ip("::1")));
    }

    #[test]
    fn a_bare_address_is_a_single_host_and_v4_mapped_forms_agree() {
        let r = SourceRange::parse("192.0.2.7").unwrap();
        assert_eq!(r.canonical(), "192.0.2.7");
        assert!(r.contains(ip("192.0.2.7")));
        assert!(r.contains(ip("::ffff:192.0.2.7")));
        assert!(!r.contains(ip("192.0.2.8")));
    }

    #[test]
    fn ipv6_blocks_and_the_zero_prefix_work() {
        let r = SourceRange::parse("2001:db8::/32").unwrap();
        assert!(r.contains(ip("2001:db8:1::5")));
        assert!(!r.contains(ip("2001:db9::1")));
        assert!(
            SourceRange::parse("0.0.0.0/0")
                .unwrap()
                .contains(ip("8.8.8.8"))
        );
    }

    #[test]
    fn malformed_ranges_are_refused() {
        for bad in [
            "",
            "nope",
            "10.0.0.0/33",
            "10.0.0.0/x",
            "::/129",
            "10.0.0.0/-1",
        ] {
            assert!(SourceRange::parse(bad).is_err(), "{bad}");
        }
    }
}
