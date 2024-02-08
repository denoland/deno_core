// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::bail;

use crate::Extension;
use crate::JsRuntimeForSnapshot;
use crate::RuntimeOptions;
use crate::Snapshot;

pub type CompressionCb = dyn Fn(&mut Vec<u8>, &[u8]);
pub type WithRuntimeCb = dyn Fn(&mut JsRuntimeForSnapshot);

pub struct CreateSnapshotOptions {
  pub cargo_manifest_dir: &'static str,
  pub snapshot_path: PathBuf,
  pub startup_snapshot: Option<Snapshot>,
  pub skip_op_registration: bool,
  pub extensions: Vec<Extension>,
  pub compression_cb: Option<Box<CompressionCb>>,
  pub with_runtime_cb: Option<Box<WithRuntimeCb>>,
}

pub struct CreateSnapshotOutput {
  /// Any files marked as LoadedFromFsDuringSnapshot are collected here and should be
  /// printed as 'cargo:rerun-if-changed' lines from your build script.
  pub files_loaded_during_snapshot: Vec<PathBuf>,
}

#[must_use = "The files listed by create_snapshot should be printed as 'cargo:rerun-if-changed' lines"]
pub fn create_snapshot(
  create_snapshot_options: CreateSnapshotOptions,
) -> CreateSnapshotOutput {
  let mut mark = Instant::now();

  let mut js_runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
    startup_snapshot: create_snapshot_options.startup_snapshot,
    extensions: create_snapshot_options.extensions,
    skip_op_registration: create_snapshot_options.skip_op_registration,
    ..Default::default()
  });
  println!(
    "JsRuntime for snapshot prepared, took {:#?} ({})",
    Instant::now().saturating_duration_since(mark),
    create_snapshot_options.snapshot_path.display()
  );
  mark = Instant::now();

  let files_loaded_during_snapshot = js_runtime
    .files_loaded_from_fs_during_snapshot()
    .iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();

  if let Some(with_runtime_cb) = create_snapshot_options.with_runtime_cb {
    with_runtime_cb(&mut js_runtime);
  }

  let snapshot = js_runtime.snapshot();
  let snapshot_slice: &[u8] = &snapshot;
  println!(
    "Snapshot size: {}, took {:#?} ({})",
    snapshot_slice.len(),
    Instant::now().saturating_duration_since(mark),
    create_snapshot_options.snapshot_path.display()
  );
  mark = Instant::now();

  let maybe_compressed_snapshot: Box<dyn AsRef<[u8]>> =
    if let Some(compression_cb) = create_snapshot_options.compression_cb {
      let mut vec = vec![];

      vec.extend_from_slice(
        &u32::try_from(snapshot.len())
          .expect("snapshot larger than 4gb")
          .to_le_bytes(),
      );

      (compression_cb)(&mut vec, snapshot_slice);

      println!(
        "Snapshot compressed size: {}, took {:#?} ({})",
        vec.len(),
        Instant::now().saturating_duration_since(mark),
        create_snapshot_options.snapshot_path.display()
      );
      mark = std::time::Instant::now();

      Box::new(vec)
    } else {
      Box::new(snapshot_slice)
    };

  std::fs::write(
    &create_snapshot_options.snapshot_path,
    &*maybe_compressed_snapshot,
  )
  .unwrap();
  println!(
    "Snapshot written, took: {:#?} ({})",
    Instant::now().saturating_duration_since(mark),
    create_snapshot_options.snapshot_path.display(),
  );
  CreateSnapshotOutput {
    files_loaded_during_snapshot,
  }
}

pub type FilterFn = Box<dyn Fn(&PathBuf) -> bool>;

pub fn get_js_files(
  cargo_manifest_dir: &'static str,
  directory: &str,
  filter: Option<FilterFn>,
) -> Vec<PathBuf> {
  let manifest_dir = Path::new(cargo_manifest_dir);
  let mut js_files = std::fs::read_dir(directory)
    .unwrap()
    .map(|dir_entry| {
      let file = dir_entry.unwrap();
      manifest_dir.join(file.path())
    })
    .filter(|path| {
      path.extension().unwrap_or_default() == "js"
        && filter.as_ref().map(|filter| filter(path)).unwrap_or(true)
    })
    .collect::<Vec<PathBuf>>();
  js_files.sort();
  js_files
}

fn data_error_to_panic(err: v8::DataError) -> ! {
  match err {
    v8::DataError::BadType { actual, expected } => {
      panic!(
        "Invalid type for snapshot data: expected {expected}, got {actual}"
      );
    }
    v8::DataError::NoData { expected } => {
      panic!("No data for snapshot data: expected {expected}");
    }
  }
}

pub(crate) struct SnapshottedData {
  pub module_map_data: v8::Global<v8::Array>,
  pub module_handles: Vec<v8::Global<v8::Module>>,
  pub js_handled_promise_rejection_cb: Option<v8::Global<v8::Function>>,
}

static MODULE_MAP_CONTEXT_DATA_INDEX: usize = 0;
static JS_HANDLED_PROMISE_REJECTION_CB_DATA_INDEX: usize = 1;

pub(crate) fn get_snapshotted_data(
  scope: &mut v8::HandleScope<()>,
  context: v8::Local<v8::Context>,
) -> SnapshottedData {
  let mut scope = v8::ContextScope::new(scope, context);

  // The 0th element is the module map itself, followed by X number of module
  // handles. We need to deserialize the "next_module_id" field from the
  // map to see how many module handles we expect.
  let result = scope.get_context_data_from_snapshot_once::<v8::Array>(
    MODULE_MAP_CONTEXT_DATA_INDEX,
  );

  let val = match result {
    Ok(v) => v,
    Err(err) => data_error_to_panic(err),
  };

  let next_module_id = {
    let info_data: v8::Local<v8::Array> =
      val.get_index(&mut scope, 2).unwrap().try_into().unwrap();
    info_data.length()
  };

  let js_handled_promise_rejection_cb = match scope
    .get_context_data_from_snapshot_once::<v8::Value>(
      JS_HANDLED_PROMISE_REJECTION_CB_DATA_INDEX,
    ) {
    Ok(val) => {
      if val.is_undefined() {
        None
      } else {
        let fn_: v8::Local<v8::Function> = val.try_into().unwrap();
        Some(v8::Global::new(&mut scope, fn_))
      }
    }
    Err(err) => data_error_to_panic(err),
  };

  // Over allocate so executing a few scripts doesn't have to resize this vec.
  let mut module_handles = Vec::with_capacity(next_module_id as usize + 16);
  for i in 2..=next_module_id + 1 {
    match scope.get_context_data_from_snapshot_once::<v8::Module>(i as usize) {
      Ok(val) => {
        let module_global = v8::Global::new(&mut scope, val);
        module_handles.push(module_global);
      }
      Err(err) => data_error_to_panic(err),
    }
  }

  SnapshottedData {
    module_map_data: v8::Global::new(&mut scope, val),
    module_handles,
    js_handled_promise_rejection_cb,
  }
}

pub(crate) fn set_snapshotted_data(
  scope: &mut v8::HandleScope<()>,
  context: v8::Global<v8::Context>,
  snapshotted_data: SnapshottedData,
) {
  let local_context = v8::Local::new(scope, context);
  let local_data = v8::Local::new(scope, snapshotted_data.module_map_data);
  let offset = scope.add_context_data(local_context, local_data);
  assert_eq!(offset, MODULE_MAP_CONTEXT_DATA_INDEX);

  let local_handled_promise_rejection_cb: v8::Local<v8::Value> =
    if let Some(handled_promise_rejection_cb) =
      snapshotted_data.js_handled_promise_rejection_cb
    {
      v8::Local::new(scope, handled_promise_rejection_cb).into()
    } else {
      v8::undefined(scope).into()
    };
  let offset =
    scope.add_context_data(local_context, local_handled_promise_rejection_cb);
  assert_eq!(offset, JS_HANDLED_PROMISE_REJECTION_CB_DATA_INDEX);

  for (index, handle) in snapshotted_data.module_handles.into_iter().enumerate()
  {
    let module_handle = v8::Local::new(scope, handle);
    let offset = scope.add_context_data(local_context, module_handle);
    assert_eq!(offset, index + 2);
  }
}

/// Returns an isolate set up for snapshotting.
pub(crate) fn create_snapshot_creator(
  external_refs: &'static v8::ExternalReferences,
  maybe_startup_snapshot: Option<Snapshot>,
) -> v8::OwnedIsolate {
  if let Some(snapshot) = maybe_startup_snapshot {
    match snapshot {
      Snapshot::Static(data) => {
        v8::Isolate::snapshot_creator_from_existing_snapshot(
          data,
          Some(external_refs),
        )
      }
      Snapshot::JustCreated(data) => {
        v8::Isolate::snapshot_creator_from_existing_snapshot(
          data,
          Some(external_refs),
        )
      }
      Snapshot::Boxed(data) => {
        v8::Isolate::snapshot_creator_from_existing_snapshot(
          data,
          Some(external_refs),
        )
      }
    }
  } else {
    v8::Isolate::snapshot_creator(Some(external_refs))
  }
}

#[derive(Debug, PartialEq)]
struct ExtensionWithOps {
  ext_name: String,
  op_names: Vec<String>,
}
type ExtensionAndOpsData = Vec<ExtensionWithOps>;

fn extension_and_ops_data_for_snapshot(data: &ExtensionAndOpsData) -> String {
  let mut result = String::new();

  result.push_str("EXT_COUNT:");
  result.push_str(&data.len().to_string());
  result.push(';');

  for ext_data in data {
    let ExtensionWithOps { ext_name, op_names } = ext_data;

    let op_count = op_names.len();

    result.push_str("EXT:");
    result.push_str(&ext_name);
    result.push_str(":");
    result.push_str(&op_count.to_string());
    result.push(';');
    if !op_names.is_empty() {
      result.push_str("OPS:");
      for (idx, op_name) in op_names.iter().enumerate() {
        result.push_str(op_name);
        if idx != (op_count - 1) {
          result.push(',');
        }
      }
      result.push(';');
    }
  }

  result
}

fn parse_extension_and_ops_data(
  input: String,
) -> Result<ExtensionAndOpsData, anyhow::Error> {
  let mut data = Vec::new();
  let mut current_ext: Option<String> = None;
  let mut current_ops: Vec<String> = Vec::new();
  let mut current_ops_count = 0;
  let mut current_ops_count_next = false;
  let mut ext_count = 0;
  let mut ext_count_next = false;

  for token in input.split(|c| c == ';' || c == ':') {
    if ext_count_next {
      ext_count_next = false;
      // TODO: handle error
      ext_count = token.parse().unwrap();
      continue;
    } else if current_ops_count_next {
      current_ops_count_next = false;
      // TODO: handle error
      current_ops_count = token.parse().unwrap();
      continue;
    }
    match token {
      "EXT_COUNT" => {
        ext_count_next = true;
      }
      "EXT" => {
        if let Some(ext_name) = current_ext.take() {
          if current_ops.len() != current_ops_count {
            eprintln!("current ops {:#?}", current_ops);
            bail!(
              "Mismatch in ops count for ext: {}; expected: {}, found: {}",
              ext_name,
              current_ops_count,
              current_ops.len()
            );
          }
          data.push(ExtensionWithOps {
            ext_name,
            op_names: current_ops.clone(),
          });
          current_ops.clear();
        }
      }
      "" => {
        // TODO: parse error
        ()
      }
      _ if current_ext.is_none() => {
        current_ext = Some(token.to_string());
        current_ops_count_next = true;
      }
      _ => {
        current_ops.push(token.to_string());
      }
    }
  }

  if let Some(ext_name) = current_ext.take() {
    data.push(ExtensionWithOps {
      ext_name,
      op_names: current_ops,
    });
  }

  if data.len() != ext_count {
    bail!(
      "Mismatch in extension count, found: {}, expected: {}",
      data.len(),
      ext_count
    );
  }

  Ok(data)
}

macro_rules! svec {
  ($($x:expr),* $(,)?) => (vec![$($x.to_string()),*]);
}

#[test]
fn extension_and_ops_data_for_snapshot_test() {
  let expected = "EXT_COUNT:6;EXT:deno_core:3;OPS:op_name1,op_name2,op_name3;EXT:ext1:3;OPS:op_ext1_1,op_ext1_2,op_ext1_3;EXT:ext2:2;OPS:op_ext2_1,op_ext2_2;EXT:ext3:1;OPS:op_ext3_1;EXT:ext4:5;OPS:op_ext4_1,op_ext4_2,op_ext4_3,op_ext4_4,op_ext4_5;EXT:ext5:0;";

  let data = vec![
    ExtensionWithOps {
      ext_name: "deno_core".to_string(),
      op_names: svec!["op_name1", "op_name2", "op_name3"],
    },
    ExtensionWithOps {
      ext_name: "ext1".to_string(),
      op_names: svec!["op_ext1_1", "op_ext1_2", "op_ext1_3"],
    },
    ExtensionWithOps {
      ext_name: "ext2".to_string(),
      op_names: svec!["op_ext2_1", "op_ext2_2"],
    },
    ExtensionWithOps {
      ext_name: "ext3".to_string(),
      op_names: svec!["op_ext3_1"],
    },
    ExtensionWithOps {
      ext_name: "ext4".to_string(),
      op_names: svec![
        "op_ext4_1",
        "op_ext4_2",
        "op_ext4_3",
        "op_ext4_4",
        "op_ext4_5",
      ],
    },
    ExtensionWithOps {
      ext_name: "ext5".to_string(),
      op_names: vec![],
    },
  ];

  let actual = extension_and_ops_data_for_snapshot(&data);
  pretty_assertions::assert_eq!(actual, expected);

  let parsed = parse_extension_and_ops_data(actual).unwrap();
  assert_eq!(parsed, data);
}
