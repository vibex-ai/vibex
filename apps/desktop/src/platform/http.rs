//! The HTTP client GPUI's asset loader fetches remote resources with.
//!
//! [`gpui::Application::with_platform`] installs a client that refuses every
//! request, so an image a Markdown document points at over `http(s)` never
//! reaches the screen: the text view hands the destination to the asset
//! loader, the loader asks the application for a client, and the answer is
//! "unsupported". A session transcript is where screenshots and figures from
//! the network show up, so the desktop installs this adapter over the reqwest
//! client it already ships for updates and Agent installs.
//!
//! Two details are deliberate:
//!
//! * The request runs on a Tokio runtime this client owns. GPUI polls asset
//!   loads on its own background executor, which is not a Tokio reactor, and
//!   reqwest needs one; the oneshot channel bridges the two.
//! * Responses are buffered under a byte cap. The image loader reads a
//!   response to the end before decoding it anyway, and a transcript is
//!   untrusted output, so a chunked body must not grow the process without
//!   bound.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures_channel::oneshot;
use futures_util::future::BoxFuture;
use futures_util::{AsyncReadExt as _, StreamExt as _};
use gpui::http_client::{
    AsyncBody, FollowRedirects, HttpClient, Inner, RedirectPolicy, Request, RequestTimeout,
    Response, StatusCode, Url,
    http::{Extensions, HeaderValue},
};

/// The longest a remote resource may take, including its body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long establishing the connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The most bytes one response may carry.
///
/// The image loader would otherwise read an unbounded body into memory; 32 MiB
/// is several times a full-screen screenshot and still a hard ceiling.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// The redirects a follow-all request may take before it gives up.
const MAX_REDIRECTS: usize = 10;

/// The name a remote server sees. Static, so the header value is validated once.
const USER_AGENT: &str = concat!("Vibex/", env!("CARGO_PKG_VERSION"));

/// The client the application installs for GPUI's asset loader.
pub(crate) fn client() -> Arc<dyn HttpClient> {
    Arc::new(ReqwestHttpClient::new())
}

/// An [`HttpClient`] backed by reqwest on a runtime of its own.
///
/// reqwest 0.13 fixes its redirect policy when the client is built, so the
/// adapter keeps one client that follows redirects and one that does not and
/// picks between them per request. [`RedirectPolicy::FollowLimit`] limits are
/// bounded by [`MAX_REDIRECTS`] rather than honored exactly; no caller on this
/// client asks for a different limit today.
pub(crate) struct ReqwestHttpClient {
    client: reqwest::Client,
    no_redirect_client: reqwest::Client,
    /// `None` only after [`Drop`] has shut the runtime down.
    runtime: Option<tokio::runtime::Runtime>,
    user_agent: HeaderValue,
}

impl ReqwestHttpClient {
    pub(crate) fn new() -> Self {
        let user_agent = HeaderValue::from_static(USER_AGENT);
        let client = client_with_redirects(&user_agent, true);
        let no_redirect_client = client_with_redirects(&user_agent, false);
        // Two workers are enough for the handful of images a transcript shows
        // and keep the footprint of a dedicated runtime small.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("vibex-http")
            .build()
            .expect("the remote-resource runtime should start");
        Self {
            client,
            no_redirect_client,
            runtime: Some(runtime),
            user_agent,
        }
    }
}

impl Drop for ReqwestHttpClient {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // A blocking shutdown panics if the client is dropped from an
            // async context, which tests and a future asset load can do; the
            // workers finish the task they are on and exit on their own.
            runtime.shutdown_background();
        }
    }
}

fn client_with_redirects(user_agent: &HeaderValue, follow: bool) -> reqwest::Client {
    let policy = if follow {
        reqwest::redirect::Policy::limited(MAX_REDIRECTS)
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(user_agent.clone())
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(policy)
        .build()
        .unwrap_or_default()
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for ReqwestHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        // reqwest already applies the environment's proxy settings.
        None
    }

    fn send(&self, request: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let client = if follows_redirects(request.extensions()) {
            self.client.clone()
        } else {
            self.no_redirect_client.clone()
        };
        let (sender, receiver) = oneshot::channel();
        let Some(runtime) = self.runtime.as_ref() else {
            return Box::pin(async { Err(anyhow!("the HTTP runtime is shut down")) });
        };
        runtime.spawn(async move {
            let _ = sender.send(execute(&client, request).await);
        });
        Box::pin(async move {
            receiver
                .await
                .map_err(|_| anyhow!("the HTTP runtime stopped before answering"))?
        })
    }
}

/// Perform one request and read its body, both under the size cap.
async fn execute(
    client: &reqwest::Client,
    request: Request<AsyncBody>,
) -> Result<Response<AsyncBody>> {
    let (parts, body) = request.into_parts();
    let uri = parts.uri.to_string();
    let mut outgoing = client.request(parts.method, uri.clone()).timeout(
        parts
            .extensions
            .get::<RequestTimeout>()
            .map_or(REQUEST_TIMEOUT, |timeout| timeout.0),
    );
    for (name, value) in &parts.headers {
        outgoing = outgoing.header(name, value);
    }
    let body = read_request_body(body).await?;
    if !body.is_empty() {
        outgoing = outgoing.body(body);
    }

    let response = outgoing
        .send()
        .await
        .with_context(|| format!("fetching {uri}"))?;
    let status = StatusCode::from_u16(response.status().as_u16())?;
    let headers = response.headers().clone();
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.with_context(|| format!("reading {uri}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("{uri} exceeds the {MAX_RESPONSE_BYTES}-byte response limit");
        }
        body.extend_from_slice(&chunk);
    }

    let mut response = Response::new(AsyncBody::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

/// Whether a request asked for redirects to be followed.
///
/// [`HttpClient::get`] encodes its `follow_redirects` flag as a
/// [`RedirectPolicy`] extension, and the explicit [`FollowRedirects`] marker
/// wins when a caller sets both.
fn follows_redirects(extensions: &Extensions) -> bool {
    if let Some(FollowRedirects(follow)) = extensions.get::<FollowRedirects>() {
        return *follow;
    }
    matches!(
        extensions.get::<RedirectPolicy>(),
        Some(RedirectPolicy::FollowLimit(_) | RedirectPolicy::FollowAll)
    )
}

/// Materialize a request body so it can be handed to reqwest.
///
/// GPUI's image loader sends an empty body; the buffering branch exists so a
/// JSON or form request through the same client still works.
async fn read_request_body(body: AsyncBody) -> Result<Vec<u8>> {
    match body.0 {
        Inner::Empty => Ok(Vec::new()),
        Inner::Bytes(cursor) => Ok(cursor.into_inner().to_vec()),
        Inner::AsyncReader(mut reader) => {
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .await
                .context("reading the request body")?;
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{
        Router,
        http::{StatusCode as AxumStatus, header},
        response::Redirect,
        routing::get,
    };

    const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nsession-image";

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{address}")
    }

    async fn read_all(response: Response<AsyncBody>) -> Vec<u8> {
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).await.unwrap();
        bytes
    }

    #[tokio::test]
    async fn fetches_a_remote_image_with_its_status_headers_and_body() {
        let router = Router::new().route(
            "/image.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], IMAGE) }),
        );
        let base = serve(router).await;

        let response = ReqwestHttpClient::new()
            .get(&format!("{base}/image.png"), ().into(), true)
            .await
            .expect("the image should load");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(read_all(response).await, IMAGE);
    }

    #[tokio::test]
    async fn follows_redirects_only_when_the_caller_asks() {
        let router = Router::new()
            .route("/image.png", get(|| async { IMAGE }))
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/image.png") }),
            );
        let base = serve(router).await;
        let client = ReqwestHttpClient::new();

        let followed = client
            .get(&format!("{base}/redirect"), ().into(), true)
            .await
            .expect("a followed redirect should load");
        assert_eq!(followed.status(), StatusCode::OK);
        assert_eq!(read_all(followed).await, IMAGE);

        let unfollowed = client
            .get(&format!("{base}/redirect"), ().into(), false)
            .await
            .expect("an unfollowed redirect is still a response");
        assert_eq!(unfollowed.status(), StatusCode::TEMPORARY_REDIRECT);
    }

    /// A server error is data the image loader reports, not a transport error.
    #[tokio::test]
    async fn keeps_an_error_status_available_to_the_caller() {
        let router = Router::new().route(
            "/missing.png",
            get(|| async { (AxumStatus::NOT_FOUND, "no such image") }),
        );
        let base = serve(router).await;

        let response = ReqwestHttpClient::new()
            .get(&format!("{base}/missing.png"), ().into(), true)
            .await
            .expect("the response should reach the caller");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(read_all(response).await, b"no such image");
    }

    #[tokio::test]
    async fn refuses_a_destination_that_is_not_a_url() {
        let error = ReqwestHttpClient::new()
            .get("session-image.png", ().into(), true)
            .await
            .err()
            .expect("a scheme-less destination cannot be fetched");
        assert!(!error.to_string().is_empty());
    }
}
