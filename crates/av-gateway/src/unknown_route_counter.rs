//! A `tower` layer counting a request whose gRPC path names no service this server serves --
//! this task's own acceptance evidence item 1(b): a raw gRPC request naming
//! `CommandAuthorityService` against this gateway's own port must be refused AND counted,
//! but `tonic` answers `Unimplemented` for an unmatched path entirely inside its own
//! generated router, before any of this workspace's code runs -- there is no typed refusal
//! value to count at that point through the normal `av_command::counters::Counters::record`
//! path. This layer is what makes that observable anyway: it inspects the raw HTTP/2 request
//! path BEFORE `tonic`'s router ever sees it, counts a path that names none of this server's
//! own known routes, and then ALWAYS forwards the call unchanged -- it never alters routing
//! or the response `tonic` itself produces, only observes.
//!
//! Attached to a `tonic::transport::Server` via `.layer(...)`, which this crate confirmed
//! (against the tonic `0.12.3` already in this workspace's lock file) applies to every
//! incoming request across every added service, before that request is dispatched to any of
//! them -- exactly the point this layer needs to run at.

use std::sync::Arc;
use std::task::{Context, Poll};

use av_command::counters::{Counted, Counters};

/// The one refusal this module counts. Not a rejection of the REQUEST in the normal sense
/// (this layer never blocks or alters it) -- a record that a caller reached for a path this
/// server does not serve at all, which is exactly the shape a crafted call to
/// `CommandAuthorityService` against this gateway's own port takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRoute {
    pub path: String,
}

impl Counted for UnknownRoute {
    fn code(&self) -> &'static str {
        "gateway_unknown_route"
    }
}

impl std::fmt::Display for UnknownRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request path {:?} names no service this server serves", self.path)
    }
}

/// The layer itself: `known_prefixes` are this server's own real route prefixes (e.g.
/// `"/altavista.v1.DataGatewayService/"`); any request whose path starts with none of them
/// increments `counters` under [`UnknownRoute`]'s code before being forwarded, unchanged, to
/// the real router.
#[derive(Clone)]
pub struct UnknownRouteCounterLayer {
    known_prefixes: Arc<Vec<String>>,
    counters: Arc<Counters>,
}

impl UnknownRouteCounterLayer {
    pub fn new(known_prefixes: Vec<String>, counters: Arc<Counters>) -> Self {
        Self { known_prefixes: Arc::new(known_prefixes), counters }
    }
}

impl<S> tower_layer::Layer<S> for UnknownRouteCounterLayer {
    type Service = UnknownRouteCounterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        UnknownRouteCounterService { inner, known_prefixes: self.known_prefixes.clone(), counters: self.counters.clone() }
    }
}

#[derive(Clone)]
pub struct UnknownRouteCounterService<S> {
    inner: S,
    known_prefixes: Arc<Vec<String>>,
    counters: Arc<Counters>,
}

impl<S, ReqBody> tower_service::Service<http::Request<ReqBody>> for UnknownRouteCounterService<S>
where
    S: tower_service::Service<http::Request<ReqBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    // Returns `S::Future` directly -- no boxing, no wrapping -- since this layer only ever
    // needs to run code BEFORE the inner call, never after it completes.
    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let path = req.uri().path();
        if !self.known_prefixes.iter().any(|known| path.starts_with(known.as_str())) {
            self.counters.record(&UnknownRoute { path: path.to_string() });
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use tower_service::Service as _;

    /// A minimal inner service: always returns a fixed response, records nothing itself --
    /// so this test isolates the LAYER's own behaviour (does it count exactly the paths it
    /// should, and does it always forward the call regardless).
    #[derive(Clone)]
    struct Echo;

    impl tower_service::Service<http::Request<()>> for Echo {
        type Response = &'static str;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Infallible>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: http::Request<()>) -> Self::Future {
            std::future::ready(Ok("forwarded"))
        }
    }

    fn request(path: &str) -> http::Request<()> {
        http::Request::builder().uri(path).body(()).unwrap()
    }

    #[tokio::test]
    async fn a_known_route_is_forwarded_and_not_counted() {
        let counters = Arc::new(Counters::new());
        let layer = UnknownRouteCounterLayer::new(vec!["/altavista.v1.DataGatewayService/".to_string()], counters.clone());
        let mut service = tower_layer::Layer::layer(&layer, Echo);
        let resp = service.call(request("/altavista.v1.DataGatewayService/Query")).await.unwrap();
        assert_eq!(resp, "forwarded");
        assert_eq!(counters.get("gateway_unknown_route"), 0);
    }

    #[tokio::test]
    async fn a_path_naming_no_known_service_is_counted_and_still_forwarded() {
        let counters = Arc::new(Counters::new());
        let layer = UnknownRouteCounterLayer::new(vec!["/altavista.v1.DataGatewayService/".to_string(), "/altavista.v1.ModelProposeService/".to_string()], counters.clone());
        let mut service = tower_layer::Layer::layer(&layer, Echo);
        let resp = service.call(request("/altavista.v1.CommandAuthorityService/Authorize")).await.unwrap();
        assert_eq!(resp, "forwarded", "the layer must never alter routing, only observe");
        assert_eq!(counters.get("gateway_unknown_route"), 1);
    }

    #[tokio::test]
    async fn several_unknown_paths_each_increment_the_same_code() {
        let counters = Arc::new(Counters::new());
        let layer = UnknownRouteCounterLayer::new(vec!["/altavista.v1.DataGatewayService/".to_string()], counters.clone());
        for path in ["/altavista.v1.CommandAuthorityService/Authorize", "/altavista.v1.CommandAuthorityService/Dispatch", "/not.even.a.real.package/Method"] {
            let mut service = tower_layer::Layer::layer(&layer, Echo);
            let _ = service.call(request(path)).await;
        }
        assert_eq!(counters.get("gateway_unknown_route"), 3);
    }
}
