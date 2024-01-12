// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::ResourceHandle;
use crate::error::not_supported;
use crate::io::BufMutView;
use crate::io::BufView;
use crate::AsyncResult;
use anyhow::Error;
use futures::Future;
use tokio::io::AsyncRead;
use std::any::type_name;
use std::any::Any;
use std::any::TypeId;
use std::borrow::Cow;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::task::ready;
use tokio::io::ReadBuf;

/// Resources are Rust objects that are attached to a [deno_core::JsRuntime].
/// They are identified in JS by a numeric ID (the resource ID, or rid).
/// Resources can be created in ops. Resources can also be retrieved in ops by
/// their rid. Resources are not thread-safe - they can only be accessed from
/// the thread that the JsRuntime lives on.
///
/// Resources are reference counted in Rust. This means that they can be
/// cloned and passed around. When the last reference is dropped, the resource
/// is automatically closed. As long as the resource exists in the resource
/// table, the reference count is at least 1.
///
/// ### Readable
/// 
/// 
/// ### Writable
///
/// Writable resources are resources that can have data written to. Examples of
/// this are files, sockets, or HTTP streams.
///
/// Writables can be written to from either JS or Rust. In JS one can use
/// `Deno.core.write()` to write to a single chunk of data to a writable. In
/// Rust one can directly call `write()`. The latter is used to implement ops
/// like `op_slice`.
pub trait Resource: Any + 'static {
  /// Returns a string representation of the resource which is made available
  /// to JavaScript code through `op_resources`. The default implementation
  /// returns the Rust type name, but specific resource types may override this
  /// trait method.
  fn name(&self) -> Cow<str> {
    type_name::<Self>().into()
  }

  /// Poll this resource for a read operation. Only one read operation will be issued on this resource
  /// at a time, and should this resource return a future, no read operations will be performed until
  /// that future resolves.
  fn poll_read<'a, 'b>(
    &self,
    cx: &mut Context,
    read_context: &'a ReadContext,
    preferred_buffer: &mut ReadBuf<'b>,
  ) -> Poll<ReadResult<'a>> {
    Poll::Ready(ReadResult::Err(not_supported()))
  }

  /// Poll this resource for a write operation. Only one write operation will be issued on this resource
  /// at a time, and should this resource return a future, no write operations will be performed until
  /// that future resolves.
  fn poll_write(
    &self,
    cx: &mut Context,
    write_context: &WriteContext,
    buffer: BufView,
  ) -> Poll<WriteResult> {
    Poll::Ready(WriteResult::ReadySync(Err(not_supported())))
  }

  /// The shutdown method can be used to asynchronously close the resource. It
  /// is not automatically called when the resource is dropped or closed.
  ///
  /// If this method is not implemented, the default implementation will error
  /// with a "not supported" error.
  fn shutdown(&self) -> AsyncResult<()> {
    Box::pin(futures::future::err(not_supported()))
  }

  /// Resources may implement the `close()` trait method if they need to do
  /// resource specific clean-ups, such as cancelling pending futures, after a
  /// resource has been removed from the resource table.
  fn close(self)
  where
    Self: Sized,
  {
  }

  /// Resources backed by a file descriptor or socket handle can let ops know
  /// to allow for low-level optimizations. If a backing handle is provided, the
  /// read and write methods are skipped.
  fn backing_handle(&self) -> Option<ResourceHandle> {
    None
  }

  fn size_hint(&self) -> (u64, Option<u64>) {
    (0, None)
  }
}

pub enum ReadResult<'a> {
  /// The read operation returned an error.
  Err(Error),
  /// The read operation could not complete, but is requesting a re-poll at a later time
  /// (potentially even right away). This is useful in certain cases where there may be internal
  /// buffering and it is simply easier to retry to read.
  PollAgain,
  /// The stream is at the end-of-file and is therefore complete. It is an error to
  /// return an empty result via the `Ready` enumeration value to prevent coding errors.
  EOF,
  Ready,
  ReadyBufMut(BufMutView),
  ReadyBuf(BufView),
  /// The read result cannot complete at this time and would prefer to wrap its state up in
  /// an `async` block. Note that it is possible, though not recommended, for this future to
  /// return another future from itself.
  Future(OpaqueReadFuture<'a>),
}

pub enum WriteResult {
  ReadySync(Result<usize, Error>),
  Future(OpaqueWriteFuture),
}

pub struct OpaqueReadFuture<'a> {
  f: Pin<Box<dyn Future<Output = ReadResult<'a>>>>
}

impl <'a> OpaqueReadFuture<'a> {
  pub(crate) fn poll_read<'b>(
    &self,
    cx: &mut Context,
    read_context: &'a ReadContext,
    preferred_buffer: &mut ReadBuf<'b>,
  ) -> Poll<ReadResult<'a>> {
    Poll::Ready(ReadResult::Err(not_supported()))
  }
}

pub struct OpaqueWriteFuture {}

impl OpaqueWriteFuture {}

pub struct ReadContext {
}

impl ReadContext {
  pub fn read_future<'a, F: Future<Output = ReadResult<'a>> + 'a>(&'a self, f: impl (FnOnce(ReadBufHolder<'a>) -> F) + 'a) -> Poll<ReadResult<'a>> {
    unimplemented!()
  }

  pub(crate) fn x<'a>(&'a self) -> Option<OpaqueReadFuture<'a>> {
    unimplemented!()
  }

  pub(crate) fn y<'a>(&'a self, buf: &'a mut [u8]) -> ReadBuf {
    ReadBuf::new(buf)
  }
}

pub struct ReadBufHolder<'a> {
  buffer: ReadBuf<'a>,
}

impl <'a> ReadBufHolder<'a> {
  /// Use this buffer for some other purpose.
  pub(crate) fn with_buf<T>(&mut self, f: impl Fn(&mut ReadBuf<'a>) -> T) -> T {
    f(&mut self.buffer)
  } 

  /// Use this buffer holder to poll a reader.
  pub(crate) fn poll_reader<R: AsyncRead + Unpin>(&mut self, cx: &mut Context, r: &mut R) -> Poll<std::io::Result<usize>>{
    Poll::Ready(ready!(Pin::new(r).poll_read(cx, &mut self.buffer)).map(|_| self.buffer.filled().len()))
  }
}

pub struct WriteContext {}

impl dyn Resource {
  #[inline(always)]
  pub(crate) fn is<T: Resource>(&self) -> bool {
    self.type_id() == TypeId::of::<T>()
  }
}
