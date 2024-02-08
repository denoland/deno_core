// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;

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
  pub extension_metadata: Vec<ExtensionSnapshotMetadata>,
}

static MODULE_MAP_CONTEXT_DATA_INDEX: usize = 0;
static JS_HANDLED_PROMISE_REJECTION_CB_DATA_INDEX: usize = 1;
// NOTE(bartlomieju): this is only used in debug builds
static EXTENSION_METADATA_INDEX: usize = 2;

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

  let extension_metadata = match scope
    .get_context_data_from_snapshot_once::<v8::Value>(EXTENSION_METADATA_INDEX)
  {
    Ok(val) => {
      #[cfg(debug_assertions)]
      {
        let v8_str: v8::Local<v8::String> = val.try_into().unwrap();
        let rust_str = v8_str.to_rust_string_lossy(&mut scope);
        parse_extension_and_ops_data(rust_str).unwrap()
      }

      #[cfg(not(debug_assertions))]
      {
        ExtensionSnapshotMetadata::default()
      }
    }
    Err(err) => data_error_to_panic(err),
  };

  // Over allocate so executing a few scripts doesn't have to resize this vec.
  let mut module_handles = Vec::with_capacity(next_module_id as usize + 16);
  for i in 3..=next_module_id + 1 {
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
    extension_metadata,
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

  #[cfg(debug_assertions)]
  let extension_metadata_val: v8::Local<v8::Data> = {
    let rust_str =
      extension_and_ops_data_for_snapshot(&snapshotted_data.extension_metadata);
    v8::String::new(scope, &rust_str).unwrap().into()
  };

  #[cfg(not(debug_assertions))]
  let extension_metadata_val: v8::Local<v8::Data> = v8::undefined(scope).into();

  let offset =
    scope.add_context_data(local_context, extension_metadata_val.into());
  assert_eq!(offset, EXTENSION_METADATA_INDEX);

  for (index, handle) in snapshotted_data.module_handles.into_iter().enumerate()
  {
    let module_handle = v8::Local::new(scope, handle);
    let offset = scope.add_context_data(local_context, module_handle);
    assert_eq!(offset, index + 3);
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

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtensionSnapshotMetadata {
  ext_name: String,
  op_names: Vec<String>,
  external_ref_count: usize,
}
type SnapshotMetadata = Vec<ExtensionSnapshotMetadata>;

fn extension_and_ops_data_for_snapshot(data: &SnapshotMetadata) -> String {
  serde_json::to_string(data).unwrap()
}

fn parse_extension_and_ops_data(
  input: String,
) -> Result<SnapshotMetadata, anyhow::Error> {
  Ok(serde_json::from_str(&input)?)
}

macro_rules! svec {
  ($($x:expr),* $(,)?) => (vec![$($x.to_string()),*]);
}

#[test]
fn extension_and_ops_data_for_snapshot_test() {
  let expected = r#"[{"ext_name":"deno_core","op_names":["op_name1","op_name2","op_name3"],"external_ref_count":0},{"ext_name":"ext1","op_names":["op_ext1_1","op_ext1_2","op_ext1_3"],"external_ref_count":0},{"ext_name":"ext2","op_names":["op_ext2_1","op_ext2_2"],"external_ref_count":0},{"ext_name":"ext3","op_names":["op_ext3_1"],"external_ref_count":5},{"ext_name":"ext4","op_names":["op_ext4_1","op_ext4_2","op_ext4_3","op_ext4_4","op_ext4_5"],"external_ref_count":0},{"ext_name":"ext5","op_names":[],"external_ref_count":0}]"#;

  let data = vec![
    ExtensionSnapshotMetadata {
      ext_name: "deno_core".to_string(),
      op_names: svec!["op_name1", "op_name2", "op_name3"],
      external_ref_count: 0,
    },
    ExtensionSnapshotMetadata {
      ext_name: "ext1".to_string(),
      op_names: svec!["op_ext1_1", "op_ext1_2", "op_ext1_3"],
      external_ref_count: 0,
    },
    ExtensionSnapshotMetadata {
      ext_name: "ext2".to_string(),
      op_names: svec!["op_ext2_1", "op_ext2_2"],
      external_ref_count: 0,
    },
    ExtensionSnapshotMetadata {
      ext_name: "ext3".to_string(),
      op_names: svec!["op_ext3_1"],
      external_ref_count: 5,
    },
    ExtensionSnapshotMetadata {
      ext_name: "ext4".to_string(),
      op_names: svec![
        "op_ext4_1",
        "op_ext4_2",
        "op_ext4_3",
        "op_ext4_4",
        "op_ext4_5",
      ],
      external_ref_count: 0,
    },
    ExtensionSnapshotMetadata {
      ext_name: "ext5".to_string(),
      op_names: vec![],
      external_ref_count: 0,
    },
  ];

  let actual = extension_and_ops_data_for_snapshot(&data);
  pretty_assertions::assert_eq!(actual, expected);

  let parsed = parse_extension_and_ops_data(actual).unwrap();
  assert_eq!(parsed, data);
}
