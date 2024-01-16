// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::Resource;
use super::ResourceHandle;
use super::ResourceHandleFd;
use super::ResourceHandleSocket;
use super::ResourceObject;
use crate::RcLike;
use crate::RcRef;
use crate::error::bad_resource_id;
use crate::error::custom_error;
use anyhow::Error;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::iter::Iterator;
use std::ops::Deref;
use std::rc::Rc;

/// A `ResourceId` is an integer value referencing a resource. It could be
/// considered to be the Deno equivalent of a `file descriptor` in POSIX like
/// operating systems. Elsewhere in the code base it is commonly abbreviated
/// to `rid`.
// TODO: use `u64` instead?
pub type ResourceId = u32;

/// Map-like data structure storing Deno's resources (equivalent to file
/// descriptors).
///
/// Provides basic methods for element access. A resource can be of any type.
/// Different types of resources can be stored in the same map, and provided
/// with a name for description.
///
/// Each resource is identified through a _resource ID (rid)_, which acts as
/// the key in the map.
#[derive(Default)]
pub struct ResourceTable {
  index: BTreeMap<ResourceId, Rc<ResourceObject<dyn Resource>>>,
  next_rid: ResourceId,
}

impl ResourceTable {
  /// Inserts resource into the resource table, which takes ownership of it.
  ///
  /// The resource type is erased at runtime and must be statically known
  /// when retrieving it through `get()`.
  ///
  /// Returns a unique resource ID, which acts as a key for this resource.
  pub fn add<T: Resource>(&mut self, resource: T) -> ResourceId {
    self.add_rc(Rc::new(ResourceObject::new(resource)))
  }

  /// Inserts a `Rc`-wrapped resource into the resource table.
  ///
  /// The resource type is erased at runtime and must be statically known
  /// when retrieving it through `get()`.
  ///
  /// Returns a unique resource ID, which acts as a key for this resource.
  pub fn add_rc<T: Resource>(
    &mut self,
    resource: Rc<ResourceObject<T>>,
  ) -> ResourceId {
    self.add_rc_dyn(resource.as_rc_dyn())
  }

  pub fn add_rc_dyn(
    &mut self,
    resource: Rc<ResourceObject<dyn Resource>>,
  ) -> ResourceId {
    let rid = self.next_rid;
    let removed_resource = self.index.insert(rid, resource);
    assert!(removed_resource.is_none());
    self.next_rid += 1;
    rid
  }

  /// Returns true if any resource with the given `rid` exists.
  pub fn has(&self, rid: ResourceId) -> bool {
    self.index.contains_key(&rid)
  }

  /// Returns a reference counted pointer to the resource of type `T` with the
  /// given `rid`. If `rid` is not present or has a type different than `T`,
  /// this function returns `None`.
  pub fn get<T: Resource>(
    &self,
    rid: ResourceId,
  ) -> Result<Rc<ResourceObject<T>>, Error> {
    self
      .index
      .get(&rid)
      .and_then(|rc| rc.downcast_rc::<T>())
      .map(Clone::clone)
      .ok_or_else(bad_resource_id)
  }

  /// Returns a reference counted pointer to the resource of type `T` with the
  /// given `rid`. If `rid` is not present or has a type different than `T`,
  /// this function returns `None`.
  pub fn get_inner<T: Resource>(
    &self,
    rid: ResourceId,
  ) -> Result<impl RcLike<T>, Error> {
    let rc = self
      .index
      .get(&rid)
      .and_then(|rc| rc.downcast_rc::<T>())
      .map(Clone::clone)
      .ok_or_else(bad_resource_id)?;
    Ok(RcRef::map( &rc, |rc| rc.deref()))
  }

  pub fn get_any(
    &self,
    rid: ResourceId,
  ) -> Result<Rc<ResourceObject<dyn Resource>>, Error> {
    self
      .index
      .get(&rid)
      .map(Clone::clone)
      .ok_or_else(bad_resource_id)
  }

  /// Replaces a resource with a new resource.
  ///
  /// Panics if the resource does not exist.
  pub fn replace<T: Resource>(&mut self, rid: ResourceId, resource: T) {
    let result = self
      .index
      .insert(rid, Rc::new(ResourceObject::new(resource)).as_rc_dyn());
    assert!(result.is_some());
  }

  /// Removes a resource of type `T` from the resource table and returns it.
  /// If a resource with the given `rid` exists but its type does not match `T`,
  /// it is not removed from the resource table. Note that the resource's
  /// `close()` method is *not* called.
  ///
  /// Also note that there might be a case where
  /// the returned `Rc<T>` is referenced by other variables. That is, we cannot
  /// assume that `Rc::strong_count(&returned_rc)` is always equal to 1 on success.
  /// In particular, be really careful when you want to extract the inner value of
  /// type `T` from `Rc<T>`.
  pub fn take<T: Resource>(
    &mut self,
    rid: ResourceId,
  ) -> Result<Rc<ResourceObject<T>>, Error> {
    let resource = self.get::<T>(rid)?;
    self.index.remove(&rid);
    Ok(resource)
  }

  /// Removes a resource from the resource table and returns it. Note that the
  /// resource's `close()` method is *not* called.
  ///
  /// Also note that there might be a
  /// case where the returned `Rc<T>` is referenced by other variables. That is,
  /// we cannot assume that `Rc::strong_count(&returned_rc)` is always equal to 1
  /// on success. In particular, be really careful when you want to extract the
  /// inner value of type `T` from `Rc<T>`.
  pub fn take_any(
    &mut self,
    rid: ResourceId,
  ) -> Result<Rc<ResourceObject<dyn Resource>>, Error> {
    self.index.remove(&rid).ok_or_else(bad_resource_id)
  }
  /// Returns an iterator that yields a `(id, name)` pair for every resource
  /// that's currently in the resource table. This can be used for debugging
  /// purposes or to implement the `op_resources` op. Note that the order in
  /// which items appear is not specified.
  ///
  /// # Example
  ///
  /// ```
  /// # use deno_core::ResourceTable;
  /// # let resource_table = ResourceTable::default();
  /// let resource_names = resource_table.names().collect::<Vec<_>>();
  /// ```
  pub fn names(&self) -> impl Iterator<Item = (ResourceId, Cow<str>)> {
    self
      .index
      .iter()
      .map(|(&id, resource)| (id, resource.name()))
  }

  /// Retrieves the [`ResourceHandleFd`] for a given resource, for potential optimization
  /// purposes within ops.
  pub fn get_fd(&self, rid: ResourceId) -> Result<ResourceHandleFd, Error> {
    let Some(handle) = self.get_any(rid)?.backing_handle() else {
      return Err(bad_resource_id());
    };
    let Some(fd) = handle.as_fd_like() else {
      return Err(bad_resource_id());
    };
    if !handle.is_valid() {
      return Err(custom_error("ReferenceError", "null or invalid handle"));
    }
    Ok(fd)
  }

  /// Retrieves the [`ResourceHandleSocket`] for a given resource, for potential optimization
  /// purposes within ops.
  pub fn get_socket(
    &self,
    rid: ResourceId,
  ) -> Result<ResourceHandleSocket, Error> {
    let Some(handle) = self.get_any(rid)?.backing_handle() else {
      return Err(bad_resource_id());
    };
    let Some(socket) = handle.as_socket_like() else {
      return Err(bad_resource_id());
    };
    if !handle.is_valid() {
      return Err(custom_error("ReferenceError", "null or invalid handle"));
    }
    Ok(socket)
  }

  /// Retrieves the [`ResourceHandle`] for a given resource, for potential optimization
  /// purposes within ops.
  pub fn get_handle(&self, rid: ResourceId) -> Result<ResourceHandle, Error> {
    let Some(handle) = self.get_any(rid)?.backing_handle() else {
      return Err(bad_resource_id());
    };
    if !handle.is_valid() {
      return Err(custom_error("ReferenceError", "null or invalid handle"));
    }
    Ok(handle)
  }
}
