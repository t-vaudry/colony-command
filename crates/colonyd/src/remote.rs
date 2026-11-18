//! Optional second listener for reaching the daemon over a VPN (Tailscale,
//! WireGuard). Off unless an address is configured, and never a wildcard: the
//! address must be one of this machine's own interfaces, so it is the user's
//! choice of which network may knock. Only a small router is served there
//! (`api::remote_router`).
//!
//! Transport decision (#89): plain `ws://` over the VPN, not `wss`. Tailscale
//! and WireGuard already encrypt and authenticate every packet between the two
//! machines, so a TLS layer would add certificate handling (MagicDNS names,
//! renewal) without adding protection on the wire. The cost is that mobile
//! webviews restrict cleartext traffic, so the mobile shell must be allowed to
//! make it (Android network security config, iOS ATS exception) for the VPN
//! address only. The credential is never put in a URL: it travels in the
//! Authorization header, or, for a webview that cannot set headers on a
//! WebSocket, as the socket's first message.

use std::net::IpAddr;

/// The address to listen on, from `COLONY_REMOTE_ADDR` or `remote_listen` in
/// settings. `None` (the default) means remote access is off.
pub fn configured(env: Option<String>, setting: Option<&str>) -> Option<String> {
    env.or_else(|| setting.map(str::to_string)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// An address that may be bound for remote access, or why not.
pub fn validate_remote_addr(text: &str) -> Result<IpAddr, String> {
    let ip: IpAddr = text.trim().parse().map_err(|_| format!("{text:?} is not an IP address"))?;
    if ip.is_unspecified() {
        return Err("a wildcard address (0.0.0.0 or ::) would listen on every network".into());
    }
    if ip.is_loopback() {
        return Err("loopback is already served locally; give the VPN interface's address".into());
    }
    if ip.is_multicast() {
        return Err("a multicast address cannot be listened on".into());
    }
    if let IpAddr::V4(v4) = ip {
        if v4.is_broadcast() {
            return Err("the broadcast address cannot be listened on".into());
        }
    }
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_vpn_address() {
        assert!(validate_remote_addr("100.101.102.103").is_ok());
        assert!(validate_remote_addr(" 10.0.0.5 ").is_ok());
        assert!(validate_remote_addr("fd7a:115c:a1e0::1").is_ok());
    }

    #[test]
    fn rejects_wildcards_loopback_multicast_broadcast_and_junk() {
        for bad in ["0.0.0.0", "::", "127.0.0.1", "::1", "224.0.0.1", "ff02::1", "255.255.255.255", "localhost", "", "100.1.1"] {
            assert!(validate_remote_addr(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn environment_beats_settings_and_blank_is_off() {
        assert_eq!(configured(Some("1.2.3.4".into()), Some("5.6.7.8")).as_deref(), Some("1.2.3.4"));
        assert_eq!(configured(None, Some("5.6.7.8")).as_deref(), Some("5.6.7.8"));
        assert_eq!(configured(Some("  ".into()), None), None);
        assert_eq!(configured(None, None), None);
    }
}
