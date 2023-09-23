use anyhow::Context;
use deno_core::anyhow::Error;
use deno_core::extension;
use deno_core::op2;
use deno_core::resolve_path;
use deno_core::v8;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::OpState;
use std::rc::Rc;

#[op2]
fn op_use_state(
  state: &mut OpState,
  #[global] callback: v8::Global<v8::Function>,
) -> Result<(), Error> {
  state.put(callback);
  Ok(())
}

extension!(
  op2_sample,
  ops = [op_use_state],
  esm_entry_point = "ext:op2_sample/op2.js",
  esm = [ dir "examples", "op2.js" ],
  docs = "A small example demonstrating op2 usage", "Contains one op"
);

fn main() -> Result<(), Error> {
  let module_name = "test.js";
  let module_code = "
      op2_sample.use_state(() => {
          console.log('Hello World');
      });
  "
  .to_string();

  let mut js_runtime = JsRuntime::new(deno_core::RuntimeOptions {
    module_loader: Some(Rc::new(FsModuleLoader)),
    extensions: vec![op2_sample::init_ops_and_esm()],
    ..Default::default()
  });

  let main_module = resolve_path(
    module_name,
    &std::env::current_dir().context("Unable to get current working directory")?,
  )?;

  let future = async move {
    let mod_id = js_runtime
      .load_main_module(
        &main_module,
        Some(deno_core::FastString::from(module_code)),
      )
      .await?;

    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(false).await?;
    result.await?
  };

  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(future)
}
