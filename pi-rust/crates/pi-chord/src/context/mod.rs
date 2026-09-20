//! Host context and cooperative cancellation — Rust port of
//! `packages/chord/src/context/index.ts`.
//!
//! A [`Context`] is an immutable, persistent chain of keyed values plus at most
//! one abort signal. Deriving a child ([`with_context_value`],
//! [`with_abort_signal`], [`with_cancel`]) never mutates the parent, which is
//! what lets a facet hold the invocation context it was given while a caller
//! adds scope on top.
//!
//! # Cancellation
//!
//! The port keeps the two upstream rules verbatim:
//!
//! - **A child inherits its parent's signal.** [`with_abort_signal`] combines
//!   the parent's signal with the supplied one, so either cancels the child.
//! - **A child cannot cancel its parent.** Cancellation travels down the chain
//!   only; a sibling is unaffected.
//!
//! [`without_abort_signal`] masks the chain, which is for mandatory cleanup
//! work: a facet tearing down a half-initialised resource must finish even when
//! the invocation that started it was cancelled.
//!
//! Cancellation is *cooperative*: aborting a signal wakes every waiter but does
//! not interrupt a future that never yields. [`await_with_context`] is the
//! observation point that turns an abort into a `Result`.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context as TaskContext, Poll, Waker};

static NEXT_KEY_ID: AtomicU64 = AtomicU64::new(1);

/// A typed key into a [`Context`].
///
/// Keys are created once and shared; the identity is the generated id, not the
/// description, so two keys with the same description never collide.
pub struct ContextKey<T> {
    id: u64,
    description: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ContextKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

// Hand-written rather than derived: `T` only appears behind `PhantomData`, so a
// key stays `Copy` even when the value type is not.
impl<T> Copy for ContextKey<T> {}

impl<T> ContextKey<T> {
    /// The human-readable description given at creation.
    pub fn description(&self) -> &'static str {
        self.description
    }

    /// The process-unique identity of the key.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl<T> fmt::Debug for ContextKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextKey")
            .field("id", &self.id)
            .field("description", &self.description)
            .finish()
    }
}

/// Create a new context key.
pub fn create_context_key<T>(description: &'static str) -> ContextKey<T> {
    ContextKey {
        id: NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed),
        description,
        _marker: PhantomData,
    }
}

/// Why a context was cancelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbortReason {
    /// A plain message, the port of a string (or any non-`Error`) reason.
    Message(String),
    /// An error reason, the port of an `Error` passed as the reason.
    Error(String),
}

impl AbortReason {
    /// The text of the reason.
    pub fn message(&self) -> &str {
        match self {
            AbortReason::Message(message) | AbortReason::Error(message) => message,
        }
    }
}

impl fmt::Display for AbortReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl Default for AbortReason {
    fn default() -> Self {
        AbortReason::Message("The operation was aborted".to_owned())
    }
}

impl From<&str> for AbortReason {
    fn from(value: &str) -> Self {
        AbortReason::Message(value.to_owned())
    }
}

impl From<String> for AbortReason {
    fn from(value: String) -> Self {
        AbortReason::Message(value)
    }
}

/// The error [`await_with_context`] rejects with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbortError {
    reason: AbortReason,
}

impl AbortError {
    /// Wrap a reason.
    pub fn new(reason: AbortReason) -> Self {
        AbortError { reason }
    }

    /// The reason the signal was cancelled with.
    pub fn reason(&self) -> &AbortReason {
        &self.reason
    }
}

impl fmt::Display for AbortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "aborted: {}", self.reason)
    }
}

impl std::error::Error for AbortError {}

/// A cancellation signal.
///
/// Cheap to clone; every clone observes the same cancellation.
#[derive(Clone)]
pub struct AbortSignal {
    inner: Arc<SignalInner>,
}

struct SignalInner {
    aborted: AtomicBool,
    reason: Mutex<Option<AbortReason>>,
    wakers: Mutex<Vec<Waker>>,
    children: Mutex<Vec<Weak<SignalInner>>>,
}

impl fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortSignal")
            .field("aborted", &self.is_aborted())
            .finish()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        AbortSignal::new()
    }
}

impl AbortSignal {
    /// A fresh signal that is not cancelled and will only cancel if someone
    /// calls [`AbortSignal::abort`] on it (directly or through a controller).
    pub fn new() -> Self {
        AbortSignal {
            inner: Arc::new(SignalInner {
                aborted: AtomicBool::new(false),
                reason: Mutex::new(None),
                wakers: Mutex::new(Vec::new()),
                children: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A signal cancelled by any of `signals`.
    ///
    /// The combined signal holds only weak references from its parents, so a
    /// long-lived parent does not keep a short-lived child alive.
    pub fn any(signals: impl IntoIterator<Item = AbortSignal>) -> Self {
        let combined = AbortSignal::new();
        for signal in signals {
            if let Some(reason) = signal.reason() {
                combined.abort(reason);
                return combined;
            }
            signal
                .inner
                .children
                .lock()
                .expect("signal children lock is never poisoned")
                .push(Arc::downgrade(&combined.inner));
        }
        combined
    }

    /// Whether the signal has been cancelled.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Acquire)
    }

    /// The reason, once cancelled.
    pub fn reason(&self) -> Option<AbortReason> {
        if !self.is_aborted() {
            return None;
        }
        self.inner
            .reason
            .lock()
            .expect("signal reason lock is never poisoned")
            .clone()
            .or_else(|| Some(AbortReason::default()))
    }

    /// Cancel the signal and every child derived from it.
    ///
    /// The first reason wins; a second call is a no-op. Usually called through
    /// an [`AbortController`].
    pub fn abort(&self, reason: impl Into<AbortReason>) {
        self.inner.abort(reason.into());
    }

    /// Await cancellation.
    pub fn wait(&self) -> AbortWait {
        AbortWait {
            signal: self.clone(),
        }
    }
}

impl SignalInner {
    fn abort(&self, reason: AbortReason) {
        if self.aborted.swap(true, Ordering::AcqRel) {
            return;
        }
        *self
            .reason
            .lock()
            .expect("signal reason lock is never poisoned") = Some(reason.clone());
        for waker in self
            .wakers
            .lock()
            .expect("signal wakers lock is never poisoned")
            .drain(..)
        {
            waker.wake();
        }
        let children: Vec<Arc<SignalInner>> = self
            .children
            .lock()
            .expect("signal children lock is never poisoned")
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for child in children {
            child.abort(reason.clone());
        }
    }
}

/// Owns a signal and can cancel it.
#[derive(Debug, Clone, Default)]
pub struct AbortController {
    signal: AbortSignal,
}

impl AbortController {
    /// Create a controller and its signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// The signal this controller cancels.
    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    /// Cancel the signal.
    pub fn abort(&self, reason: impl Into<AbortReason>) {
        self.signal.abort(reason);
    }
}

/// A future that resolves when an [`AbortSignal`] is cancelled.
#[derive(Debug)]
pub struct AbortWait {
    signal: AbortSignal,
}

impl Future for AbortWait {
    type Output = AbortReason;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<AbortReason> {
        if let Some(reason) = self.signal.reason() {
            return Poll::Ready(reason);
        }
        {
            let mut wakers = self
                .signal
                .inner
                .wakers
                .lock()
                .expect("signal wakers lock is never poisoned");
            if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
        }
        if let Some(reason) = self.signal.reason() {
            return Poll::Ready(reason);
        }
        Poll::Pending
    }
}

/// How a context node overrides the inherited abort signal.
#[derive(Clone)]
enum AbortSlot {
    /// Inherit from the parent.
    Inherit,
    /// Mask the parent's signal; the node has none.
    Mask,
    /// Replace the parent's signal with this one.
    Signal(AbortSignal),
}

struct Node {
    parent: Option<Arc<Node>>,
    key: Option<u64>,
    value: Box<dyn Any + Send + Sync>,
    abort: AbortSlot,
    label: String,
}

/// An immutable chain of context values.
#[derive(Clone)]
pub struct Context {
    node: Arc<Node>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.node.label)
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.node.label)
    }
}

impl Default for Context {
    fn default() -> Self {
        background_context().clone()
    }
}

impl Context {
    fn empty(label: &str) -> Context {
        Context {
            node: Arc::new(Node {
                parent: None,
                key: None,
                value: Box::new(()),
                abort: AbortSlot::Inherit,
                label: label.to_owned(),
            }),
        }
    }

    /// Look a value up the chain.
    pub fn value<T: 'static>(&self, key: &ContextKey<T>) -> Option<&T> {
        let mut current = Some(&*self.node);
        while let Some(node) = current {
            if node.key == Some(key.id) {
                return node.value.downcast_ref::<T>();
            }
            current = node.parent.as_deref();
        }
        None
    }

    /// The nearest abort signal, unless the chain masks it.
    pub fn abort_signal(&self) -> Option<&AbortSignal> {
        let mut current = Some(&*self.node);
        while let Some(node) = current {
            match &node.abort {
                AbortSlot::Inherit => {}
                AbortSlot::Mask => return None,
                AbortSlot::Signal(signal) => return Some(signal),
            }
            current = node.parent.as_deref();
        }
        None
    }

    /// A human-readable label, including every derived value.
    pub fn label(&self) -> &str {
        &self.node.label
    }

    /// Derive a child with one additional (or replaced) value.
    pub fn with_value<T: Send + Sync + 'static>(&self, key: ContextKey<T>, value: T) -> Context {
        Context {
            node: Arc::new(Node {
                parent: Some(self.node.clone()),
                key: Some(key.id),
                value: Box::new(value),
                abort: AbortSlot::Inherit,
                label: format!("{}.WithValue({})", self.node.label, key.description),
            }),
        }
    }

    fn with_abort(&self, slot: AbortSlot, label: String) -> Context {
        Context {
            node: Arc::new(Node {
                parent: Some(self.node.clone()),
                key: None,
                value: Box::new(()),
                abort: slot,
                label,
            }),
        }
    }
}

/// The context for work with no invocation behind it.
pub fn background_context() -> &'static Context {
    static CELL: OnceLock<Context> = OnceLock::new();
    CELL.get_or_init(|| Context::empty("[Context BACKGROUND_CONTEXT]"))
}

/// The context for a deferred TODO that outlives its invocation.
pub fn todo_context() -> &'static Context {
    static CELL: OnceLock<Context> = OnceLock::new();
    CELL.get_or_init(|| Context::empty("[Context TODO_CONTEXT]"))
}

/// Derive a context containing one additional or replaced value.
pub fn with_context_value<T: Send + Sync + 'static>(
    key: ContextKey<T>,
    value: T,
    parent: &Context,
) -> Context {
    parent.with_value(key, value)
}

/// Derive a context cancelled by either the parent signal or `signal`.
///
/// The parent context is unchanged.
pub fn with_abort_signal(signal: AbortSignal, context: &Context) -> Context {
    let combined = match context.abort_signal() {
        Some(parent) => AbortSignal::any([parent.clone(), signal]),
        None => signal,
    };
    context.with_abort(
        AbortSlot::Signal(combined),
        format!("{}.WithAbortSignal", context.label()),
    )
}

/// Derive a context retaining all values except caller cancellation.
///
/// Intended for mandatory cleanup only: teardown must complete even when the
/// invocation that started it was cancelled.
pub fn without_abort_signal(context: &Context) -> Context {
    context.with_abort(
        AbortSlot::Mask,
        format!("{}.WithoutAbortSignal", context.label()),
    )
}

/// Derive an independently cancellable child context.
pub fn with_cancel(context: &Context) -> (Context, CancelHandle) {
    let controller = AbortController::new();
    let child = with_abort_signal(controller.signal(), context);
    (child, CancelHandle { controller })
}

/// The canceller returned by [`with_cancel`].
#[derive(Debug, Clone)]
pub struct CancelHandle {
    controller: AbortController,
}

impl CancelHandle {
    /// Cancel the derived context.
    pub fn cancel(&self, reason: impl Into<AbortReason>) {
        self.controller.abort(reason);
    }

    /// The signal, for nested waits.
    pub fn signal(&self) -> AbortSignal {
        self.controller.signal()
    }
}

/// Observe a future until it settles or the invocation is cancelled.
///
/// Cancellation rejects only this waiter; it does not cancel the underlying
/// future. A context with no signal observes the future directly.
pub async fn await_with_context<F: Future>(
    future: F,
    context: &Context,
) -> Result<F::Output, AbortError> {
    let Some(signal) = context.abort_signal().cloned() else {
        return Ok(future.await);
    };
    if let Some(reason) = signal.reason() {
        return Err(AbortError::new(reason));
    }
    let mut future = std::pin::pin!(future);
    let mut wait = std::pin::pin!(signal.wait());
    std::future::poll_fn(move |cx| {
        if let Some(reason) = signal.reason() {
            return Poll::Ready(Err(AbortError::new(reason)));
        }
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if wait.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(AbortError::new(signal.reason().unwrap_or_default())));
        }
        Poll::Pending
    })
    .await
}

/// Drives `future` to completion on the calling thread.
///
/// `pi-chord` is runtime-agnostic and therefore ships the smallest executor that can run its
/// futures: a park/unpark loop over a thread waker. Embedders that already have an async runtime
/// should poll the futures on their own executor instead; this entry point exists so the crate's
/// tests (and small synchronous hosts) need no runtime dependency.
///
/// The future must not rely on a timer or on another task being polled concurrently, since nothing
/// else runs on this thread while it is parked.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl std::task::Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = TaskContext::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    #[test]
    fn a_value_is_visible_to_children_only() {
        let key = create_context_key::<u32>("answer");
        let root = background_context();
        let child = with_context_value(key, 42, root);
        assert_eq!(child.value(&key), Some(&42));
        assert_eq!(root.value(&key), None);
        assert!(child.to_string().contains("WithValue(answer)"));
    }

    #[test]
    fn the_nearest_value_wins() {
        let key = create_context_key::<u32>("answer");
        let root = background_context();
        let child = with_context_value(key, 1, root);
        let grandchild = with_context_value(key, 2, &child);
        assert_eq!(grandchild.value(&key), Some(&2));
        assert_eq!(child.value(&key), Some(&1));
        assert_eq!(root.value(&key), None);
        let sibling = with_context_value(key, 3, root);
        assert_eq!(sibling.value(&key), Some(&3));
    }

    #[test]
    fn keys_are_distinct_even_with_the_same_description() {
        let first = create_context_key::<u32>("answer");
        let second = create_context_key::<u32>("answer");
        let child = with_context_value(first, 1, background_context());
        assert_eq!(child.value(&second), None);
    }

    #[test]
    fn a_child_inherits_its_parent_signal() {
        let (parent, cancel) = with_cancel(background_context());
        let (child, _child_cancel) = with_cancel(&parent);
        cancel.cancel("parent");
        assert!(child.abort_signal().expect("signal").is_aborted());
    }

    #[test]
    fn a_child_cannot_cancel_its_parent() {
        let parent = background_context().clone();
        let (child, cancel) = with_cancel(&parent);
        cancel.cancel("child");
        assert!(parent.abort_signal().is_none());
        assert!(child.abort_signal().expect("signal").is_aborted());
    }

    #[test]
    fn masking_removes_the_signal_for_cleanup() {
        let (cancelled, cancel) = with_cancel(background_context());
        let cleanup = without_abort_signal(&cancelled);
        assert!(cleanup.abort_signal().is_none());
        cancel.cancel("now");
        assert!(cleanup.abort_signal().is_none());
    }

    #[test]
    fn any_signal_cancels_the_combination() {
        let controller = AbortController::new();
        let combined = AbortSignal::any([AbortSignal::new(), controller.signal()]);
        assert!(!combined.is_aborted());
        controller.abort("stop");
        assert!(combined.is_aborted());
        assert_eq!(combined.reason(), Some(AbortReason::Message("stop".into())));
    }

    #[test]
    fn cancellation_rejects_only_the_waiter() {
        let (context, cancel) = with_cancel(background_context());
        let wait = await_with_context(pending::<()>(), &context);
        cancel.cancel("gone");
        assert!(block_on(wait).is_err());
        assert_eq!(
            block_on(await_with_context(ready(1u8), background_context())),
            Ok(1)
        );
    }

    #[test]
    fn a_pending_waiter_is_woken_by_cancellation() {
        let (context, cancel) = with_cancel(background_context());
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cancel.cancel("late");
        });
        let result = block_on(await_with_context(pending::<()>(), &context));
        canceller.join().expect("canceller thread panicked");
        assert_eq!(result.unwrap_err().reason().message(), "late");
    }
}
