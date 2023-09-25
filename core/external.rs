use std::{marker::PhantomData, rc::Rc, cell::RefCell};

/// Define an external type.
#[macro_export]
macro_rules! external {
  ($name:literal, $type:ident) => {
      
  };
}
pub use external;

pub trait Externalizable {
  fn marker() -> u64;
}

pub struct ExternalDefinition {
  name: &'static str,
}

#[repr(C)]
struct ExternalWithMarker<T> {
  marker: u64,
  external: RefCell<T>,
}

/// A strongly-typed external pointer.
#[repr(transparent)]
pub struct ExternalPointer<E: Externalizable> {
  ptr: *mut ExternalWithMarker<E>,
  _type: std::marker::PhantomData<E>,
}

impl <E: Externalizable> ExternalPointer<E> {
  pub fn destroy(self) {

  }
}

impl <E: Externalizable> TryFrom<*mut std::ffi::c_void> for ExternalPointer<E> {
  type Error = &'static str;

  #[inline(always)]
  fn try_from(value: *mut std::ffi::c_void) -> Result<Self, Self::Error> {
    let expected_marker = E::marker();
    // SAFETY: we assume the pointer is valid. If it is not, we risk a crash but that's
    // unfortunately not something we can easily test.
    if value.align_offset(std::mem::align_of::<u64>()) != 0 
    || unsafe { std::ptr::read::<u64>(value) } != expected_marker {
      return Err("Invalid v8::External");
    }
    Ok(Self { ptr: value as _, _type: PhantomData })
  }
}
