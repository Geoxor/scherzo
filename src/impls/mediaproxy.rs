use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use harmony_rust_sdk::api::mediaproxy::{fetch_link_metadata_response::Data, *};
use hyper::{body::Buf, StatusCode, Uri};
use webpage::HTML;

use std::time::Instant;

use super::{get_mimetype, http, prelude::*, HttpClient};

#[derive(Clone)]
enum Metadata {
    Site(HTML),
    Media {
        filename: SmolStr,
        mimetype: SmolStr,
    },
}

struct TimedCacheValue<T> {
    value: T,
    since: Instant,
}

// TODO: investigate possible optimization since the key will always be an URL?
lazy_static::lazy_static! {
    static ref CACHE: DashMap<String, TimedCacheValue<Metadata>, RandomState> = DashMap::with_capacity_and_hasher(512, RandomState::new());
}

fn get_from_cache(url: &str) -> Option<Ref<'_, String, TimedCacheValue<Metadata>, RandomState>> {
    match CACHE.get(url) {
        // Value is available, check if it is expired
        Some(val) => {
            // Remove value if it is expired
            if val.since.elapsed().as_secs() >= 30 * 60 {
                drop(val); // explicit drop to tell we don't need it anymore
                CACHE.remove(url);
                None
            } else {
                Some(val)
            }
        }
        // No value available
        None => None,
    }
}

pub struct MediaproxyServer {
    http: HttpClient,
    valid_sessions: SessionMap,
    disable_ratelimits: bool,
}

impl MediaproxyServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            http: deps.http.clone(),
            valid_sessions: deps.valid_sessions.clone(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
        }
    }

    async fn fetch_metadata(&self, raw_url: String) -> Result<Metadata, ServerError> {
        // Get from cache if available
        if let Some(value) = get_from_cache(&raw_url) {
            return Ok(value.value.clone());
        }

        let url: Uri = raw_url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url.clone()).await?;
        if !response.status().is_success() {
            let err = if response.status() == StatusCode::NOT_FOUND {
                ServerError::LinkNotFound(url)
            } else {
                // TODO: change to proper error
                ServerError::InternalServerError
            };
            return Err(err);
        }

        let is_html = response
            .headers()
            .get(&http::header::CONTENT_TYPE)
            .and_then(|v| v.as_bytes().get(0..9))
            .map_or(false, |v| v.eq_ignore_ascii_case(b"text/html"));

        let metadata = if is_html {
            let body = hyper::body::aggregate(response.into_body()).await?;
            let html = String::from_utf8_lossy(body.chunk());
            let html = webpage::HTML::from_string(html.into(), Some(raw_url))?;
            Metadata::Site(html)
        } else {
            let filename = response
                .headers()
                .get(&http::header::CONTENT_DISPOSITION)
                .and_then(|val| val.to_str().ok())
                .map(|s| {
                    const FILENAME: &str = "filename=";
                    s.find(FILENAME)
                        .and_then(|f| s.get(f + FILENAME.len()..))
                        .unwrap_or(s)
                })
                .or_else(|| url.path().split('/').last().filter(|n| !n.is_empty()))
                .unwrap_or("unknown")
                .into();
            Metadata::Media {
                filename,
                mimetype: get_mimetype(&response).into(),
            }
        };

        // Insert to cache since successful
        CACHE.insert(
            url.to_string(),
            TimedCacheValue {
                value: metadata.clone(),
                since: Instant::now(),
            },
        );

        Ok(metadata)
    }
}

#[async_trait]
impl media_proxy_service_server::MediaProxyService for MediaproxyServer {
    #[rate(2, 1)]
    async fn fetch_link_metadata(
        &self,
        request: Request<FetchLinkMetadataRequest>,
    ) -> ServerResult<Response<FetchLinkMetadataResponse>> {
        auth!();

        let FetchLinkMetadataRequest { url } = request.into_message().await?;

        let data = match self.fetch_metadata(url).await? {
            Metadata::Site(mut html) => Data::IsSite(SiteMetadata {
                page_title: html.title.unwrap_or_default(),
                description: html.description.unwrap_or_default(),
                url: html.url.unwrap_or_default(),
                image: html
                    .opengraph
                    .images
                    .pop()
                    .map(|og| og.url)
                    .unwrap_or_default(),
                ..Default::default()
            }),
            Metadata::Media { filename, mimetype } => Data::IsMedia(MediaMetadata {
                mimetype: mimetype.into(),
                filename: filename.into(),
            }),
        };

        Ok((FetchLinkMetadataResponse { data: Some(data) }).into_response())
    }

    #[rate(1, 5)]
    async fn instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> ServerResult<Response<InstantViewResponse>> {
        auth!();

        let InstantViewRequest { url } = request.into_message().await?;

        let data = self.fetch_metadata(url).await?;

        let msg = if let Metadata::Site(html) = data {
            let metadata = SiteMetadata {
                page_title: html.title.unwrap_or_default(),
                description: html.description.unwrap_or_default(),
                url: html.url.unwrap_or_default(),
                ..Default::default()
            };

            InstantViewResponse {
                content: html.text_content,
                is_valid: true,
                metadata: Some(metadata),
            }
        } else {
            InstantViewResponse::default()
        };

        Ok(msg.into_response())
    }

    #[rate(20, 5)]
    async fn can_instant_view(
        &self,
        request: Request<CanInstantViewRequest>,
    ) -> ServerResult<Response<CanInstantViewResponse>> {
        auth!();

        let CanInstantViewRequest { url } = request.into_message().await?;

        if let Some(val) = get_from_cache(&url) {
            return Ok((CanInstantViewResponse {
                can_instant_view: matches!(val.value, Metadata::Site(_)),
            })
            .into_response());
        }

        let url: Uri = url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url).await.map_err(ServerError::from)?;

        let ok = get_mimetype(&response).eq("text/html");

        Ok((CanInstantViewResponse {
            can_instant_view: ok,
        })
        .into_response())
    }
}
