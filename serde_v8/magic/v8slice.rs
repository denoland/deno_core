// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::fmt::Debug;
use std::ops::Deref;
use std::ops::DerefMut;
use std::ops::Range;
use std::rc::Rc;

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

impl Debug for V8Slice {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_fmt(format_args!(
      "V8Slice({:?} of {} bytes)",
      self.range,
      self.store.len()
    ))
  }
}

// SAFETY: unsafe trait must have unsafe implementation
unsafe impl Send for V8Slice {}

impl V8Slice {
  /// Create one of these for testing. We create and forget an isolate here. If we decide to perform more v8-requiring tests,
  /// this code will probably need to be hoisted to another location.
  #[cfg(test)]
  fn very_unsafe_new_only_for_test(byte_length: usize) -> Self {
    static V8_ONCE: std::sync::Once = std::sync::Once::new();

    V8_ONCE.call_once(|| {
      let platform = v8::new_default_platform(0, false).make_shared();
      v8::V8::initialize_platform(platform);
      v8::V8::initialize();
    });

    let mut isolate = v8::Isolate::new(Default::default());
    // SAFETY: This is not safe in any way whatsoever, but it's only for testing non-buffer functions.
    unsafe {
      let ptr = v8::ArrayBuffer::new_backing_store(&mut isolate, byte_length);
      std::mem::forget(isolate);
      Self::from_parts(ptr.into(), 0..byte_length)
    }
  }

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

  pub fn len(&self) -> usize {
    self.range.len()
  }

  pub fn is_empty(&self) -> bool {
    self.range.is_empty()
  }

  /// Create a [`Vec<u8>`] copy of this slice data.
  pub fn to_vec(&self) -> Vec<u8> {
    self.as_slice().to_vec()
  }

  /// Create a [`Box<[u8]>`] copy of this slice data.
  pub fn to_boxed_slice(&self) -> Box<[u8]> {
    self.to_vec().into_boxed_slice()
  }

  /// Returns the slice to the parts it came from.
  pub fn into_parts(self) -> (v8::SharedRef<v8::BackingStore>, Range<usize>) {
    (self.store, self.range)
  }

  /// Splits the buffer into two at the given index.
  ///
  /// Afterwards `self` contains elements `[at, len)`, and the returned `V8Slice` contains elements `[0, at)`.
  ///
  /// # Panics
  ///
  /// Panics if `at > len`.
  pub fn split_to(&mut self, at: usize) -> Self {
    let len = self.len();
    assert!(at <= len);
    let offset = self.range.start;
    let mut other = self.clone();
    self.range = offset + at..offset + len;
    other.range = offset..offset + at;
    other
  }

  /// Splits the buffer into two at the given index.
  ///
  /// Afterwards `self` contains elements `[0, at)`, and the returned `V8Slice` contains elements `[at, len)`.
  ///
  /// # Panics
  ///
  /// Panics if `at > len`.
  pub fn split_off(&mut self, at: usize) -> Self {
    let len = self.len();
    assert!(at <= len);
    let offset = self.range.start;
    let mut other = self.clone();
    self.range = offset..offset + at;
    other.range = offset + at..offset + len;
    other
  }

  /// Shortens the buffer, keeping the first `len` bytes and dropping the rest.
  ///
  /// If `len` is greater than the buffer's current length, this has no effect.
  pub fn truncate(&mut self, len: usize) {
    let offset = self.range.start;
    self.range.end = std::cmp::min(offset + len, self.range.end)
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
          Err(crate::Error::ExpectedBuffer(value.type_repr()))
        } else {
          Ok(V8Slice { store, range })
        }
      }
      Err(_) => Err(crate::Error::ExpectedBuffer(value.type_repr())),
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

#[cfg(test)]
mod tests {
  use super::V8Slice;

  #[test]
  pub fn test_split_off() {
    let mut slice = V8Slice::very_unsafe_new_only_for_test(1024);
    let mut other = slice.split_off(16);
    assert_eq!(0..16, slice.range);
    assert_eq!(16..1024, other.range);
    let other2 = other.split_off(16);
    assert_eq!(16..32, other.range);
    assert_eq!(32..1024, other2.range);
  }

  #[test]
  pub fn test_split_to() {
    let mut slice = V8Slice::very_unsafe_new_only_for_test(1024);
    let other = slice.split_to(16);
    assert_eq!(16..1024, slice.range);
    assert_eq!(0..16, other.range);
    let other2 = slice.split_to(16);
    assert_eq!(32..1024, slice.range);
    assert_eq!(16..32, other2.range);
  }

  #[test]
  pub fn test_truncate() {
    let mut slice = V8Slice::very_unsafe_new_only_for_test(1024);
    slice.truncate(16);
    assert_eq!(0..16, slice.range);
  }

  #[test]
  pub fn test_truncate_after_split() {
    let mut slice = V8Slice::very_unsafe_new_only_for_test(1024);
    _ = slice.split_to(16);
    assert_eq!(16..1024, slice.range);
    slice.truncate(16);
    assert_eq!(16..32, slice.range);
  }
}
