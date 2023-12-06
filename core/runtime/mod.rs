// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
pub(crate) mod bindings;
pub(crate) mod exception_state;
mod jsrealm;
mod jsruntime;
#[doc(hidden)]
pub mod ops;
pub mod ops_rust_to_v8;
mod snapshot_util;

#[cfg(all(test, not(miri)))]
mod tests;

pub const V8_WRAPPER_TYPE_INDEX: i32 = 0;
pub const V8_WRAPPER_OBJECT_INDEX: i32 = 1;

pub(crate) use jsrealm::ContextState;
pub(crate) use jsrealm::JsRealm;
pub use jsruntime::CompiledWasmModuleStore;
pub use jsruntime::CreateRealmOptions;
pub use jsruntime::CrossIsolateStore;
pub(crate) use jsruntime::InitMode;
pub use jsruntime::JsRuntime;
pub use jsruntime::JsRuntimeForSnapshot;
pub use jsruntime::JsRuntimeState;
pub use jsruntime::PollEventLoopOptions;
pub use jsruntime::RuntimeOptions;
pub use jsruntime::SharedArrayBufferStore;
pub use jsruntime::Snapshot;
pub use snapshot_util::create_snapshot;
pub use snapshot_util::get_js_files;
pub use snapshot_util::CreateSnapshotOptions;
pub use snapshot_util::CreateSnapshotOutput;
pub use snapshot_util::FilterFn;
pub(crate) use snapshot_util::SnapshottedData;

pub use bindings::script_origin;
