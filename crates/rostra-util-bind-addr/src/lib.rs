use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindAddr {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl FromStr for BindAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // First try to parse as a SocketAddr
        if let Ok(socket_addr) = s.parse::<SocketAddr>() {
            return Ok(BindAddr::Tcp(socket_addr));
        }

        // If that fails, treat it as a Unix socket path
        Ok(BindAddr::Unix(PathBuf::from(s)))
    }
}

impl std::fmt::Display for BindAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindAddr::Tcp(addr) => write!(f, "{}", addr),
            BindAddr::Unix(path) => write!(f, "{}", path.display()),
        }
    }
}