//! Object storage for the EPUB library: hand-rolled AWS SigV4 presigning.
//!
//! Why hand-rolled: `aws-sdk-s3` is dozens of crates for what is here
//! exactly three operations — presign a GET/HEAD, presign a PUT, and a
//! streaming PUT. The daemon already vendors ring (HMAC) and sha2 (APNs
//! signs its ES256 JWTs the same way), so SigV4 is a small module over
//! those, with reqwest doing the talking.
//!
//! Configuration (`S3Config::from_env`, mirroring `push::ApnsArgs::config`):
//! - `TAISCE_BOOKS_BUCKET` / `TAISCE_BOOKS_REGION`: the library's bucket
//!   and its region (required with a bucket: no silent us-east-1). No
//!   bucket → `None`, and the whole library surface 404s, like
//!   `shares::ServerSide` in LOCAL mode.
//! - Credentials: explicit `TAISCE_BOOKS_ACCESS_KEY` /
//!   `TAISCE_BOOKS_SECRET_KEY` (+ optional `TAISCE_BOOKS_SESSION_TOKEN`)
//!   for dev, else the box's instance profile via IMDS, cached and
//!   refreshed before it expires.
//!
//! Downloads 302 to presigned URLs (the box never holds the bytes); an
//! ereader that opted into `stream_downloads` is proxied chunk-by-chunk
//! instead — see `library::book_file`. Uploads are write-through: the
//! request is answered only after the object is in the bucket.

use sha2::Digest as _;
use std::time::{Duration, SystemTime};

/// Presigned URLs live this long: long enough for an ereader to follow a
/// redirect, short enough that a leaked URL goes stale on its own.
pub const PRESIGN_TTL: Duration = Duration::from_secs(300);

const IMDS: &str = "http://169.254.169.254";
/// Refresh instance-profile credentials when this close to their expiry.
const IMDS_REFRESH_BEFORE: i64 = 600;

/// The HTTP method a URL is signed for (the signature binds it; a HEAD
/// download needs its own URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Put,
    Delete,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

/// An access key plus its secret (and session token for temporary creds).
/// Cloned out of the cache per signing; the secret never leaves the box.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

struct CachedCredentials {
    creds: Credentials,
    /// unix seconds; refreshed once within `IMDS_REFRESH_BEFORE` of it
    expires: i64,
}

enum Creds {
    /// Explicit env credentials (dev): they don't expire.
    Fixed(Credentials),
    /// Instance-profile credentials: refreshed from IMDS before expiry.
    Imds(std::sync::Mutex<Option<CachedCredentials>>),
}

/// The resolved bucket configuration: where, and how to sign for it.
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    creds: Creds,
    http: reqwest::Client,
    /// Tests: object writes and deletes land here instead of the network
    /// (presigning still runs for real — it is pure computation).
    #[cfg(test)]
    pub(crate) memory: std::sync::Arc<MemoryBucket>,
}

/// Tests' stand-in bucket: what was PUT (key → bytes) and DELETEd, and a
/// switch that fails PUTs to keys ending in a suffix (the error paths).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryBucket {
    pub objects: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
    pub deleted: std::sync::Mutex<Vec<String>>,
    pub fail_put_suffix: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl MemoryBucket {
    fn put(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        if let Some(sfx) = self.fail_put_suffix.lock().unwrap().as_deref()
            && key.ends_with(sfx)
        {
            anyhow::bail!("S3 PUT {key}: 503 SlowDown (test)");
        }
        self.objects.lock().unwrap().insert(key.to_string(), bytes);
        Ok(())
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }
}

/// An object on its way to a client ([`S3Config::get_object`]): S3's
/// status (200, 206 or 416), the length and range it named, the body.
pub struct ObjectBody {
    pub status: u16,
    pub content_length: Option<String>,
    pub content_range: Option<String>,
    pub body: axum::body::Body,
}

#[cfg(test)]
impl ObjectBody {
    /// The stand-in bucket's answer, with S3's single-range semantics
    /// (`bytes=a-b`, `bytes=a-`, `bytes=-n`); anything else is the whole
    /// object, as S3 ignores a range it cannot parse.
    fn from_memory(bytes: Vec<u8>, head: bool, range: Option<&str>) -> Self {
        let len = bytes.len() as u64;
        let parsed = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| {
            let (a, b) = r.split_once('-')?;
            match (a.trim(), b.trim()) {
                ("", n) => n.parse::<u64>().ok().map(|n| (len.saturating_sub(n), len.saturating_sub(1))),
                (a, "") => a.parse::<u64>().ok().map(|a| (a, len.saturating_sub(1))),
                (a, b) => Some((a.parse().ok()?, b.parse::<u64>().ok()?.min(len.saturating_sub(1)))),
            }
        });
        match parsed {
            Some((a, b)) if a >= len || a > b => ObjectBody {
                status: 416,
                content_length: None,
                content_range: Some(format!("bytes */{len}")),
                body: axum::body::Body::empty(),
            },
            Some((a, b)) => {
                let slice = bytes[a as usize..=b as usize].to_vec();
                ObjectBody {
                    status: 206,
                    content_length: Some(slice.len().to_string()),
                    content_range: Some(format!("bytes {a}-{b}/{len}")),
                    body: if head { axum::body::Body::empty() } else { slice.into() },
                }
            }
            None => ObjectBody {
                status: 200,
                content_length: Some(len.to_string()),
                content_range: None,
                body: if head { axum::body::Body::empty() } else { bytes.into() },
            },
        }
    }
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl S3Config {
    /// The library's object storage from the environment. `None` when no
    /// bucket is configured (LOCAL mode, or a server without one) — every
    /// library route then answers 404. An unreadable configuration (half
    /// the env creds, no instance profile) is an error the caller logs,
    /// like APNs.
    pub async fn from_env() -> anyhow::Result<Option<S3Config>> {
        let Some(bucket) = env_non_empty("TAISCE_BOOKS_BUCKET") else {
            return Ok(None);
        };
        // no default region: a bucket signed for the wrong region answers
        // every request with a redirect the presigned URLs cannot follow, so
        // a bucket without its region is a configuration error (logged, and
        // the library stays off) rather than a silent us-east-1
        let region = env_non_empty("TAISCE_BOOKS_REGION").ok_or_else(|| {
            anyhow::anyhow!("TAISCE_BOOKS_BUCKET is set but TAISCE_BOOKS_REGION is not (e.g. eu-west-1)")
        })?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // no total timeout: book bodies stream at the reader's pace,
            // and a 100 MB EPUB upload must not be cut by a wall-clock cap
            .user_agent(concat!("taisce/", env!("CARGO_PKG_VERSION"), " (library)"))
            .build()?;
        let creds = match (
            env_non_empty("TAISCE_BOOKS_ACCESS_KEY"),
            env_non_empty("TAISCE_BOOKS_SECRET_KEY"),
        ) {
            (Some(access_key), Some(secret_key)) => Creds::Fixed(Credentials {
                access_key,
                secret_key,
                session_token: env_non_empty("TAISCE_BOOKS_SESSION_TOKEN"),
            }),
            (None, None) => {
                // fetched now so a box with no profile fails loudly at boot
                let cached = fetch_imds_credentials().await?;
                Creds::Imds(std::sync::Mutex::new(Some(cached)))
            }
            _ => anyhow::bail!(
                "TAISCE_BOOKS_ACCESS_KEY and TAISCE_BOOKS_SECRET_KEY must be set together"
            ),
        };
        Ok(Some(S3Config {
            bucket,
            region,
            creds,
            http,
            #[cfg(test)]
            memory: Default::default(),
        }))
    }

    /// The S3 HTTP client (no total timeout: book bodies stream at the
    /// reader's pace). `library`'s stream-through downloads talk to
    /// presigned URLs through it.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Tests: a dummy bucket with fixed credentials. Presigning is pure
    /// computation, so every 302 path runs offline; only an actual PUT or
    /// stream-through would touch the network (tests never do one).
    #[cfg(test)]
    pub(crate) fn for_test() -> S3Config {
        // reqwest needs a process-wide rustls provider (main installs it)
        let _ = rustls::crypto::ring::default_provider().install_default();
        S3Config {
            bucket: "test-library".into(),
            region: "us-east-1".into(),
            creds: Creds::Fixed(Credentials {
                access_key: "AKIATEST".into(),
                secret_key: "testsecret".into(),
                session_token: None,
            }),
            http: reqwest::Client::new(),
            memory: Default::default(),
        }
    }

    /// The current credentials, refreshing the instance-profile cache when
    /// it is near expiry. A failed refresh is a hard error: signing with
    /// stale creds would mint URLs that only fail later, at the ereader.
    async fn credentials(&self) -> anyhow::Result<Credentials> {
        match &self.creds {
            Creds::Fixed(c) => Ok(c.clone()),
            Creds::Imds(cache) => {
                let now = crate::auth::now();
                {
                    let g = cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(c) = g.as_ref()
                        && c.expires > now + IMDS_REFRESH_BEFORE
                    {
                        return Ok(c.creds.clone());
                    }
                }
                let fresh = fetch_imds_credentials().await?;
                let creds = fresh.creds.clone();
                *cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fresh);
                Ok(creds)
            }
        }
    }

    /// A presigned GET or HEAD URL for `key`, optionally with S3's
    /// `response-content-disposition` override (how the ereader's download
    /// gets its derived filename on a redirect).
    pub async fn presign_get(
        &self,
        key: &str,
        method: Method,
        response_content_disposition: Option<&str>,
        ttl: Duration,
    ) -> anyhow::Result<String> {
        debug_assert!(method != Method::Put);
        let creds = self.credentials().await?;
        Ok(presign(
            &creds,
            &self.bucket,
            &self.region,
            method,
            key,
            response_content_disposition,
            ttl,
            SystemTime::now(),
        ))
    }

    /// A presigned PUT URL for `key`.
    pub async fn presign_put(&self, key: &str, ttl: Duration) -> anyhow::Result<String> {
        let creds = self.credentials().await?;
        Ok(presign(
            &creds,
            &self.bucket,
            &self.region,
            Method::Put,
            key,
            None,
            ttl,
            SystemTime::now(),
        ))
    }

    /// PUT an in-memory body (a cover: a few MB at most, capped at
    /// ingest). Errors carry S3's own message (its XML names the reason).
    pub async fn put_bytes(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        let _ = content_type;
        #[cfg(test)]
        return self.memory.put(key, body.to_vec());
        #[allow(unreachable_code)]
        let url = self.presign_put(key, PRESIGN_TTL).await?;
        let res = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .header(reqwest::header::CONTENT_LENGTH, body.len())
            .body(body.to_vec())
            .send()
            .await?;
        put_result(res, key).await
    }

    /// PUT a file, streaming it: a 100 MB EPUB must never sit whole in
    /// memory (the box has 1 GB and other work to do). `Content-Length`
    /// comes from the file's metadata — S3 refuses a chunked presigned
    /// PUT, and the spool doesn't change under us.
    pub async fn put_file(
        &self,
        key: &str,
        path: &std::path::Path,
        content_type: &str,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        let _ = content_type;
        #[cfg(test)]
        return self.memory.put(key, tokio::fs::read(path).await?);
        #[allow(unreachable_code)]
        let file = tokio::fs::File::open(path).await?;
        let len = file.metadata().await?.len();
        let url = self.presign_put(key, PRESIGN_TTL).await?;
        let res = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(reqwest::Body::from(file))
            .send()
            .await?;
        put_result(res, key).await
    }
}

impl S3Config {
    /// GET an object whole, refusing one over `cap` bytes (a cover: the
    /// colour-cover review reads covers stored before it existed). `None`
    /// when there is no such object.
    pub async fn get_bytes(&self, key: &str, cap: usize) -> anyhow::Result<Option<Vec<u8>>> {
        #[cfg(test)]
        {
            let got = self.memory.objects.lock().unwrap().get(key).cloned();
            if got.as_ref().is_some_and(|b| b.len() > cap) {
                anyhow::bail!("S3 GET {key}: over the {cap}-byte cap");
            }
            return Ok(got);
        }
        #[allow(unreachable_code)]
        let Some(mut res) = self.get_response(key).await? else {
            return Ok(None);
        };
        let mut out = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            if out.len() + chunk.len() > cap {
                anyhow::bail!("S3 GET {key}: over the {cap}-byte cap");
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Some(out))
    }

    /// GET an object into a file, streaming (an EPUB the colour-cover
    /// lookup reads the ISBNs of: never whole in memory), refusing one over
    /// `cap` bytes. False when there is no such object.
    pub async fn get_to_file(&self, key: &str, path: &std::path::Path, cap: u64) -> anyhow::Result<bool> {
        #[cfg(test)]
        {
            let got = self.memory.objects.lock().unwrap().get(key).cloned();
            return match got {
                None => Ok(false),
                Some(b) if b.len() as u64 > cap => anyhow::bail!("S3 GET {key}: over the {cap}-byte cap"),
                Some(b) => {
                    tokio::fs::write(path, b).await?;
                    Ok(true)
                }
            };
        }
        #[allow(unreachable_code)]
        {
            use tokio::io::AsyncWriteExt as _;
            let Some(mut res) = self.get_response(key).await? else {
                return Ok(false);
            };
            let mut file = tokio::fs::File::create(path).await?;
            let mut written: u64 = 0;
            while let Some(chunk) = res.chunk().await? {
                written += chunk.len() as u64;
                if written > cap {
                    anyhow::bail!("S3 GET {key}: over the {cap}-byte cap");
                }
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            Ok(true)
        }
    }

    /// One object for a client, streamed through the daemon (the web
    /// reader: a browser under `connect-src 'self'` cannot follow a 302 to
    /// the bucket). `range` is the client's `Range` header, passed to S3 as
    /// is (a presigned URL signs only `host`, so it rides unsigned); S3
    /// answers 206 with `Content-Range`, or 416. `None` when there is no
    /// such object. A HEAD asks S3 for a HEAD.
    pub async fn get_object(&self, key: &str, head: bool, range: Option<&str>) -> anyhow::Result<Option<ObjectBody>> {
        #[cfg(test)]
        {
            let got = self.memory.objects.lock().unwrap().get(key).cloned();
            return Ok(got.map(|b| ObjectBody::from_memory(b, head, range)));
        }
        #[allow(unreachable_code)]
        {
            let m = if head { Method::Head } else { Method::Get };
            let url = self.presign_get(key, m, None, PRESIGN_TTL).await?;
            let req = if head { self.http.head(url) } else { self.http.get(url) };
            let req = match range {
                Some(r) => req.header(reqwest::header::RANGE, r),
                None => req,
            };
            let res = req.send().await?;
            let status = res.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !status.is_success() && status != reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                let body = res.text().await.unwrap_or_default();
                let excerpt: String = body.chars().take(300).collect();
                anyhow::bail!("S3 GET {key}: {status} {excerpt}");
            }
            let header = |h: reqwest::header::HeaderName| res.headers().get(h).and_then(|v| v.to_str().ok()).map(str::to_owned);
            let content_length = header(reqwest::header::CONTENT_LENGTH);
            let content_range = header(reqwest::header::CONTENT_RANGE);
            let body = if head || status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                axum::body::Body::empty()
            } else {
                axum::body::Body::from_stream(res.bytes_stream())
            };
            Ok(Some(ObjectBody { status: status.as_u16(), content_length, content_range, body }))
        }
    }

    /// A presigned GET's response: `None` on 404, an error on any other
    /// refusal (S3's message excerpted).
    #[cfg_attr(test, allow(dead_code))]
    async fn get_response(&self, key: &str) -> anyhow::Result<Option<reqwest::Response>> {
        let url = self.presign_get(key, Method::Get, None, PRESIGN_TTL).await?;
        let res = self.http.get(url).send().await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            let excerpt: String = body.chars().take(300).collect();
            anyhow::bail!("S3 GET {key}: {status} {excerpt}");
        }
        Ok(Some(res))
    }
}

impl S3Config {
    /// DELETE an object. On the versioned bucket this only adds a delete
    /// marker: the object becomes a noncurrent version, which the bucket's
    /// lifecycle expires after 30 days (the undo window). A key that does
    /// not exist is a success (S3 answers 204 either way).
    pub async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        #[cfg(test)]
        {
            self.memory.objects.lock().unwrap().remove(key);
            self.memory.deleted.lock().unwrap().push(key.to_string());
            return Ok(());
        }
        #[allow(unreachable_code)]
        let creds = self.credentials().await?;
        let url = presign(&creds, &self.bucket, &self.region, Method::Delete, key, None, PRESIGN_TTL, SystemTime::now());
        let res = self.http.delete(url).send().await?;
        if res.status().is_success() {
            return Ok(());
        }
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(300).collect();
        anyhow::bail!("S3 DELETE {key}: {status} {excerpt}")
    }
}

async fn put_result(res: reqwest::Response, key: &str) -> anyhow::Result<()> {
    if res.status().is_success() {
        return Ok(());
    }
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let excerpt: String = body.chars().take(300).collect();
    anyhow::bail!("S3 PUT {key}: {status} {excerpt}")
}

/// Instance-profile credentials from IMDS (v2, falling back to v1 on
/// boxes without the token endpoint). The metadata address gets tiny
/// timeouts and no proxy: it is loopback-only, and a proxying setup must
/// fail fast rather than leak signing creds off-box.
async fn fetch_imds_credentials() -> anyhow::Result<CachedCredentials> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(3))
        .user_agent(concat!("taisce/", env!("CARGO_PKG_VERSION"), " (imds)"))
        .build()?;
    let token = client
        .put(format!("{IMDS}/latest/api/token"))
        .header("x-aws-ec2-metadata-token-ttl-seconds", "21600")
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success());
    let token = match token {
        Some(r) => Some(r.text().await.unwrap_or_default()),
        None => None,
    };
    let get = |path: String| {
        let req = client.get(format!("{IMDS}{path}"));
        match &token {
            Some(t) => req.header("x-aws-ec2-metadata-token", t.clone()),
            None => req,
        }
    };
    let role = get("/latest/meta-data/iam/security-credentials".into())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let role = role
        .lines()
        .next()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| anyhow::anyhow!("IMDS: this box has no instance profile"))?;
    let doc = get(format!("/latest/meta-data/iam/security-credentials/{role}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let v: serde_json::Value = serde_json::from_str(&doc)?;
    let field = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let access_key = field("AccessKeyId").ok_or_else(|| anyhow::anyhow!("IMDS: no AccessKeyId"))?;
    let secret_key =
        field("SecretAccessKey").ok_or_else(|| anyhow::anyhow!("IMDS: no SecretAccessKey"))?;
    let session_token = field("Token");
    let expires = field("Expiration")
        .and_then(|e| chrono::DateTime::parse_from_rfc3339(&e).ok())
        .map(|d| d.timestamp())
        // an instance profile without an expiry: re-read it within the hour
        .unwrap_or_else(|| crate::auth::now() + 3600);
    Ok(CachedCredentials {
        creds: Credentials {
            access_key,
            secret_key,
            session_token,
        },
        expires,
    })
}

// ---- the signing itself (pure: the clock is a parameter, so tests pin it) ----

/// AWS's URI encode: unreserved `[A-Za-z0-9-._~]` pass through, everything
/// else becomes `%XX` uppercase; `/` is kept in paths, encoded in values.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (b == b'/' && !encode_slash)
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The canonical query: names and values encoded, sorted by (name, value),
/// joined with `&`.
fn canonical_query(params: Vec<(&str, String)>) -> String {
    let mut encoded: Vec<(String, String)> = params
        .into_iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(&v, true)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&key, data);
    tag.as_ref().try_into().expect("HMAC-SHA256 is 32 bytes")
}

/// `kSigning`, the date/region/service derivation chain.
fn signing_key(secret: &str, date: &str, region: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    hmac_sha256(&k_service, b"aws4_request")
}

/// One presigned URL. Virtual-hosted–style bucket addressing on the
/// regional endpoint; only `host` is signed (a presigned URL's other
/// headers are the caller's business), the payload is UNSIGNED-PAYLOAD.
#[allow(clippy::too_many_arguments)]
fn presign(
    creds: &Credentials,
    bucket: &str,
    region: &str,
    method: Method,
    key: &str,
    response_content_disposition: Option<&str>,
    ttl: Duration,
    now: SystemTime,
) -> String {
    let host = format!("{bucket}.s3.{region}.amazonaws.com");
    presign_at(creds, &host, region, method, key, response_content_disposition, ttl, now)
}

/// [`presign`] against an explicit virtual host. Production always signs
/// for the regional host; the test pins AWS's documented vector, which was
/// published against the legacy global one (`examplebucket.s3.amazonaws.com`).
#[allow(clippy::too_many_arguments)]
fn presign_at(
    creds: &Credentials,
    host: &str,
    region: &str,
    method: Method,
    key: &str,
    response_content_disposition: Option<&str>,
    ttl: Duration,
    now: SystemTime,
) -> String {
    let t: chrono::DateTime<chrono::Utc> = now.into();
    let amz_date = t.format("%Y%m%dT%H%M%SZ").to_string();
    let date = t.format("%Y%m%d").to_string();
    let scope = format!("{date}/{region}/s3/aws4_request");
    let mut params = vec![
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", format!("{}/{scope}", creds.access_key)),
        ("X-Amz-Date", amz_date.clone()),
        ("X-Amz-Expires", ttl.as_secs().to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
    ];
    if let Some(token) = &creds.session_token {
        params.push(("X-Amz-Security-Token", token.clone()));
    }
    if let Some(d) = response_content_disposition {
        params.push(("response-content-disposition", d.to_string()));
    }
    let query = canonical_query(params);
    let uri = format!("/{}", uri_encode(key, false));
    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
        method.as_str(),
        uri,
        query,
        host
    );
    let request_hash = hex::encode(sha2::Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{request_hash}");
    let signature = hex::encode(hmac_sha256(
        &signing_key(&creds.secret_key, &date, region),
        string_to_sign.as_bytes(),
    ));
    format!("https://{host}{uri}?{query}&X-Amz-Signature={signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs_creds() -> Credentials {
        Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    /// AWS's own documented SigV4 presigned-URL example ("GET Object",
    /// docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html):
    /// bucket examplebucket, key test.txt, 24 h from 2013-05-24T00:00:00Z.
    /// The whole URL — canonical query, string to sign and signature — must
    /// match byte for byte.
    #[test]
    fn aws_documented_presigned_get_object() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1369353600);
        let url = presign_at(
            &docs_creds(),
            "examplebucket.s3.amazonaws.com",
            "us-east-1",
            Method::Get,
            "test.txt",
            None,
            Duration::from_secs(86400),
            now,
        );
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt?\
X-Amz-Algorithm=AWS4-HMAC-SHA256&\
X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&\
X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host&\
X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn uri_encode_keeps_unreserved_and_encodes_the_rest() {
        assert_eq!(
            uri_encode("library/u-1/Book (1).epub", false),
            "library/u-1/Book%20%281%29.epub"
        );
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("~tömü_", true), "~t%C3%B6m%C3%BC_");
    }

    /// The canonical query sorts by encoded name: a session token lands
    /// before SignedHeaders ("Se" < "Si"), the lowercase response-override
    /// after every X-Amz-*.
    #[test]
    fn canonical_query_sorts_by_encoded_name() {
        let q = canonical_query(vec![
            ("response-content-disposition".into(), "attachment".into()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
            ("X-Amz-Security-Token".into(), "tok en".into()),
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
        ]);
        assert_eq!(
            q,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Security-Token=tok%20en&X-Amz-SignedHeaders=host&response-content-disposition=attachment"
        );
    }

    /// A session token is signed into the URL; a disposition changes the
    /// signature (it is part of what S3 is asked to do).
    #[test]
    fn session_tokens_and_dispositions_are_signed() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1369353600);
        let plain = presign(
            &docs_creds(),
            "b",
            "us-east-1",
            Method::Get,
            "k",
            None,
            PRESIGN_TTL,
            now,
        );
        let with_token = presign(
            &Credentials {
                session_token: Some("tok".into()),
                ..docs_creds()
            },
            "b",
            "us-east-1",
            Method::Get,
            "k",
            None,
            PRESIGN_TTL,
            now,
        );
        assert!(with_token.contains("X-Amz-Security-Token=tok"));
        assert_ne!(plain, with_token, "the token is part of the signature");
        let with_disp = presign(
            &docs_creds(),
            "b",
            "us-east-1",
            Method::Get,
            "k",
            Some("attachment; filename=\"a.epub\""),
            PRESIGN_TTL,
            now,
        );
        assert!(
            with_disp
                .contains("response-content-disposition=attachment%3B%20filename%3D%22a.epub%22")
        );
        assert_ne!(plain, with_disp);
        // HEAD signs its own method: GET and HEAD URLs differ
        let head = presign(
            &docs_creds(),
            "b",
            "us-east-1",
            Method::Head,
            "k",
            None,
            PRESIGN_TTL,
            now,
        );
        assert_ne!(plain, head);
    }
}
