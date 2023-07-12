// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::ops::Deref;
use std::ops::DerefMut;
use std::ops::Range;
use std::rc::Rc;

use crate::error::value_to_type_str;

use super::rawbytes;
use super::transl8::FromV8;

/// A V8Slice encapsulates a slice that's been borrowed from a JavaScript
/// ArrayBuffer object. JavaScript objects can normally be garbage collected,
/// but the existence of a V8Slice inhibits this until it is dropped. It
/// behaves much like an Arc<[u8]>.
///
/// # Cloning
/// Cloning a V8Slice does not clone the contents of the buffer,
/// it creates a new reference to that buffer.
///
/// To actually clone the contents of the buffer do
/// `let copy = Vec::from(&*zero_copy_buf);`
#[derive(Clone)]
pub struct V8Slice {
  pub(crate) store: v8::SharedRef<v8::BackingStore>,
  pub(crate) range: Range<usize>,
}

// SAFETY: unsafe trait must have unsafe implementation
unsafe impl Send for V8Slice {}

impl V8Slice {
  /// Create a V8Slice from raw parts.
  ///
  /// # Safety
  ///
  /// The `range` passed to this function *must* be within the bounds of the backing store, as we may
  /// create a slice from this. The [`v8::BackingStore`] must be valid, and valid for use for the purposes
  /// of this `V8Slice` (ie: the caller must understand the repercussions of using shared/resizable
  /// buffers).
  pub unsafe fn from_parts(
    store: v8::SharedRef<v8::BackingStore>,
    range: Range<usize>,
  ) -> Self {
    Self { store, range }
  }

  fn as_slice(&self) -> &[u8] {
    let store = &self.store;
    let Some(ptr) = store.data() else {
      return &[];
    };
    let ptr = ptr.cast::<u8>().as_ptr();
    // SAFETY: v8::SharedRef<v8::BackingStore> is similar to Arc<[u8]>,
    // it points to a fixed continuous slice of bytes on the heap.
    // We assume it's initialized and thus safe to read (though may not contain
    // meaningful data).
    // Note that we are likely violating Rust's safety rules here by assuming
    // nobody is mutating this buffer elsewhere, however in practice V8Slices
    // do not have overlapping read/write phases.
    unsafe {
      let ptr = ptr.add(self.range.start);
      std::slice::from_raw_parts(ptr, self.range.len())
    }
  }

  fn as_slice_mut(&mut self) -> &mut [u8] {
    let store = &self.store;
    let Some(ptr) = store.data() else {
      return &mut [];
    };
    let ptr = ptr.cast::<u8>().as_ptr();
    // SAFETY: v8::SharedRef<v8::BackingStore> is similar to Arc<[u8]>,
    // it points to a fixed continuous slice of bytes on the heap.
    // We assume it's initialized and thus safe to read (though may not contain
    // meaningful data).
    // Note that we are likely violating Rust's safety rules here by assuming
    // nobody is mutating this buffer elsewhere, however in practice V8Slices
    // do not have overlapping read/write phases.
    unsafe {
      let ptr = ptr.add(self.range.start);
      std::slice::from_raw_parts_mut(ptr, self.range.len())
    }
  }

  /// Create a [`Vec<u8>`] copy of this slice data.
  pub fn to_vec(&self) -> Vec<u8> {
    self.as_slice().to_vec()
  }

  /// Create a [`Box<[u8]>`] copy of this slice data.
  pub fn to_boxed_slice(&self) -> Box<[u8]> {
    self.to_vec().into_boxed_slice()
  }
}

pub(crate) fn to_ranged_buffer<'s>(
  scope: &mut v8::HandleScope<'s>,
  value: v8::Local<v8::Value>,
) -> Result<(v8::Local<'s, v8::ArrayBuffer>, Range<usize>), v8::DataError> {
  if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(value) {
    let (offset, len) = (view.byte_offset(), view.byte_length());
    let buffer = view.buffer(scope).ok_or(v8::DataError::NoData {
      expected: "view to have a buffer",
    })?;
    let buffer = v8::Local::new(scope, buffer); // recreate handle to avoid lifetime issues
    return Ok((buffer, offset..offset + len));
  }
  let b: v8::Local<v8::ArrayBuffer> = value.try_into()?;
  let b = v8::Local::new(scope, b); // recreate handle to avoid lifetime issues
  Ok((b, 0..b.byte_length()))
}

impl FromV8 for V8Slice {
  fn from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Result<Self, crate::Error> {
    match to_ranged_buffer(scope, value) {
      Ok((b, range)) => {
        let store = b.get_backing_store();
        if store.is_resizable_by_user_javascript() {
          Err(crate::Error::ResizableBackingStoreNotSupported)
        } else if store.is_shared() {
          Err(crate::Error::ExpectedBuffer(value_to_type_str(value)))
        } else {
          Ok(V8Slice { store, range })
        }
      }
      Err(_) => Err(crate::Error::ExpectedBuffer(value_to_type_str(value))),
    }
  }
}

impl Deref for V8Slice {
  type Target = [u8];
  fn deref(&self) -> &[u8] {
    self.as_slice()
  }
}

impl DerefMut for V8Slice {
  fn deref_mut(&mut self) -> &mut [u8] {
    self.as_slice_mut()
  }
}

impl AsRef<[u8]> for V8Slice {
  fn as_ref(&self) -> &[u8] {
    self.as_slice()
  }
}

impl AsMut<[u8]> for V8Slice {
  fn as_mut(&mut self) -> &mut [u8] {
    self.as_slice_mut()
  }
}

// Implement V8Slice -> bytes::Bytes
impl V8Slice {
  fn rc_into_byte_parts(self: Rc<Self>) -> (*const u8, usize, *mut V8Slice) {
    let (ptr, len) = {
      let slice = self.as_ref();
      (slice.as_ptr(), slice.len())
    };
    let rc_raw = Rc::into_raw(self);
    let data = rc_raw as *mut V8Slice;
    (ptr, len, data)
  }
}

impl From<V8Slice> for bytes::Bytes {
  fn from(v8slice: V8Slice) -> Self {
    let (ptr, len, data) = Rc::new(v8slice).rc_into_byte_parts();
    rawbytes::RawBytes::new_raw(ptr, len, data.cast(), &V8SLICE_VTABLE)
  }
}

// NOTE: in the limit we could avoid extra-indirection and use the C++ shared_ptr
// but we can't store both the underlying data ptr & ctrl ptr ... so instead we
// use a shared rust ptr (Rc/Arc) that itself controls the C++ shared_ptr
const V8SLICE_VTABLE: rawbytes::Vtable = rawbytes::Vtable {
  clone: v8slice_clone,
  drop: v8slice_drop,
  to_vec: v8slice_to_vec,
};

unsafe fn v8slice_clone(
  data: &rawbytes::AtomicPtr<()>,
  ptr: *const u8,
  len: usize,
) -> bytes::Bytes {
  let rc = Rc::from_raw(*data as *const V8Slice);
  let (_, _, data) = rc.clone().rc_into_byte_parts();
  std::mem::forget(rc);
  // NOTE: `bytes::Bytes` does bounds checking so we trust its ptr, len inputs
  // and must use them to allow cloning Bytes it has sliced
  rawbytes::RawBytes::new_raw(ptr, len, data.cast(), &V8SLICE_VTABLE)
}

unsafe fn v8slice_to_vec(
  _data: &rawbytes::AtomicPtr<()>,
  ptr: *const u8,
  len: usize,
) -> Vec<u8> {
  // SAFETY: It's extremely unlikely we can convert this backing store to a vector directly, as this would require
  // the store to have been created with the appropriate allocator, and have no other references (either
  // in JS or in Rust). We instead just create a copy here, using the raw buffer/length.
  let vec = unsafe { std::slice::from_raw_parts(ptr, len).to_vec() };
  vec
}

unsafe fn v8slice_drop(
  data: &mut rawbytes::AtomicPtr<()>,
  _: *const u8,
  _: usize,
) {
  drop(Rc::from_raw(*data as *const V8Slice))
}
