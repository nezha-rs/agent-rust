use crate::{
    config::AgentConfig,
    proto::{GeoIp, Ip},
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

const DEFAULT_ENDPOINTS: [&str; 4] = [
    "https://blog.cloudflare.com/cdn-cgi/trace",
    "https://developers.cloudflare.com/cdn-cgi/trace",
    "https://hostinger.com/cdn-cgi/trace",
    "https://ahrefs.com/cdn-cgi/trace",
];

fn parse_ip(body: &str, want_ipv6: bool) -> Option<String> {
    let value = body
        .lines()
        .find_map(|line| line.strip_prefix("ip="))
        .unwrap_or(body.trim())
        .trim();
    let address: IpAddr = value.parse().ok()?;
    if address.is_ipv6() == want_ipv6 {
        Some(address.to_string())
    } else {
        None
    }
}

async fn fetch_one(endpoints: &[String], want_ipv6: bool, dns: &[String]) -> Option<String> {
    let local = if want_ipv6 {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    };
    let builder = reqwest::Client::builder()
        .local_address(local)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5));
    let client = crate::platform::http_client_builder(builder, dns)
        .ok()?
        .build()
        .ok()?;
    for endpoint in endpoints {
        let response = match client
            .get(endpoint)
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 NezhaAgent")
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => continue,
        };
        if let Ok(body) = response.text().await {
            if let Some(ip) = parse_ip(&body, want_ipv6) {
                return Some(ip);
            }
        }
    }
    None
}

pub async fn fetch(config: &AgentConfig) -> Option<GeoIp> {
    let endpoints = if config.custom_ip_api.is_empty() {
        DEFAULT_ENDPOINTS
            .iter()
            .map(|value| value.to_string())
            .collect()
    } else {
        config.custom_ip_api.clone()
    };
    let (ipv4, ipv6) = tokio::join!(
        fetch_one(&endpoints, false, &config.dns),
        fetch_one(&endpoints, true, &config.dns)
    );
    if ipv4.is_none() && ipv6.is_none() {
        return None;
    }
    Some(GeoIp {
        use6: config.use_ipv6_country_code,
        ip: Some(Ip {
            ipv4: ipv4.unwrap_or_default(),
            ipv6: ipv6.unwrap_or_default(),
        }),
        country_code: String::new(),
        dashboard_boot_time: 0,
    })
}

pub fn fallback() -> GeoIp {
    GeoIp {
        use6: false,
        ip: Some(Ip::default()),
        ..Default::default()
    }
}

pub fn query_ip(report: &GeoIp) -> &str {
    let Some(ip) = report.ip.as_ref() else {
        return "";
    };
    if !ip.ipv6.is_empty() && (report.use6 || ip.ipv4.is_empty()) {
        &ip.ipv6
    } else {
        &ip.ipv4
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_ip, query_ip};
    use crate::proto::{GeoIp, Ip};

    #[test]
    fn accepts_plain_and_cloudflare_trace_responses() {
        assert_eq!(
            parse_ip("ip=192.0.2.4\nloc=SG\n", false).as_deref(),
            Some("192.0.2.4")
        );
        assert_eq!(
            parse_ip("2001:db8::1\n", true).as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(parse_ip("192.0.2.4", true), None);
    }

    #[test]
    fn country_lookup_selects_available_address_family() {
        let mut report = GeoIp {
            use6: true,
            ip: Some(Ip {
                ipv4: "192.0.2.1".into(),
                ipv6: "2001:db8::1".into(),
            }),
            ..Default::default()
        };
        assert_eq!(query_ip(&report), "2001:db8::1");
        report.use6 = false;
        assert_eq!(query_ip(&report), "192.0.2.1");
        report.ip.as_mut().unwrap().ipv4.clear();
        assert_eq!(query_ip(&report), "2001:db8::1");
    }
}
