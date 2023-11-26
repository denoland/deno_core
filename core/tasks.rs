use std::marker::PhantomData;
use std::ops::DerefMut;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

use futures::lock::MutexGuard;
use futures::task::AtomicWaker;

type UnsendTask = Box<dyn FnOnce(&mut v8::HandleScope) + 'static>;
type SendTask = Box<dyn FnOnce(&mut v8::HandleScope) + Send + 'static>;

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
  _unsend_marker: PhantomData<MutexGuard<'static, ()>>,
}

impl V8TaskSpawner {
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
  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::HandleScope) + Send + 'static,
  {
    self.tasks.spawn(Box::new(f))
  }

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
    // complete fully, deadlock or panic.
    let task: SendTask = unsafe { std::mem::transmute(task) };
    self.tasks.spawn(task);
    rx.recv().unwrap()
  }
}
