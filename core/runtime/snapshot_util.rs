// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;

use crate::modules::ModuleMapSnapshotData;
use crate::Extension;
use crate::JsRuntimeForSnapshot;
use crate::RuntimeOptions;
use crate::Snapshot;

pub type CompressionCb = dyn Fn(&mut Vec<u8>, &[u8]);
pub type WithRuntimeCb = dyn Fn(&mut JsRuntimeForSnapshot);

pub type SnapshotDataId = u32;

#[derive(Default)]
pub struct SnapshotLoadDataStore {
  data: Vec<Option<v8::Global<v8::Data>>>,
}

impl SnapshotLoadDataStore {
  pub fn get<'s, T>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    id: SnapshotDataId,
  ) -> v8::Global<T>
  where
    v8::Local<'s, T>: TryFrom<v8::Local<'s, v8::Data>>,
  {
    let Some(data) = self.data.get_mut(id as usize) else {
      panic!(
        "Attempted to read snapshot data out of range: {id} (of {})",
        self.data.len()
      );
    };
    let Some(data) = data.take() else {
      panic!("Attempted to read the snapshot data at index {id} twice");
    };
    let local = v8::Local::new(scope, data);
    let local = v8::Local::<T>::try_from(local).unwrap_or_else(|_| {
      panic!(
        "Invalid data type at index {id}, expected '{}'",
        std::any::type_name::<T>()
      )
    });
    v8::Global::new(scope, local)
  }
}

#[derive(Default)]
pub struct SnapshotStoreDataStore {
  data: Vec<v8::Global<v8::Data>>,
}

impl SnapshotStoreDataStore {
  pub fn register<T>(&mut self, global: v8::Global<T>) -> SnapshotDataId
  where
    for<'s> v8::Local<'s, v8::Data>: From<v8::Local<'s, T>>,
  {
    let id = self.data.len();
    // TODO(mmastrac): v8::Global needs From/Into
    // SAFETY: Because we've tested that Local<Data>: From<Local<T>>, we can assume this is safe.
    unsafe {
      self.data.push(std::mem::transmute(global));
    }
    id as _
  }
}

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
  warmup_script: Option<&'static str>,
) -> CreateSnapshotOutput {
  let mut mark = Instant::now();

  // Get the extensions for a second pass if we want to warm up the snapshot.
  let warmup_exts = warmup_script.map(|_| {
    create_snapshot_options
      .extensions
      .iter()
      .map(|e| e.for_warmup())
      .collect::<Vec<_>>()
  });

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

  let mut snapshot = js_runtime.snapshot();
  if let Some(warmup_script) = warmup_script {
    let warmup_exts = warmup_exts.unwrap();

    // Warm up the snapshot bootstrap.
    //
    // - Create a new isolate with cold snapshot blob.
    // - Run warmup script in new context.
    // - Serialize the new context into a new snapshot blob.
    let mut js_runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      startup_snapshot: Some(Snapshot::JustCreated(snapshot)),
      extensions: warmup_exts,
      skip_op_registration: true,
      ..Default::default()
    });
    js_runtime
      .execute_script_static("warmup", warmup_script)
      .unwrap();

    snapshot = js_runtime.snapshot();
  }

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
  pub module_map_data: ModuleMapSnapshotData,
  pub js_handled_promise_rejection_cb: Option<v8::Global<v8::Function>>,
  pub ext_source_maps: HashMap<String, Vec<u8>>,
  // pub extension_metadata: Vec<ExtensionSnapshotMetadata>,
}

#[derive(Serialize, Deserialize)]
struct RawSnapshottedData {
  data_count: u32,
  module_map_data: ModuleMapSnapshotData,
  js_handled_promise_rejection_cb: Option<SnapshotDataId>,
  ext_source_maps: HashMap<String, Vec<u8>>,
}

static RAW_SNAPSHOTTED_DATA_INDEX: usize = 0;

pub(crate) fn get_snapshotted_data(
  scope: &mut v8::HandleScope<()>,
  context: v8::Local<v8::Context>,
) -> (SnapshottedData, SnapshotLoadDataStore) {
  let scope = &mut v8::ContextScope::new(scope, context);

  let result = scope.get_context_data_from_snapshot_once::<v8::ArrayBuffer>(
    RAW_SNAPSHOTTED_DATA_INDEX,
  );
  let val = match result {
    Ok(v) => v,
    Err(err) => data_error_to_panic(err),
  };

  let slice = val.data().unwrap().as_ptr();
  let len = val.byte_length();
  let slice = unsafe {
    std::ptr::slice_from_raw_parts_mut(slice as *mut u8, len)
      .as_mut()
      .unwrap()
  };

  #[cfg(all(
    feature = "snapshot_data_json",
    not(feature = "snapshot_data_bincode")
  ))]
  let raw_data: RawSnapshottedData = serde_json::from_slice(slice).unwrap();
  #[cfg(feature = "snapshot_data_bincode")]
  let raw_data: RawSnapshottedData =
    bincode::deserialize(slice).expect("Failed to deserialize snapshot data");

  // let extension_metadata = match scope
  //   .get_context_data_from_snapshot_once::<v8::Value>(EXTENSION_METADATA_INDEX)
  // {
  //   Ok(val) => {
  //     #[cfg(debug_assertions)]
  //     {
  //       let v8_str: v8::Local<v8::String> = val.try_into().unwrap();
  //       let rust_str = v8_str.to_rust_string_lossy(&mut scope);
  //       parse_extension_and_ops_data(rust_str).unwrap()
  //     }

  //     #[cfg(not(debug_assertions))]
  //     {
  //       ExtensionSnapshotMetadata::default()
  //     }
  //   }
  //   Err(err) => data_error_to_panic(err),
  // };

  let mut data = SnapshotLoadDataStore::default();
  for i in 0..raw_data.data_count {
    let item = scope
      .get_context_data_from_snapshot_once::<v8::Data>(i as usize + 1)
      .unwrap();
    let item = v8::Global::new(scope, item);
    data.data.push(Some(item));
  }

  (
    SnapshottedData {
      module_map_data: raw_data.module_map_data,
      js_handled_promise_rejection_cb: raw_data
        .js_handled_promise_rejection_cb
        .map(|x| data.get(scope, x)),
      ext_source_maps: raw_data.ext_source_maps,
    },
    data,
  )
}

pub(crate) fn set_snapshotted_data(
  scope: &mut v8::HandleScope,
  context: v8::Global<v8::Context>,
  snapshotted_data: SnapshottedData,
  mut data_store: SnapshotStoreDataStore,
) {
  let local_context = v8::Local::new(scope, context);

  let js_handled_promise_rejection_cb = snapshotted_data
    .js_handled_promise_rejection_cb
    .map(|v| data_store.register(v));
  let raw_snapshot_data = RawSnapshottedData {
    data_count: data_store.data.len() as _,
    module_map_data: snapshotted_data.module_map_data,
    js_handled_promise_rejection_cb,
    ext_source_maps: snapshotted_data.ext_source_maps,
  };

  #[cfg(all(
    feature = "snapshot_data_json",
    not(feature = "snapshot_data_bincode")
  ))]
  let local_data = serde_json::to_vec(&raw_snapshot_data).unwrap();
  #[cfg(feature = "snapshot_data_bincode")]
  let local_data = bincode::serialize(&raw_snapshot_data).unwrap();

  let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(local_data);
  let data =
    v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
  let offset = scope.add_context_data(local_context, data);
  assert_eq!(offset, RAW_SNAPSHOTTED_DATA_INDEX);

  for data in data_store.data.drain(..) {
    let data = v8::Local::new(scope, data);
    scope.add_context_data(local_context, data);
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
