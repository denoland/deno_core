// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use anyhow::anyhow;
use anyhow::Context;
use deno_core::anyhow::Error;
use deno_core::error::generic_error;
use deno_core::v8;
use deno_core::CustomModuleEvaluationKind;
use deno_core::FastString;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::ModuleSourceCode;
use deno_core::ModuleType;
use deno_core::RuntimeOptions;
use serde::Deserialize;
use std::borrow::Cow;
use std::rc::Rc;

fn custom_module_evaluation_cb(
  scope: &mut v8::HandleScope,
  module_type: Cow<'_, str>,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
  match &*module_type {
    "bytes" => bytes_module(scope, code),
    "text" => text_module(scope, module_name, code),
    "wasm" => wasm_module(scope, module_name, code),
    _ => Err(anyhow!(
      "Can't import {:?} because of unknown module type {}",
      module_name,
      module_type
    )),
  }
}

fn bytes_module(
  scope: &mut v8::HandleScope,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };
  let owned_buf = buf.to_vec();
  let buf_len: usize = owned_buf.len();
  let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(owned_buf);
  let backing_store_shared = backing_store.make_shared();
  let ab = v8::ArrayBuffer::with_backing_store(scope, &backing_store_shared);
  let uint8_array = v8::Uint8Array::new(scope, ab, 0, buf_len).unwrap();
  let value: v8::Local<v8::Value> = uint8_array.into();
  Ok(CustomModuleEvaluationKind::Synthetic(v8::Global::new(
    scope, value,
  )))
}

#[derive(Debug)]
struct WasmModuleAnalysis {
  pub imports: Vec<WebAssemblyImportDescriptor>,
  pub exports: Vec<WebAssemblyExportDescriptor>,
}
#[derive(Debug, Deserialize)]
struct WebAssemblyImportDescriptor {
  pub module: String,
  pub name: String,
  pub kind: String,
}

#[derive(Debug, Deserialize)]
struct WebAssemblyExportDescriptor {
  pub name: String,
  pub kind: String,
}

fn wasm_module(
  scope: &mut v8::HandleScope,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };
  let Some(wasm_module) = v8::WasmModuleObject::compile(scope, buf.as_bytes())
  else {
    return Err(generic_error(format!(
      "Failed to compile WASM module '{}'",
      module_name.as_str()
    )));
  };
  let wasm_module_value: v8::Local<v8::Value> = wasm_module.into();

  // Get imports and exports of the WASM module, then rendered a shim JS module
  // that will be the actual module evaluated.
  let js_wasm_module_source = {
    let wasm_module_analysis = analyze_wasm_module(scope, wasm_module_value);
    let js_wasm_module_source =
      render_js_wasm_module(module_name.as_str(), wasm_module_analysis);
    js_wasm_module_source
  };

  let wasm_module_value_global = v8::Global::new(scope, wasm_module_value);
  let synthetic_module_type = ModuleType::Other("wasm-module".into());

  Ok(CustomModuleEvaluationKind::ComputedAndSynthetic(
    js_wasm_module_source.into(),
    wasm_module_value_global,
    synthetic_module_type,
  ))
}

fn analyze_wasm_module(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> WasmModuleAnalysis {
  // TODO(bartlomieju): these should be stored on JsRuntime startup and called
  // from JsRuntime - so that users can't tamper with these globals
  let context = scope.get_current_context();
  let global = context.global(scope);
  let web_assembly_key =
    v8::String::new_external_onebyte_static(scope, b"WebAssembly").unwrap();
  let web_assembly_module_key =
    v8::String::new_external_onebyte_static(scope, b"Module").unwrap();
  let web_assembly_module_imports_key =
    v8::String::new_external_onebyte_static(scope, b"imports").unwrap();
  let web_assembly_module_exports_key =
    v8::String::new_external_onebyte_static(scope, b"exports").unwrap();

  let web_assembly_object: v8::Local<v8::Object> = global
    .get(scope, web_assembly_key.into())
    .unwrap()
    .try_into()
    .unwrap();
  let web_assembly_module_object: v8::Local<v8::Object> = web_assembly_object
    .get(scope, web_assembly_module_key.into())
    .unwrap()
    .try_into()
    .unwrap();
  let web_assembly_module_imports_fn: v8::Local<v8::Function> =
    web_assembly_module_object
      .get(scope, web_assembly_module_imports_key.into())
      .unwrap()
      .try_into()
      .unwrap();
  let web_assembly_module_exports_fn: v8::Local<v8::Function> =
    web_assembly_module_object
      .get(scope, web_assembly_module_exports_key.into())
      .unwrap()
      .try_into()
      .unwrap();
  let receiver = v8::undefined(scope);
  let import_result = web_assembly_module_imports_fn
    .call(scope, receiver.into(), &[value])
    .unwrap();
  let imports: Vec<WebAssemblyImportDescriptor> =
    serde_v8::from_v8(scope, import_result).unwrap();
  let export_result = web_assembly_module_exports_fn
    .call(scope, receiver.into(), &[value])
    .unwrap();
  let exports: Vec<WebAssemblyExportDescriptor> =
    serde_v8::from_v8(scope, export_result).unwrap();

  WasmModuleAnalysis { imports, exports }
}

fn render_js_wasm_module(
  specifier: &str,
  wasm_module_analysis: WasmModuleAnalysis,
) -> String {
  // TODO:
  let mut src = Vec::with_capacity(1024);

  src.push(format!(
    r#"import wasmMod from "{}" with {{ type: "wasm-module" }};"#,
    specifier,
  ));

  // TODO(bartlomieju): handle imports collisions?
  if !wasm_module_analysis.imports.is_empty() {
    for import_desc in &wasm_module_analysis.imports {
      src.push(format!(
        r#"import {{ {} }} from "{}";"#,
        import_desc.name, import_desc.module
      ))
    }

    src.push("const importsObject = {};".to_string());

    for import_desc in &wasm_module_analysis.imports {
      src.push(format!(
        r#"importsObject["{}"] ??= {{}};
importsObject["{}"]["{}"] = {};"#,
        import_desc.module,
        import_desc.module,
        import_desc.name,
        import_desc.name,
      ))
    }

    src.push(
      "const modInstance = await WebAssembly.instantiate(wasmMod, importsObject);".to_string(),
    )
  } else {
    src.push(
      "const modInstance = await WebAssembly.instantiate(wasmMod);".to_string(),
    )
  }

  if !wasm_module_analysis.exports.is_empty() {
    for export_desc in &wasm_module_analysis.exports {
      if export_desc.name == "default" {
        src.push(format!(
          "export default modInstance.exports.{};",
          export_desc.name
        ));
      } else {
        src.push(format!(
          "export const {} = modInstance.exports.{};",
          export_desc.name, export_desc.name
        ));
      }
    }
  }

  src.join("\n")
}

fn text_module(
  scope: &mut v8::HandleScope,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };

  let code = std::str::from_utf8(buf.as_bytes()).with_context(|| {
    format!("Can't convert {:?} source code to string", module_name)
  })?;
  let str_ = v8::String::new(scope, code).unwrap();
  let value: v8::Local<v8::Value> = str_.into();
  Ok(CustomModuleEvaluationKind::Synthetic(v8::Global::new(
    scope, value,
  )))
}

fn main() -> Result<(), Error> {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 2 {
    println!("Usage: target/examples/debug/fs_module_loader <path_to_module>");
    std::process::exit(1);
  }
  let main_url = &args[1];
  println!("Run {main_url}");

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(FsModuleLoader)),
    custom_module_evaluation_cb: Some(Box::new(custom_module_evaluation_cb)),
    ..Default::default()
  });

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;

  let main_module = deno_core::resolve_path(
    main_url,
    &std::env::current_dir().context("Unable to get CWD")?,
  )?;

  let future = async move {
    let mod_id = js_runtime.load_main_module(&main_module, None).await?;
    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(Default::default()).await?;
    result.await
  };
  runtime.block_on(future)
}
