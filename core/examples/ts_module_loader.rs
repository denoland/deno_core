// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
//! This example shows how to use swc to transpile TypeScript and JSX/TSX
//! modules.
//!
//! It will only transpile, not typecheck (like Deno's `--no-check` flag).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Error;
use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceMapOption;
use deno_core::error::AnyError;
use deno_core::resolve_import;
use deno_core::resolve_path;
use deno_core::JsRuntime;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::ModuleType;
use deno_core::RequestedModuleType;
use deno_core::ResolutionKind;
use deno_core::RuntimeOptions;

// TODO(bartlomieju): this is duplicated in `testing/checkin`
type SourceMapStore = Rc<RefCell<HashMap<String, Vec<u8>>>>;

// TODO(bartlomieju): this is duplicated in `testing/checkin`
struct TypescriptModuleLoader {
  source_maps: SourceMapStore,
}

// TODO(bartlomieju): this is duplicated in `testing/checkin`
impl ModuleLoader for TypescriptModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, Error> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let source_maps = self.source_maps.clone();
    fn load(
      source_maps: SourceMapStore,
      module_specifier: &ModuleSpecifier,
    ) -> Result<ModuleSource, AnyError> {
      let path = module_specifier
        .to_file_path()
        .map_err(|_| anyhow!("Only file:// URLs are supported."))?;

      let media_type = MediaType::from_path(&path);
      let (module_type, should_transpile) = match MediaType::from_path(&path) {
        MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
          (ModuleType::JavaScript, false)
        }
        MediaType::Jsx => (ModuleType::JavaScript, true),
        MediaType::TypeScript
        | MediaType::Mts
        | MediaType::Cts
        | MediaType::Dts
        | MediaType::Dmts
        | MediaType::Dcts
        | MediaType::Tsx => (ModuleType::JavaScript, true),
        MediaType::Json => (ModuleType::Json, false),
        _ => bail!("Unknown extension {:?}", path.extension()),
      };

      let code = std::fs::read_to_string(&path)?;
      let code = if should_transpile {
        let parsed = deno_ast::parse_module(ParseParams {
          specifier: module_specifier.clone(),
          text: code.into(),
          media_type,
          capture_tokens: false,
          scope_analysis: false,
          maybe_syntax: None,
        })?;
        let res = parsed.transpile(
          &deno_ast::TranspileOptions {
            imports_not_used_as_values:
              deno_ast::ImportsNotUsedAsValues::Remove,
            use_decorators_proposal: true,
            ..Default::default()
          },
          &deno_ast::EmitOptions {
            source_map: SourceMapOption::Separate,
            inline_sources: true,
            ..Default::default()
          },
        )?;
        let res = res.into_source();
        let source_map = res.source_map.unwrap();
        source_maps
          .borrow_mut()
          .insert(module_specifier.to_string(), source_map);
        String::from_utf8(res.source).unwrap()
      } else {
        code
      };
      Ok(ModuleSource::new(
        module_type,
        ModuleSourceCode::String(code.into()),
        module_specifier,
        None,
      ))
    }

    ModuleLoadResponse::Sync(load(source_maps, module_specifier))
  }

  fn get_source_map(&self, specifier: &str) -> Option<Vec<u8>> {
    self.source_maps.borrow().get(specifier).cloned()
  }
}

fn main() -> Result<(), Error> {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 2 {
    println!("Usage: target/examples/debug/ts_module_loader <path_to_module>");
    std::process::exit(1);
  }
  let main_url = &args[1];
  println!("Run {main_url}");

  let source_map_store = Rc::new(RefCell::new(HashMap::new()));

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(TypescriptModuleLoader {
      source_maps: source_map_store,
    })),
    ..Default::default()
  });

  let main_module = resolve_path(
    main_url,
    &std::env::current_dir().context("Unable to get CWD")?,
  )?;

  let future = async move {
    let mod_id = js_runtime.load_main_es_module(&main_module).await?;
    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(Default::default()).await?;
    result.await
  };

  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(future)
}
