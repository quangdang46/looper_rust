use std::net::TcpListener;

/// Bind to `127.0.0.1:0` and return the OS-allocated free port.
///
/// # Panics
/// Panics if the OS cannot assign a free port.
pub fn must_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("unable to bind for free port");
    let port = listener.local_addr().expect("no local address").port();
    // Drop the listener so the port is available again.
    drop(listener);
    port
}

/// Build a `http://host:port` base URL string.
pub fn base_url(host: &str, port: u16) -> String {
    format!("http://{}:{}", host, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_must_free_port_returns_valid_port() {
        let port = must_free_port();
        assert!(port > 0, "port should be non-zero");
        assert!(port <= 65535, "port should be within range");
    }

    #[test]
    fn test_base_url_localhost() {
        let url = base_url("127.0.0.1", 7391);
        assert_eq!(url, "http://127.0.0.1:7391");
    }

    #[test]
    fn test_must_free_port_unique_per_call() {
        let p1 = must_free_port();
        let p2 = must_free_port();
        // Usually different, not guaranteed — just exercise both.
        assert_ne!(p1, 0);
        assert_ne!(p2, 0);
    }
}
