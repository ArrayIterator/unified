// Middleware manager: a priority-ordered chain of HTTP middleware.
//
// Modules contribute middleware through `Module::register_middleware`;
// the server builds one `Dispatcher` from the manager and wraps the
// Actix application with it, so every request flows through the chain
// before reaching the route handlers. A middleware either calls `next`
// to continue the chain or short-circuits by returning its own
// response (an authentication redirect, for example).
//
// Priorities decide the order: higher priorities run first, and
// middleware with the same priority runs in registration order. The
// built-in request lifecycle (pause / maintenance blocking, request
// hooks, logging) lives in `crate::http::server` outside this chain,
// so it always runs first regardless of module priorities.

use actix_service::boxed::BoxFuture;
use actix_web::body::{BoxBody, MessageBody};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready};
use std::fmt::Debug;
use std::future::{Ready, ready};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

/// The boxed future a middleware (or the rest of the chain) returns.
pub type MiddlewareResult = BoxFuture<Result<ServiceResponse<BoxBody>, actix_web::Error>>;

/// Calls the rest of the middleware chain with the request.
pub type NextFn = Box<dyn FnOnce(ServiceRequest) -> MiddlewareResult>;

/// One middleware in the chain.
///
/// `handle` receives the request and a `next` function for the rest of
/// the chain. Call `next(req).await` to continue; return a response
/// directly to short-circuit the chain.
pub trait Middleware: Send + Sync + Debug + 'static {
    /// Execution priority; higher runs first. Defaults to 0.
    fn priority(&self) -> isize {
        0
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult;
}

/// The Actix `Transform` built from a `MiddlewareManager`. One
/// dispatcher is created per worker thread.
#[derive(Debug, Clone)]
pub struct Dispatcher(Rc<Vec<Arc<dyn Middleware>>>);

impl Dispatcher {
    pub fn new(middlewares: Vec<Arc<dyn Middleware>>) -> Self {
        Dispatcher(Rc::new(middlewares))
    }
}

/// The service the dispatcher wraps around the inner application.
#[derive(Debug, Clone)]
pub struct DispatcherMiddleware<S> {
    middlewares: Rc<Vec<Arc<dyn Middleware>>>,
    inner: Rc<S>,
}

/// Future returned by the dispatcher service. It converts the inner
/// service's response body into a boxed body.
pub struct DispatcherFuture<F> {
    fut: F,
}

impl<F, B> Future for DispatcherFuture<F>
where
    F: Future<Output = Result<ServiceResponse<B>, actix_web::Error>>,
    B: MessageBody + 'static,
{
    type Output = Result<ServiceResponse, actix_web::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        match unsafe { Pin::new_unchecked(&mut this.fut) }.poll(cx) {
            Poll::Ready(Ok(res)) => Poll::Ready(Ok(res.map_into_boxed_body())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, B> Service<ServiceRequest> for DispatcherMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Future = DispatcherFuture<MiddlewareResult>;
    forward_ready!(inner);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        DispatcherFuture {
            fut: Self::do_dispatch(self.middlewares.clone(), self.inner.clone(), req, 0),
        }
    }
}

impl<S, B> DispatcherMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    fn do_dispatch(
        middlewares: Rc<Vec<Arc<dyn Middleware>>>,
        inner: Rc<S>,
        req: ServiceRequest,
        index: usize,
    ) -> MiddlewareResult {
        if index >= middlewares.len() {
            let fut = inner.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_boxed_body()) });
        }
        let middleware = middlewares[index].clone();
        let middlewares_clone = middlewares.clone();
        let inner_clone = inner.clone();
        let next: NextFn = Box::new(move |request| {
            Self::do_dispatch(middlewares_clone, inner_clone, request, index + 1)
        });
        middleware.handle(req, next)
    }
}

impl<S, B> Transform<S, ServiceRequest> for Dispatcher
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Transform = DispatcherMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        // Sort once per worker: higher priority first, registration
        // order kept within the same priority.
        let mut middlewares = (*self.0).clone();
        middlewares.sort_by(|a, b| b.priority().cmp(&a.priority()));
        ready(Ok(DispatcherMiddleware {
            middlewares: Rc::new(middlewares),
            inner: Rc::new(service),
        }))
    }
}

/// Wraps a plain function as middleware, for `register_function`.
#[derive(Clone)]
struct MiddlewareFunctionWrapper {
    func: Arc<dyn Fn(ServiceRequest, NextFn) -> MiddlewareResult + Send + Sync + 'static>,
}

impl Middleware for MiddlewareFunctionWrapper {
    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        (self.func)(req, next)
    }
}

impl Debug for MiddlewareFunctionWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MiddlewareFunctionWrapper")
    }
}

/// Manages the ordered collection of middleware in the pipeline.
///
/// Middleware can be appended in registration order; the dispatcher
/// sorts by priority when the application is built.
#[derive(Debug, Clone, Default)]
pub struct MiddlewareManager {
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareManager {
    /// Creates a new empty middleware manager.
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    /// Append a middleware to the pipeline.
    pub fn register<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    /// Append a `Default`-constructible middleware to the pipeline.
    pub fn register_by_type<M: Middleware + Default>(&mut self) -> &mut Self {
        self.register(M::default())
    }

    /// Append a plain function as middleware.
    pub fn register_function<F>(&mut self, func: F) -> &mut Self
    where
        F: Fn(ServiceRequest, NextFn) -> MiddlewareResult + Send + Sync + 'static,
    {
        self.middlewares.push(Arc::new(MiddlewareFunctionWrapper {
            func: Arc::new(func),
        }));
        self
    }

    /// Number of registered middleware.
    pub fn len(&self) -> usize {
        self.middlewares.len()
    }

    /// True when no middleware is registered.
    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    /// Remove every registered middleware.
    pub fn clear(&mut self) {
        self.middlewares.clear();
    }

    /// Build the Actix dispatcher for this pipeline. The dispatcher
    /// sorts by priority when it is installed on a worker.
    pub fn create_dispatcher(&self) -> Dispatcher {
        Dispatcher::new(self.middlewares.clone())
    }
}

impl IntoIterator for MiddlewareManager {
    type Item = Arc<dyn Middleware>;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.middlewares.into_iter()
    }
}
