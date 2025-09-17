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
pub type SessionProxySender = UnboundedSender<InspectorMsg>;
// TODO(bartlomieju): remove this
pub type SessionProxyReceiver = UnboundedReceiver<String>;

/// Encapsulates an UnboundedSender/UnboundedReceiver pair that together form
/// a duplex channel for sending/receiving messages in V8 session.
pub struct InspectorSessionProxy {
  pub tx: SessionProxySender,
  pub rx: SessionProxyReceiver,
  pub options: InspectorSessionOptions,
}

pub type InspectorSessionSend = Box<dyn Fn(InspectorMsg)>;

#[derive(Clone, Copy, Debug)]
enum PollState {
  Idle,
  Woken,
  Polling,
  Parked,
  Dropped,
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
  new_session_tx: UnboundedSender<InspectorSessionProxy>,
  deregister_tx: RefCell<Option<oneshot::Sender<()>>>,
  state: JsRuntimeInspectorState,
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

impl v8::inspector::V8InspectorClientImpl for JsRuntimeInspectorState {
  fn run_message_loop_on_pause(&self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.flags.borrow_mut().on_pause = true;
    let _ = self.poll_sessions(None);
  }

  fn quit_message_loop_on_pause(&self) {
    self.flags.borrow_mut().on_pause = false;
  }

  fn run_if_waiting_for_debugger(&self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.flags.borrow_mut().waiting_for_session = false;
  }

  fn ensure_default_context_in_group(
    &self,
    context_group_id: i32,
  ) -> Option<v8::Local<'_, v8::Context>> {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    let context = self.context.clone();
    let isolate: &mut v8::Isolate = unsafe { &mut *(self.isolate_ptr) };
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

    self.waker.update(|w| {
      match w.poll_state {
        PollState::Idle | PollState::Woken => w.poll_state = PollState::Polling,
        _ => unreachable!(),
      };
    });

    // Create a new Context object that will make downstream futures
    // use the InspectorWaker when they are ready to be polled again.
    let waker_ref = task::waker_ref(&self.waker);
    let cx = &mut Context::from_waker(&waker_ref);

    loop {
      loop {
        // Do one "handshake" with a newly connected session at a time.
        if let Some(session) = sessions.handshake.take() {
          let mut fut =
            pump_inspector_session_messages(session.clone()).boxed_local();
          let _ = fut.poll_unpin(cx);
          sessions.established.push(fut);
          let id = sessions.next_local_id;
          sessions.next_local_id += 1;
          sessions.local.insert(id, session);
          continue;
        }

        // Accept new connections.
        let poll_result = sessions.session_rx.poll_next_unpin(cx);
        if let Poll::Ready(Some(session_proxy)) = poll_result {
          let session = InspectorSession::new(
            sessions.v8_inspector.as_ref().unwrap().clone(),
            self.is_dispatching_message.clone(),
            Box::new(move |msg| {
              let _ = session_proxy.tx.unbounded_send(msg);
            }),
            Some(session_proxy.rx),
            session_proxy.options,
          );
          let prev = sessions.handshake.replace(session);
          assert!(prev.is_none());
        }

        // Poll established sessions.
        match sessions.established.poll_next_unpin(cx) {
          Poll::Ready(Some(())) => {
            continue;
          }
          Poll::Ready(None) => break,
          Poll::Pending => break,
        };
      }

      let should_block = {
        let flags = self.flags.borrow();
        flags.on_pause || flags.waiting_for_session
      };

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
            w.inspector_state_ptr = NonNull::new(self as *const _ as *mut Self);
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
    let (new_session_tx, new_session_rx) =
      mpsc::unbounded::<InspectorSessionProxy>();

    let waker = InspectorWaker::new(scope.thread_safe_handle());

    let state = JsRuntimeInspectorState {
      waker,
      flags: Default::default(),
      isolate_ptr,
      context: v8::Global::new(scope, context),
      sessions: Rc::new(
        RefCell::new(SessionContainer::temporary_placeholder()),
      ),
      is_dispatching_message: Default::default(),
    };
    let v8_inspector_client =
      v8::inspector::V8InspectorClient::new(Box::new(state.clone()));
    let v8_inspector = Rc::new(v8::inspector::V8Inspector::create(
      scope,
      v8_inspector_client,
    ));

    *state.sessions.borrow_mut() =
      SessionContainer::new(v8_inspector.clone(), new_session_rx);

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
    v8_inspector.context_created(
      context,
      Self::CONTEXT_GROUP_ID,
      context_name,
      aux_data_view,
    );

    // Poll the session handler so we will get notified whenever there is
    // new incoming debugger activity.
    let _ = state.poll_sessions(None).unwrap();

    Rc::new(Self {
      v8_inspector,
      state,
      new_session_tx,
      deregister_tx: RefCell::new(None),
    })
  }

  pub fn is_dispatching_message(&self) -> bool {
    *self.state.is_dispatching_message.borrow()
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

  pub fn sessions_state(&self) -> SessionsState {
    self.state.sessions.borrow().sessions_state()
  }

  pub fn poll_sessions_from_event_loop(&self, cx: &mut Context) {
    let _ = self.state.poll_sessions(Some(cx)).unwrap();
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  pub fn wait_for_session(&mut self) {
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
  pub fn wait_for_session_and_break_on_next_statement(&mut self) {
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

  /// Obtain a sender for proxy channels.
  pub fn get_session_sender(&self) -> UnboundedSender<InspectorSessionProxy> {
    self.new_session_tx.clone()
  }

  /// Create a channel that notifies the frontend when inspector is dropped.
  ///
  /// NOTE: Only a single handler is currently available.
  pub fn add_deregister_handler(&self) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel::<()>();
    let prev = self.deregister_tx.borrow_mut().replace(tx);
    assert!(
      prev.is_none(),
      "Only a single deregister handler is allowed"
    );
    rx
  }

  pub fn create_local_session(
    inspector: Rc<JsRuntimeInspector>,
    callback: InspectorSessionSend,
    options: InspectorSessionOptions,
  ) -> LocalInspectorSession {
    let (session_id, sessions) = {
      let sessions = inspector.state.sessions.clone();

      let inspector_session = InspectorSession::new(
        inspector.v8_inspector.clone(),
        inspector.state.is_dispatching_message.clone(),
        callback,
        None,
        options,
      );

      let session_id = {
        let mut s = sessions.borrow_mut();
        let id = s.next_local_id;
        s.next_local_id += 1;
        assert!(s.local.insert(id, inspector_session).is_none());
        id
      };

      take(&mut inspector.state.flags.borrow_mut().waiting_for_session);
      (session_id, sessions)
    };

    LocalInspectorSession::new(session_id, sessions)
  }
}

#[derive(Debug)]
pub struct InspectorSessionOptions {
  pub kind: InspectorSessionKind,
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
  session_rx: UnboundedReceiver<InspectorSessionProxy>,
  handshake: Option<Rc<InspectorSession>>,
  established: FuturesUnordered<InspectorSessionPumpMessages>,
  next_local_id: i32,
  local: HashMap<i32, Rc<InspectorSession>>,
}

impl SessionContainer {
  fn new(
    v8_inspector: Rc<v8::inspector::V8Inspector>,
    new_session_rx: UnboundedReceiver<InspectorSessionProxy>,
  ) -> Self {
    Self {
      v8_inspector: Some(v8_inspector),
      session_rx: new_session_rx,
      handshake: None,
      established: FuturesUnordered::new(),
      next_local_id: 1,
      local: HashMap::new(),
    }
  }

  /// V8 automatically deletes all sessions when an `V8Inspector` instance is
  /// deleted, however InspectorSession also has a drop handler that cleans
  /// up after itself. To avoid a double free, we need to manually drop
  /// all sessions before dropping the inspector instance.
  fn drop_sessions(&mut self) {
    self.v8_inspector = Default::default();
    self.handshake.take();
    self.established.clear();
    self.local.clear();
  }

  fn sessions_state(&self) -> SessionsState {
    SessionsState {
      has_active: !self.established.is_empty()
        || self.handshake.is_some()
        || !self.local.is_empty(),
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
    let (_tx, rx) = mpsc::unbounded::<InspectorSessionProxy>();
    Self {
      v8_inspector: Default::default(),
      session_rx: rx,
      handshake: None,
      established: FuturesUnordered::new(),
      next_local_id: 1,
      local: HashMap::new(),
    }
  }

  pub fn dispatch_message_from_frontend(
    &mut self,
    session_id: i32,
    message: String,
  ) {
    let session = self.local.get(&session_id).unwrap();
    session.dispatch_message(message);
  }
}

struct InspectorWakerInner {
  poll_state: PollState,
  task_waker: Option<task::Waker>,
  parked_thread: Option<thread::Thread>,
  inspector_state_ptr: Option<NonNull<JsRuntimeInspectorState>>,
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
      inspector_state_ptr: None,
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
          if let Some(arg) = w
            .inspector_state_ptr
            .take()
            .map(|ptr| ptr.as_ptr() as *mut c_void)
          {
            w.isolate_handle.request_interrupt(handle_interrupt, arg);
          }
          extern "C" fn handle_interrupt(
            _isolate: &mut v8::Isolate,
            arg: *mut c_void,
          ) {
            // SAFETY: `InspectorWaker` is owned by `JsRuntimeInspector`, so the
            // pointer to the latter is valid as long as waker is alive.
            let inspector_state =
              unsafe { &*(arg as *mut JsRuntimeInspectorState) };
            let _ = inspector_state.poll_sessions(None);
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
  rx: Rc<RefCell<Option<SessionProxyReceiver>>>,
  // Describes if session should keep event loop alive, eg. a local REPL
  // session should keep event loop alive, but a Websocket session shouldn't.
  kind: InspectorSessionKind,
}

/// An inspector session that proxies messages to concrete "transport layer",
/// eg. Websocket or another set of channels.
struct InspectorSession {
  v8_session: v8::inspector::V8InspectorSession,
  state: InspectorSessionState,
}

impl InspectorSession {
  const CONTEXT_GROUP_ID: i32 = 1;

  pub fn new(
    v8_inspector: Rc<v8::inspector::V8Inspector>,
    is_dispatching_message: Rc<RefCell<bool>>,
    send: InspectorSessionSend,
    rx: Option<SessionProxyReceiver>,
    options: InspectorSessionOptions,
  ) -> Rc<Self> {
    let state = InspectorSessionState {
      is_dispatching_message,
      send: Rc::new(send),
      rx: Rc::new(RefCell::new(rx)),
      kind: options.kind,
    };

    let v8_session = v8_inspector.connect(
      Self::CONTEXT_GROUP_ID,
      v8::inspector::Channel::new(Box::new(state.clone())),
      v8::inspector::StringView::empty(),
      v8::inspector::V8InspectorClientTrustLevel::FullyTrusted,
    );

    Rc::new(Self { v8_session, state })
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

async fn pump_inspector_session_messages(session: Rc<InspectorSession>) {
  let mut rx = session.state.rx.borrow_mut().take().unwrap();
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
