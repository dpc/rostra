use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use url::{Host, Url};

/// Validated loopback HTTP origin of the live Rostra site.
#[derive(Clone, Debug)]
pub struct SiteOrigin {
    /// Parsed origin URL, always ending in `/`.
    url: Url,
    /// Socket used for the readiness probe.
    socket: SocketAddr,
}

impl SiteOrigin {
    /// Parse and validate an HTTP origin with a literal loopback address.
    pub fn parse(input: &str) -> Result<Self> {
        let mut url = Url::parse(input).context("invalid site origin")?;
        if url.scheme() != "http"
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            bail!("origin must be a plain loopback HTTP origin without a path");
        }

        let ip = match url.host().context("origin has no host")? {
            Host::Ipv4(ip) => IpAddr::V4(ip),
            Host::Ipv6(ip) => IpAddr::V6(ip),
            Host::Domain(_) => bail!("origin host must be a literal loopback IP address"),
        };
        if !ip.is_loopback() {
            bail!("origin must use a loopback IP address");
        }
        let port = url.port_or_known_default().context("origin has no port")?;
        url.set_path("/");

        Ok(Self {
            url,
            socket: SocketAddr::new(ip, port),
        })
    }

    /// Resolve a same-origin relative path or validate a same-origin absolute
    /// URL.
    pub fn target(&self, target: &str) -> Result<Url> {
        let url = self.url.join(target).context("invalid navigation target")?;
        if url.origin() != self.url.origin() {
            bail!("navigation target must remain at {self}");
        }
        Ok(url)
    }

    /// Return whether a browser-reported URL remains at this exact origin.
    pub fn contains(&self, target: &str) -> bool {
        Url::parse(target).is_ok_and(|target| target.origin() == self.url.origin())
    }

    /// Return the configured development-server TCP port.
    pub fn port(&self) -> u16 {
        self.socket.port()
    }

    /// Verify that the endpoint is an HTTP page recognizably served by Rostra.
    pub fn probe(&self) -> Result<()> {
        let mut stream = TcpStream::connect_timeout(&self.socket, Duration::from_secs(2))
            .context("could not connect")?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(
            format!(
                "GET / HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
                self.url.authority()
            )
            .as_bytes(),
        )?;

        let mut response = String::new();
        stream.take(256 * 1024).read_to_string(&mut response)?;
        if !response.starts_with("HTTP/1.") || !response.to_ascii_lowercase().contains("rostra") {
            bail!("endpoint did not return a recognizable Rostra page");
        }
        Ok(())
    }
}

impl fmt::Display for SiteOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.url.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::SiteOrigin;

    #[test]
    fn accepts_literal_loopback_origins() {
        assert!(SiteOrigin::parse("http://[::1]:2345").is_ok());
        assert!(SiteOrigin::parse("http://127.0.0.1:2345").is_ok());
    }

    #[test]
    fn rejects_non_loopback_or_decorated_origins() {
        assert!(SiteOrigin::parse("https://127.0.0.1:2345").is_err());
        assert!(SiteOrigin::parse("http://localhost:2345").is_err());
        assert!(SiteOrigin::parse("http://192.0.2.1:2345").is_err());
        assert!(SiteOrigin::parse("http://[::1]:2345/news").is_err());
    }

    #[test]
    fn navigation_cannot_change_origin() {
        let origin = SiteOrigin::parse("http://[::1]:2345").unwrap();
        assert!(origin.target("/settings/identity").is_ok());
        assert!(origin.target("https://example.com").is_err());
        assert!(origin.target("//127.0.0.1:2345/").is_err());
        assert!(origin.contains("http://[::1]:2345/news"));
        assert!(!origin.contains("http://127.0.0.1:2345/news"));
    }
}
