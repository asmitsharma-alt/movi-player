mod auth;
mod media;
mod net;
mod providers;

use auth::{Tokens, check_password, cookie, hash_password};
use axum::{
    Form, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header::*},
    middleware,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use media::Media;
use moka::future::Cache;
use providers::{
    fourkhdhub::FourKHdHubClient,
    models::{CatalogItem, MediaType, PlaybackSource, Release},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    io::{self, BufRead},
    sync::Arc,
    time::{Duration, Instant},
};
use tower_http::services::ServeDir;

const SESSION_TTL: u64 = 8 * 60 * 60;
const MEDIA_TTL: u64 = 60 * 60;

#[derive(Clone)]
struct App {
    provider: FourKHdHubClient,
    origin: String,
    users: Arc<HashMap<String, String>>,
    tokens: Tokens,
    searches: Cache<String, Vec<CatalogItem>>,
    releases: Cache<String, Vec<Release>>,
    resolved: Cache<ReleaseKey, PlaybackSource>,
    relay_limits: Cache<String, u64>,
    login_attempts: Cache<String, Arc<std::sync::atomic::AtomicU32>>,
    requests: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Deserialize, Serialize, Eq, Hash, PartialEq)]
struct ReleaseKey {
    movie: String,
    filename: String,
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct Search {
    q: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MovieCard {
    id: String,
    title: String,
    year: Option<String>,
    poster_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseCard {
    id: String,
    filename: String,
    quality: Option<String>,
    codec: Option<String>,
    language: Option<String>,
    size_bytes: Option<u64>,
    recommended: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackRequest {
    release_id: String,
    previous: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Playback {
    url: String,
    relay_url: String,
    token: String,
    expires_in: u64,
}

type ApiError = (StatusCode, Json<serde_json::Value>);

fn error(status: StatusCode, message: &str) -> ApiError {
    (status, Json(serde_json::json!({ "error": message })))
}

impl App {
    fn user(&self, headers: &HeaderMap) -> Result<String, ApiError> {
        self.tokens
            .session(headers)
            .filter(|user| self.users.contains_key(user))
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "Sign in to continue."))
    }

    fn check_origin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if headers.get(ORIGIN).and_then(|header| header.to_str().ok()) == Some(self.origin.as_str())
        {
            Ok(())
        } else {
            Err(error(
                StatusCode::FORBIDDEN,
                "Request origin is not allowed.",
            ))
        }
    }

    async fn releases(&self, movie: String) -> Result<Vec<Release>, ApiError> {
        self.releases
            .try_get_with(movie.clone(), self.provider.releases(&movie, 0, 0))
            .await
            .map_err(|failure| error(StatusCode::BAD_GATEWAY, failure.user_message()))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().nth(1).as_deref() == Some("hash-password") {
        let password = io::stdin()
            .lock()
            .lines()
            .next()
            .transpose()?
            .unwrap_or_default();
        if !(12..=128).contains(&password.len()) {
            return Err("Password must be 12–128 bytes.".into());
        }
        println!("{}", hash_password(&password)?);
        return Ok(());
    }

    let origin = env::var("MOVI_ORIGIN")?;
    let parsed_origin = reqwest::Url::parse(&origin)?;
    if parsed_origin.origin().ascii_serialization() != origin
        || !(parsed_origin.scheme() == "https"
            || (parsed_origin.scheme() == "http" && parsed_origin.host_str() == Some("localhost")))
    {
        return Err(
            "MOVI_ORIGIN must be the exact HTTPS origin or http://localhost for development".into(),
        );
    }
    let users: HashMap<String, String> = serde_json::from_str(&env::var("MOVI_ACCOUNTS")?)?;
    if users.is_empty()
        || users.iter().any(|(name, hash)| {
            name.is_empty()
                || name.len() > 80
                || !hash.starts_with("$argon2id$")
                || argon2::PasswordHash::new(hash).is_err()
        })
    {
        return Err("MOVI_ACCOUNTS must contain usernames and valid Argon2id hashes".into());
    }

    let app = App {
        provider: FourKHdHubClient::new()?,
        origin,
        users: Arc::new(users),
        tokens: Tokens::new(&env::var("MOVI_TOKEN_KEY")?)
            .ok_or("Expected a base64url-encoded 32-byte key")?,
        searches: Cache::builder()
            .max_capacity(128)
            .time_to_live(Duration::from_secs(60))
            .build(),
        releases: Cache::builder()
            .max_capacity(128)
            .time_to_live(Duration::from_secs(90))
            .build(),
        resolved: Cache::builder()
            .max_capacity(64)
            .time_to_live(Duration::from_secs(25))
            .build(),
        relay_limits: Cache::builder()
            .max_capacity(256)
            .time_to_live(Duration::from_secs(900))
            .build(),
        login_attempts: Cache::builder()
            .max_capacity(128)
            .time_to_live(Duration::from_secs(60))
            .build(),
        requests: Arc::new(tokio::sync::Semaphore::new(32)),
    };
    let port = env::var("X_ZOHO_CATALYST_LISTEN_PORT")
        .unwrap_or_else(|_| "8766".into())
        .parse::<u16>()?;
    let assets = env::var("MOVI_PLAYER_ASSETS").unwrap_or_else(|_| "dist".into());
    let router = Router::new()
        .route("/", get(|| async { Redirect::to("/movies") }))
        .route("/health", get(|| async { "ok" }))
        .route("/movies", get(movies))
        .route(
            "/sign-in",
            get(|| async { Html(include_str!("../../app/movies-login.html")) }).post(sign_in),
        )
        .route("/sign-out", post(sign_out))
        .route(
            "/movies.css",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../app/movies.css"),
                )
            }),
        )
        .route(
            "/movies.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../app/movies.js"),
                )
            }),
        )
        .route(
            "/movies-shaka.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../app/movies-shaka.js"),
                )
            }),
        )
        .route("/api/movies", get(search))
        .route("/api/movies/{id}/releases", get(releases))
        .route("/api/playback", post(playback))
        .route("/api/media/{token}", get(stream))
        .nest_service("/player", ServeDir::new(assets).precompressed_gzip())
        .layer(DefaultBodyLimit::max(24 * 1024))
        .layer(middleware::map_response(harden))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("Listening on port {port}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("content-security-policy", HeaderValue::from_static(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob:; connect-src 'self' https:; media-src 'self' https: blob:; worker-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
    ));
    headers
        .entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("private, no-store"));
    response
}

async fn movies(State(app): State<App>, headers: HeaderMap) -> Response {
    if app.user(&headers).is_err() {
        return Redirect::to("/sign-in").into_response();
    }
    Html(include_str!("../../app/movies.html")).into_response()
}

async fn sign_in(
    State(app): State<App>,
    headers: HeaderMap,
    Form(body): Form<Credentials>,
) -> Result<Response, ApiError> {
    use std::sync::atomic::{AtomicU32, Ordering};
    app.check_origin(&headers)?;
    if body.username.len() > 80 || body.password.len() > 128 {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid sign-in details."));
    }
    let bucket = if app.users.contains_key(body.username.trim()) {
        body.username.trim().to_owned()
    } else {
        String::new()
    };
    let attempts = app
        .login_attempts
        .get_with(bucket, async { Arc::new(AtomicU32::new(0)) })
        .await;
    if attempts.fetch_add(1, Ordering::Relaxed) >= 10 {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "Try signing in again in a minute.",
        ));
    }
    let user = body.username.trim().to_owned();
    let name = user.clone();
    let users = app.users.clone();
    let valid = tokio::task::spawn_blocking(move || check_password(&users, &name, &body.password))
        .await
        .unwrap_or(false);
    if !valid {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Html(include_str!("../../app/movies-login.html").replace(
                "<!-- LOGIN_ERROR -->",
                "<p role=\"alert\">The username or password is incorrect.</p>",
            )),
        )
            .into_response());
    }
    let session = app.tokens.seal(b"session", user, SESSION_TTL);
    Ok((
        [(SET_COOKIE, cookie(&session, SESSION_TTL))],
        Redirect::to("/movies"),
    )
        .into_response())
}

async fn sign_out(State(app): State<App>, headers: HeaderMap) -> Result<Response, ApiError> {
    app.check_origin(&headers)?;
    Ok(([(SET_COOKIE, cookie("", 0))], Redirect::to("/sign-in")).into_response())
}

fn timed<T: Serialize>(kind: &str, start: Instant, body: T) -> Response {
    let duration = start.elapsed().as_millis();
    (
        [("server-timing", format!("{kind};dur={duration}"))],
        Json(body),
    )
        .into_response()
}

async fn search(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<Search>,
) -> Result<Response, ApiError> {
    app.user(&headers)?;
    let started = Instant::now();
    let query = query.q.split_whitespace().collect::<Vec<_>>().join(" ");
    if !(2..=120).contains(&query.len()) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Search for a title using 2–120 bytes.",
        ));
    }
    let key = query.to_lowercase();
    let items = app
        .searches
        .try_get_with(key, app.provider.search(&query))
        .await
        .map_err(|failure| error(StatusCode::BAD_GATEWAY, failure.user_message()))?;
    let movies = items
        .into_iter()
        .filter(|item| item.media_type == MediaType::Movie)
        .map(|item| MovieCard {
            id: app.tokens.seal(b"movie", item.id.value, SESSION_TTL),
            title: item.title,
            year: item.year,
            poster_url: item.poster_url.filter(|url| {
                reqwest::Url::parse(url)
                    .ok()
                    .is_some_and(|url| net::valid_url(&url))
            }),
        })
        .collect::<Vec<_>>();
    Ok(timed("search", started, movies))
}

async fn releases(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    app.user(&headers)?;
    let started = Instant::now();
    let movie: String = app
        .tokens
        .open(b"movie", &id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Search again to reopen this movie."))?;
    let releases = app.releases(movie.clone()).await?;
    let recommended = releases
        .iter()
        .enumerate()
        .min_by_key(|(_, release)| {
            let resolution = match release.quality.as_deref() {
                Some("1080p") => 0,
                Some("720p" | "480p" | "360p") => 1,
                Some("2160p") => 2,
                _ => 3,
            };
            let hardware_friendly = matches!(
                release.codec.as_deref(),
                Some("H.264" | "H264" | "x264" | "AVC")
            );
            (
                resolution,
                !hardware_friendly,
                release.size_bytes.unwrap_or(u64::MAX),
            )
        })
        .map_or(0, |(index, _)| index);
    let cards = releases
        .into_iter()
        .enumerate()
        .map(|(index, release)| ReleaseCard {
            id: app.tokens.seal(
                b"release",
                ReleaseKey {
                    movie: movie.clone(),
                    filename: release.filename.clone(),
                },
                SESSION_TTL,
            ),
            filename: release.filename,
            quality: release.quality,
            codec: release.codec,
            language: release.language,
            size_bytes: release.size_bytes,
            recommended: index == recommended,
        })
        .collect::<Vec<_>>();
    Ok(timed("releases", started, cards))
}

async fn playback(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<PlaybackRequest>,
) -> Result<Response, ApiError> {
    let user = app.user(&headers)?;
    app.check_origin(&headers)?;
    let started = Instant::now();
    let key: ReleaseKey = app
        .tokens
        .open(b"release", &body.release_id)
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "Reopen this movie to load its releases again.",
            )
        })?;
    let excluded = match body.previous {
        Some(token) => {
            let media: Media = app
                .tokens
                .open(b"previous", &token)
                .filter(|media: &Media| {
                    media.user == user && media.movie == key.movie && media.release == key.filename
                })
                .ok_or_else(|| {
                    error(
                        StatusCode::BAD_REQUEST,
                        "This playback refresh has expired.",
                    )
                })?;
            Some(media.target)
        }
        None => None,
    };

    let resolve = async {
        let _permit = app.requests.clone().try_acquire_owned().map_err(|_| {
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "The resolver is busy. Try again shortly.",
            )
        })?;
        let releases = app.releases(key.movie.clone()).await?;
        let release = releases
            .iter()
            .find(|release| release.filename == key.filename)
            .ok_or_else(|| {
                error(
                    StatusCode::NOT_FOUND,
                    "This release is no longer available.",
                )
            })?;
        app.provider
            .resolve_release(release, &app.origin, excluded.as_deref())
            .await
            .map_err(|failure| error(StatusCode::BAD_GATEWAY, failure.user_message()))
    };

    let source = if excluded.is_some() {
        app.resolved.invalidate(&key).await;
        resolve.await?
    } else {
        app.resolved
            .try_get_with(key.clone(), resolve)
            .await
            .map_err(|failure| (*failure).clone())?
    };
    let media = Media {
        user,
        target: source.url.clone(),
        headers: source.headers,
        movie: key.movie,
        release: key.filename,
    };
    let handle = app.tokens.seal(b"media", &media, MEDIA_TTL);
    let relay_url = format!("/api/media/{handle}");
    Ok(timed(
        "resolve",
        started,
        Playback {
            url: if source.browser_readable {
                source.url
            } else {
                relay_url.clone()
            },
            relay_url,
            token: app.tokens.seal(b"previous", media, SESSION_TTL),
            expires_in: 20,
        },
    ))
}

async fn stream(
    State(app): State<App>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Ok(user) = app.user(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(media) = app
        .tokens
        .open::<Media>(b"media", &token)
        .filter(|media| media.user == user)
    else {
        return StatusCode::GONE.into_response();
    };
    let Ok(permit) = app.requests.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    media::relay(
        app.provider.http(),
        &app.relay_limits,
        media,
        method,
        &headers,
        permit,
    )
    .await
    .unwrap_or_else(IntoResponse::into_response)
}
