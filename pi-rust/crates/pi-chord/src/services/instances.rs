//! Keyed instance lifetime and cancellable observation, ported from
//! `packages/chord/src/services/instances.ts`.
//!
//! [`InstanceDirectory`] owns two things upstream also owns in one place:
//!
//! * the keyed instances, addressed by key and fenced by generation, and
//! * one cancellable observation task per (observer, instance) pair.
//!
//! Upstream keeps the task list because a handler may be `async` and must be cancelled when its
//! instance disappears. This port is synchronous, so "cancelling the task" means aborting the
//! per-task [`Context`](crate::context::Context) derived with
//! [`with_cancel`](crate::context::with_cancel): a handler that stored the context observes the
//! abort, and — as upstream's keyed observation test asserts — any later access through the guarded
//! view fails with `service_stale_instance`.

use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::context::{background_context, with_cancel, CancelHandle, Context};

use super::errors::ServiceError;

/// One entry in an [`InstanceDirectory`].
///
/// Upstream's `InstanceDirectoryEntry` is `{ key, generation, service, deactivate }`. `service` is
/// the value handed to observers; in the keyed binding it is the per-instance
/// [`ServiceFacade`](super::consumer::ServiceFacade).
pub trait InstanceDirectoryEntry: Send + Sync + 'static {
    /// The value observers receive for this entry.
    type Service: Clone + Send + Sync + 'static;

    /// The instance key.
    fn key(&self) -> &str;

    /// The instance generation.
    fn generation(&self) -> u64;

    /// The observable service value.
    fn service(&self) -> Self::Service;

    /// Marks the entry inactive and drops its observer-visible state.
    fn deactivate(&self);
}

/// Reports failures that escape an observer handler.
pub type DirectoryErrorReporter = Arc<dyn Fn(ServiceError) + Send + Sync>;

type DirectoryHandler<E> =
    Arc<dyn Fn(<E as InstanceDirectoryEntry>::Service, &Context) + Send + Sync + 'static>;

struct Observer<E: InstanceDirectoryEntry> {
    handler: DirectoryHandler<E>,
    tasks: Mutex<HashMap<usize, CancelHandle>>,
    closed: AtomicBool,
}

/// Owns keyed instance lifetime and the cancellable tasks observing those instances.
pub struct InstanceDirectory<E: InstanceDirectoryEntry> {
    entries: Mutex<BTreeMap<String, Arc<E>>>,
    observers: Mutex<Vec<Arc<Observer<E>>>>,
    report_error: DirectoryErrorReporter,
    ready: AtomicBool,
    disposed: AtomicBool,
}

impl<E: InstanceDirectoryEntry> InstanceDirectory<E> {
    /// Creates a directory. `ready` starts `false` while a snapshot is being installed.
    pub fn new(ready: bool, report_error: DirectoryErrorReporter) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(Vec::new()),
            report_error,
            ready: AtomicBool::new(ready),
            disposed: AtomicBool::new(false),
        }
    }

    /// How many observers are registered.
    pub fn observer_count(&self) -> usize {
        self.observers.lock().len()
    }

    /// Looks an entry up by key.
    pub fn get(&self, key: &str) -> Option<Arc<E>> {
        self.entries.lock().get(key).cloned()
    }

    /// Inserts a new entry. Fails when the key is already live.
    pub fn insert(&self, entry: Arc<E>) -> Result<(), ServiceError> {
        self.assert_active()?;
        let key = entry.key().to_owned();
        if self.entries.lock().contains_key(&key) {
            return Err(ServiceError::message(format!(
                "Keyed service already has a live instance with key {key}"
            )));
        }
        self.entries.lock().insert(key, Arc::clone(&entry));
        if self.ready.load(Ordering::SeqCst) {
            self.start_all(&entry);
        }
        Ok(())
    }

    /// Inserts or replaces an entry. A repeated live generation is refused.
    pub fn replace(&self, entry: Arc<E>) -> Result<(), ServiceError> {
        self.assert_active()?;
        let key = entry.key().to_owned();
        let previous = self.entries.lock().get(&key).cloned();
        if let Some(previous) = previous {
            if previous.generation() == entry.generation() {
                return Err(ServiceError::message(
                    "Keyed service repeated a live generation",
                ));
            }
            self.remove_entry(&previous);
        }
        self.entries.lock().insert(key, Arc::clone(&entry));
        if self.ready.load(Ordering::SeqCst) {
            self.start_all(&entry);
        }
        Ok(())
    }

    /// Removes `entry` if it is still the live one for its key.
    pub fn remove(&self, entry: &Arc<E>) {
        if self
            .entries
            .lock()
            .get(entry.key())
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            self.remove_entry(entry);
        }
    }

    /// Marks the directory ready, starting observer handlers for every live entry.
    pub fn ready(&self) -> Result<(), ServiceError> {
        self.assert_active()?;
        if self.ready.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        for entry in self.entries.lock().values().cloned().collect::<Vec<_>>() {
            self.start_all(&entry);
        }
        Ok(())
    }

    /// Clears every entry and returns to the not-ready state. Observers stay registered.
    pub fn reset(&self) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        self.ready.store(false, Ordering::SeqCst);
        for entry in std::mem::take(&mut *self.entries.lock()).into_values() {
            self.deactivate(&entry);
        }
    }

    /// Registers `handler` for every current and future entry.
    ///
    /// The returned handle stops the observation; dropping the handle does not.
    pub fn observe<F>(self: &Arc<Self>, handler: F) -> Result<ObserveHandle<E>, ServiceError>
    where
        F: Fn(E::Service, &Context) + Send + Sync + 'static,
    {
        self.assert_active()?;
        let observer = Arc::new(Observer {
            handler: Arc::new(handler),
            tasks: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        self.observers.lock().push(Arc::clone(&observer));
        if self.ready.load(Ordering::SeqCst) {
            for entry in self.entries.lock().values().cloned().collect::<Vec<_>>() {
                self.start(&observer, &entry);
            }
        }
        Ok(ObserveHandle {
            directory: Arc::clone(self),
            observer,
            stopped: AtomicBool::new(false),
        })
    }

    /// Cancels every observer task and deactivates every entry.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        for observer in self.observers.lock().drain(..) {
            observer.closed.store(true, Ordering::SeqCst);
            for (_, cancel) in observer.tasks.lock().drain() {
                cancel.cancel("observation disposed");
            }
        }
        for entry in std::mem::take(&mut *self.entries.lock()).into_values() {
            entry.deactivate();
        }
    }

    fn assert_active(&self) -> Result<(), ServiceError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Keyed service directory is disposed"));
        }
        Ok(())
    }

    fn remove_entry(&self, entry: &Arc<E>) {
        self.entries.lock().remove(entry.key());
        self.deactivate(entry);
    }

    fn deactivate(&self, entry: &Arc<E>) {
        entry.deactivate();
        let pointer = entry_pointer(entry);
        for observer in self.observers.lock().iter() {
            if let Some(cancel) = observer.tasks.lock().remove(&pointer) {
                cancel.cancel("keyed instance removed");
            }
        }
    }

    fn start_all(&self, entry: &Arc<E>) {
        for observer in self.observers.lock().iter().cloned().collect::<Vec<_>>() {
            self.start(&observer, entry);
        }
    }

    fn start(&self, observer: &Arc<Observer<E>>, entry: &Arc<E>) {
        if observer.closed.load(Ordering::SeqCst) {
            return;
        }
        let pointer = entry_pointer(entry);
        if observer.tasks.lock().contains_key(&pointer) {
            return;
        }
        let (context, cancel) = with_cancel(background_context());
        observer.tasks.lock().insert(pointer, cancel);
        let handler = Arc::clone(&observer.handler);
        let service = entry.service();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| handler(service, &context)));
        if let Err(payload) = outcome {
            let aborted = context
                .abort_signal()
                .is_some_and(crate::context::AbortSignal::is_aborted);
            if !aborted {
                (self.report_error)(ServiceError::message(panic_message(&*payload)));
            }
        }
    }
}

/// The handle returned by [`InstanceDirectory::observe`].
pub struct ObserveHandle<E: InstanceDirectoryEntry> {
    directory: Arc<InstanceDirectory<E>>,
    observer: Arc<Observer<E>>,
    stopped: AtomicBool,
}

impl<E: InstanceDirectoryEntry> ObserveHandle<E> {
    /// Stops the observation. Idempotent.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        self.observer.closed.store(true, Ordering::SeqCst);
        for (_, cancel) in self.observer.tasks.lock().drain() {
            cancel.cancel("observation stopped");
        }
        self.directory
            .observers
            .lock()
            .retain(|candidate| !Arc::ptr_eq(candidate, &self.observer));
    }

    /// Whether the observation has been stopped.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

impl<E: InstanceDirectoryEntry> Drop for ObserveHandle<E> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn entry_pointer<E: InstanceDirectoryEntry>(entry: &Arc<E>) -> usize {
    Arc::as_ptr(entry) as *const () as usize
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "Remote service observer failed".to_owned()
    }
}
