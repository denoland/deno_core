// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use deno_core::anyhow::anyhow;
use deno_core::anyhow::Error;
use deno_core::cdp;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::RuntimeOptions;
use deno_unsync::spawn_blocking;
use futures::StreamExt;
use parking_lot::Mutex;
use rustyline::error::ReadlineError;
use rustyline::CompletionType;
use rustyline::Config;
use rustyline::Editor;
use rustyline_derive::Completer;
use rustyline_derive::Helper;
use rustyline_derive::Highlighter;
use rustyline_derive::Hinter;
use rustyline_derive::Validator;
use std::cell::RefCell;
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

mod session;
use session::ReplSession;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
  let js_runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(FsModuleLoader)),
    is_main: true,
    ..Default::default()
  });

  let session = ReplSession::initialize(js_runtime).await?;
  let rustyline_channel = rustyline_channel();
  let helper = EditorHelper {
    context_id: session.context_id,
    sync_sender: rustyline_channel.0,
  };
  let editor = ReplEditor::new(helper)?;
  let mut repl = Repl {
    session,
    editor,
    message_handler: rustyline_channel.1,
  };
  repl.run().await?;
  Ok(())
}

struct Repl {
  session: ReplSession,
  editor: ReplEditor,
  message_handler: RustylineSyncMessageHandler,
}

impl Repl {
  async fn run(&mut self) -> Result<(), AnyError> {
    loop {
      let line = read_line_and_poll_session(
        &mut self.session,
        &mut self.message_handler,
        self.editor.clone(),
      )
      .await;
      match line {
        Ok(line) => {
          self.editor.set_should_exit_on_interrupt(false);
          let output = self.session.evaluate_line_and_get_output(&line).await;

          println!("{}", output);
        }
        Err(ReadlineError::Interrupted) => {
          if self.editor.should_exit_on_interrupt() {
            break;
          }
          self.editor.set_should_exit_on_interrupt(true);
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
  let mut notifications = notifications_rc.lock().await;

  loop {
    tokio::select! {
      result = &mut line_fut => {
        return result.unwrap();
      }
      result = message_handler.recv() => {
        if let Some(RustylineSyncMessage::PostMessage { method, params }) = result {
            let result = repl_session
              .post_message_with_event_loop(&method, params)
              .await;
            message_handler.send(RustylineSyncResponse::PostMessage(result)).unwrap();
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
pub struct EditorHelper {
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
