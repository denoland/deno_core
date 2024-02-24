// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use anyhow::Error;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use crate::modules::ModuleMapSnapshotData;
use crate::Extension;
use crate::JsRuntimeForSnapshot;
use crate::RuntimeOptions;

pub type WithRuntimeCb = dyn Fn(&mut JsRuntimeForSnapshot);

pub type SnapshotDataId = u32;

/// We use this constant a few times
const ULEN: usize = std::mem::size_of::<usize>();

/// The input snapshot source for a runtime.
pub enum Snapshot {
  /// Embedded in static data.
  Static(&'static [u8]),
  /// Freshly created, serialized as a boxed slice.
  Boxed(Box<[u8]>),
}

// TODO(bartlomieju): remove?
/// The v8 lifetime is different than the sidecar data, so we
/// allow for it to be split out.
pub(crate) enum V8StartupData {
  /// Embedded in static data.
  Static(&'static [u8]),
}

pub(crate) fn deconstruct(
  slice: &'static [u8],
) -> (V8StartupData, SerializableSnapshotSidecarData) {
  let len =
    usize::from_le_bytes(slice[slice.len() - ULEN..].try_into().unwrap());
  let data = SerializableSnapshotSidecarData::from_slice(
    &slice[len..slice.len() - ULEN],
  );
  (V8StartupData::Static(&slice[0..len]), data)
}

pub(crate) fn serialize(
  v8_data: v8::StartupData,
  sidecar_data: SerializableSnapshotSidecarData,
) -> Box<[u8]> {
  let v8_data_len = v8_data.len();
  let sidecar_data = sidecar_data.into_bytes();
  let mut data = Vec::with_capacity(v8_data_len + sidecar_data.len() + ULEN);

  // add ulen
  data.extend_from_slice(&v8_data);
  data.extend_from_slice(&sidecar_data);
  data.extend_from_slice(&v8_data_len.to_le_bytes());

  data.into_boxed_slice()
}

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
  pub startup_snapshot: Option<&'static [u8]>,
  pub skip_op_registration: bool,
  pub extensions: Vec<Extension>,
  pub with_runtime_cb: Option<Box<WithRuntimeCb>>,
}

pub struct CreateSnapshotOutput {
  /// Any files marked as LoadedFromFsDuringSnapshot are collected here and should be
  /// printed as 'cargo:rerun-if-changed' lines from your build script.
  pub files_loaded_during_snapshot: Vec<PathBuf>,
  pub output: Box<[u8]>,
}

#[must_use = "The files listed by create_snapshot should be printed as 'cargo:rerun-if-changed' lines"]
pub fn create_snapshot(
  create_snapshot_options: CreateSnapshotOptions,
  warmup_script: Option<&'static str>,
) -> Result<CreateSnapshotOutput, Error> {
  let mut mark = Instant::now();
  println!("Creating a snapshot...",);

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

  println!("JsRuntimeForSnapshot prepared, took {:#?}", mark.elapsed(),);
  mark = Instant::now();

  let files_loaded_during_snapshot = js_runtime
    .files_loaded_from_fs_during_snapshot()
    .iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();

  if let Some(ref with_runtime_cb) = create_snapshot_options.with_runtime_cb {
    with_runtime_cb(&mut js_runtime);
  }

  let mut snapshot = js_runtime.snapshot();
  if let Some(warmup_script) = warmup_script {
    let leaked_snapshot = Box::leak(snapshot);
    let warmup_exts = warmup_exts.unwrap();

    // Warm up the snapshot bootstrap.
    //
    // - Create a new isolate with cold snapshot blob.
    // - Run warmup script in new context.
    // - Serialize the new context into a new snapshot blob.
    let mut js_runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      startup_snapshot: Some(leaked_snapshot),
      extensions: warmup_exts,
      skip_op_registration: true,
      ..Default::default()
    });

    if let Some(with_runtime_cb) = create_snapshot_options.with_runtime_cb {
      with_runtime_cb(&mut js_runtime);
    }

    js_runtime.execute_script_static("warmup", warmup_script)?;

    snapshot = js_runtime.snapshot();
  }

  println!(
    "Snapshot size: {}, took {:#?}",
    snapshot.len(),
    mark.elapsed(),
  );
  mark = Instant::now();

  println!(
    "Snapshot written, took: {:#?}",
    Instant::now().saturating_duration_since(mark),
  );

  Ok(CreateSnapshotOutput {
    files_loaded_during_snapshot,
    output: snapshot,
  })
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

pub(crate) struct SnapshottedData {
  pub module_map_data: ModuleMapSnapshotData,
  pub js_handled_promise_rejection_cb: Option<v8::Global<v8::Function>>,
  pub ext_source_maps: HashMap<String, Vec<u8>>,
}

/// Snapshot sidecar data that is safe to serialize via serde.
#[derive(Serialize, Deserialize)]
pub(crate) struct SerializableSnapshotSidecarData {
  data_count: u32,
  module_map_data: ModuleMapSnapshotData,
  js_handled_promise_rejection_cb: Option<SnapshotDataId>,
  ext_source_maps: HashMap<String, Vec<u8>>,
}

impl SerializableSnapshotSidecarData {
  fn from_slice(slice: &[u8]) -> Self {
    #[cfg(all(
      feature = "snapshot_data_json",
      not(feature = "snapshot_data_bincode")
    ))]
    let raw_data: SerializableSnapshotSidecarData =
      serde_json::from_slice(slice)
        .expect("Failed to deserialize snapshot data");

    #[cfg(feature = "snapshot_data_bincode")]
    let raw_data: SerializableSnapshotSidecarData =
      bincode::deserialize(slice).expect("Failed to deserialize snapshot data");

    raw_data
  }

  fn into_bytes(self) -> Box<[u8]> {
    #[cfg(all(
      feature = "snapshot_data_json",
      not(feature = "snapshot_data_bincode")
    ))]
    let local_data = serde_json::to_vec(&self).unwrap();
    #[cfg(feature = "snapshot_data_bincode")]
    let local_data = bincode::serialize(&self).unwrap();

    local_data.into_boxed_slice()
  }
}

/// Given the sidecar data and a scope to extract data from, reconstructs the
/// `SnapshottedData` and `SnapshotLoadDataStore`.
pub(crate) fn load_snapshotted_data_from_snapshot(
  scope: &mut v8::HandleScope<()>,
  context: v8::Local<v8::Context>,
  raw_data: SerializableSnapshotSidecarData,
) -> (SnapshottedData, SnapshotLoadDataStore) {
  let scope = &mut v8::ContextScope::new(scope, context);
  let mut data = SnapshotLoadDataStore::default();
  for i in 0..raw_data.data_count {
    let item = scope
      .get_context_data_from_snapshot_once::<v8::Data>(i as usize)
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

/// Given a `SnapshottedData` and `SnapshotStoreDataStore`, attaches the data to the
/// context and returns the serialized sidecar data.
pub(crate) fn store_snapshotted_data_for_snapshot(
  scope: &mut v8::HandleScope,
  context: v8::Global<v8::Context>,
  snapshotted_data: SnapshottedData,
  mut data_store: SnapshotStoreDataStore,
) -> SerializableSnapshotSidecarData {
  let context = v8::Local::new(scope, context);
  let js_handled_promise_rejection_cb = snapshotted_data
    .js_handled_promise_rejection_cb
    .map(|v| data_store.register(v));
  let raw_snapshot_data = SerializableSnapshotSidecarData {
    data_count: data_store.data.len() as _,
    module_map_data: snapshotted_data.module_map_data,
    js_handled_promise_rejection_cb,
    ext_source_maps: snapshotted_data.ext_source_maps,
  };

  for data in data_store.data.drain(..) {
    let data = v8::Local::new(scope, data);
    scope.add_context_data(context, data);
  }

  raw_snapshot_data
}

/// Returns an isolate set up for snapshotting.
pub(crate) fn create_snapshot_creator(
  external_refs: &'static v8::ExternalReferences,
  maybe_startup_snapshot: Option<V8StartupData>,
) -> v8::OwnedIsolate {
  if let Some(snapshot) = maybe_startup_snapshot {
    match snapshot {
      V8StartupData::Static(snapshot) => {
        v8::Isolate::snapshot_creator_from_existing_snapshot(
          snapshot,
          Some(external_refs),
        )
      }
    }
  } else {
    v8::Isolate::snapshot_creator(Some(external_refs))
  }
}
