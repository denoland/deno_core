// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use anyhow::Context as _;
use cdp::EvaluateResponse;
use deno_core::anyhow::anyhow;
use deno_core::anyhow::Error;
use deno_core::error::AnyError;
use deno_core::futures::channel::mpsc::UnboundedReceiver as FUnboundedReceiver;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::LocalInspectorSession;
use deno_core::PollEventLoopOptions;
use deno_core::RuntimeOptions;
use deno_unsync::spawn_blocking;
use futures::FutureExt;
use futures::StreamExt;
use parking_lot::Mutex;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::validate::ValidationContext;
use rustyline::validate::ValidationResult;
use rustyline::validate::Validator;
use rustyline::Cmd;
use rustyline::CompletionType;
use rustyline::Config;
use rustyline::Context;
use rustyline::Editor;
use rustyline::KeyCode;
use rustyline::KeyEvent;
use rustyline::Modifiers;
use rustyline::RepeatCount;
use rustyline_derive::Completer;
use rustyline_derive::Helper;
use rustyline_derive::Highlighter;
use rustyline_derive::Hinter;
use rustyline_derive::Validator;
use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

mod cdp;

fn main() -> Result<(), Error> {
  //   let args: Vec<String> = std::env::args().collect();
  //   if args.len() < 2 {
  //     println!("Usage: target/examples/debug/fs_module_loader <path_to_module>");
  //     std::process::exit(1);
  //   }
  //   let main_url = &args[1];
  //   println!("Run {main_url}");

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(FsModuleLoader)),
    is_main: true,
    ..Default::default()
  });

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;

  let future = async move {
    let mut repl_session = ReplSession::initialize(js_runtime).await?;
    let mut rustyline_channel = rustyline_channel();
    let helper = EditorHelper {
      context_id: repl_session.context_id,
      sync_sender: rustyline_channel.0,
    };
    let editor = ReplEditor::new(helper)?;

    loop {
      let line = read_line_and_poll_session(
        &mut repl_session,
        &mut rustyline_channel.1,
        editor.clone(),
      )
      .await;
      match line {
        Ok(line) => {
          editor.set_should_exit_on_interrupt(false);
          let output = repl_session.evaluate_line_and_get_output(&line).await;

          print!("{}", output);
        }
        Err(ReadlineError::Interrupted) => {
          if editor.should_exit_on_interrupt() {
            break;
          }
          editor.set_should_exit_on_interrupt(true);
          println!("press ctrl+c again to exit");
          continue;
        }
        Err(ReadlineError::Eof) => {
          break;
        }
        Err(err) => {
          println!("Error: {err:?}");
          break;
        }
      };
    }
    Ok(())
  };
  runtime.block_on(future)
}

pub enum EvaluationOutput {
  Value(String),
  Error(String),
}

impl std::fmt::Display for EvaluationOutput {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      EvaluationOutput::Value(value) => f.write_str(value),
      EvaluationOutput::Error(value) => f.write_str(value),
    }
  }
}

pub fn result_to_evaluation_output(
  r: Result<EvaluationOutput, AnyError>,
) -> EvaluationOutput {
  match r {
    Ok(value) => value,
    Err(err) => EvaluationOutput::Error(format!("error: {:#}", err)),
  }
}

struct ReplSession {
  runtime: JsRuntime,
  session: LocalInspectorSession,
  context_id: u64,
  notifications: Rc<RefCell<FUnboundedReceiver<serde_json::Value>>>,
}

impl ReplSession {
  pub async fn initialize(mut runtime: JsRuntime) -> Result<Self, Error> {
    runtime.maybe_init_inspector();
    let mut session = runtime.inspector().borrow().create_local_session();

    runtime
      .with_event_loop_future(
        session
          .post_message::<()>("Runtime.enable", None)
          .boxed_local(),
        PollEventLoopOptions::default(),
      )
      .await?;

    // Enabling the runtime domain will always send trigger one executionContextCreated for each
    // context the inspector knows about so we grab the execution context from that since
    // our inspector does not support a default context (0 is an invalid context id).
    let context_id: u64;
    let mut notification_rx = session.take_notification_rx();

    loop {
      let notification = notification_rx.next().await.unwrap();
      let method = notification.get("method").unwrap().as_str().unwrap();
      let params = notification.get("params").unwrap();
      if method == "Runtime.executionContextCreated" {
        let context = params.get("context").unwrap();
        assert!(context
          .get("auxData")
          .unwrap()
          .get("isDefault")
          .unwrap()
          .as_bool()
          .unwrap());
        context_id = context.get("id").unwrap().as_u64().unwrap();
        break;
      }
    }
    assert_ne!(context_id, 0);

    // TODO(bartlomieju): maybe execute some prelude here

    Ok(Self {
      runtime,
      session,
      context_id,
      notifications: Rc::new(RefCell::new(notification_rx)),
    })
  }

  pub async fn post_message_with_event_loop<T: serde::Serialize>(
    &mut self,
    method: &str,
    params: Option<T>,
  ) -> Result<Value, AnyError> {
    self
      .runtime
      .with_event_loop_future(
        self.session.post_message(method, params).boxed_local(),
        PollEventLoopOptions {
          // NOTE(bartlomieju): this is an important bit; we don't want to pump V8
          // message loop here, so that GC won't run. Otherwise, the resulting
          // object might be GC'ed before we have a chance to inspect it.
          pump_v8_message_loop: false,
          ..Default::default()
        },
      )
      .await
  }

  pub async fn run_event_loop(&mut self) -> Result<(), AnyError> {
    self
      .runtime
      .run_event_loop(PollEventLoopOptions {
        wait_for_inspector: true,
        pump_v8_message_loop: true,
      })
      .await
  }

  async fn evaluate_expression(
    &mut self,
    expression: &str,
  ) -> Result<cdp::EvaluateResponse, AnyError> {
    let expr = format!("'use strict'; void 0;{expression}");

    self
      .post_message_with_event_loop(
        "Runtime.evaluate",
        Some(cdp::EvaluateArgs {
          expression: expr,
          object_group: None,
          include_command_line_api: None,
          silent: None,
          context_id: Some(self.context_id),
          return_by_value: None,
          generate_preview: None,
          user_gesture: None,
          await_promise: None,
          throw_on_side_effect: None,
          timeout: None,
          disable_breaks: None,
          repl_mode: Some(true),
          allow_unsafe_eval_blocked_by_csp: None,
          unique_context_id: None,
        }),
      )
      .await
      .and_then(|res| serde_json::from_value(res).map_err(|e| e.into()))
  }

  async fn evaluate_line_and_get_output(
    &mut self,
    line: &str,
  ) -> EvaluationOutput {
    // Expressions like { "foo": "bar" } are interpreted as block expressions at the
    // statement level rather than an object literal so we interpret it as an expression statement
    // to match the behavior found in a typical prompt including browser developer tools.
    let wrapped_line = if line.trim_start().starts_with('{')
      && !line.trim_end().ends_with(';')
    {
      format!("({})", &line)
    } else {
      line.to_string()
    };

    let evaluate_response = self.evaluate_expression(&wrapped_line).await;

    // If that fails, we retry it without wrapping in parens letting the error bubble up to the
    // user if it is still an error.
    let result = if wrapped_line != line
      && (evaluate_response.is_err()
        || evaluate_response
          .as_ref()
          .unwrap()
          .exception_details
          .is_some())
    {
      self.evaluate_expression(line).await
    } else {
      evaluate_response
    };

    let output = match result {
      Ok(evaluate_response) => {
        let cdp::EvaluateResponse {
          result,
          exception_details,
        } = evaluate_response;

        if let Some(exception_details) = exception_details {
          let description = match exception_details.exception {
            Some(exception) => {
              if let Some(description) = exception.description {
                description
              } else if let Some(value) = exception.value {
                value.to_string()
              } else {
                "undefined".to_string()
              }
            }
            None => "Unknown exception".to_string(),
          };
          EvaluationOutput::Error(format!(
            "{} {}",
            exception_details.text, description
          ))
        } else {
          EvaluationOutput::Value(format!("{:#?}", result))
        }
      }
      Err(err) => EvaluationOutput::Error(err.to_string()),
    };
    output
  }
}

async fn read_line_and_poll_session(
  repl_session: &mut ReplSession,
  message_handler: &mut RustylineSyncMessageHandler,
  editor: ReplEditor,
) -> Result<String, ReadlineError> {
  let mut line_fut = spawn_blocking(move || editor.readline());
  let mut poll_worker = true;
  let notifications_rc = repl_session.notifications.clone();
  let mut notifications = notifications_rc.borrow_mut();

  loop {
    tokio::select! {
      result = &mut line_fut => {
        return result.unwrap();
      }
      result = message_handler.recv() => {
        match result {
          Some(RustylineSyncMessage::PostMessage { method, params }) => {
            let result = repl_session
              .post_message_with_event_loop(&method, params)
              .await;
            message_handler.send(RustylineSyncResponse::PostMessage(result)).unwrap();
          },
          None => {}, // channel closed
        }

        poll_worker = true;
      }
      message = notifications.next() => {
        if let Some(message) = message {
          let notification: cdp::Notification = serde_json::from_value(message).unwrap();
          if notification.method == "Runtime.exceptionThrown" {
            let exception_thrown: cdp::ExceptionThrown = serde_json::from_value(notification.params).unwrap();
            let (message, description) = exception_thrown.exception_details.get_message_and_description();
            println!("{} {}", message, description);
          }
        }
      }
      _ = repl_session.run_event_loop(), if poll_worker => {
        poll_worker = false;
      }
    }
  }
}

#[derive(Clone)]
struct ReplEditor {
  inner: Arc<Mutex<Editor<EditorHelper, rustyline::history::FileHistory>>>,
  should_exit_on_interrupt: Arc<AtomicBool>,
}

impl ReplEditor {
  pub fn new(helper: EditorHelper) -> Result<Self, Error> {
    let editor_config = Config::builder()
      .completion_type(CompletionType::List)
      .build();
    let mut editor = Editor::with_config(editor_config).unwrap();
    editor.set_helper(Some(helper));
    Ok(Self {
      inner: Arc::new(Mutex::new(editor)),
      should_exit_on_interrupt: Arc::new(AtomicBool::new(false)),
    })
  }

  pub fn readline(&self) -> Result<String, ReadlineError> {
    // TODO(bartlomieju): make prompt configurable
    self.inner.lock().readline("> ")
  }

  pub fn should_exit_on_interrupt(&self) -> bool {
    self.should_exit_on_interrupt.load(Relaxed)
  }

  pub fn set_should_exit_on_interrupt(&self, yes: bool) {
    self.should_exit_on_interrupt.store(yes, Relaxed);
  }
}

#[derive(Helper, Hinter, Validator, Highlighter, Completer)]
struct EditorHelper {
  pub context_id: u64,
  pub sync_sender: RustylineSyncMessageSender,
}

/// Rustyline uses synchronous methods in its interfaces, but we need to call
/// async methods. To get around this, we communicate with async code by using
/// a channel and blocking on the result.
pub fn rustyline_channel(
) -> (RustylineSyncMessageSender, RustylineSyncMessageHandler) {
  let (message_tx, message_rx) = channel(1);
  let (response_tx, response_rx) = unbounded_channel();

  (
    RustylineSyncMessageSender {
      message_tx,
      response_rx: RefCell::new(response_rx),
    },
    RustylineSyncMessageHandler {
      response_tx,
      message_rx,
    },
  )
}

pub enum RustylineSyncMessage {
  PostMessage {
    method: String,
    params: Option<Value>,
  },
  //   LspCompletions {
  //     line_text: String,
  //     position: usize,
  //   },
}

pub enum RustylineSyncResponse {
  PostMessage(Result<Value, AnyError>),
  //   LspCompletions(Vec<ReplCompletionItem>),
}

pub struct RustylineSyncMessageSender {
  message_tx: Sender<RustylineSyncMessage>,
  response_rx: RefCell<UnboundedReceiver<RustylineSyncResponse>>,
}

impl RustylineSyncMessageSender {
  pub fn post_message<T: serde::Serialize>(
    &self,
    method: &str,
    params: Option<T>,
  ) -> Result<Value, AnyError> {
    if let Err(err) =
      self
        .message_tx
        .blocking_send(RustylineSyncMessage::PostMessage {
          method: method.to_string(),
          params: params
            .map(|params| serde_json::to_value(params))
            .transpose()?,
        })
    {
      Err(anyhow!("{}", err))
    } else {
      match self.response_rx.borrow_mut().blocking_recv().unwrap() {
        RustylineSyncResponse::PostMessage(result) => result,
        // RustylineSyncResponse::LspCompletions(_) => unreachable!(),
      }
    }
  }
}

pub struct RustylineSyncMessageHandler {
  message_rx: Receiver<RustylineSyncMessage>,
  response_tx: UnboundedSender<RustylineSyncResponse>,
}

impl RustylineSyncMessageHandler {
  pub async fn recv(&mut self) -> Option<RustylineSyncMessage> {
    self.message_rx.recv().await
  }

  pub fn send(&self, response: RustylineSyncResponse) -> Result<(), AnyError> {
    self
      .response_tx
      .send(response)
      .map_err(|err| anyhow!("{}", err))
  }
}
