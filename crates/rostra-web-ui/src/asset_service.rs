use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures::future::BoxFuture;
use tower_service::Service;

use crate::asset_cache::StaticAssets;

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
            dbg!("NOT FOUND", path);
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
