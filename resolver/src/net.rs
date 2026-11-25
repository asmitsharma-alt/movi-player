use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, Url, redirect};
use std::{io, net::IpAddr, sync::Arc, time::Duration};

pub const DEFAULT_BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";

struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|address| public_ip(address.ip()))
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(io::Error::other("no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(
                (a, b, c),
                (0 | 10 | 127 | 224..=255, _, _)
                    | (100, 64..=127, _)
                    | (169, 254, _)
                    | (172, 16..=31, _)
                    | (192, 0, 0 | 2)
                    | (192, 168, _)
                    | (198, 18 | 19, _)
                    | (198, 51, 100)
                    | (203, 0, 113)
            )
        }
        IpAddr::V6(ip) => {
            let [prefix, second, ..] = ip.segments();
            (0x2000..=0x3fff).contains(&prefix)
                && prefix != 0x2002
                && !(prefix == 0x2001 && (second < 0x0200 || second == 0x0db8))
                && prefix != 0x3fff
        }
    }
}

pub fn valid_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.host(), Some(url::Host::Domain(host)) if host.contains('.'))
}

pub fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .dns_resolver(Arc::new(PublicDns))
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(20))
        .user_agent(DEFAULT_BROWSER_USER_AGENT)
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && valid_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("invalid upstream redirect")
            }
        }))
        .build()
}
