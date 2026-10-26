use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether a webhook may never be sent to this address: private, loopback, link-local (which includes the cloud
/// metadata address), shared, reserved and multicast ranges.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240
}

fn blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4);
    }
    let seg = ip.segments();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || seg[0] & 0xfe00 == 0xfc00
        || seg[0] & 0xffc0 == 0xfe80
        || seg[0] & 0xffc0 == 0xfec0
        || (seg[0] == 0x0064 && seg[1] == 0xff9b)
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        || seg[0] == 0x2002
        || ip.to_ipv4().is_some_and(blocked_v4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(text: &str) -> bool {
        is_blocked(text.parse().unwrap())
    }

    #[test]
    fn private_loopback_link_local_and_metadata_ranges_are_blocked() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00:ec2::254",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "ff02::1",
        ] {
            assert!(blocked(ip), "{ip}");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for ip in [
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1",
            "100.63.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!blocked(ip), "{ip}");
        }
    }
}
