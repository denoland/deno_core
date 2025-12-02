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
  // Optional worker debugging channels
  // When set, this proxy represents a worker and these channels should be registered
  pub worker_tx: Option<UnboundedSender<String>>,
  pub worker_rx: Option<UnboundedReceiver<InspectorMsg>>,
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
    // self.state.sessions.borrow_mut().drop_sessions();
    self.state.sessions.lock().drop_sessions();

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
  // sessions: Rc<RefCell<SessionContainer>>,
  sessions: Rc<Mutex<SessionContainer>>,
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
    // let Ok(mut sessions) = self.sessions.try_borrow_mut() else {
    let Some(mut sessions) = self.sessions.try_lock() else {
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

            // Process any buffered worker proxies
            let pending = std::mem::take(&mut sessions.pending_worker_proxies);
            if !pending.is_empty() {
              eprintln!(
                "[WORKER DEBUG] Processing {} buffered workers",
                pending.len()
              );
              let main_send = session.state.send.clone();
              for (worker_id, worker_send, worker_tx, worker_rx) in pending {
                eprintln!(
                  "[WORKER DEBUG] Registering buffered worker {}",
                  worker_id
                );
                sessions.register_worker_session(
                  worker_id,
                  worker_send,
                  &*main_send,
                );
                sessions
                  .register_worker_channels(worker_id, worker_tx, worker_rx);
              }
            }
          }

          continue;
        }

        // Accept new connections.
        let poll_result = sessions.session_rx.poll_next_unpin(cx);
        if let Poll::Ready(Some(mut session_proxy)) = poll_result {
          // Check if this is a worker proxy (has worker channels)
          let is_worker_proxy = session_proxy.worker_tx.is_some()
            && session_proxy.worker_rx.is_some();

          if is_worker_proxy {
            // This is a worker proxy - don't create a normal session
            // Just extract the channels and register the worker directly
            let worker_tx = session_proxy.worker_tx.take().unwrap();
            let worker_rx = session_proxy.worker_rx.take().unwrap();

            eprintln!(
              "[WORKER DEBUG] Received worker proxy, registering worker"
            );

            // Create a dummy send function for the worker target session
            let worker_send = Rc::new(Box::new(move |msg: InspectorMsg| {
              // Worker communication happens through worker_tx/worker_rx, not this send
              eprintln!(
                "[WORKER DEBUG] Dummy worker send called: {:?}",
                msg.kind
              );
            }) as InspectorSessionSend);

            // Get the next local ID for this worker
            let worker_id = sessions.next_local_id;
            sessions.next_local_id += 1;

            // Get main session send function
            let main_session_send = sessions
              .main_session_id
              .and_then(|main_id| sessions.local.get(&main_id))
              .map(|s| s.state.send.clone());

            if let Some(main_send) = main_session_send {
              // Register the worker session with the Target domain
              sessions.register_worker_session(
                worker_id,
                worker_send.clone(),
                &*main_send,
              );

              // Register the worker channels
              sessions
                .register_worker_channels(worker_id, worker_tx, worker_rx);

              eprintln!(
                "[WORKER DEBUG] Worker registered with ID {}",
                worker_id
              );
            } else {
              // Buffer the worker until main session connects
              eprintln!(
                "[WORKER DEBUG] No main session yet, buffering worker with ID {}",
                worker_id
              );
              sessions.pending_worker_proxies.push((
                worker_id,
                worker_send,
                worker_tx,
                worker_rx,
              ));
            }

            continue;
          }

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

            for target_session in sessions.target_sessions.values_mut() {
              if let Some(worker_rx) = &mut target_session.worker_rx {
                match worker_rx.poll_next_unpin(cx) {
                  Poll::Ready(Some(msg)) => {
                    eprintln!(
                      "[WORKER DEBUG] Received message from worker: {}, forwarding to main session",
                      msg.content
                    );

                    // Wrap the message in Target.receivedMessageFromTarget
                    let wrapped = json!({
                      "method": "Target.receivedMessageFromTarget",
                      "params": {
                        "sessionId": target_session.session_id,
                        "message": msg.content,
                        "targetId": target_session.target_id
                      }
                    });

                    let wrapped_msg = InspectorMsg {
                      kind: InspectorMsgKind::Notification,
                      content: wrapped.to_string(),
                    };

                    send(wrapped_msg);
                    has_worker_message = true;
                  }
                  Poll::Ready(None) => {
                    eprintln!("[WORKER DEBUG] Worker channel closed");
                  }
                  Poll::Pending => {}
                }
              }
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
        // RefCell::new(SessionContainer::temporary_placeholder()),
        Mutex::new(SessionContainer::temporary_placeholder()),
      ),
      is_dispatching_message: Default::default(),
    });
    let client = Box::new(JsRuntimeInspectorClient(state.clone()));
    let v8_inspector_client = v8::inspector::V8InspectorClient::new(client);
    let v8_inspector = Rc::new(v8::inspector::V8Inspector::create(
      scope,
      v8_inspector_client,
    ));

    // *state.sessions.borrow_mut() =
    *state.sessions.lock() =
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
    // self.state.sessions.borrow().sessions_state()
    self.state.sessions.lock().sessions_state()
  }

  pub fn poll_sessions_from_event_loop(&self, cx: &mut Context) {
    let _ = self.state.poll_sessions(Some(cx)).unwrap();
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  pub fn wait_for_session(&self) {
    loop {
      if let Some(_session) =
        // self.state.sessions.borrow_mut().local.values().next()
        self.state.sessions.lock().local.values().next()
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
        // self.state.sessions.borrow_mut().local.values().next()
        self.state.sessions.lock().local.values().next()
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
        // let mut s = sessions.borrow_mut();
        let mut s = sessions.lock();
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

  // Target domain support for worker debugging
  next_target_session_id: i32,
  target_sessions: HashMap<String, TargetSession>, // sessionId -> TargetSession
  auto_attach_enabled: bool,
  main_session_id: Option<i32>, // The first session that should receive Target events
  pending_worker_channels:
    Option<(UnboundedSender<String>, UnboundedReceiver<InspectorMsg>)>,
  pending_worker_proxies: Vec<(
    i32,
    Rc<InspectorSessionSend>,
    UnboundedSender<String>,
    UnboundedReceiver<InspectorMsg>,
  )>,
}

/// Represents a CDP Target session (e.g., a worker)
struct TargetSession {
  target_id: String,
  session_id: String,
  local_session_id: i32,
  send: Rc<InspectorSessionSend>,
  // Channels for communicating with the worker's V8 inspector
  // main -> worker: send CDP messages to worker
  worker_tx: Option<UnboundedSender<String>>,
  // worker -> main: receive CDP messages from worker
  worker_rx: Option<UnboundedReceiver<InspectorMsg>>,
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

      next_target_session_id: 1,
      target_sessions: HashMap::new(),
      auto_attach_enabled: false,
      main_session_id: None,
      pending_worker_channels: None,
      pending_worker_proxies: Vec::new(),
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

      next_target_session_id: 1,
      target_sessions: HashMap::new(),
      auto_attach_enabled: false,
      main_session_id: None,
      pending_worker_channels: None,
      pending_worker_proxies: Vec::new(),
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

  /// Handle Target domain CDP messages for worker debugging
  fn handle_target_message(
    &mut self,
    method: &str,
    params: Option<serde_json::Value>,
    id: Option<i32>,
    send: &InspectorSessionSend,
  ) -> Result<serde_json::Value, String> {
    eprintln!(
      "[WORKER DEBUG] handle_target_message called: method={}",
      method
    );
    match method {
      "Target.setDiscoverTargets" => {
        // Enable target discovery
        let discover = params
          .as_ref()
          .and_then(|p| p.get("discover"))
          .and_then(|d| d.as_bool())
          .unwrap_or(false);

        eprintln!(
          "[WORKER DEBUG] Target.setDiscoverTargets: discover={}, target_sessions.len()={}",
          discover,
          self.target_sessions.len()
        );

        // Send targetCreated events for all existing workers
        if discover {
          for target_session in self.target_sessions.values() {
            eprintln!(
              "[WORKER DEBUG] Sending Target.targetCreated for {}",
              target_session.target_id
            );
            let event = json!({
              "method": "Target.targetCreated",
              "params": {
                "targetInfo": {
                  "targetId": target_session.target_id,
                  "type": "worker",
                  "title": format!("Worker {}", target_session.local_session_id),
                  "url": "",
                  "attached": false,
                  "canAccessOpener": false
                }
              }
            });

            send(InspectorMsg {
              kind: InspectorMsgKind::Notification,
              content: event.to_string(),
            });
          }
        }

        Ok(json!({}))
      }
      "Target.setAutoAttach" => {
        // Enable auto-attach to workers
        let auto_attach = params
          .as_ref()
          .and_then(|p| p.get("autoAttach"))
          .and_then(|a| a.as_bool())
          .unwrap_or(false);

        self.auto_attach_enabled = auto_attach;
        eprintln!(
          "[WORKER DEBUG] Target.setAutoAttach: autoAttach={}",
          auto_attach
        );
        Ok(json!({}))
      }
      "Target.sendMessageToTarget" => {
        // Route message to worker session
        let session_id = params
          .as_ref()
          .and_then(|p| p.get("sessionId"))
          .and_then(|s| s.as_str())
          .ok_or("Missing sessionId")
          .unwrap();
        // .ok_or("Missing sessionId")?;

        let message = params
          .as_ref()
          .and_then(|p| p.get("message"))
          .and_then(|m| m.as_str())
          .ok_or("Missing message")
          .unwrap();
        // .ok_or("Missing message")?;

        eprintln!(
          "[WORKER DEBUG] Target.sendMessageToTarget: sessionId={}, message={}",
          session_id, message
        );

        if let Some(target_session) = self.target_sessions.get(session_id) {
          // Use worker channel if available, otherwise fall back to local session
          if let Some(worker_tx) = &target_session.worker_tx {
            eprintln!(
              "[WORKER DEBUG] Forwarding message to worker via channel"
            );
            if let Err(e) = worker_tx.unbounded_send(message.to_string()) {
              eprintln!(
                "[WORKER DEBUG] Failed to send message to worker: {}",
                e
              );
              return Err(format!("Failed to send message to worker: {}", e));
            }
          } else {
            eprintln!(
              "[WORKER DEBUG] Warning: No worker channel, using local session (won't work for real workers)"
            );
            let local_session =
              self.local.get(&target_session.local_session_id).unwrap();
            local_session.dispatch_message(message.to_string());
          }
          Ok(json!({}))
        } else {
          Err(format!("Target session not found: {}", session_id))
        }
      }
      _ => {
        eprintln!("[WORKER DEBUG] Unhandled Target method: {}", method);
        Ok(json!({}))
      }
    }
  }

  /// Register a worker session and send Target.attachedToTarget event
  fn register_worker_session(
    &mut self,
    local_session_id: i32,
    send: Rc<InspectorSessionSend>,
    main_session_send: &InspectorSessionSend,
  ) {
    let target_id = format!("worker-{}", local_session_id);
    let session_id = format!("session-{}", self.next_target_session_id);
    self.next_target_session_id += 1;

    let target_session = TargetSession {
      target_id: target_id.clone(),
      session_id: session_id.clone(),
      local_session_id,
      send,
      worker_tx: None,
      worker_rx: None,
    };
    self
      .target_sessions
      .insert(session_id.clone(), target_session);

    // Target.targetCreated still goes directly to the main session.
    let created_event = json!({
      "method": "Target.targetCreated",
      "params": {
        "targetInfo": {
          "targetId": target_id,
          "type": "worker",
          "title": format!("Worker {}", local_session_id),
          "url": "",
          "attached": false,
          "canAccessOpener": false
        }
      }
    });
    main_session_send(InspectorMsg {
      kind: InspectorMsgKind::Notification,
      content: created_event.to_string(),
    });

    if self.auto_attach_enabled {
      // 1) Use the correct key: sessionId
      let attached_event = json!({
        "method": "Target.attachedToTarget",
        "params": {
          "sessionId": session_id,
          "targetInfo": {
            "targetId": target_id,
            "type": "worker",
            "title": format!("Worker {}", local_session_id),
            "url": "",
            "attached": true,
            "canAccessOpener": false
          },
          "waitingForDebugger": false
        }
      });
      main_session_send(InspectorMsg {
        kind: InspectorMsgKind::Notification,
        content: attached_event.to_string(),
      });

      // 2) Runtime.* events must arrive via Target.receivedMessageFromTarget.
      let runtime_event = json!({
        "method": "Runtime.executionContextCreated",
        "params": {
          "context": {
            "id": local_session_id,
            "origin": "",
            "name": format!("Worker {}", local_session_id),
            "uniqueId": format!("worker-{}", local_session_id),
            "auxData": {
              "isDefault": true,
              "type": "worker"
            }
          }
        }
      });

      let wrapped = json!({
        "method": "Target.receivedMessageFromTarget",
        "params": {
          "sessionId": format!("session-{}", self.next_target_session_id - 1),
          "targetId": format!("worker-{}", local_session_id),
          "message": runtime_event.to_string()
        }
      });
      main_session_send(InspectorMsg {
        kind: InspectorMsgKind::Notification,
        content: wrapped.to_string(),
      });
    }
  }

  /// Register the communication channels for a worker's V8 inspector
  /// This is called from the worker side to establish bidirectional communication
  pub fn register_worker_channels(
    &mut self,
    local_session_id: i32,
    worker_tx: UnboundedSender<String>,
    worker_rx: UnboundedReceiver<InspectorMsg>,
  ) -> bool {
    // Find the target session for this local session ID
    for target_session in self.target_sessions.values_mut() {
      if target_session.local_session_id == local_session_id {
        eprintln!(
          "[WORKER DEBUG] Registering channels for worker session: {}",
          target_session.session_id
        );
        target_session.worker_tx = Some(worker_tx);
        target_session.worker_rx = Some(worker_rx);
        return true;
      }
    }
    eprintln!(
      "[WORKER DEBUG] Warning: Could not find target session for local_session_id={}",
      local_session_id
    );
    false
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
  sessions: Rc<Mutex<SessionContainer>>,
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
    // sessions: Rc<RefCell<SessionContainer>>,
    sessions: Rc<Mutex<SessionContainer>>,
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
    eprintln!("[WORKER DEBUG] Received message from frontend: {}", msg);

    // Check if this is a Target domain message
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
      if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
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

          let message = params
            .as_ref()
            .and_then(|p| p.get("message"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_owned())
            .unwrap_or_default();

          // Handle Target domain messages inline to avoid deadlock
          let result: serde_json::Value = match method {
            "Target.setDiscoverTargets" => {
              let discover = params
                .as_ref()
                .and_then(|p| p.get("discover"))
                .and_then(|d| d.as_bool())
                .unwrap_or(false);

              if discover {
                // Send targetCreated AND attachedToTarget events for existing workers
                // (like Node.js --experimental-worker-inspection)
                let sessions = session.state.sessions.clone();
                let send = session.state.send.clone();
                deno_core::unsync::spawn(async move {
                  let sessions = sessions.lock();
                  eprintln!(
                    "[WORKER DEBUG] Target.setDiscoverTargets: discover=true, target_sessions.len()={}",
                    sessions.target_sessions.len()
                  );

                  for target_session in sessions.target_sessions.values() {
                    eprintln!(
                      "[WORKER DEBUG] Sending Target.targetCreated for {}",
                      target_session.target_id
                    );
                    // Send targetCreated event
                    let target_created = json!({
                      "method": "Target.targetCreated",
                      "params": {
                        "targetInfo": {
                          "targetId": target_session.target_id,
                          "type": "worker",
                          "title": format!("Worker {}", target_session.local_session_id),
                          "url": "",
                          "attached": true,
                          "canAccessOpener": false
                        }
                      }
                    });

                    send(InspectorMsg {
                      kind: InspectorMsgKind::Notification,
                      content: target_created.to_string(),
                    });

                    eprintln!(
                      "[WORKER DEBUG] Sending Target.attachedToTarget for {}",
                      target_session.target_id
                    );
                    // Send attachedToTarget event immediately after
                    let attached_to_target = json!({
                      "method": "Target.attachedToTarget",
                      "params": {
                        "sessionId": target_session.session_id,
                        "targetInfo": {
                          "targetId": target_session.target_id,
                          "type": "worker",
                          "title": format!("Worker {}", target_session.local_session_id),
                          "url": "",
                          "attached": true,
                          "canAccessOpener": false
                        },
                        "waitingForDebugger": false
                      }
                    });

                    send(InspectorMsg {
                      kind: InspectorMsgKind::Notification,
                      content: attached_to_target.to_string(),
                    });
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
                let mut sessions = sessions.lock();
                sessions.auto_attach_enabled = auto_attach;
                eprintln!(
                  "[WORKER DEBUG] Target.setAutoAttach: autoAttach={}, sending attachedToTarget for {} workers",
                  auto_attach,
                  sessions.target_sessions.len()
                );

                // Send Target.attachedToTarget for all existing workers
                if auto_attach {
                  for target_session in sessions.target_sessions.values() {
                    eprintln!(
                      "[WORKER DEBUG] Sending Target.attachedToTarget for {}",
                      target_session.target_id
                    );
                    let event = json!({
                      "method": "Target.attachedToTarget",
                      "params": {
                        "sessionId": target_session.session_id,
                        "targetInfo": {
                          "targetId": target_session.target_id,
                          "type": "worker",
                          "title": format!("Worker {}", target_session.local_session_id),
                          "url": "",
                          "attached": true,
                          "canAccessOpener": false
                        },
                        "waitingForDebugger": false
                      }
                    });

                    send(InspectorMsg {
                      kind: InspectorMsgKind::Notification,
                      content: event.to_string(),
                    });
                  }
                }
              });
              json!({})
            }
            "Target.sendMessageToTarget" => {
              eprintln!(
                "[WORKER DEBUG] Target.sendMessageToTarget: sessionId={}, message={}",
                session_id, message
              );

              // Route the message to the worker
              let sessions = session.state.sessions.clone();
              let message_str = message.to_string();
              deno_core::unsync::spawn(async move {
                let sessions = sessions.lock();
                if let Some(target_session) =
                  sessions.target_sessions.get(&session_id)
                {
                  if let Some(worker_tx) = &target_session.worker_tx {
                    eprintln!(
                      "[WORKER DEBUG] Sending message to worker via channel"
                    );
                    if let Err(e) = worker_tx.unbounded_send(message_str) {
                      eprintln!(
                        "[WORKER DEBUG] Failed to send to worker: {}",
                        e
                      );
                    }
                  } else {
                    eprintln!(
                      "[WORKER DEBUG] No worker_tx channel for session {}",
                      session_id
                    );
                  }
                } else {
                  eprintln!("[WORKER DEBUG] Session not found: {}", session_id);
                }
              });

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

            // let response = match result {
            //   Ok(result_value) => {
            //     json!({
            //       "id": id,
            //       "result": result_value
            //     })
            //   }
            //   Err(error) => {
            //     json!({
            //       "id": id,
            //       "error": {
            //         "code": -32000,
            //         "message": error
            //       }
            //     })
            //   }
            // };

            let response_msg = InspectorMsg {
              kind: InspectorMsgKind::Message(id),
              content: response.to_string(),
            };

            (session.state.send)(response_msg);
          }

          continue; // Don't dispatch to V8
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
  sessions: Rc<Mutex<SessionContainer>>,
  session_id: i32,
}

impl LocalInspectorSession {
  pub fn new(session_id: i32, sessions: Rc<Mutex<SessionContainer>>) -> Self {
    Self {
      sessions,
      session_id,
    }
  }

  pub fn dispatch(&mut self, msg: String) {
    self
      .sessions
      .lock()
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
