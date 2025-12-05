// Copyright 2018-2025 the Deno authors. MIT license.

//! The documentation for the inspector API is sparse, but these are helpful:
//! <https://chromedevtools.github.io/devtools-protocol/>
//! <https://hyperandroid.com/2020/02/12/v8-inspector-from-an-embedder-standpoint/>

use crate::futures::channel::mpsc;
use crate::futures::channel::mpsc::UnboundedReceiver;
use crate::futures::channel::mpsc::UnboundedSender;
use crate::futures::channel::oneshot;
use crate::futures::prelude::*;
use crate::futures::stream::FuturesUnordered;
use crate::futures::stream::StreamExt;
use crate::futures::task;
use crate::serde_json::json;

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
  pub kind: InspectorSessionKind,
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
  isolate_ptr: v8::UnsafeRawIsolatePtr,
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
    let mut isolate =
      unsafe { v8::Isolate::from_raw_isolate_ptr(self.0.isolate_ptr) };
    let isolate = &mut isolate;
    v8::callback_scope!(unsafe scope, isolate);
    let local = v8::Local::new(scope, context);
    Some(unsafe { local.extend_lifetime_unchecked() })
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
          sessions.local.insert(id, session.clone());

          // Track the first session as the main session for Target events
          if sessions.main_session_id.is_none() {
            eprintln!("[WORKER DEBUG] Setting main session ID: {}", id);
            sessions.main_session_id = Some(id);
          }

          continue;
        }

        // Accept new connections.
        let poll_result = sessions.session_rx.poll_next_unpin(cx);
        if let Poll::Ready(Some(mut session_proxy)) = poll_result {
          // Normal session (not a worker)
          let session = InspectorSession::new(
            sessions.v8_inspector.as_ref().unwrap().clone(),
            self.is_dispatching_message.clone(),
            Box::new(move |msg| {
              let _ = session_proxy.tx.unbounded_send(msg);
            }),
            Some(session_proxy.rx),
            session_proxy.kind,
            self.sessions.clone(),
          );

          let prev = sessions.handshake.replace(session);
          assert!(prev.is_none());
          continue;
        }

        // Poll worker message channels - forward messages from workers to main session
        if let Some(main_id) = sessions.main_session_id {
          // Get main session send function before mutably iterating over target_sessions
          let main_session_send =
            sessions.local.get(&main_id).map(|s| s.state.send.clone());

          if let Some(send) = main_session_send {
            let mut has_worker_message = false;
            let mut execution_context_to_forward: Option<String> = None;

            // for target_session in sessions.target_sessions.values() {}

            // Send execution context after loop to avoid borrow issues
            if let Some(context_msg) = execution_context_to_forward {
              send(InspectorMsg {
                kind: InspectorMsgKind::Notification,
                content: context_msg,
              });
              // Increment counter for next worker
              sessions.next_worker_context_id += 1;
            }

            if has_worker_message {
              continue;
            }
          }
        }

        // Poll established sessions.
        match sessions.established.poll_next_unpin(cx) {
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
    isolate_ptr: v8::UnsafeRawIsolatePtr,
    scope: &mut v8::PinScope,
    context: v8::Local<v8::Context>,
    is_main_runtime: bool,
  ) -> Rc<Self> {
    let (new_session_tx, new_session_rx) =
      mpsc::unbounded::<InspectorSessionProxy>();

    let waker = InspectorWaker::new(scope.thread_safe_handle());
    let random_str = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_nanos()
      .to_string();

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

    *state.sessions.borrow_mut() =
      SessionContainer::new(v8_inspector.clone(), new_session_rx);

    // Tell the inspector about the main realm.
    // let context_name = v8::inspector::StringView::from(&b"main realm"[..]);
    let str = format!("deno-{}", random_str);
    let b = str.as_bytes();

    let context_name = v8::inspector::StringView::from(b);
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
    scope: &mut v8::PinScope<'_, '_>,
    context: v8::Global<v8::Context>,
  ) {
    let context = v8::Local::new(scope, context);
    self.v8_inspector.context_destroyed(context);
  }

  pub fn exception_thrown(
    &self,
    scope: &mut v8::PinScope<'_, '_>,
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
    kind: InspectorSessionKind,
  ) -> LocalInspectorSession {
    let (session_id, sessions) = {
      let sessions = inspector.state.sessions.clone();

      let inspector_session = InspectorSession::new(
        inspector.v8_inspector.clone(),
        inspector.state.is_dispatching_message.clone(),
        callback,
        None,
        kind,
        sessions.clone(),
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

  target_sessions: HashMap<String, Rc<TargetSession>>, // sessionId -> TargetSession
  auto_attach_enabled: bool,
  main_session_id: Option<i32>, // The first session that should receive Target events

  // Store breakpoint commands set on main thread to replay to new workers (for VSCode)
  stored_breakpoints: Vec<String>, // Vector of Debugger.setBreakpointByUrl commands

  // Track next execution context ID for workers (start from high number to avoid conflicts)
  next_worker_context_id: i32,
}

/// Represents a CDP Target session (e.g., a worker)
struct TargetSession {
  target_id: String,
  session_id: String,
  local_session_id: i32,
  send: Rc<InspectorSessionSend>,
  // Channels for communicating with the worker's V8 inspector
  // Worker URL for DevTools display
  url: String,
  // Track if we've already sent Target.attachedToTarget for this worker
  attached: RefCell<bool>,
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

      target_sessions: HashMap::new(),
      auto_attach_enabled: false,
      main_session_id: None,
      stored_breakpoints: Vec::new(),
      next_worker_context_id: 1000, // Start from 1000 to avoid conflicts with main context
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

      target_sessions: HashMap::new(),
      auto_attach_enabled: false,
      main_session_id: None,
      stored_breakpoints: Vec::new(),
      next_worker_context_id: 1000,
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
  sessions: Rc<RefCell<SessionContainer>>,
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
    kind: InspectorSessionKind,
    sessions: Rc<RefCell<SessionContainer>>,
  ) -> Rc<Self> {
    let state = InspectorSessionState {
      is_dispatching_message,
      send: Rc::new(send),
      rx: Rc::new(RefCell::new(rx)),
      kind,
      sessions,
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
    // Log ALL incoming messages for debugging
    eprintln!("[INSPECTOR DEBUG] Incoming: {}", &msg[..msg.len().min(200)]);

    // Parse the incoming message
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
      // FIRST: Check if this message has a top-level sessionId field
      // This is how DevTools routes messages to worker sessions after Target.attachedToTarget
      if let Some(target_session_id_str) =
        parsed.get("sessionId").and_then(|s| s.as_str())
      {
        if !target_session_id_str.is_empty() {
          // This message is intended for a worker session, not the main session
          let target_session_id = target_session_id_str.to_owned();
          eprintln!(
            "[WORKER DEBUG] Routing message to session: {}",
            target_session_id
          );

          // Remove the sessionId field before sending to worker's V8 inspector
          // V8 inspector doesn't understand this CDP Target domain concept
          let mut cleaned = parsed.clone();
          if let Some(obj) = cleaned.as_object_mut() {
            obj.remove("sessionId");
          }
          let cleaned_msg = cleaned.to_string();

          let sessions = session.state.sessions.clone();
          deno_core::unsync::spawn(async move {
            let sessions = sessions.borrow();
            // if let Some(target_session) =
            //   sessions.target_sessions.get(&target_session_id)
            // {
            //   // if let Some(worker_tx) =
            //   //   target_session.worker_tx.borrow().as_ref()
            //   // {
            //   //   eprintln!(
            //   //     "[WORKER DEBUG] Sending to worker: {}",
            //   //     &cleaned_msg[..cleaned_msg.len().min(100)]
            //   //   );
            //   //   if let Err(e) = worker_tx.unbounded_send(cleaned_msg) {
            //   //     eprintln!("[WORKER DEBUG] Failed to send to worker: {}", e);
            //   //   }
            //   // } else {
            //   //   eprintln!(
            //   //     "[WORKER DEBUG] No worker_tx for session: {}",
            //   //     target_session_id
            //   //   );
            //   // }
            // } else {
            //   eprintln!(
            //     "[WORKER DEBUG] Session not found: {}",
            //     target_session_id
            //   );
            // }
          });
          continue; // Don't process this message locally
        }
      }

      // SECOND: Check if this is a NodeWorker or Target domain message (for the main session)
      if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
        if method.starts_with("NodeWorker.") {
          eprintln!(
            "[WORKER DEBUG] Intercepted NodeWorker message: {}",
            method
          );
          let id = parsed.get("id").and_then(|i| i.as_i64()).map(|i| i as i32);
          let params = parsed.get("params").cloned();

          let result: serde_json::Value = match method {
            "NodeWorker.enable" => {
              // VSCode uses NodeWorker.enable to request worker debugging
              let wait_for_debugger = params
                .as_ref()
                .and_then(|p| p.get("waitForDebuggerOnStart"))
                .and_then(|w| w.as_bool())
                .unwrap_or(false);

              eprintln!(
                "[WORKER DEBUG] NodeWorker.enable called with waitForDebuggerOnStart={}",
                wait_for_debugger
              );

              // Send any existing workers as NodeWorker.attachedToWorker events
              let sessions = session.state.sessions.clone();
              let send = session.state.send.clone();
              deno_core::unsync::spawn(async move {
                let sessions = sessions.borrow();
                for target_session in sessions.target_sessions.values() {
                  eprintln!(
                    "[WORKER DEBUG] Sending NodeWorker.attachedToWorker for worker: {}",
                    target_session.url
                  );

                  let attached_event = json!({
                    "method": "NodeWorker.attachedToWorker",
                    "params": {
                      "sessionId": target_session.session_id,
                      "workerInfo": {
                        "workerId": target_session.target_id,
                        "type": "worker",
                        "title": format!("[worker {}] WorkerThread", target_session.local_session_id),
                        "url": target_session.url
                      },
                      "waitingForDebugger": false
                    }
                  });

                  send(InspectorMsg {
                    kind: InspectorMsgKind::Notification,
                    content: attached_event.to_string(),
                  });
                }
              });

              json!({})
            }
            "NodeWorker.sendMessageToWorker" => {
              // VSCode uses this to send messages to workers
              let message = params
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();

              let session_id = params
                .as_ref()
                .and_then(|p| p.get("sessionId"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();

              eprintln!(
                "[WORKER DEBUG] NodeWorker.sendMessageToWorker: sessionId={}",
                session_id
              );

              // Check if this is a Debugger.enable message or step/resume command
              let (is_debugger_enable, is_step_resume) = if let Ok(msg_parsed) =
                serde_json::from_str::<serde_json::Value>(&message)
              {
                let method = msg_parsed.get("method").and_then(|m| m.as_str());
                let is_enable = method == Some("Debugger.enable");
                let is_step = matches!(
                  method,
                  Some("Debugger.stepOver")
                    | Some("Debugger.resume")
                    | Some("Debugger.stepInto")
                    | Some("Debugger.stepOut")
                );
                (is_enable, is_step)
              } else {
                (false, false)
              };

              if is_step_resume {
                eprintln!(
                  "[WORKER DEBUG] >>> Sending step/resume command to worker: {}",
                  message
                );
              }

              let sessions = session.state.sessions.clone();
              deno_core::unsync::spawn(async move {
                let sessions = sessions.borrow();
                if let Some(target_session) =
                  sessions.target_sessions.get(&session_id)
                {
                  // if let Some(worker_tx) =
                  //   target_session.worker_tx.borrow().as_ref()
                  // {
                  //   eprintln!(
                  //     "[WORKER DEBUG] Sending to worker via NodeWorker: {}",
                  //     &message[..message.len().min(100)]
                  //   );
                  //   let _ = worker_tx.unbounded_send(message);
                  //
                  //   // If this is Debugger.enable, replay all stored breakpoints to this worker
                  //   if is_debugger_enable {
                  //     eprintln!(
                  //       "[BREAKPOINT] Debugger.enable received for worker, replaying {} stored breakpoints",
                  //       sessions.stored_breakpoints.len()
                  //     );
                  //     for breakpoint_cmd in &sessions.stored_breakpoints {
                  //       eprintln!(
                  //         "[BREAKPOINT] Replaying breakpoint to worker: {}",
                  //         &breakpoint_cmd[..breakpoint_cmd.len().min(100)]
                  //       );
                  //       let _ =
                  //         worker_tx.unbounded_send(breakpoint_cmd.clone());
                  //     }
                  //   }
                  // }
                }
              });

              json!({})
            }
            _ => {
              eprintln!(
                "[WORKER DEBUG] Unhandled NodeWorker method: {}",
                method
              );
              json!({})
            }
          };

          // Send response if this was a request (has id)
          if let Some(id) = id {
            let response = json!({
              "id": id,
              "result": result
            });

            (session.state.send)(InspectorMsg {
              kind: InspectorMsgKind::Message(id as i32),
              content: response.to_string(),
            });
          }

          continue; // Don't process this message locally
        }

        if method.starts_with("Target.") {
          eprintln!("[WORKER DEBUG] Intercepted Target message: {}", method);
          let id = parsed.get("id").and_then(|i| i.as_i64()).map(|i| i as i32);
          let params = parsed.get("params").cloned();
          let session_id = params
            .as_ref()
            .and_then(|p| p.get("sessionId"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_owned())
            .unwrap_or_default();

          let result: serde_json::Value = match method {
            "Target.setDiscoverTargets" => {
              let discover = params
                .as_ref()
                .and_then(|p| p.get("discover"))
                .and_then(|d| d.as_bool())
                .unwrap_or(false);

              if discover {
                // When DevTools calls setDiscoverTargets with discover=true,
                // send attachedToTarget for all existing workers
                let sessions = session.state.sessions.clone();
                let send = session.state.send.clone();
                deno_core::unsync::spawn(async move {
                  let sessions = sessions.borrow();
                  for target_session in sessions.target_sessions.values() {
                    if !*target_session.attached.borrow() {
                      *target_session.attached.borrow_mut() = true;

                      let attached_to_target = json!({
                        "method": "Target.attachedToTarget",
                        "params": {
                          "sessionId": target_session.session_id,
                          "targetInfo": {
                            "targetId": target_session.target_id,
                            "type": "worker",
                            "title": format!("[worker {}] WorkerThread", target_session.local_session_id),
                            "url": target_session.url,
                            "attached": false,
                            "canAccessOpener": true
                          },
                          "waitingForDebugger": true
                        }
                      });

                      send(InspectorMsg {
                        kind: InspectorMsgKind::Notification,
                        content: attached_to_target.to_string(),
                      });
                    }
                  }
                });
              }
              json!({})
            }
            "Target.setAutoAttach" => {
              let auto_attach = params
                .as_ref()
                .and_then(|p| p.get("autoAttach"))
                .and_then(|a| a.as_bool())
                .unwrap_or(false);

              let sessions = session.state.sessions.clone();
              let send = session.state.send.clone();
              deno_core::unsync::spawn(async move {
                let mut sessions = sessions.borrow_mut();
                sessions.auto_attach_enabled = auto_attach;

                if auto_attach {
                  for target_session in sessions.target_sessions.values() {
                    // Only attach if not already attached (like Node.js does)
                    if !*target_session.attached.borrow() {
                      *target_session.attached.borrow_mut() = true;

                      // Send Target.attachedToTarget (targetCreated was already sent when worker registered)
                      let attached_to_target = json!({
                        "method": "Target.attachedToTarget",
                        "params": {
                          "sessionId": target_session.session_id,
                          "targetInfo": {
                            "targetId": target_session.target_id,
                            "type": "worker",
                            "title": format!("[worker {}] WorkerThread", target_session.local_session_id),
                            "url": target_session.url,
                            "attached": false,
                            "canAccessOpener": true
                          },
                          "waitingForDebugger": true
                        }
                      });

                      eprintln!(
                        "[WORKER DEBUG] Attaching to worker: {}",
                        target_session.url
                      );

                      send(InspectorMsg {
                        kind: InspectorMsgKind::Notification,
                        content: attached_to_target.to_string(),
                      });
                    }
                  }
                }
              });
              json!({})
            }
            "Target.attachToTarget" => {
              // VSCode and other debuggers use this to explicitly attach to a target
              let target_id = params
                .as_ref()
                .and_then(|p| p.get("targetId"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();

              let flatten = params
                .as_ref()
                .and_then(|p| p.get("flatten"))
                .and_then(|f| f.as_bool())
                .unwrap_or(false);

              eprintln!(
                "[WORKER DEBUG] attachToTarget: targetId={}, flatten={}",
                target_id, flatten
              );

              let sessions = session.state.sessions.clone();
              let send = session.state.send.clone();
              let target_id_clone = target_id.clone();

              // Find the target and attach
              deno_core::unsync::spawn(async move {
                let sessions = sessions.borrow();

                // Look for target by target_id (which is the worker-N id)
                for target_session in sessions.target_sessions.values() {
                  if target_session.target_id == target_id_clone {
                    eprintln!(
                      "[WORKER DEBUG] Found target session: {}",
                      target_session.session_id
                    );

                    // Mark as attached
                    *target_session.attached.borrow_mut() = true;

                    // Send attachedToTarget event
                    let attached_event = json!({
                      "method": "Target.attachedToTarget",
                      "params": {
                        "sessionId": target_session.session_id,
                        "targetInfo": {
                          "targetId": target_session.target_id,
                          "type": "worker",
                          "title": format!("[worker {}] WorkerThread", target_session.local_session_id),
                          "url": target_session.url,
                          "attached": true,
                          "canAccessOpener": true
                        },
                        "waitingForDebugger": false
                      }
                    });

                    send(InspectorMsg {
                      kind: InspectorMsgKind::Notification,
                      content: attached_event.to_string(),
                    });

                    break;
                  }
                }
              });

              // Return the sessionId for this target
              let sessions = session.state.sessions.borrow();
              let mut result = json!({});

              for target_session in sessions.target_sessions.values() {
                if target_session.target_id == target_id {
                  result = json!({
                    "sessionId": target_session.session_id
                  });
                  break;
                }
              }

              result
            }
            "Target.sendMessageToTarget" => {
              // Extract the inner message and route it to the worker
              let inner_message = params
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();

              let sessions = session.state.sessions.clone();
              eprintln!(
                "[WORKER DEBUG] sendMessageToTarget: looking for session_id={}",
                session_id
              );
              // deno_core::unsync::spawn(async move {
              //   let sessions = sessions.borrow();
              //   eprintln!(
              //     "[WORKER DEBUG] Available sessions: {:?}",
              //     sessions.target_sessions.keys().collect::<Vec<_>>()
              //   );
              //   if let Some(target_session) =
              //     sessions.target_sessions.get(&session_id)
              //   {
              //   } else {
              //     eprintln!("[WORKER DEBUG] Session not found: {}", session_id);
              //   }
              // });

              json!({})
            }
            _ => {
              eprintln!("[WORKER DEBUG] Unhandled Target method: {}", method);
              json!({})
            }
          };

          // Send response if this was a request (has id)
          if let Some(id) = id {
            let response = json!({
              "id": id,
              "result": result
            });

            let response_msg = InspectorMsg {
              kind: InspectorMsgKind::Message(id),
              content: response.to_string(),
            };

            (session.state.send)(response_msg);
          }

          continue; // Don't dispatch to V8
        }

        // THIRD: Intercept Debugger.setBreakpointByUrl on main thread to store for replaying to workers
        // This is needed for VSCode which sets breakpoints on main thread before workers exist
        if method == "Debugger.setBreakpointByUrl" {
          eprintln!(
            "[BREAKPOINT] Intercepting Debugger.setBreakpointByUrl on main thread"
          );
          // Store this breakpoint command for replaying to new workers
          let sessions = session.state.sessions.clone();
          let msg_clone = msg.clone();
          deno_core::unsync::spawn(async move {
            let mut sessions = sessions.borrow_mut();
            sessions.stored_breakpoints.push(msg_clone);
            eprintln!(
              "[BREAKPOINT] Stored breakpoint, total: {}",
              sessions.stored_breakpoints.len()
            );
          });
          // Fall through to dispatch to V8 main session as well
        }
      }
    }

    // Normal message - dispatch to V8
    session.dispatch_message(msg);
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

  //TODO: use this instead
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
