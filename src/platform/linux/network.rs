use hickory_resolver::{
    config::{ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts},
    net::runtime::TokioRuntimeProvider,
    TokioResolver,
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpStream;

const DEFAULT_DNS_SERVERS: [&str; 8] = [
    "8.8.8.8:53",
    "8.8.4.4:53",
    "1.1.1.1:53",
    "1.0.0.1:53",
    "[2001:4860:4860::8888]:53",
    "[2001:4860:4860::8844]:53",
    "[2606:4700:4700::1111]:53",
    "[2606:4700:4700::1001]:53",
];

struct AgentDns(Arc<TokioResolver>);

impl AgentDns {
    fn new(servers: &[String]) -> io::Result<Self> {
        let sources: Vec<&str> = if servers.is_empty() {
            DEFAULT_DNS_SERVERS.to_vec()
        } else {
            servers.iter().map(String::as_str).collect()
        };
        let mut nameservers = Vec::with_capacity(sources.len());
        for server in sources {
            let address: SocketAddr = server.parse().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid DNS server: {error}"),
                )
            })?;
            let mut udp = ConnectionConfig::udp();
            udp.port = address.port();
            let mut tcp = ConnectionConfig::tcp();
            tcp.port = address.port();
            nameservers.push(NameServerConfig::new(address.ip(), true, vec![udp, tcp]));
        }
        let mut options = ResolverOpts::default();
        options.timeout = Duration::from_secs(5);
        options.num_concurrent_reqs = 1;
        options.try_tcp_on_error = true;
        let resolver = TokioResolver::builder_with_config(
            ResolverConfig::from_name_servers(nameservers),
            TokioRuntimeProvider::default(),
        )
        .with_options(options)
        .build()
        .map_err(io::Error::other)?;
        Ok(Self(Arc::new(resolver)))
    }

    async fn lookup(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        if let Ok(address) = host.parse() {
            return Ok(vec![address]);
        }
        let addresses = self.0.lookup_ip(host).await.map_err(io::Error::other)?;
        let addresses: Vec<_> = addresses.iter().collect();
        if addresses.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "DNS returned no addresses",
            ));
        }
        Ok(addresses)
    }
}

impl Resolve for AgentDns {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = Self(Arc::clone(&self.0));
        Box::pin(async move {
            let addresses = resolver.lookup(name.as_str()).await?;
            Ok(Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0))) as Addrs)
        })
    }
}

pub(crate) fn http_client_builder(
    builder: reqwest::ClientBuilder,
    servers: &[String],
) -> io::Result<reqwest::ClientBuilder> {
    Ok(builder.dns_resolver(Arc::new(AgentDns::new(servers)?)))
}

pub(crate) async fn resolve_first_ip(host: &str, servers: &[String]) -> io::Result<IpAddr> {
    if let Ok(address) = host.parse() {
        return Ok(address);
    }
    AgentDns::new(servers)?
        .lookup(host)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "DNS returned no addresses"))
}

pub(crate) async fn connect_tcp_host(
    host: &str,
    port: u16,
    servers: &[String],
) -> io::Result<TcpStream> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return TcpStream::connect(SocketAddr::new(address, port)).await;
    }
    let mut last_error = None;
    for ip in AgentDns::new(servers)?.lookup(host).await? {
        match TcpStream::connect(SocketAddr::new(ip, port)).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no TCP address")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    async fn dns_fixture() -> (String, tokio::task::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap().to_string();
        let job = tokio::spawn(async move {
            let mut buffer = [0u8; 512];
            while let Ok((length, peer)) = socket.recv_from(&mut buffer).await {
                let query = &buffer[..length];
                let mut end = 12;
                while end < query.len() && query[end] != 0 {
                    end += 1 + query[end] as usize;
                }
                end += 5;
                if end > query.len() {
                    continue;
                }
                let is_a = query[end - 4..end - 2] == [0, 1];
                let mut reply = query[..12].to_vec();
                reply[2] = 0x81;
                reply[3] = 0x80;
                reply[6..12].copy_from_slice(&[0, u8::from(is_a), 0, 0, 0, 0]);
                reply.extend_from_slice(&query[12..end]);
                if is_a {
                    reply.extend_from_slice(&[
                        0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 42,
                    ]);
                }
                let _ = socket.send_to(&reply, peer).await;
            }
        });
        (address, job)
    }

    #[tokio::test]
    async fn configured_dns_resolves_names_and_bypasses_numeric_ips() {
        let (server, job) = dns_fixture().await;
        let servers = vec![server];
        let resolved = tokio::time::timeout(
            Duration::from_secs(3),
            resolve_first_ip("probe.test", &servers),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resolved, "127.0.0.42".parse::<IpAddr>().unwrap());
        assert_eq!(
            resolve_first_ip("127.0.0.1", &servers).await.unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        job.abort();
    }

    #[tokio::test]
    async fn default_dns_preserves_local_hosts() {
        let address = resolve_first_ip("localhost", &[]).await.unwrap();
        assert!(address.is_loopback());
    }

    #[tokio::test]
    async fn configured_dns_controls_tcp_backend() {
        let (server, job) = dns_fixture().await;
        let listener = TcpListener::bind("127.0.0.42:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connected = tokio::time::timeout(
            Duration::from_secs(3),
            connect_tcp_host("probe.test", port, &[server]),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            connected.peer_addr().unwrap().ip(),
            "127.0.0.42".parse::<IpAddr>().unwrap()
        );
        job.abort();
    }

    #[tokio::test]
    async fn configured_dns_controls_http_client() {
        let (server, job) = dns_fixture().await;
        let listener = TcpListener::bind("127.0.0.42:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let responder = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let client = http_client_builder(reqwest::Client::builder().no_proxy(), &[server])
            .unwrap()
            .build()
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            client.get(format!("http://probe.test:{port}/")).send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        responder.await.unwrap();
        job.abort();
    }
}
