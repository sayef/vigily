//! Where an engine in server mode may send webhooks.
//!
//! A shared engine posts to URLs its users configure, so a channel must not reach the engine's own
//! network: in server mode a webhook, Slack, Teams or Discord target is resolved first, and the
//! request is refused when any address is loopback, link-local, private (RFC 1918), shared
//! (100.64.0.0/10), unique-local (`fc00::/7`), unspecified, multicast or broadcast, including
//! IPv4-mapped forms. The request then connects to the checked addresses only, so a second DNS
//! answer cannot point it elsewhere, and follows no redirects. `[notifications]
//! allow_private_networks = true` in vigily.toml lifts this; local mode (one user on a laptop)
//! allows every address, and so does the operator's own `[alerts] webhook`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// Whether a server-mode engine may connect to `ip`.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // IETF protocol assignments, 192.0.0.0/24.
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // Reserved, 240.0.0.0/4.
        || o[0] >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // Unique local, fc00::/7.
        || (s[0] & 0xfe00) == 0xfc00
        // Link-local, fe80::/10.
        || (s[0] & 0xffc0) == 0xfe80
        // Site-local (deprecated), fec0::/10.
        || (s[0] & 0xffc0) == 0xfec0
        // IPv4-compatible (deprecated) and NAT64 well-known prefix forms of private addresses.
        || (s[0..6] == [0, 0, 0, 0, 0, 0] && !public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        )))
        || (s[0..6] == [0x64, 0xff9b, 0, 0, 0, 0] && !public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ))))
}

/// An HTTP client for `url`. With `guard`, the host is resolved and every address checked; the
/// client then connects to those addresses only. Errors name the host, never the URL's path.
pub async fn client_for(
    url: &str,
    guard: bool,
    timeout: Duration,
) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    if !guard {
        return builder.build().map_err(|e| format!("HTTP client: {e}"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "the URL does not parse".to_string())?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "the URL has no host".to_string())?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| format!("could not resolve {host}: {e}"))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(format!("{host} resolves to no address"));
    }
    if let Some(bad) = addrs.iter().find(|a| !public(a.ip())) {
        return Err(format!(
            "refused to send to {host}: it resolves to {}, a loopback, private or link-local \
             address. Set allow_private_networks = true under [notifications] in vigily.toml to \
             allow internal targets",
            bad.ip()
        ));
    }
    builder
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        public(s.parse().unwrap())
    }

    #[test]
    fn internal_addresses_are_refused() {
        for s in [
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
        ] {
            assert!(!ok(s), "{s} is internal");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(ok(s), "{s} is public");
        }
    }

    #[tokio::test]
    async fn a_loopback_target_is_refused_before_connecting() {
        let e = client_for(
            "http://127.0.0.1:9/hook/secret-path",
            true,
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(
            e.contains("127.0.0.1") && e.contains("allow_private_networks"),
            "{e}"
        );
        assert!(!e.contains("secret-path"));
        let e = client_for("http://localhost:9/x", true, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(e.contains("refused"), "{e}");
        assert!(
            client_for("http://127.0.0.1:9/x", false, Duration::from_secs(1))
                .await
                .is_ok()
        );
    }
}
