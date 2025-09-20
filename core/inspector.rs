// Copyright 2018-2025 the Deno authors. MIT license.

//! The documentation for the inspector API is sparse, but these are helpful:
//! <https://chromedevtools.github.io/devtools-protocol/>
//! <https://hyperandroid.com/2020/02/12/v8-inspector-from-an-embedder-standpoint/>

use crate::error::CoreError;
use crate::error::CoreErrorKind;
use crate::futures::channel::mpsc;
use crate::futures::channel::mpsc::UnboundedReceiver;
use crate::futures::channel::mpsc::UnboundedSender;
use crate::futures::channel::oneshot;
use crate::futures::prelude::*;
use crate::futures::stream::FuturesUnordered;
use crate::futures::stream::StreamExt;
use crate::futures::task;
use crate::serde_json::json;

use boxed_error::Boxed;
use deno_error::JsErrorBox;
use futures::channel::oneshot::Canceled;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::take;
use std::pin::Pin;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::thread;
use thiserror::Error;
use v8::HandleScope;

#[derive(Debug)]
pub enum InspectorMsgKind {
  Notification,
  Message(i32),
}

#[derive(Debug)]
pub struct InspectorMsg {
  pub kind: InspectorMsgKind,
  pub content: String,
}

// TODO(bartlomieju): remove this
type NewSessionTxMsg = (
  InspectorSessionSend,
  UnboundedReceiver<String>,
  InspectorSessionKind,
);

pub type InspectorSessionSend = Box<dyn Fn(InspectorMsg) + Send>;

#[derive(Clone, Copy, Debug)]
enum PollState {
  Idle,
  Woken,
  Polling,
  Parked,
  Dropped,
}

pub struct InspectorIoDelegate {
  pub new_session_tx: UnboundedSender<NewSessionTxMsg>,
  pub deregister_rx: Mutex<oneshot::Receiver<()>>,
}

impl InspectorIoDelegate {
  pub fn new_remote_session(
    &self,
    callback: InspectorSessionSend,
    // TODO(bartlomieju): renamec
    rx: UnboundedReceiver<String>,
    kind: InspectorSessionKind,
  ) {
    let _ = self.new_session_tx.unbounded_send((callback, rx, kind));
  }

  // TODO(bartlomieju): this is an antipattern - it should be implemented in drop
  // or something like this...
  pub fn poll_deregister_rx(
    &self,
    cx: &mut Context,
  ) -> Poll<Result<(), Canceled>> {
    let mut rx = self.deregister_rx.lock();
    rx.poll_unpin(cx)
  }
}

/// This structure is used responsible for providing inspector interface
/// to the `JsRuntime`.
///
/// It stores an instance of `v8::inspector::V8Inspector` and additionally
/// implements `v8::inspector::V8InspectorClientImpl`.
///
/// After creating this structure it's possible to connect multiple sessions
/// to the inspector, in case of Deno it's either: a "websocket session" that
/// provides integration with Chrome Devtools, or an "in-memory session" that
/// is used for REPL or coverage collection.
pub struct JsRuntimeInspector {
  v8_inspector: Rc<v8::inspector::V8Inspector>,
  new_session_tx: UnboundedSender<NewSessionTxMsg>,
  deregister_tx: RefCell<Option<oneshot::Sender<()>>>,
  state: Rc<JsRuntimeInspectorState>,
}

impl Drop for JsRuntimeInspector {
  fn drop(&mut self) {
    // Since the waker is cloneable, it might outlive the inspector itself.
    // Set the poll state to 'dropped' so it doesn't attempt to request an
    // interrupt from the isolate.
    self
      .state
      .waker
      .update(|w| w.poll_state = PollState::Dropped);

    // V8 automatically deletes all sessions when an `V8Inspector` instance is
    // deleted, however InspectorSession also has a drop handler that cleans
    // up after itself. To avoid a double free, make sure the inspector is
    // dropped last.
    self.state.sessions.borrow_mut().drop_sessions();

    // Notify counterparty that this instance is being destroyed. Ignoring
    // result because counterparty waiting for the signal might have already
    // dropped the other end of channel.
    if let Some(deregister_tx) = self.deregister_tx.borrow_mut().take() {
      let _ = deregister_tx.send(());
    }
  }
}

#[derive(Clone)]
struct JsRuntimeInspectorState {
  isolate_ptr: *mut v8::Isolate,
  context: v8::Global<v8::Context>,
  flags: Rc<RefCell<InspectorFlags>>,
  waker: Arc<InspectorWaker>,
  sessions: Rc<RefCell<SessionContainer>>,
  is_dispatching_message: Rc<RefCell<bool>>,
}

struct JsRuntimeInspectorClient(Rc<JsRuntimeInspectorState>);

impl v8::inspector::V8InspectorClientImpl for JsRuntimeInspectorClient {
  fn run_message_loop_on_pause(&self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.0.flags.borrow_mut().on_pause = true;
    let _ = self.0.poll_sessions(None);
  }

  fn quit_message_loop_on_pause(&self) {
    self.0.flags.borrow_mut().on_pause = false;
  }

  fn run_if_waiting_for_debugger(&self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.0.flags.borrow_mut().waiting_for_session = false;
  }

  fn ensure_default_context_in_group(
    &self,
    context_group_id: i32,
  ) -> Option<v8::Local<'_, v8::Context>> {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    let context = self.0.context.clone();
    let isolate: &mut v8::Isolate = unsafe { &mut *(self.0.isolate_ptr) };
    let scope = &mut unsafe { v8::CallbackScope::new(isolate) };
    Some(v8::Local::new(scope, context))
  }

  fn resource_name_to_url(
    &self,
    resource_name: &v8::inspector::StringView,
  ) -> Option<v8::UniquePtr<v8::inspector::StringBuffer>> {
    let resource_name = resource_name.to_string();
    let url = url::Url::from_file_path(resource_name).ok()?;
    let src_view = v8::inspector::StringView::from(url.as_str().as_bytes());
    Some(v8::inspector::StringBuffer::create(src_view))
  }
}

impl JsRuntimeInspectorState {
  #[allow(clippy::result_unit_err)]
  pub fn poll_sessions(
    &self,
    mut invoker_cx: Option<&mut Context>,
  ) -> Result<Poll<()>, ()> {
    // The futures this function uses do not have re-entrant poll() functions.
    // However it is can happen that poll_sessions() gets re-entered, e.g.
    // when an interrupt request is honored while the inspector future is polled
    // by the task executor. We let the caller know by returning some error.
    let Ok(mut sessions) = self.sessions.try_borrow_mut() else {
      return Err(());
    };

    // current_state -> PollState::Idle
    self.waker.update(|w| {
      match w.poll_state {
        PollState::Idle | PollState::Woken => w.poll_state = PollState::Polling,
        _ => unreachable!(),
      };
    });
    // current_state -> PollState::Polling

    // Create a new Context object that will make downstream futures
    // use the InspectorWaker when they are ready to be polled again.
    let waker_ref = task::waker_ref(&self.waker);
    let cx = &mut Context::from_waker(&waker_ref);

    loop {
      // current_state -> PollState::Polling
      // starts polling for session_rx -> must be non-ready
      // self.established is empty - must be pending/poll::ready(None)
      sessions.pump_messages_for_remote_sessions(cx);

      // current_state -> PollState::Polling
      // both flags are false
      let should_block = {
        let flags = self.flags.borrow();
        flags.on_pause || flags.waiting_for_session
      };

      // current_state -> PollState::Polling and should_block = false
      // going to idle, registers the waker context if any - and it's `None` because it's on creation
      // stores the state_ptr
      // when the waker wakes from the outside, the `state_ptr` is used to poll remote sessions
      // and make progress
      let new_state = self.waker.update(|w| {
        match w.poll_state {
          PollState::Woken => {
            // The inspector was woken while the session handler was being
            // polled, so we poll it another time.
            w.poll_state = PollState::Polling;
          }
          PollState::Polling if !should_block => {
            // The session handler doesn't need to be polled any longer, and
            // there's no reason to block (execution is not paused), so this
            // function is about to return.
            w.poll_state = PollState::Idle;
            // Register the task waker that can be used to wake the parent
            // task that will poll the inspector future.
            if let Some(cx) = invoker_cx.take() {
              w.task_waker.replace(cx.waker().clone());
            }
            // Register the address of the inspector, which allows the waker
            // to request an interrupt from the isolate.
            w.state_ptr = NonNull::new(self as *const _ as *mut Self);
          }
          PollState::Polling if should_block => {
            // Isolate execution has been paused but there are no more
            // events to process, so this thread will be parked. Therefore,
            // store the current thread handle in the waker so it knows
            // which thread to unpark when new events arrive.
            w.poll_state = PollState::Parked;
            w.parked_thread.replace(thread::current());
          }
          _ => unreachable!(),
        };
        w.poll_state
      });
      match new_state {
        PollState::Idle => break,            // Yield to task.
        PollState::Polling => continue,      // Poll the session handler again.
        PollState::Parked => thread::park(), // Park the thread.
        _ => unreachable!(),
      };
    }

    Ok(Poll::Pending)
  }
}

impl JsRuntimeInspector {
  /// Currently Deno supports only a single context in `JsRuntime`
  /// and thus it's id is provided as an associated constant.
  const CONTEXT_GROUP_ID: i32 = 1;

  pub fn new(
    isolate_ptr: *mut v8::Isolate,
    scope: &mut v8::HandleScope,
    context: v8::Local<v8::Context>,
    is_main_runtime: bool,
  ) -> Rc<Self> {
    let (new_session_tx, new_session_rx) = mpsc::unbounded::<NewSessionTxMsg>();

    let waker = InspectorWaker::new(scope.thread_safe_handle());

    let state = Rc::new(JsRuntimeInspectorState {
      waker,
      flags: Default::default(),
      isolate_ptr,
      context: v8::Global::new(scope, context),
      sessions: Rc::new(
        RefCell::new(SessionContainer::temporary_placeholder()),
      ),
      is_dispatching_message: Default::default(),
    });
    let client = Box::new(JsRuntimeInspectorClient(state.clone()));
    let v8_inspector_client = v8::inspector::V8InspectorClient::new(client);
    let v8_inspector = Rc::new(v8::inspector::V8Inspector::create(
      scope,
      v8_inspector_client,
    ));

    *state.sessions.borrow_mut() = SessionContainer::new(
      v8_inspector.clone(),
      new_session_rx,
      state.is_dispatching_message.clone(),
    );

    let inspector = Rc::new(Self {
      v8_inspector,
      state,
      new_session_tx,
      deregister_tx: RefCell::new(None),
    });

    inspector.context_created(context, is_main_runtime);
    // TODO(bartlomieju): we need to only poll `session_rx` once - just
    // to register the task context, at this point it's guaranteed there are
    // senders, so no need to call the complicated `poll_sessions`.

    // Poll the session handler so we will get notified whenever there is
    // new incoming debugger activity.
    let _ = inspector.state.poll_sessions(None).unwrap();

    inspector
  }

  pub(crate) fn is_dispatching_message(&self) -> bool {
    *self.state.is_dispatching_message.borrow()
  }

  pub fn context_created(
    &self,
    context: v8::Local<v8::Context>,
    is_main_runtime: bool,
  ) {
    // Tell the inspector about the main realm.
    let context_name = v8::inspector::StringView::from(&b"main realm"[..]);
    // NOTE(bartlomieju): this is what Node.js does and it turns out some
    // debuggers (like VSCode) rely on this information to disconnect after
    // program completes
    let aux_data = if is_main_runtime {
      r#"{"isDefault": true}"#
    } else {
      r#"{"isDefault": false}"#
    };
    let aux_data_view = v8::inspector::StringView::from(aux_data.as_bytes());
    self.v8_inspector.context_created(
      context,
      Self::CONTEXT_GROUP_ID,
      context_name,
      aux_data_view,
    );
  }

  pub fn context_destroyed(
    &self,
    scope: &mut HandleScope,
    context: v8::Global<v8::Context>,
  ) {
    let context = v8::Local::new(scope, context);
    self.v8_inspector.context_destroyed(context);
  }

  pub fn exception_thrown(
    &self,
    scope: &mut HandleScope,
    exception: v8::Local<'_, v8::Value>,
    in_promise: bool,
  ) {
    let context = scope.get_current_context();
    let message = v8::Exception::create_message(scope, exception);
    let stack_trace = message.get_stack_trace(scope);
    let stack_trace = self.v8_inspector.create_stack_trace(stack_trace);
    self.v8_inspector.exception_thrown(
      context,
      if in_promise {
        v8::inspector::StringView::from("Uncaught (in promise)".as_bytes())
      } else {
        v8::inspector::StringView::from("Uncaught".as_bytes())
      },
      exception,
      v8::inspector::StringView::from("".as_bytes()),
      v8::inspector::StringView::from("".as_bytes()),
      0,
      0,
      stack_trace,
      0,
    );
  }

  pub(crate) fn sessions_state(&self) -> SessionsState {
    self.state.sessions.borrow().sessions_state()
  }

  pub(crate) fn poll_sessions_from_event_loop(&self, cx: &mut Context) {
    let _ = self.state.poll_sessions(Some(cx)).unwrap();
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  pub fn wait_for_session(&self) {
    loop {
      if let Some(_session) =
        self.state.sessions.borrow_mut().local.values().next()
      {
        self.state.flags.borrow_mut().waiting_for_session = false;
        break;
      } else {
        self.state.flags.borrow_mut().waiting_for_session = true;
        let _ = self.state.poll_sessions(None).unwrap();
      }
    }
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  ///
  /// After that, it instructs V8 to pause at the next statement.
  /// Frontend must send "Runtime.runIfWaitingForDebugger" message to resume
  /// execution.
  pub fn wait_for_session_and_break_on_next_statement(&self) {
    loop {
      if let Some(session) =
        self.state.sessions.borrow_mut().local.values().next()
      {
        break session.break_on_next_statement();
      } else {
        self.state.flags.borrow_mut().waiting_for_session = true;
        let _ = self.state.poll_sessions(None).unwrap();
      }
    }
  }

  pub fn create_io_delegate(&self) -> Arc<InspectorIoDelegate> {
    let (tx, rx) = oneshot::channel::<()>();
    let prev = self.deregister_tx.borrow_mut().replace(tx);
    assert!(
      prev.is_none(),
      "Only a single IO delegate allowed per inspector"
    );

    Arc::new(InspectorIoDelegate {
      new_session_tx: self.new_session_tx.clone(),
      deregister_rx: Mutex::new(rx),
    })
  }

  pub fn create_local_session(
    inspector: Rc<JsRuntimeInspector>,
    callback: InspectorSessionSend,
    kind: InspectorSessionKind,
  ) -> LocalInspectorSession {
    let sessions = inspector.state.sessions.clone();

    let session = {
      let mut s = sessions.borrow_mut();
      s.create_new_session(callback, kind)
    };

    take(&mut inspector.state.flags.borrow_mut().waiting_for_session);

    LocalInspectorSession::new(session.id, sessions)
  }
}

#[derive(Default)]
struct InspectorFlags {
  waiting_for_session: bool,
  on_pause: bool,
}

#[derive(Debug)]
pub struct SessionsState {
  pub has_active: bool,
  pub has_blocking: bool,
  pub has_nonblocking: bool,
  pub has_nonblocking_wait_for_disconnect: bool,
}

/// A helper structure that helps coordinate sessions during different
/// parts of their lifecycle.
pub struct SessionContainer {
  v8_inspector: Option<Rc<v8::inspector::V8Inspector>>,
  session_rx: UnboundedReceiver<NewSessionTxMsg>,
  is_dispatching_message: Rc<RefCell<bool>>,
  established: FuturesUnordered<InspectorSessionPumpMessages>,
  next_local_id: i32,
  local: HashMap<i32, Rc<InspectorSession>>,
}

impl SessionContainer {
  fn new(
    v8_inspector: Rc<v8::inspector::V8Inspector>,
    new_session_rx: UnboundedReceiver<NewSessionTxMsg>,
    is_dispatching_message: Rc<RefCell<bool>>,
  ) -> Self {
    Self {
      v8_inspector: Some(v8_inspector),
      session_rx: new_session_rx,
      established: FuturesUnordered::new(),
      next_local_id: 1,
      local: HashMap::new(),
      is_dispatching_message,
    }
  }

  /// V8 automatically deletes all sessions when an `V8Inspector` instance is
  /// deleted, however InspectorSession also has a drop handler that cleans
  /// up after itself. To avoid a double free, we need to manually drop
  /// all sessions before dropping the inspector instance.
  fn drop_sessions(&mut self) {
    self.v8_inspector = Default::default();
    self.established.clear();
    self.local.clear();
  }

  fn sessions_state(&self) -> SessionsState {
    SessionsState {
      has_active: !self.established.is_empty() || !self.local.is_empty(),
      has_blocking: self
        .local
        .values()
        .any(|s| matches!(s.state.kind, InspectorSessionKind::Blocking)),
      has_nonblocking: self.local.values().any(|s| {
        matches!(s.state.kind, InspectorSessionKind::NonBlocking { .. })
      }),
      has_nonblocking_wait_for_disconnect: self.local.values().any(|s| {
        matches!(
          s.state.kind,
          InspectorSessionKind::NonBlocking {
            wait_for_disconnect: true
          }
        )
      }),
    }
  }

  /// A temporary placeholder that should be used before actual
  /// instance of V8Inspector is created. It's used in favor
  /// of `Default` implementation to signal that it's not meant
  /// for actual use.
  fn temporary_placeholder() -> Self {
    let (_tx, rx) = mpsc::unbounded::<NewSessionTxMsg>();
    Self {
      v8_inspector: Default::default(),
      session_rx: rx,
      established: FuturesUnordered::new(),
      next_local_id: 1,
      local: HashMap::new(),
      is_dispatching_message: Default::default(),
    }
  }

  pub fn dispatch_message_from_frontend(
    &mut self,
    session_id: i32,
    message: String,
  ) {
    if let Some(session) = self.local.get(&session_id) {
      session.dispatch_message(message);
    }
  }

  fn create_new_session(
    &mut self,
    callback: InspectorSessionSend,
    kind: InspectorSessionKind,
  ) -> Rc<InspectorSession> {
    let id = self.next_local_id;
    self.next_local_id += 1;

    let session = InspectorSession::new(
      id,
      self.v8_inspector.as_ref().unwrap().clone(),
      self.is_dispatching_message.clone(),
      callback,
      kind,
    );

    assert!(self.local.insert(id, session.clone()).is_none());
    session
  }

  // TODO(bartlomieju): keep simplifying this function
  fn pump_messages_for_remote_sessions(&mut self, cx: &mut Context) {
    loop {
      // Accept new connections.
      let poll_result = self.session_rx.poll_next_unpin(cx);
      if let Poll::Ready(Some(new_session_msg)) = poll_result {
        let (callback, rx, kind) = new_session_msg;
        let session = self.create_new_session(callback, kind);

        let mut fut =
          pump_inspector_session_messages(session, rx).boxed_local();
        let _ = fut.poll_unpin(cx);
        self.established.push(fut);

        // TODO(bartlomieju): decide on this - should we drain the `session_rx` queue fully
        // before polling `established` sessions?
        continue;
      }

      // Poll established sessions.
      match self.established.poll_next_unpin(cx) {
        Poll::Ready(Some(())) => {
          continue;
        }
        Poll::Ready(None) => {
          break;
        }
        Poll::Pending => {
          break;
        }
      };
    }
  }
}

struct InspectorWakerInner {
  poll_state: PollState,
  task_waker: Option<task::Waker>,
  parked_thread: Option<thread::Thread>,
  state_ptr: Option<NonNull<JsRuntimeInspectorState>>,
  isolate_handle: v8::IsolateHandle,
}

// SAFETY: unsafe trait must have unsafe implementation
unsafe impl Send for InspectorWakerInner {}

struct InspectorWaker(Mutex<InspectorWakerInner>);

impl InspectorWaker {
  fn new(isolate_handle: v8::IsolateHandle) -> Arc<Self> {
    let inner = InspectorWakerInner {
      poll_state: PollState::Idle,
      task_waker: None,
      parked_thread: None,
      state_ptr: None,
      isolate_handle,
    };
    Arc::new(Self(Mutex::new(inner)))
  }

  fn update<F, R>(&self, update_fn: F) -> R
  where
    F: FnOnce(&mut InspectorWakerInner) -> R,
  {
    let mut g = self.0.lock();
    update_fn(&mut g)
  }
}

extern "C" fn handle_interrupt(_isolate: &mut v8::Isolate, arg: *mut c_void) {
  // SAFETY: `InspectorWaker` is owned by `JsRuntimeInspector`, so the
  // pointer to the latter is valid as long as waker is alive.
  let inspector_state = unsafe { &*(arg as *mut JsRuntimeInspectorState) };
  let _ = inspector_state.poll_sessions(None);
}

impl task::ArcWake for InspectorWaker {
  fn wake_by_ref(arc_self: &Arc<Self>) {
    arc_self.update(|w| {
      match w.poll_state {
        PollState::Idle => {
          // Wake the task, if any, that has polled the Inspector future last.
          if let Some(waker) = w.task_waker.take() {
            waker.wake()
          }
          // Request an interrupt from the isolate if it's running and there's
          // not unhandled interrupt request in flight.
          if let Some(arg) =
            w.state_ptr.take().map(|ptr| ptr.as_ptr() as *mut c_void)
          {
            w.isolate_handle.request_interrupt(handle_interrupt, arg);
          }
        }
        PollState::Parked => {
          // Unpark the isolate thread.
          let parked_thread = w.parked_thread.take().unwrap();
          assert_ne!(parked_thread.id(), thread::current().id());
          parked_thread.unpark();
        }
        _ => {}
      };
      w.poll_state = PollState::Woken;
    });
  }
}

#[derive(Clone, Copy, Debug)]
pub enum InspectorSessionKind {
  Blocking,
  NonBlocking { wait_for_disconnect: bool },
}

#[derive(Clone)]
struct InspectorSessionState {
  is_dispatching_message: Rc<RefCell<bool>>,
  send: Rc<InspectorSessionSend>,
  // Describes if session should keep event loop alive, eg. a local REPL
  // session should keep event loop alive, but a Websocket session shouldn't.
  kind: InspectorSessionKind,
}

/// An inspector session that proxies messages to concrete "transport layer",
/// eg. Websocket or another set of channels.
struct InspectorSession {
  pub id: i32,
  v8_session: v8::inspector::V8InspectorSession,
  state: InspectorSessionState,
}

impl InspectorSession {
  const CONTEXT_GROUP_ID: i32 = 1;

  pub fn new(
    id: i32,
    v8_inspector: Rc<v8::inspector::V8Inspector>,
    is_dispatching_message: Rc<RefCell<bool>>,
    send: InspectorSessionSend,
    kind: InspectorSessionKind,
  ) -> Rc<Self> {
    let state = InspectorSessionState {
      is_dispatching_message,
      send: Rc::new(send),
      kind,
    };

    let v8_session = v8_inspector.connect(
      Self::CONTEXT_GROUP_ID,
      v8::inspector::Channel::new(Box::new(state.clone())),
      v8::inspector::StringView::empty(),
      v8::inspector::V8InspectorClientTrustLevel::FullyTrusted,
    );

    Rc::new(Self {
      id,
      v8_session,
      state,
    })
  }

  // Dispatch message to V8 session
  fn dispatch_message(&self, msg: String) {
    *self.state.is_dispatching_message.borrow_mut() = true;
    let msg = v8::inspector::StringView::from(msg.as_bytes());
    self.v8_session.dispatch_protocol_message(msg);
    *self.state.is_dispatching_message.borrow_mut() = false;
  }

  pub fn break_on_next_statement(&self) {
    let reason = v8::inspector::StringView::from(&b"debugCommand"[..]);
    let detail = v8::inspector::StringView::empty();
    self
      .v8_session
      .schedule_pause_on_next_statement(reason, detail);
  }
}

impl InspectorSessionState {
  fn send_message(
    &self,
    msg_kind: InspectorMsgKind,
    msg: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    let msg = msg.unwrap().string().to_string();
    (self.send)(InspectorMsg {
      kind: msg_kind,
      content: msg,
    });
  }
}

impl v8::inspector::ChannelImpl for InspectorSessionState {
  fn send_response(
    &self,
    call_id: i32,
    message: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    self.send_message(InspectorMsgKind::Message(call_id), message);
  }

  fn send_notification(
    &self,
    message: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    self.send_message(InspectorMsgKind::Notification, message);
  }

  fn flush_protocol_notifications(&self) {}
}

type InspectorSessionPumpMessages = Pin<Box<dyn Future<Output = ()>>>;

async fn pump_inspector_session_messages(
  session: Rc<InspectorSession>,
  mut rx: UnboundedReceiver<String>,
) {
  while let Some(msg) = rx.next().await {
    session.dispatch_message(msg);
  }
}

#[derive(Debug, Boxed)]
pub struct InspectorPostMessageError(pub Box<InspectorPostMessageErrorKind>);

#[derive(Debug, Error)]
pub enum InspectorPostMessageErrorKind {
  #[error(transparent)]
  JsBox(#[from] JsErrorBox),
  #[error(transparent)]
  FutureCanceled(futures::channel::oneshot::Canceled),
}

impl From<InspectorPostMessageError> for CoreError {
  fn from(value: InspectorPostMessageError) -> Self {
    CoreErrorKind::JsBox(value.into_js_error_box()).into_box()
  }
}

impl InspectorPostMessageError {
  pub fn into_js_error_box(self) -> JsErrorBox {
    match self.into_kind() {
      InspectorPostMessageErrorKind::JsBox(e) => e,
      InspectorPostMessageErrorKind::FutureCanceled(e) => {
        JsErrorBox::generic(e.to_string())
      }
    }
  }
}

/// A local inspector session that can be used to send and receive protocol messages directly on
/// the same thread as an isolate.
///
/// Does not provide any abstraction over CDP messages.
pub struct LocalInspectorSession {
  sessions: Rc<RefCell<SessionContainer>>,
  session_id: i32,
}

impl LocalInspectorSession {
  pub fn new(session_id: i32, sessions: Rc<RefCell<SessionContainer>>) -> Self {
    Self {
      sessions,
      session_id,
    }
  }

  pub fn dispatch(&mut self, msg: String) {
    self
      .sessions
      .borrow_mut()
      .dispatch_message_from_frontend(self.session_id, msg);
  }

  pub fn post_message<T: serde::Serialize>(
    &mut self,
    id: i32,
    method: &str,
    params: Option<T>,
  ) {
    let message = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    let stringified_msg = serde_json::to_string(&message).unwrap();
    self.dispatch(stringified_msg);
  }
}
