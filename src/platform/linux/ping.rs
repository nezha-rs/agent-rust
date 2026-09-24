use socket2::Type;
use std::{net::IpAddr, time::Duration};
use surge_ping::{Client, Config, PingIdentifier, PingSequence, ICMP};
use tokio::{task::JoinSet, time};

const COUNT: u16 = 5;
const DEADLINE: Duration = Duration::from_secs(20);

pub(crate) async fn icmp_ping(target: IpAddr) -> Result<Option<f32>, String> {
    let kind = if target.is_ipv4() { ICMP::V4 } else { ICMP::V6 };
    let config = Config::builder()
        .kind(kind)
        .sock_type_hint(Type::RAW)
        .build();
    let client = Client::new(&config).map_err(|error| error.to_string())?;
    let identifier = PingIdentifier(uuid::Uuid::new_v4().as_u128() as u16);
    let mut probes = JoinSet::new();
    let started = time::Instant::now();
    let mut interval = time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut sent = 0;
    let mut received = 0;
    let mut total = Duration::ZERO;

    loop {
        tokio::select! {
            biased;
            _ = time::sleep_until(started + DEADLINE) => break,
            Some(joined) = probes.join_next(), if !probes.is_empty() => {
                if let Ok(Ok(duration)) = joined {
                    received += 1;
                    total += duration;
                    if received == COUNT {
                        break;
                    }
                }
            }
            _ = interval.tick(), if sent < COUNT => {
                let sequence = PingSequence(sent);
                let mut pinger = client.pinger(target, identifier).await;
                pinger.timeout(DEADLINE);
                probes.spawn(async move {
                    pinger.ping(sequence, &[0; 24]).await.map(|(_, duration)| duration)
                });
                sent += 1;
            }
        }
    }
    probes.abort_all();
    if received == 0 {
        Ok(None)
    } else {
        let average = total / u32::from(received);
        Ok(Some(average.as_micros() as f32 / 1000.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_uses_native_icmp_without_ping_executable() {
        let target = "127.0.0.1".parse().unwrap();
        let average = icmp_ping(target).await.unwrap().unwrap();
        assert!((0.0..1000.0).contains(&average), "{average}");
    }

    #[tokio::test]
    async fn ipv6_loopback_uses_native_icmp() {
        let target = "::1".parse().unwrap();
        let average = icmp_ping(target).await.unwrap().unwrap();
        assert!((0.0..1000.0).contains(&average), "{average}");
    }
}
