// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use deno_core::anyhow::Error;
use deno_core::cdp;
use deno_core::error::AnyError;
use deno_core::futures::channel::mpsc::UnboundedReceiver as FUnboundedReceiver;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::JsRuntime;
use deno_core::LocalInspectorSession;
use deno_core::PollEventLoopOptions;
use futures::FutureExt;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct ReplSession {
  runtime: JsRuntime,
  session: LocalInspectorSession,
  pub context_id: u64,
  pub notifications: Arc<Mutex<FUnboundedReceiver<serde_json::Value>>>,
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
      let notification =
        serde_json::from_value::<cdp::Notification>(notification)?;
      if notification.method == "Runtime.executionContextCreated" {
        let execution_context_created = serde_json::from_value::<
          cdp::ExecutionContextCreated,
        >(notification.params)?;
        assert!(execution_context_created
          .context
          .aux_data
          .get("isDefault")
          .unwrap()
          .as_bool()
          .unwrap());
        context_id = execution_context_created.context.id;
        break;
      }
    }
    assert_ne!(context_id, 0);

    // TODO(bartlomieju): maybe execute some prelude here

    Ok(Self {
      runtime,
      session,
      context_id,
      notifications: Arc::new(Mutex::new(notification_rx)),
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

  pub async fn evaluate_line_and_get_output(
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

    match result {
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
    }
  }
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
