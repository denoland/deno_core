use futures::task::AtomicWaker;
use std::marker::PhantomData;
use std::ops::DerefMut;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

type UnsendTask = Box<dyn FnOnce(&mut v8::HandleScope) + 'static>;
type SendTask = Box<dyn FnOnce(&mut v8::HandleScope) + Send + 'static>;

static_assertions::assert_not_impl_any!(V8TaskSpawner: Send);
static_assertions::assert_impl_all!(V8CrossThreadTaskSpawner: Send);

#[derive(Default)]
pub(crate) struct V8TaskSpawnerFactory {
  // TODO(mmastrac): ideally we wouldn't box if we could use arena allocation and a max submission size
  tasks: Mutex<Vec<SendTask>>,
  has_tasks: AtomicBool,
  waker: AtomicWaker,
}

impl V8TaskSpawnerFactory {
  pub fn new_same_thread_spawner(self: Arc<Self>) -> V8TaskSpawner {
    V8TaskSpawner {
      tasks: self,
      _unsend_marker: PhantomData,
    }
  }

  pub fn new_cross_thread_spawner(self: Arc<Self>) -> V8CrossThreadTaskSpawner {
    V8CrossThreadTaskSpawner { tasks: self }
  }

  pub fn has_pending_tasks(&self) -> bool {
    self.has_tasks.load(std::sync::atomic::Ordering::SeqCst)
  }

  pub fn poll_inner(&self, cx: &mut Context) -> Poll<Vec<UnsendTask>> {
    if !self
      .has_tasks
      .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
      self.waker.register(cx.waker());
      return Poll::Pending;
    }
    let tasks = std::mem::take(self.tasks.lock().unwrap().deref_mut());
    // SAFETY: we are removing !Send bounds as we return the tasks here
    let tasks = unsafe { std::mem::transmute(tasks) };
    Poll::Ready(tasks)
  }

  fn spawn(&self, task: SendTask) {
    self.tasks.lock().unwrap().push(task);
    // TODO(mmastrac): can we use a looser ordering here?
    self
      .has_tasks
      .store(true, std::sync::atomic::Ordering::SeqCst);
    self.waker.wake();
  }
}

/// Allows for submission of v8 tasks on the same thread.
#[derive(Clone)]
pub struct V8TaskSpawner {
  // TODO(mmastrac): can we split the waker into a send and !send one?
  tasks: Arc<V8TaskSpawnerFactory>,
  _unsend_marker: PhantomData<*const ()>,
}

impl V8TaskSpawner {
  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop from the same thread
  /// that the runtime is running on. This function is re-entrant safe and may be called from
  /// ops, from outside of a [`v8::HandleScope`], or even from within another, previously-spawned
  /// task.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen.
  ///
  /// # Important Notes
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::HandleScope) + 'static,
  {
    // SAFETY: we are transmuting Send into a !Send handle but we can guarantee this object will never
    // leave the current thread because `V8TaskSpawner` is !Send.
    let task: Box<dyn FnOnce(&mut v8::HandleScope<'_>)> = Box::new(f);
    let task: Box<dyn FnOnce(&mut v8::HandleScope<'_>) + Send> =
      unsafe { std::mem::transmute(task) };
    self.tasks.spawn(task)
  }
}

/// Allows for submission of v8 tasks on any thread.
#[derive(Clone)]
pub struct V8CrossThreadTaskSpawner {
  tasks: Arc<V8TaskSpawnerFactory>,
}

impl V8CrossThreadTaskSpawner {
  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop, potentially from a different
  /// thread than the runtime is running on.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen.
  ///
  /// # Important Notes
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::HandleScope) + Send + 'static,
  {
    self.tasks.spawn(Box::new(f))
  }

  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop from a different thread
  /// than the runtime is running on.
  ///
  /// This function will deadlock if called from the same thread as the [`crate::JsRuntime`], and
  /// there are no checks for this case.
  ///
  /// As this function blocks until the task has run to completion (or panics/deadlocks), it is
  /// safe to borrow data from the local environment and use it within the closure.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen, however the function will not return until the
  /// task has been fully run to completion.
  ///
  /// ```
  /// # let factory = Arc::new(V8TaskSpawnerFactory::default());
  /// # let spawner = factory.new_cross_thread_spawner();
  /// let mut v = vec!["string"];
  /// // We can safely access surrounding locals in this method
  /// let slice2 = spawner.spawn_blocking(|_| {
  ///   v.as_mut_slice()
  /// });
  /// # assert_eq!(*slice2.get(0).unwrap(), "string");
  /// ```
  ///
  /// # Important Notes
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn_blocking<'a, F, T>(&self, f: F) -> T
  where
    F: FnOnce(&mut v8::HandleScope) -> T + Send + 'a,
    T: Send + 'a,
  {
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    let task: Box<dyn FnOnce(&mut v8::HandleScope<'_>) + Send> =
      Box::new(|scope| {
        let r = f(scope);
        _ = tx.send(r);
      });
    // SAFETY: We can safely transmute to the 'static lifetime because we guarantee this method will either
    // complete fully by the time it returns, deadlock or panic.
    let task: SendTask = unsafe { std::mem::transmute(task) };
    self.tasks.spawn(task);
    rx.recv().unwrap()
  }
}
