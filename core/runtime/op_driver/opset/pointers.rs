use std::cell::UnsafeCell;
use std::fmt::Debug;

/// Like an [`AtomicPtr`] but protected by a mutex.
#[repr(transparent)]
pub struct IndirectPtr<N>(UnsafeCell<*mut N>);

unsafe impl<N> Send for IndirectPtr<N> where N: Send {}
unsafe impl<N> Sync for IndirectPtr<N> where N: Sync {}

impl<N> IndirectPtr<N> {
  #[inline(always)]
  pub fn null() -> Self {
    std::ptr::null_mut::<N>().into()
  }

  #[inline(always)]
  pub fn is_null(&self) -> bool {
    unsafe { (*self.0.get()).is_null() }
  }
}

impl<N> Clone for IndirectPtr<N> {
  #[inline(always)]
  fn clone(&self) -> Self {
    unsafe { IndirectPtr(UnsafeCell::new(*self.0.get())) }
  }
}

impl<N> Default for IndirectPtr<N> {
  #[inline(always)]
  fn default() -> Self {
    Self::null()
  }
}

impl<N> Debug for IndirectPtr<N> {
  #[inline(always)]
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

impl<N> PartialEq for IndirectPtr<N> {
  #[inline(always)]
  fn eq(&self, other: &Self) -> bool {
    unsafe { *self.0.get() == *other.0.get() }
  }
}

impl<N> Eq for IndirectPtr<N> {}

impl<N> From<*mut N> for IndirectPtr<N> {
  #[inline(always)]
  fn from(value: *mut N) -> Self {
    Self(value.into())
  }
}

impl<N> Into<*mut N> for IndirectPtr<N> {
  #[inline(always)]
  fn into(self) -> *mut N {
    unsafe { *self.0.get() }
  }
}

/// Like a raw pointer, but [`Send`] and [`Sync`] if the underlying type is [`Send`] and [`Sync`].
pub struct Ptr<N>(*mut N);

unsafe impl<N> Sync for Ptr<N> where N: Sync {}
unsafe impl<N> Send for Ptr<N> where N: Send {}

impl<N> Ptr<N> {
  #[inline(always)]
  pub fn null() -> Self {
    std::ptr::null_mut::<N>().into()
  }

  #[inline(always)]
  pub fn is_null(&self) -> bool {
    self.0.is_null()
  }

  #[inline(always)]
  pub fn into_raw(self) -> *mut N {
    self.0
  }
}

impl<N> Copy for Ptr<N> {}
impl<N> Clone for Ptr<N> {
  #[inline(always)]
  fn clone(&self) -> Self {
    *self
  }
}

impl<N> Default for Ptr<N> {
  #[inline(always)]
  fn default() -> Self {
    Self::null()
  }
}

impl<N> Debug for Ptr<N> {
  #[inline(always)]
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

impl<N> PartialEq for Ptr<N> {
  #[inline(always)]
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}

impl<N> Eq for Ptr<N> {}

impl<N> PartialOrd for Ptr<N> {
  #[inline(always)]
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    self.0.partial_cmp(&other.0)
  }
}

impl<N> Ord for Ptr<N> {
  #[inline(always)]
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    self.0.cmp(&other.0)
  }
}

impl<N> From<*mut N> for Ptr<N> {
  #[inline(always)]
  fn from(value: *mut N) -> Self {
    Self(value)
  }
}

impl<N> Into<*mut N> for Ptr<N> {
  #[inline(always)]
  fn into(self) -> *mut N {
    self.0
  }
}

pub trait LoadStore<N> {
  fn load(self) -> Ptr<N>;
  fn store(self, ptr: Ptr<N>);
}

impl<N> LoadStore<N> for *const IndirectPtr<N> {
  #[inline(always)]
  fn load(self) -> Ptr<N> {
    unsafe { Ptr(*(*self).0.get()) }
  }

  #[inline(always)]
  fn store(self, ptr: Ptr<N>) {
    unsafe {
      *(*self).0.get() = ptr.into();
    }
  }
}
