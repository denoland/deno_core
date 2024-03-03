// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
pub(crate) type AtomicPtr<T> = *mut T;
#[allow(unused)]
pub(crate) struct RawBytes {
  ptr: *const u8,
  len: usize,
  // inlined "trait object"
  data: AtomicPtr<()>,
  vtable: &'static Vtable,
}

impl RawBytes {
  pub fn new_raw(
    ptr: *const u8,
    len: usize,
    data: AtomicPtr<()>,
    vtable: &'static Vtable,
  ) -> bytes::Bytes {
    RawBytes {
      ptr,
      len,
      data,
      vtable,
    }
    .into()
  }
}

impl Drop for RawBytes {
  #[inline]
  fn drop(&mut self) {
    unsafe { (self.vtable.drop)(&mut self.data, self.ptr, self.len) }
  }
}

// Validate some bytes::Bytes layout assumptions at compile time.
const _: () = {
  assert!(
    core::mem::size_of::<RawBytes>() == core::mem::size_of::<bytes::Bytes>(),
  );
  assert!(
    core::mem::align_of::<RawBytes>() == core::mem::align_of::<bytes::Bytes>(),
  );
};

#[allow(unused)]
pub(crate) struct Vtable {
  /// fn(data, ptr, len)
  pub clone: unsafe fn(&AtomicPtr<()>, *const u8, usize) -> bytes::Bytes,
  /// fn(data, ptr, len)
  ///
  /// takes `Bytes` to value
  pub to_vec: unsafe fn(&AtomicPtr<()>, *const u8, usize) -> Vec<u8>,
  /// fn(data, ptr, len)
  pub drop: unsafe fn(&mut AtomicPtr<()>, *const u8, usize),
}

impl From<RawBytes> for bytes::Bytes {
  fn from(b: RawBytes) -> Self {
    // SAFETY: RawBytes has the same layout as bytes::Bytes
    // this is tested below, both are composed of usize-d ptrs/values
    // thus aren't currently subject to rust's field re-ordering to minimize padding
    unsafe { std::mem::transmute(b) }
  }
}

impl From<bytes::Bytes> for RawBytes {
  fn from(b: bytes::Bytes) -> Self {
    // SAFETY: RawBytes has the same layout as bytes::Bytes
    // this is tested below, both are composed of usize-d ptrs/values
    // thus aren't currently subject to rust's field re-ordering to minimize padding
    unsafe { std::mem::transmute(b) }
  }
}

const ARC_U8_VTABLE: Vtable = Vtable {
  clone: arc_u8_clone,
  drop: arc_u8_drop,
  to_vec: arc_u8_to_vec,
};

unsafe fn arc_u8_clone(
  data: &AtomicPtr<()>,
  ptr: *const u8,
  len: usize,
) -> bytes::Bytes {
  let boxed_arc: Box<std::sync::Arc<[u8]>> = Box::from_raw((*data) as _);
  let arc = (*boxed_arc).clone();
  std::mem::forget(boxed_arc);
  let data = Box::into_raw(Box::new(arc));
  RawBytes {
    ptr,
    len,
    data: data as _,
    vtable: &ARC_U8_VTABLE,
  }
  .into()
}

unsafe fn arc_u8_to_vec(
  _: &AtomicPtr<()>,
  ptr: *const u8,
  len: usize,
) -> Vec<u8> {
  std::slice::from_raw_parts(ptr, len).to_vec()
}

unsafe fn arc_u8_drop(data: &mut AtomicPtr<()>, _: *const u8, _: usize) {
  let boxed_arc: Box<std::sync::Arc<[u8]>> = Box::from_raw((*data) as _);
  drop(boxed_arc);
}

impl From<std::sync::Arc<[u8]>> for RawBytes {
  fn from(b: std::sync::Arc<[u8]>) -> Self {
    let len = b.len();
    let ptr = b.as_ref().as_ptr();
    let data = Box::into_raw(Box::new(b));
    RawBytes {
      ptr,
      len,
      data: data as _,
      vtable: &ARC_U8_VTABLE,
    }
  }
}

#[cfg(test)]
mod tests {
  use bytes::Bytes;

  use super::*;
  use std::mem;
  use std::sync::Arc;

  const HELLO: &str = "hello";

  // ===== impl StaticVtable =====

  const STATIC_VTABLE: Vtable = Vtable {
    clone: static_clone,
    drop: static_drop,
    to_vec: static_to_vec,
  };

  unsafe fn static_clone(
    _: &AtomicPtr<()>,
    ptr: *const u8,
    len: usize,
  ) -> bytes::Bytes {
    from_static(std::slice::from_raw_parts(ptr, len)).into()
  }

  unsafe fn static_to_vec(
    _: &AtomicPtr<()>,
    ptr: *const u8,
    len: usize,
  ) -> Vec<u8> {
    let slice = std::slice::from_raw_parts(ptr, len);
    slice.to_vec()
  }

  unsafe fn static_drop(_: &mut AtomicPtr<()>, _: *const u8, _: usize) {
    // nothing to drop for &'static [u8]
  }

  fn from_static(bytes: &'static [u8]) -> RawBytes {
    RawBytes {
      ptr: bytes.as_ptr(),
      len: bytes.len(),
      data: std::ptr::null_mut(),
      vtable: &STATIC_VTABLE,
    }
  }

  #[test]
  fn bytes_identity() {
    let b1: bytes::Bytes = from_static(HELLO.as_bytes()).into();
    let b2 = bytes::Bytes::from_static(HELLO.as_bytes());
    assert_eq!(b1, b2); // Values are equal
  }

  #[test]
  fn bytes_layout() {
    let u1: [usize; 4] =
      // SAFETY: ensuring layout is the same
      unsafe { mem::transmute(from_static(HELLO.as_bytes())) };
    let u2: [usize; 4] =
      // SAFETY: ensuring layout is the same
      unsafe { mem::transmute(bytes::Bytes::from_static(HELLO.as_bytes())) };

    // Assert that only one field varies between Bytes and our fake Bytes (the vtable field). We're doing something
    // that's somewhat dangerous here, but if this test passes we can guarantee that Rust laid out the fields between
    // these structs in the same way.
    let mut diffs = 0;
    for i in 0..4 {
      if u1[i] != u2[i] {
        diffs += 1;
      }
    }
    assert_eq!(diffs, 1);
  }

  #[test]
  fn bytes_drop() {
    let bytes: Arc<[u8]> = Arc::new([1, 2, 3]);
    assert_eq!(1, Arc::strong_count(&bytes));
    let clone = bytes.clone();
    assert_eq!(2, Arc::strong_count(&bytes));
    let raw_bytes = RawBytes::from(bytes);
    assert_eq!(2, Arc::strong_count(&clone));
    drop(raw_bytes);
    assert_eq!(1, Arc::strong_count(&clone));
  }

  #[test]
  fn bytes_clone_and_drop() {
    let bytes: Arc<[u8]> = Arc::new([1, 2, 3]);
    assert_eq!(1, Arc::strong_count(&bytes));
    let clone = bytes.clone();
    assert_eq!(2, Arc::strong_count(&bytes));
    let raw_bytes = Bytes::from(RawBytes::from(bytes));
    let raw_bytes2 = raw_bytes.clone();
    assert_eq!(3, Arc::strong_count(&clone));
    drop(raw_bytes);
    assert_eq!(2, Arc::strong_count(&clone));
    drop(raw_bytes2);
    assert_eq!(1, Arc::strong_count(&clone));
  }
}
