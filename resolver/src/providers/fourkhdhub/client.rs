use super::{hubcloud, parser};
use crate::providers::models::{CatalogItem, PlaybackSource, Release, ResolutionIntent};
use reqwest::Url;

const DEFAULT_BASE_URL: &str = "https://4khdhub.one/";

#[derive(thiserror::Error, Debug)]
pub enum FourKHdHubError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid provider URL: {0}")]
    InvalidUrl(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("no playable mirrors: {0}")]
    NoPlayableMirror(String),
}
impl FourKHdHubError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Network(err) => {
                if err.is_timeout() {
                    "4KHDHub timed out."
                } else if let Some(status) = err.status() {
                    match status.as_u16() {
                        403 => "4KHDHub blocked (HTTP 403).",
                        404 => "4KHDHub page not found (HTTP 404).",
                        429 => "4KHDHub rate limited (HTTP 429).",
                        500..=599 => "4KHDHub server error.",
                        _ => "Cannot reach 4KHDHub.",
                    }
                } else {
                    "Cannot reach 4KHDHub."
                }
            }
            Self::InvalidUrl(_) => "Invalid 4KHDHub URL.",
            Self::Parse(_) => "4KHDHub parse error.",
            Self::NoPlayableMirror(reason) => {
                let lower = reason.to_ascii_lowercase();
                if lower.contains("timed out") || lower.contains("timeout") {
                    "4KHDHub mirror probe timed out."
                } else if lower.contains("quota") {
                    "4KHDHub mirror quota exceeded."
                } else if lower.contains("expired") {
                    "4KHDHub mirror link expired."
                } else {
                    "No working 4KHDHub mirrors found."
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct FourKHdHubClient {
    client: reqwest::Client,
    base_url: Url,
}

impl FourKHdHubClient {
    pub fn new() -> Result<Self, FourKHdHubError> {
        let base = std::env::var("MOVIEBOX_FOURKHDHUB_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self::with_base_url(&base)
    }

    pub fn with_base_url(base: &str) -> Result<Self, FourKHdHubError> {
        let base_url =
            Url::parse(base).map_err(|_| FourKHdHubError::InvalidUrl(base.to_string()))?;
        if !crate::net::valid_url(&base_url) {
            return Err(FourKHdHubError::InvalidUrl(base.to_string()));
        }
        Ok(Self {
            client: crate::net::client()?,
            base_url,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<CatalogItem>, FourKHdHubError> {
        let mut url = self.base_url.clone();
        url.query_pairs_mut().append_pair("s", query);
        let html = self.fetch_text(url).await?;
        parser::parse_search(&self.base_url, &html)
    }

    pub async fn releases(
        &self,
        id: &str,
        season: usize,
        episode: usize,
    ) -> Result<Vec<Release>, FourKHdHubError> {
        let html = self.fetch_text(self.provider_url(id)?).await?;
        parser::parse_releases(&html, season, episode)
    }

    pub async fn resolve_release(
        &self,
        release: &Release,
        origin: &str,
        excluded: Option<&str>,
    ) -> Result<PlaybackSource, FourKHdHubError> {
        use futures::{StreamExt, stream::FuturesUnordered};
        use std::{collections::HashSet, time::Duration};

        let intent = ResolutionIntent::Playback;
        let mut lookups = FuturesUnordered::new();
        for mirror in &release.mirrors {
            let url = &mirror.resolver_url;
            let host = Url::parse(url)
                .ok()
                .filter(crate::net::valid_url)
                .and_then(|url| url.host_str().map(str::to_owned));
            let Some(host) = host else { continue };
            lookups.push(async move {
                tokio::time::timeout(Duration::from_secs(7), async {
                    if host.starts_with("hubcloud.") {
                        hubcloud::resolve(&self.client, url, intent).await
                    } else if host.starts_with("hubdrive.") {
                        hubcloud::resolve_hubdrive(&self.client, url, intent).await
                    } else if host.starts_with("greenmotors.")
                        || host.starts_with("greenmountmotors.")
                    {
                        hubcloud::resolve_greenmotors(&self.client, url, intent).await
                    } else {
                        hubcloud::validate_playback_url(url)
                            .map(|url| vec![(url, mirror.label.clone(), mirror.headers.clone())])
                    }
                })
                .await
                .ok()
                .and_then(Result::ok)
            });
        }

        let mut probes = FuturesUnordered::new();
        let mut seen = HashSet::new();
        let mut admitted = 0;
        let mut best: Option<(u8, PlaybackSource)> = None;
        let grace = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(grace);

        loop {
            tokio::select! {
                Some(result) = lookups.next(), if admitted < 6 && !lookups.is_empty() => {
                    let Some(mut candidates) = result else { continue };
                    candidates.sort_by_key(|(url, label, _)| hubcloud::score(url, label, intent));
                    for (url, label, mut headers) in candidates {
                        if admitted == 6 { break }
                        if excluded == Some(&url) || !seen.insert(url.clone()) { continue }
                        admitted += 1;
                        let referer = self.base_url.as_str().trim_end_matches('/');
                        headers.push(("Referer".into(), referer.to_owned()));
                        probes.push(async move {
                            let source = self.probe(&url, headers, origin).await?;
                            let score = hubcloud::score(&url, &label, intent)
                                .min(hubcloud::score(&source.url, &label, intent));
                            Some((score, source))
                        });
                    }
                }
                Some(result) = probes.next(), if !probes.is_empty() => {
                    let Some((score, source)) = result else { continue };
                    if excluded == Some(&source.url) { continue }
                    if score <= 1 { return Ok(source) }
                    if best.is_none() {
                        grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(500));
                    }
                    if best.as_ref().is_none_or(|(best_score, _)| score < *best_score) {
                        best = Some((score, source));
                    }
                }
                _ = &mut grace, if best.is_some() => break,
                else => break,
            }
        }

        best.map(|(_, source)| source).ok_or_else(|| {
            FourKHdHubError::NoPlayableMirror("no mirror returned a usable byte range".into())
        })
    }

    async fn probe(
        &self,
        url: &str,
        mut headers: Vec<(String, String)>,
        origin: &str,
    ) -> Option<PlaybackSource> {
        use reqwest::header::*;
        let initial = Url::parse(url).ok().filter(crate::net::valid_url)?;
        headers.push(("Origin".into(), origin.to_owned()));
        tokio::time::timeout(std::time::Duration::from_millis(3500), async {
            let mut target = initial;
            for _ in 0..2 {
                let mut request = self
                    .client
                    .get(target.clone())
                    .header(RANGE, "bytes=0-4194303")
                    .header(ACCEPT_ENCODING, "identity");
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                let response = request.send().await.ok()?.error_for_status().ok()?;
                let final_url = response.url();
                let mime = response
                    .headers()
                    .get(CONTENT_TYPE)?
                    .to_str()
                    .ok()?
                    .to_ascii_lowercase();
                if mime.starts_with("text/") || mime.contains("application/zip") {
                    let wrapped = final_url.query_pairs().find(|(key, _)| key == "link")?.1;
                    target = Url::parse(&wrapped).ok().filter(crate::net::valid_url)?;
                    continue;
                }
                if !crate::media::media_type(&mime)
                    || response.status() != reqwest::StatusCode::PARTIAL_CONTENT
                {
                    return None;
                }
                let (start, end, size) = crate::media::content_range(
                    response.headers().get(CONTENT_RANGE)?.to_str().ok()?,
                )?;
                if start != 0 || end != (4 * 1024 * 1024 - 1).min(size - 1) {
                    return None;
                }
                let allowed = response
                    .headers()
                    .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                    .and_then(|header| header.to_str().ok());
                let exposes_range = response
                    .headers()
                    .get(ACCESS_CONTROL_EXPOSE_HEADERS)
                    .and_then(|header| header.to_str().ok())
                    .is_some_and(|value| {
                        value.split(',').any(|field| {
                            let field = field.trim();
                            field == "*" || field.eq_ignore_ascii_case("content-range")
                        })
                    });
                return Some(PlaybackSource {
                    url: final_url.to_string(),
                    headers,
                    browser_readable: exposes_range
                        && (allowed == Some("*") || allowed == Some(origin)),
                });
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }

    async fn fetch_text(&self, url: Url) -> Result<String, FourKHdHubError> {
        let response = self.client.get(url).send().await?.error_for_status()?;
        Ok(response.text().await?)
    }

    fn provider_url(&self, id: &str) -> Result<Url, FourKHdHubError> {
        let url = self
            .base_url
            .join(id.trim_start_matches('/'))
            .map_err(|_| FourKHdHubError::InvalidUrl(id.to_string()))?;
        if !crate::net::valid_url(&url) || url.host_str() != self.base_url.host_str() {
            return Err(FourKHdHubError::InvalidUrl(id.to_string()));
        }
        Ok(url)
    }
}
