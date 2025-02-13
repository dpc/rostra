use std::collections::HashMap;
use std::future::ready;
use std::io::{self, Write as _};
use std::path::{self, PathBuf};
use std::string::String;
use std::sync::LazyLock;

use axum::extract::Path;
use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt};
use rostra_util_error::WhateverResult;
use snafu::{OptionExt as _, ResultExt as _};
use tokio_stream::wrappers::ReadDirStream;
use tracing::{debug, info};

use crate::LOG_TARGET;

const HASH_SPLIT_CHAR: char = '.';

/// Maps static asset filenames to their compressed bytes and content type. This
/// is used to serve static assets from the build directory without reading from
/// disk, as the cache stays in RAM for the life of the server.
///
/// This type should be accessed via the `cache` property in `AppState`.
#[derive(Debug)]
pub struct AssetCache(HashMap<String, StaticAsset>);

impl AssetCache {
    /// Attempts to return a static asset from the cache from a cache key. If
    /// the asset is not found, `None` is returned.
    pub fn get(&self, key: &str) -> Option<&StaticAsset> {
        self.0.get(key)
    }

    /// Helper method to get a static asset from an extracted request path.
    pub fn get_from_path(&self, path: &Path<String>) -> Option<&StaticAsset> {
        let key = Self::get_cache_key(path);
        self.get(&key)
    }

    fn get_cache_key(path: &str) -> String {
        let mut parts = path.split(['.', HASH_SPLIT_CHAR]);

        let basename = parts.next().unwrap_or_default();
        let ext = parts.next_back().unwrap_or_default();

        format!("{}.{}", basename, ext)
    }

    pub async fn load_files(root_dir: &path::Path) -> WhateverResult<Self> {
        info!(target: LOG_TARGET, dir=%root_dir.display(), "Loading assets");
        let mut cache = HashMap::default();

        let assets: Vec<WhateverResult<(String, StaticAsset)>> =
            read_dir_stream(root_dir.to_owned())
                .map(|file| async move {
                    let path = file.whatever_context("Failed to read file metadata")?;
                    // let filename = path.file_name().and_then(|n| n.to_str());
                    let filename = path.strip_prefix(root_dir).expect("Can't fail").to_str();
                    // .and_then(|n| n.to_str());
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
                        .whatever_context("Invalid path")?;
                    tracing::debug!(path = %stored_path, "Loading asset");

                    let raw = tokio::fs::read(&path)
                        .await
                        .whatever_context("Could not read file")?;

                    let compressed = match ext {
                        "css" | "js" | "svg" | "json" => Some(compress_data(&raw)),
                        _ => None,
                    }
                    .map(Bytes::from);

                    Ok(Some((
                        filename.to_string(),
                        StaticAsset {
                            path: stored_path,
                            raw: if let Some(compressed) = compressed.as_ref() {
                                // if we have compressed copy, don't store raw data
                                // decompress the raw one one the fly if anyone actually asks
                                let compressed = compressed.clone();
                                LazyLock::new(Box::new(move || {
                                    debug!(target: LOG_TARGET, "Decompressing raw data from compressed version");
                                    Bytes::from(decompress_data(&compressed))
                                }))
                            } else {
                                LazyLock::new(Box::new(|| Bytes::from(raw)))
                            },
                            compressed,
                        },
                    )))
                })
                .buffered(16)
                .filter_map(
                    |res_opt: WhateverResult<std::option::Option<(String, StaticAsset)>>| {
                        ready(res_opt.transpose())
                    },
                )
                .collect::<Vec<_>>()
                .await;

        for asset_res in assets {
            let (filename, asset) = asset_res?;
            cache.insert(Self::get_cache_key(&filename), asset);
        }

        for (key, asset) in &cache {
            tracing::debug!(%key, path = %asset.path, "Asset loaded");
        }
        tracing::debug!(len = cache.len(), "Loaded assets");

        Ok(Self(cache))
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
// async fn read_dir_stream(dir: impl AsRef<path::Path>) -> impl Stream<Item =
// io::Result<PathBuf>> {
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
