pub(crate) fn split_host_port(address: &str) -> Result<(&str, u16), String> {
    let (host, port) = if let Some(bracketed) = address.strip_prefix('[') {
        let end = bracketed
            .find(']')
            .ok_or_else(|| "missing closing bracket in address".to_string())?;
        let host = &bracketed[..end];
        let port = bracketed[end + 1..]
            .strip_prefix(':')
            .ok_or_else(|| "missing port in address".to_string())?;
        (host, port)
    } else {
        let (host, port) = address
            .rsplit_once(':')
            .ok_or_else(|| "missing port in address".to_string())?;
        if host.contains(':') || host.contains(['[', ']']) {
            return Err("too many colons in address".into());
        }
        (host, port)
    };
    if host.is_empty() {
        return Err("missing host in address".into());
    }
    let port = port.parse::<u16>().map_err(|error| error.to_string())?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hostname_ipv4_and_bracketed_ipv6() {
        assert_eq!(
            split_host_port("example.test:443").unwrap(),
            ("example.test", 443)
        );
        assert_eq!(split_host_port("127.0.0.1:80").unwrap(), ("127.0.0.1", 80));
        assert_eq!(split_host_port("[::1]:8008").unwrap(), ("::1", 8008));
        assert_eq!(
            split_host_port("[host.test]:80").unwrap(),
            ("host.test", 80)
        );
    }

    #[test]
    fn rejects_unbracketed_ipv6_and_malformed_brackets() {
        for address in ["::1:8008", "[::1:8008", "[::1]8008", "[::1]:x"] {
            assert!(split_host_port(address).is_err(), "{address}");
        }
    }
}
