use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::future::ready;
use std::hash::{Hash, Hasher};
use std::io::{self, Write as _};
use std::path::{self, PathBuf};
use std::string::String;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use snafu::{OptionExt as _, ResultExt as _, Snafu};
use tokio_stream::wrappers::ReadDirStream;
use tower_service::Service;
use tracing::{debug, info};

const LOG_TARGET: &str = "axum::dpc";

pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type BoxedErrorResult<T> = std::result::Result<T, BoxedError>;

/// Handles ETag-based conditional requests
///
/// Takes the request headers, the ETag value, and response headers to modify.
/// If the client already has the current version (based on If-None-Match
/// header), returns a 304 Not Modified response.
///
/// Returns:
/// - Some(Response) if a 304 Not Modified should be returned
/// - None if processing should continue normally
pub fn handle_etag(
    req_headers: &axum::http::HeaderMap,
    etag: &str,
    resp_headers: &mut axum::http::HeaderMap,
) -> Option<axum::response::Response> {
    use axum::http::StatusCode;
    use axum::http::header::{ETAG, IF_NONE_MATCH};
    use axum::response::IntoResponse;

    // Add ETag header to response
    if let Ok(etag_value) = axum::http::HeaderValue::from_str(etag) {
        resp_headers.insert(ETAG, etag_value);
    }

    // Check if client already has this version
    if let Some(if_none_match) = req_headers.get(IF_NONE_MATCH) {
        if if_none_match.as_bytes() == etag.as_bytes() {
            return Some((StatusCode::NOT_MODIFIED, resp_headers.clone()).into_response());
        }
    }

    None
}

#[derive(Debug, Snafu)]
pub enum LoadError {
    #[snafu(display("IO error for {}", path.display()))]
    IO { source: io::Error, path: PathBuf },
    #[snafu(display("Invalid path: {}", path.display()))]
    InvalidPath { path: PathBuf },
}

/// Pre-loaded and pre-compressed static assets. This
/// is used to serve static assets from the build directory without reading from
/// disk, as the cache stays in RAM for the life of the server.
#[derive(Debug)]
pub struct StaticAssets(HashMap<String, StaticAsset>);

impl StaticAssets {
    pub async fn load(root_dir: &path::Path) -> Result<Self, LoadError> {
        info!(target: LOG_TARGET, dir=%root_dir.display(), "Loading assets");
        let mut cache = HashMap::default();

        let assets: Vec<Result<(String, StaticAsset), LoadError>> =
            read_dir_stream(root_dir.to_owned())
                .map(|file| async move {
                    let path = file.with_context(|_e| IOSnafu {  path: root_dir.to_owned() })?;
                    let filename = path.strip_prefix(root_dir).expect("Can't fail").to_str();
                    let ext = path.extension().and_then(|p| p.to_str());

                    let (filename, ext) = match (filename, ext) {
                        (Some(filename), Some(ext)) => (filename, ext),
                        _ => return Ok(None),
                    };

                    let stored_path = path
                        .clone()
                        .into_os_string()
                        .into_string()
                        .ok()
                        .with_context(|| InvalidPathSnafu { path: path.to_owned() })?;
                    tracing::debug!(path = %stored_path, "Loading asset");

                    let raw = tokio::fs::read(&path)
                        .await
                        .with_context(|_e| IOSnafu { path: path.to_owned()})?;

                    let compressed = match ext {
                        "css" | "js" | "svg" | "json" => Some(compress_data(&raw)),
                        _ => None,
                    }
                    .map(Bytes::from);

                    let raw_for_etag = raw.clone();
                    let path_for_etag = stored_path.clone();

                    Ok(Some((
                        filename.to_string(),
                        StaticAsset {
                            path: stored_path,
                            raw: match compressed.as_ref() { Some(compressed) => {
                                // if we have compressed copy, don't store raw data
                                // decompress the raw one one the fly if anyone actually asks
                                let compressed = compressed.clone();
                                LazyLock::new(Box::new(move || {
                                    debug!(target: LOG_TARGET, "Decompressing raw data from compressed version");
                                    Bytes::from(decompress_data(&compressed))
                                }))
                            } _ => {
                                LazyLock::new(Box::new(|| Bytes::from(raw)))
                            }},
                            compressed,
                            etag: LazyLock::new(Box::new(move || {
                                debug!(target: LOG_TARGET, path = %path_for_etag, "Calculating ETag for asset");
                                calculate_etag(&raw_for_etag)
                            })),
                        },
                    )))
                })
                .buffered(32)
                .filter_map(
                    |res_opt: Result<std::option::Option<(String, StaticAsset)>, LoadError>| {
                        ready(res_opt.transpose())
                    },
                )
                .collect::<Vec<_>>()
                .await;

        for asset_res in assets {
            let (filename, asset) = asset_res?;
            cache.insert(filename, asset);
        }

        for (key, asset) in &cache {
            tracing::debug!(%key, path = %asset.path, "Asset loaded");
        }
        tracing::debug!(target: LOG_TARGET, len = cache.len(), "Loaded assets");

        Ok(Self(cache))
    }

    /// Attempts to return a static asset from the cache from a cache key. If
    /// the asset is not found, `None` is returned.
    pub fn get(&self, key: &str) -> Option<&StaticAsset> {
        self.0.get(key)
    }
}

/// Represents a single static asset from the build directory. Assets are
/// represented as pre-compressed bytes via Brotli and their original content
/// type so the set_content_type middleware service can set the correct
/// Content-Type header.
#[derive(Debug)]
pub struct StaticAsset {
    pub path: String,
    pub compressed: Option<Bytes>,
    pub raw: LazyLock<Bytes, Box<dyn FnOnce() -> Bytes + Send>>,
    pub etag: LazyLock<String, Box<dyn FnOnce() -> String + Send>>,
}

impl StaticAsset {
    pub fn ext(&self) -> Option<&str> {
        let parts: Vec<&str> = self.path.split('.').collect();

        parts.last().copied()
    }
    /// Returns the content type of the asset based on its file extension.
    pub fn content_type(&self) -> Option<&'static str> {
        self.ext().and_then(|ext| {
            Some(match ext {
                "js" => "application/javascript",
                "css" => "text/css",
                "svg" => "image/svg+xml",
                "ico" => "image/x-icon",
                _ => return None,
            })
        })
    }
}

fn compress_data(input: &[u8]) -> Vec<u8> {
    let mut bytes = vec![];

    let mut writer = brotli::CompressorWriter::new(&mut bytes, 4096, 6, 20);

    writer.write_all(input).expect("Can't fail");

    drop(writer);

    bytes
}

fn decompress_data(input: &[u8]) -> Vec<u8> {
    let mut bytes = vec![];

    let mut writer = brotli::DecompressorWriter::new(&mut bytes, 4096);

    writer.write_all(input).expect("Can't fail");

    drop(writer);

    bytes
}

/// Calculate a hash-based ETag for the given data
fn calculate_etag(data: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("\"{}\"", hasher.finish())
}

fn read_dir_stream(dir: PathBuf) -> BoxStream<'static, io::Result<PathBuf>> {
    async_stream::try_stream! {
        let entries = ReadDirStream::new(
            tokio::fs::read_dir(dir).await?
        );

        futures::pin_mut!(entries);
        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type().await?;

            if file_type.is_dir() {
                let subdir = read_dir_stream(path);
                futures::pin_mut!(subdir);
                while let Some(res) = subdir.next().await {
                    let res = res?;
                    yield res;
                }
            } else if file_type.is_file() {
                yield path;
            }
        }
    }
    .boxed()
}

#[derive(Clone)]
pub struct StaticAssetService {
    assets: Arc<StaticAssets>,
}

impl StaticAssetService {
    pub fn new(assets: Arc<StaticAssets>) -> Self {
        Self { assets }
    }

    fn handle_request(&self, req: Request<Body>) -> Response<Body> {
        let path = req.uri().path().trim_start_matches('/');
        let Some(asset) = self.assets.get(path) else {
            return (StatusCode::NOT_FOUND, Body::empty()).into_response();
        };

        let req_headers = req.headers();
        let mut resp_headers = HeaderMap::new();

        // Set content type
        resp_headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(asset.content_type().unwrap_or("application/octet-stream")),
        );

        // Handle ETag and conditional request
        let etag = asset.etag.clone();
        if let Some(response) = crate::handle_etag(req_headers, &etag, &mut resp_headers) {
            return response;
        }

        let accepts_brotli = req_headers
            .get_all(ACCEPT_ENCODING)
            .into_iter()
            .any(|encodings| {
                let Ok(str) = encodings.to_str() else {
                    return false;
                };

                str.split(',').any(|s| s.trim() == "br")
            });

        let content = match (accepts_brotli, asset.compressed.as_ref()) {
            (true, Some(compressed)) => {
                resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
                compressed.clone()
            }
            _ => asset.raw.clone(),
        };

        (resp_headers, content).into_response()
    }
}

impl<B> Service<Request<B>> for StaticAssetService
where
    B: Send + 'static,
{
    type Response = Response<Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), std::convert::Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        req: Request<B>,
    ) -> BoxFuture<'static, Result<Response<Body>, std::convert::Infallible>> {
        let service = self.clone();
        let uri = req.uri().clone();

        Box::pin(async move {
            // Convert to a Request<Body> by extracting just the URI
            let new_req = Request::builder().uri(uri).body(Body::empty()).unwrap();

            Ok(service.handle_request(new_req))
        })
    }
}
