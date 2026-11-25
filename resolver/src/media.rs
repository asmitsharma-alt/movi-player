use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header::*},
    response::Response,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_RANGE: u64 = 8 * 1024 * 1024;
const OPENING_RANGE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Media {
    pub user: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub movie: String,
    pub release: String,
}

pub fn media_type(value: &str) -> bool {
    let mime = value.split(';').next().unwrap_or_default().trim();
    mime.starts_with("video/")
        || matches!(mime, "application/octet-stream" | "application/x-matroska")
}

fn range(value: &str) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let limit = start.checked_add(MAX_RANGE - 1)?;
    let end = if end.is_empty() {
        limit
    } else {
        end.parse::<u64>().ok()?.min(limit)
    };
    (end >= start).then_some((start, end))
}

pub fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (bounds, size) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    let (start, end, size) = (start.parse().ok()?, end.parse().ok()?, size.parse().ok()?);
    (start <= end && end < size).then_some((start, end, size))
}

pub async fn relay(
    client: &Client,
    range_limits: &moka::future::Cache<String, u64>,
    media: Media,
    method: Method,
    headers: &HeaderMap,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, StatusCode> {
    let url = Url::parse(&media.target)
        .ok()
        .filter(crate::net::valid_url)
        .ok_or(StatusCode::BAD_GATEWAY)?;
    let head = method == Method::HEAD;
    let (start, requested_end) = if head {
        (0, 0)
    } else {
        headers
            .get(RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(range)
            .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?
    };
    let cached_limit = if head {
        MAX_RANGE
    } else {
        range_limits.get(&media.target).await.unwrap_or(MAX_RANGE)
    };
    let mut size_limit = (requested_end - start + 1).min(cached_limit);
    let upstream = loop {
        let result =
            request_range(client, &url, &media.headers, start, start + size_limit - 1).await;
        match result {
            Ok(response)
                if response.status() == StatusCode::PARTIAL_CONTENT
                    || response.status() == StatusCode::RANGE_NOT_SATISFIABLE
                    || size_limit <= 512 * 1024 =>
            {
                break response;
            }
            Err(error) if size_limit <= 512 * 1024 => return Err(error),
            _ => {
                size_limit = if size_limit > OPENING_RANGE {
                    OPENING_RANGE
                } else {
                    512 * 1024
                }
            }
        }
    };
    let upstream_headers = upstream.headers();
    if upstream.status() == StatusCode::RANGE_NOT_SATISFIABLE {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
        if let Some(value) = upstream_headers.get(CONTENT_RANGE) {
            response.headers_mut().insert(CONTENT_RANGE, value.clone());
        }
        return Ok(response);
    }
    let retryable = StatusCode::BAD_GATEWAY;
    let upstream_status = upstream.status();
    let failure = |reason| {
        eprintln!("media relay: {reason}; upstream status={upstream_status}");
        retryable
    };
    if upstream_status != StatusCode::PARTIAL_CONTENT {
        return Err(failure("range rejected"));
    }
    let mime = upstream_headers
        .get(CONTENT_TYPE)
        .filter(|value| value.to_str().ok().is_some_and(media_type))
        .ok_or_else(|| failure("unsupported media type"))?;
    let bounds = upstream_headers
        .get(CONTENT_RANGE)
        .ok_or_else(|| failure("missing Content-Range"))?;
    let (served_start, served_end, size) = bounds
        .to_str()
        .ok()
        .and_then(content_range)
        .ok_or_else(|| failure("invalid Content-Range"))?;
    if served_start != start || served_end > requested_end {
        return Err(failure("unexpected range bounds"));
    }
    let size = if head {
        size
    } else {
        served_end - served_start + 1
    };
    if !head
        && upstream
            .content_length()
            .is_some_and(|length| length != size)
    {
        return Err(failure("range length differs from Content-Length"));
    }
    if !head && size_limit < cached_limit && size_limit < requested_end - start + 1 {
        range_limits.insert(media.target, size_limit).await;
    }
    let mut response = Response::builder()
        .status(if head {
            StatusCode::OK
        } else {
            StatusCode::PARTIAL_CONTENT
        })
        .header(CONTENT_TYPE, mime)
        .header(CONTENT_LENGTH, size)
        .header(ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .header(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    if !head {
        response = response.header(CONTENT_RANGE, bounds);
    }
    let body = if head {
        Body::empty()
    } else {
        use futures::StreamExt;
        let mut remaining = size;
        Body::from_stream(upstream.bytes_stream().map(move |chunk| {
            let _hold_permit = &permit;
            let chunk = chunk.map_err(std::io::Error::other)?;
            remaining = remaining
                .checked_sub(chunk.len() as u64)
                .ok_or_else(|| std::io::Error::other("CDN exceeded its declared range"))?;
            Ok::<_, std::io::Error>(chunk)
        }))
    };
    response.body(body).map_err(|_| retryable)
}

async fn request_range(
    client: &Client,
    url: &Url,
    headers: &[(String, String)],
    start: u64,
    end: u64,
) -> Result<reqwest::Response, StatusCode> {
    let mut request = client
        .get(url.clone())
        .header(RANGE, format!("bytes={start}-{end}"))
        .header(ACCEPT_ENCODING, "identity")
        .timeout(Duration::from_secs(19));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    tokio::time::timeout(Duration::from_secs(4), request.send())
        .await
        .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
        .map_err(|_| StatusCode::BAD_GATEWAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_relay_requests_and_checks_cdn_response() {
        assert_eq!(range("bytes=0-4194303"), Some((0, 4194303)));
        assert_eq!(range("bytes=9-"), Some((9, 9 + MAX_RANGE - 1)));
        assert_eq!(range("bytes=9-1"), None);
        assert_eq!(range("bytes=1-9,12-13"), None);
        assert_eq!(range("bytes=18446744073709551615-"), None);
        assert_eq!(content_range("bytes 3-5/6"), Some((3, 5, 6)));
        assert_eq!(content_range("bytes 3-6/6"), None);
    }
}
