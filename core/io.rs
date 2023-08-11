// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use bytes::Buf;
use bytes::BytesMut;
use serde_v8::JsBuffer;
use serde_v8::V8Slice;

use anyhow::Error;
use futures::Future;
use futures::Stream;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ops::DerefMut;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;

use crate::io_adapters::DenoChannelBytesWriteResource;
use crate::io_adapters::DenoResourceStreamResource;
use crate::io_adapters::DenoStreamBytesReadResource;
use crate::io_adapters::DenoTokioAsyncReadResource;
use crate::AsyncRefCell;
use crate::RcRef;
use crate::Resource;
use crate::ResourceStreamRead;
use crate::ResourceStreamWrite;
use crate::io_adapters::DenoTokioAsyncReadWriteResource;

/// BufView is a wrapper around an underlying contiguous chunk of bytes. It can
/// be created from a [JsBuffer], [bytes::Bytes], or [Vec<u8>] and implements
/// `Deref<[u8]>` and `AsRef<[u8]>`.
///
/// The wrapper has the ability to constrain the exposed view to a sub-region of
/// the underlying buffer. This is useful for write operations, because they may
/// have to be called multiple times, with different views onto the buffer to be
/// able to write it entirely.
#[derive(Debug)]
pub struct BufView {
  inner: BufViewInner,
  cursor: usize,
}

#[derive(Debug)]
enum BufViewInner {
  Empty,
  Bytes(bytes::Bytes),
  JsBuffer(V8Slice),
}

impl BufView {
  const fn from_inner(inner: BufViewInner) -> Self {
    Self { inner, cursor: 0 }
  }

  pub const fn empty() -> Self {
    Self::from_inner(BufViewInner::Empty)
  }

  /// Get the length of the buffer view. This is the length of the underlying
  /// buffer minus the cursor position.
  pub fn len(&self) -> usize {
    match &self.inner {
      BufViewInner::Empty => 0,
      BufViewInner::Bytes(bytes) => bytes.len() - self.cursor,
      BufViewInner::JsBuffer(js_buf) => js_buf.len() - self.cursor,
    }
  }

  /// Is the buffer view empty?
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Advance the internal cursor of the buffer view by `n` bytes.
  pub fn advance_cursor(&mut self, n: usize) {
    assert!(self.len() >= n);
    self.cursor += n;
  }

  /// Reset the internal cursor of the buffer view to the beginning of the
  /// buffer. Returns the old cursor position.
  pub fn reset_cursor(&mut self) -> usize {
    let old = self.cursor;
    self.cursor = 0;
    old
  }

  /// Adjust the length of the remaining buffer. If the requested size is greater than the current
  /// length, no changes are made.
  pub fn truncate(&mut self, size: usize) {
    match &mut self.inner {
      BufViewInner::Empty => {}
      BufViewInner::Bytes(bytes) => bytes.truncate(size + self.cursor),
      BufViewInner::JsBuffer(buffer) => buffer.truncate(size + self.cursor),
    }
  }

  /// Split the underlying buffer. The other piece will maintain the current cursor position while this buffer
  /// will have a cursor of zero.
  pub fn split_off(&mut self, at: usize) -> Self {
    let at = at + self.cursor;
    assert!(at <= self.len());
    let other = match &mut self.inner {
      BufViewInner::Empty => BufViewInner::Empty,
      BufViewInner::Bytes(bytes) => BufViewInner::Bytes(bytes.split_off(at)),
      BufViewInner::JsBuffer(buffer) => {
        BufViewInner::JsBuffer(buffer.split_off(at))
      }
    };
    Self {
      inner: other,
      cursor: 0,
    }
  }

  /// Split the underlying buffer. The other piece will have a cursor of zero while this buffer
  /// will maintain the current cursor position.
  pub fn split_to(&mut self, at: usize) -> Self {
    assert!(at <= self.len());
    let at = at + self.cursor;
    let other = match &mut self.inner {
      BufViewInner::Empty => BufViewInner::Empty,
      BufViewInner::Bytes(bytes) => BufViewInner::Bytes(bytes.split_to(at)),
      BufViewInner::JsBuffer(buffer) => {
        BufViewInner::JsBuffer(buffer.split_to(at))
      }
    };
    let cursor = std::mem::take(&mut self.cursor);
    Self {
      inner: other,
      cursor,
    }
  }
}

impl Buf for BufView {
  fn remaining(&self) -> usize {
    self.len()
  }

  fn chunk(&self) -> &[u8] {
    self.deref()
  }

  fn advance(&mut self, cnt: usize) {
    self.advance_cursor(cnt)
  }
}

impl Deref for BufView {
  type Target = [u8];

  fn deref(&self) -> &[u8] {
    let buf = match &self.inner {
      BufViewInner::Empty => &[],
      BufViewInner::Bytes(bytes) => bytes.deref(),
      BufViewInner::JsBuffer(js_buf) => js_buf.deref(),
    };
    &buf[self.cursor..]
  }
}

impl AsRef<[u8]> for BufView {
  fn as_ref(&self) -> &[u8] {
    self.deref()
  }
}

impl From<JsBuffer> for BufView {
  fn from(buf: JsBuffer) -> Self {
    Self::from_inner(BufViewInner::JsBuffer(buf.into_parts()))
  }
}

impl From<Vec<u8>> for BufView {
  fn from(vec: Vec<u8>) -> Self {
    Self::from_inner(BufViewInner::Bytes(vec.into()))
  }
}

impl From<bytes::Bytes> for BufView {
  fn from(buf: bytes::Bytes) -> Self {
    Self::from_inner(BufViewInner::Bytes(buf))
  }
}

impl From<BufView> for bytes::Bytes {
  fn from(buf: BufView) -> Self {
    match buf.inner {
      BufViewInner::Empty => bytes::Bytes::new(),
      BufViewInner::Bytes(bytes) => bytes,
      BufViewInner::JsBuffer(js_buf) => js_buf.into(),
    }
  }
}

/// BufMutView is a wrapper around an underlying contiguous chunk of writable
/// bytes. It can be created from a `JsBuffer` or a `Vec<u8>` and implements
/// `DerefMut<[u8]>` and `AsMut<[u8]>`.
///
/// The wrapper has the ability to constrain the exposed view to a sub-region of
/// the underlying buffer. This is useful for write operations, because they may
/// have to be called multiple times, with different views onto the buffer to be
/// able to write it entirely.
///
/// A `BufMutView` can be turned into a `BufView` by calling `BufMutView::into_view`.
#[derive(Debug)]
pub struct BufMutView {
  inner: BufMutViewInner,
  cursor: usize,
}

#[derive(Debug)]
enum BufMutViewInner {
  JsBuffer(V8Slice),
  Bytes(BytesMut),
}

impl BufMutView {
  fn from_inner(inner: BufMutViewInner) -> Self {
    Self { inner, cursor: 0 }
  }

  pub fn new(len: usize) -> Self {
    let bytes = BytesMut::zeroed(len);
    Self::from_inner(BufMutViewInner::Bytes(bytes))
  }

  /// Get the length of the buffer view. This is the length of the underlying
  /// buffer minus the cursor position.
  pub fn len(&self) -> usize {
    match &self.inner {
      BufMutViewInner::JsBuffer(js_buf) => js_buf.len() - self.cursor,
      BufMutViewInner::Bytes(bytes) => bytes.len() - self.cursor,
    }
  }

  /// Is the buffer view empty?
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Advance the internal cursor of the buffer view by `n` bytes.
  pub fn advance_cursor(&mut self, n: usize) {
    assert!(self.len() >= n);
    self.cursor += n;
  }

  /// Reset the internal cursor of the buffer view to the beginning of the
  /// buffer. Returns the old cursor position.
  pub fn reset_cursor(&mut self) -> usize {
    let old = self.cursor;
    self.cursor = 0;
    old
  }

  /// Turn this `BufMutView` into a `BufView`.
  pub fn into_view(self) -> BufView {
    let inner = match self.inner {
      BufMutViewInner::JsBuffer(js_buf) => BufViewInner::JsBuffer(js_buf),
      BufMutViewInner::Bytes(bytes) => BufViewInner::Bytes(bytes.into()),
    };
    BufView {
      inner,
      cursor: self.cursor,
    }
  }

  /// Attempts to unwrap the underlying buffer into a [`BytesMut`], consuming the `BufMutView`. If
  /// this buffer does not have a [`BytesMut`], returns `Self`.
  pub fn maybe_unwrap_bytes(self) -> Result<BytesMut, Self> {
    match self.inner {
      BufMutViewInner::JsBuffer(_) => Err(self),
      BufMutViewInner::Bytes(bytes) => Ok(bytes),
    }
  }

  /// This attempts to grow the `BufMutView` to a target size, by a maximum increment. This method
  /// will be replaced by a better API in the future and should not be used at this time.
  #[must_use = "The result of this method should be tested"]
  #[deprecated = "API will be replaced in the future"]
  #[doc(hidden)]
  pub fn maybe_resize(
    &mut self,
    target_size: usize,
    maximum_increment: usize,
  ) -> Option<usize> {
    if let BufMutViewInner::Bytes(bytes) = &mut self.inner {
      use std::cmp::Ordering::*;
      let len = bytes.len();
      let target_size = target_size + self.cursor;
      match target_size.cmp(&len) {
        Greater => {
          bytes.resize(std::cmp::min(target_size, len + maximum_increment), 0);
        }
        Less => {
          bytes.truncate(target_size);
        }
        Equal => {}
      }
      Some(bytes.len())
    } else {
      None
    }
  }

  /// This attempts to grow the `BufMutView` to a target size, by a maximum increment. This method
  /// will be replaced by a better API in the future and should not be used at this time.
  #[must_use = "The result of this method should be tested"]
  #[deprecated = "API will be replaced in the future"]
  #[doc(hidden)]
  pub fn maybe_grow(&mut self, target_size: usize) -> Option<usize> {
    if let BufMutViewInner::Bytes(bytes) = &mut self.inner {
      let len = bytes.len();
      let target_size = target_size + self.cursor;
      if target_size > len {
        bytes.resize(target_size, 0);
      }
      Some(bytes.len())
    } else {
      None
    }
  }

  /// Adjust the length of the remaining buffer and ensure that the cursor continues to
  /// stay in-bounds.
  pub fn truncate(&mut self, size: usize) {
    match &mut self.inner {
      BufMutViewInner::Bytes(bytes) => bytes.truncate(size + self.cursor),
      BufMutViewInner::JsBuffer(buffer) => buffer.truncate(size + self.cursor),
    }
    self.cursor = std::cmp::min(self.cursor, self.len());
  }

  /// Split the underlying buffer. The other piece will maintain the current cursor position while this buffer
  /// will have a cursor of zero.
  pub fn split_off(&mut self, at: usize) -> Self {
    let at = at + self.cursor;
    assert!(at <= self.len());
    let other = match &mut self.inner {
      BufMutViewInner::Bytes(bytes) => {
        BufMutViewInner::Bytes(bytes.split_off(at))
      }
      BufMutViewInner::JsBuffer(buffer) => {
        BufMutViewInner::JsBuffer(buffer.split_off(at))
      }
    };
    Self {
      inner: other,
      cursor: 0,
    }
  }

  /// Split the underlying buffer. The other piece will have a cursor of zero while this buffer
  /// will maintain the current cursor position.
  pub fn split_to(&mut self, at: usize) -> Self {
    assert!(at <= self.len());
    let at = at + self.cursor;
    let other = match &mut self.inner {
      BufMutViewInner::Bytes(bytes) => {
        BufMutViewInner::Bytes(bytes.split_to(at))
      }
      BufMutViewInner::JsBuffer(buffer) => {
        BufMutViewInner::JsBuffer(buffer.split_to(at))
      }
    };
    let cursor = std::mem::take(&mut self.cursor);
    Self {
      inner: other,
      cursor,
    }
  }
}

impl Buf for BufMutView {
  fn remaining(&self) -> usize {
    self.len()
  }

  fn chunk(&self) -> &[u8] {
    self.deref()
  }

  fn advance(&mut self, cnt: usize) {
    self.advance_cursor(cnt)
  }
}

impl Deref for BufMutView {
  type Target = [u8];

  fn deref(&self) -> &[u8] {
    let buf = match &self.inner {
      BufMutViewInner::JsBuffer(js_buf) => js_buf.deref(),
      BufMutViewInner::Bytes(vec) => vec.deref(),
    };
    &buf[self.cursor..]
  }
}

impl DerefMut for BufMutView {
  fn deref_mut(&mut self) -> &mut [u8] {
    let buf = match &mut self.inner {
      BufMutViewInner::JsBuffer(js_buf) => js_buf.deref_mut(),
      BufMutViewInner::Bytes(vec) => vec.deref_mut(),
    };
    &mut buf[self.cursor..]
  }
}

impl AsRef<[u8]> for BufMutView {
  fn as_ref(&self) -> &[u8] {
    self.deref()
  }
}

impl AsMut<[u8]> for BufMutView {
  fn as_mut(&mut self) -> &mut [u8] {
    self.deref_mut()
  }
}

impl From<JsBuffer> for BufMutView {
  fn from(buf: JsBuffer) -> Self {
    Self::from_inner(BufMutViewInner::JsBuffer(buf.into_parts()))
  }
}

impl From<BytesMut> for BufMutView {
  fn from(buf: BytesMut) -> Self {
    Self::from_inner(BufMutViewInner::Bytes(buf))
  }
}

pub enum WriteOutcome {
  Partial { nwritten: usize, view: BufView },
  Full { nwritten: usize },
}

impl WriteOutcome {
  pub fn nwritten(&self) -> usize {
    match self {
      WriteOutcome::Partial { nwritten, .. } => *nwritten,
      WriteOutcome::Full { nwritten } => *nwritten,
    }
  }
}

trait ResourceData<D> {
  /// Returns an asynchronously borrowable version of this resource's data.
  fn data(&self) -> &RcRef<AsyncRefCell<D>>;
}

pub trait TokioAsyncRead: tokio::io::AsyncRead + Unpin + 'static {}
impl<R> TokioAsyncRead for R where Self: tokio::io::AsyncRead + Unpin + 'static {}

pub trait TokioAsyncWrite: tokio::io::AsyncWrite + Unpin + 'static {}
impl<R> TokioAsyncWrite for R where Self: tokio::io::AsyncWrite + Unpin + 'static
{}

pub trait StreamBytesRead:
  futures::stream::Stream<Item = Result<BufView, anyhow::Error>>
  + Unpin
  + 'static
{
}
impl<R> StreamBytesRead for R where
  Self: futures::stream::Stream<Item = Result<BufView, anyhow::Error>>
    + Unpin
    + 'static
{
}

pub trait ChannelBytesRead: Unpin + 'static {
  fn poll_recv(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<BufView, anyhow::Error>>>;
}

impl ChannelBytesRead for tokio::sync::mpsc::Receiver<BufView> {
  fn poll_recv(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<BufView, anyhow::Error>>> {
    let res = self.poll_recv(cx);
    res.map(|res| res.map(Ok))
  }
}

impl ChannelBytesRead for tokio::sync::mpsc::Receiver<Result<BufView, Error>> {
  fn poll_recv(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<BufView, anyhow::Error>>> {
    self.poll_recv(cx)
  }
}

impl ChannelBytesRead for tokio::sync::mpsc::Receiver<bytes::Bytes> {
  fn poll_recv(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<BufView, anyhow::Error>>> {
    let res = self.poll_recv(cx);
    res.map(|res| res.map(|r| r.into()).map(Ok))
  }
}

impl ChannelBytesRead
  for tokio::sync::mpsc::Receiver<Result<bytes::Bytes, anyhow::Error>>
{
  fn poll_recv(
    &mut self,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<BufView, anyhow::Error>>> {
    self.poll_recv(cx).map(|res| res.map(|res| res.map(|res| res.into())))
  }
}

pub trait ChannelBytesWrite: 'static {
  type WriteFuture: Future<Output = Result<(), anyhow::Error>>;
  fn send(&self, buffer: BufView) -> Self::WriteFuture;
  fn send_error(&self, error: Error) -> Self::WriteFuture;
}

impl ChannelBytesWrite for tokio::sync::mpsc::Sender<Result<BufView, Error>> {
  type WriteFuture =
    Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + 'static>>;
  fn send(&self, buffer: BufView) -> Self::WriteFuture {
    use futures::FutureExt;
    let future = self.clone().reserve_owned();
    future
      .map(|r| {
        r?.send(Ok(buffer));
        Ok(())
      })
      .boxed_local()
  }

  fn send_error(&self, error: Error) -> Self::WriteFuture {
    use futures::FutureExt;
    let future = self.clone().reserve_owned();
    future
      .map(|r| {
        r?.send(Err(error));
        Ok(())
      })
      .boxed_local()
  }
}

#[repr(transparent)]
struct ChannelStreamAdapter<C>(C);

impl<C> Stream for ChannelStreamAdapter<C>
where
  C: ChannelBytesRead,
{
  type Item = Result<BufView, anyhow::Error>;
  fn poll_next(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Self::Item>> {
    self.0.poll_recv(cx)
  }
}

#[doc(hidden)]
pub trait IsEmptyTuple: Default {}
impl IsEmptyTuple for () {}

type ReaderFn<R> = for<'x> fn(&'x Rc<dyn Resource>) -> Option<&'x Rc<R>>;

pub struct ResourceBuilder<Reader: ?Sized, Data = ()> {
  underlying: PhantomData<Reader>,
  data: PhantomData<Data>,
  this: *const (),
  constructor_data: fn(*const (), Reader, Data) -> Rc<dyn Resource>,
  retrieve_data: fn(&Rc<dyn Resource>) -> Option<&Data>,
  retrieve_reader: Option<ReaderFn<()>>,
}

unsafe impl<Reader: ?Sized, Data> Send for ResourceBuilder<Reader, Data> {}
unsafe impl<Reader: ?Sized, Data> Sync for ResourceBuilder<Reader, Data> {}

impl<Reader, Data> ResourceBuilder<Reader, Data> {
  pub fn build(&self, reader: Reader) -> Rc<dyn Resource>
  where
    Data: IsEmptyTuple,
  {
    (self.constructor_data)(self.this, reader, Data::default())
  }

  pub fn build_with_data(
    &self,
    reader: Reader,
    data: Data,
  ) -> Rc<dyn Resource> {
    (self.constructor_data)(self.this, reader, data)
  }

  pub fn data<'b>(
    &self,
    resource: &'b Rc<dyn Resource>,
  ) -> Option<&'b Data> {
    (self.retrieve_data)(resource)
  }

  /// If the underlying `Reader` is [`RcLike`], return an [`Rc`] to it if we can.
  pub fn reader<'b, R: ?Sized>(
    &self,
    resource: &'b Rc<dyn Resource>,
  ) -> Option<&'b Rc<R>>
  where
    Reader: Into<Rc<R>>,
  {
    if let Some(f) = self.retrieve_reader {
      let f: ReaderFn<R> = unsafe { std::mem::transmute(f) };
      unsafe { f(resource) }
    } else {
      None
    }
  }
}

/// Builds a [`Resource`] from an underlying stream or other producer.
pub struct ResourceBuilderImpl<State = (), Data = ()> {
  name: &'static str,
  on_shutdown: Option<fn(Data) -> ()>,
  state: State,
  data: PhantomData<Data>,
}

pub struct ResourceBuilderTokioAsyncRead<R: TokioAsyncRead> {
  reader: PhantomData<R>,
  fd: bool,
}

pub struct ResourceBuilderTokioAsyncWrite<W: TokioAsyncWrite> {
  writer: PhantomData<W>,
  fd: bool,
}

pub struct ResourceBuilderTokioAsyncReadWrite<R: TokioAsyncRead, W: TokioAsyncWrite> {
  reader: PhantomData<R>,
  writer: PhantomData<W>,
  fd: bool,
}

pub struct ResourceBuilderTokioDuplex<
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
> {
  duplex: S,
  fd: Option<std::os::fd::RawFd>,
}

pub struct ResourceBuilderStreamBytesRead<S: StreamBytesRead> {
  stream: PhantomData<S>,
}

pub struct ResourceBuilderChannelBytesRead<C: ChannelBytesRead> {
  channel: PhantomData<C>,
}

pub struct ResourceBuilderChannelBytesWrite<C: ChannelBytesWrite> {
  channel: PhantomData<C>,
}

pub struct ResourceBuilderResourceStream<
  S: ResourceStreamRead + ResourceStreamWrite + ?Sized,
> {
  stream: PhantomData<S>,
}

impl ResourceBuilderImpl<(), ()> {
  pub const fn new(name: &'static str) -> Self {
    Self {
      name,
      on_shutdown: None,
      state: (),
      data: PhantomData,
    }
  }

  pub const fn new_with_data<D>(
    name: &'static str,
  ) -> ResourceBuilderImpl<(), D> {
    ResourceBuilderImpl {
      name,
      on_shutdown: None,
      state: (),
      data: PhantomData,
    }
  }
}

impl<D> ResourceBuilderImpl<(), D> {
  const fn with_state<S>(self, state: S) -> ResourceBuilderImpl<S, D> {
    ResourceBuilderImpl {
      name: self.name,
      on_shutdown: self.on_shutdown,
      data: self.data,
      state,
    }
  }

  pub const fn with_reader<R: TokioAsyncRead>(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderTokioAsyncRead<R>, D> {
    self.with_state(ResourceBuilderTokioAsyncRead {
      reader: PhantomData,
      fd: false,
    })
  }

  pub const fn with_reader_and_writer<
    R: TokioAsyncRead,
    W: TokioAsyncWrite
  >(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderTokioAsyncReadWrite<R, W>, D> {
    self.with_state(ResourceBuilderTokioAsyncReadWrite {
      reader: PhantomData,
      writer: PhantomData,
      fd: false,
    })
  }

  pub const fn with_stream<S: StreamBytesRead>(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderStreamBytesRead<S>, D> {
    self.with_state(ResourceBuilderStreamBytesRead {
      stream: PhantomData,
    })
  }

  pub const fn with_read_channel<C: ChannelBytesRead>(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderChannelBytesRead<C>, D> {
    self.with_state(ResourceBuilderChannelBytesRead {
      channel: PhantomData,
    })
  }

  pub const fn with_write_channel<C: ChannelBytesWrite>(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderChannelBytesWrite<C>, D> {
    self.with_state(ResourceBuilderChannelBytesWrite {
      channel: PhantomData,
    })
  }

  pub const fn with_read_write_resource_stream<
    S: ResourceStreamRead + ResourceStreamWrite + ?Sized,
  >(
    self,
  ) -> ResourceBuilderImpl<ResourceBuilderResourceStream<S>, D> {
    self.with_state(ResourceBuilderResourceStream {
      stream: PhantomData,
    })
  }
}

impl<R: TokioAsyncRead, D>
  ResourceBuilderImpl<ResourceBuilderTokioAsyncRead<R>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<R, D> {
    let this = self as *const _ as *const ();
    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, reader, data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoTokioAsyncReadResource::new(
          this.name, None, None, reader, data,
        ))
      },
      retrieve_data: |resource| {
        resource
          .downcast_rc::<DenoTokioAsyncReadResource<R, D>>()
          .map(|res| &res.data)
      },
      retrieve_reader: None,
    }
  }

  pub const fn and_fd(mut self) -> Self
  where
    R: std::os::fd::AsRawFd,
  {
    self.state.fd = true;
    self
  }
}


impl<R: TokioAsyncRead, W: TokioAsyncWrite, D>
  ResourceBuilderImpl<ResourceBuilderTokioAsyncReadWrite<R, W>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<(R, W), D> {
    let this = self as *const _ as *const ();
    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, (reader, writer), data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoTokioAsyncReadWriteResource::new(
          this.name, None, None, reader, writer, data,
        ))
      },
      retrieve_data: |resource| {
        resource
          .downcast_rc::<DenoTokioAsyncReadWriteResource<R, W, D>>()
          .map(|res| &res.data)
      },
      retrieve_reader: None,
    }
  }

  pub const fn and_fd(mut self) -> Self
  where
    R: std::os::fd::AsRawFd,
  {
    self.state.fd = true;
    self
  }
}

impl<R: StreamBytesRead, D>
  ResourceBuilderImpl<ResourceBuilderStreamBytesRead<R>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<R, D> {
    let this = self as *const _ as *const ();
    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, reader, data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoStreamBytesReadResource::new(
          this.name, None, reader, data,
        ))
      },
      retrieve_data: |resource| {
        resource
          .downcast_rc::<DenoStreamBytesReadResource<R, D>>()
          .map(|res| &res.data)
      },
      retrieve_reader: None,
    }
  }
}

impl<C: ChannelBytesRead, D>
  ResourceBuilderImpl<ResourceBuilderChannelBytesRead<C>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<C, D> {
    let this = self as *const _ as *const ();
    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, reader, data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoStreamBytesReadResource::new(
          this.name,
          None,
          ChannelStreamAdapter(reader),
          data,
        ))
      },
      retrieve_data: |resource| {
        resource.downcast_rc::<DenoStreamBytesReadResource::<ChannelStreamAdapter<C>, D>>().map(|res| &res.data)
      },
      retrieve_reader: None,
    }
  }
}

impl<C: ChannelBytesWrite, D>
  ResourceBuilderImpl<ResourceBuilderChannelBytesWrite<C>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<C, D> {
    let this = self as *const _ as *const ();
    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, underlying, data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoChannelBytesWriteResource::new(
          this.name, None, underlying, data,
        ))
      },
      retrieve_data: |resource| {
        resource
          .downcast_rc::<DenoChannelBytesWriteResource<C, D>>()
          .map(|res| &res.data)
      },
      retrieve_reader: None,
    }
  }
}

impl<S: std::any::Any + ResourceStreamRead + ResourceStreamWrite + ?Sized, D>
  ResourceBuilderImpl<ResourceBuilderResourceStream<S>, D>
{
  pub const fn build(&'static self) -> ResourceBuilder<Rc<S>, D> {
    let this = self as *const _ as *const ();
    let retrieve_reader: ReaderFn<S> = |resource: &Rc<dyn Resource>| {
      resource
        .downcast_rc::<DenoResourceStreamResource<S, D>>()
        .map(|res| &res.underlying)
    };

    ResourceBuilder {
      underlying: PhantomData,
      data: PhantomData,
      this,
      constructor_data: |this, underlying, data| {
        let this = unsafe { (this as *const Self).as_ref().unwrap() };
        Rc::new(DenoResourceStreamResource::new(
          this.name, None, underlying, data,
        ))
      },
      retrieve_data: |resource| {
        resource
          .downcast_rc::<DenoResourceStreamResource<S, D>>()
          .map(|res| &res.data)
      },
      retrieve_reader: Some(unsafe {
        std::mem::transmute(retrieve_reader as ReaderFn<S>)
      }),
    }
  }
}


#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  pub fn bufview_read_and_truncate() {
    let mut buf = BufView::from(vec![1, 2, 3, 4]);
    assert_eq!(4, buf.len());
    assert_eq!(0, buf.cursor);
    assert_eq!(1, buf.get_u8());
    assert_eq!(3, buf.len());
    // The cursor is at position 1, so this truncates the underlying buffer to 2+1
    buf.truncate(2);
    assert_eq!(2, buf.len());
    assert_eq!(2, buf.get_u8());
    assert_eq!(1, buf.len());

    buf.reset_cursor();
    assert_eq!(3, buf.len());
  }

  #[test]
  pub fn bufview_split() {
    let mut buf = BufView::from(Vec::from_iter(0..100));
    assert_eq!(100, buf.len());
    buf.advance_cursor(25);
    assert_eq!(75, buf.len());
    let mut other = buf.split_off(10);
    assert_eq!(25, buf.cursor);
    assert_eq!(10, buf.len());
    assert_eq!(65, other.len());

    let other2 = other.split_to(20);
    assert_eq!(20, other2.len());
    assert_eq!(45, other.len());

    assert_eq!(100, buf.cursor + buf.len() + other.len() + other2.len());
    buf.reset_cursor();
    assert_eq!(100, buf.cursor + buf.len() + other.len() + other2.len());
  }

  #[test]
  pub fn bufmutview_read_and_truncate() {
    let mut buf = BufMutView::from(BytesMut::from([1, 2, 3, 4].as_slice()));
    assert_eq!(4, buf.len());
    assert_eq!(0, buf.cursor);
    assert_eq!(1, buf.get_u8());
    assert_eq!(3, buf.len());
    // The cursor is at position 1, so this truncates the underlying buffer to 2+1
    buf.truncate(2);
    assert_eq!(2, buf.len());
    assert_eq!(2, buf.get_u8());
    assert_eq!(1, buf.len());

    buf.reset_cursor();
    assert_eq!(3, buf.len());
  }

  #[test]
  pub fn bufmutview_split() {
    let mut buf =
      BufMutView::from(BytesMut::from(Vec::from_iter(0..100).as_slice()));
    assert_eq!(100, buf.len());
    buf.advance_cursor(25);
    assert_eq!(75, buf.len());
    let mut other = buf.split_off(10);
    assert_eq!(25, buf.cursor);
    assert_eq!(10, buf.len());
    assert_eq!(65, other.len());

    let other2 = other.split_to(20);
    assert_eq!(20, other2.len());
    assert_eq!(45, other.len());

    assert_eq!(100, buf.cursor + buf.len() + other.len() + other2.len());
    buf.reset_cursor();
    assert_eq!(100, buf.cursor + buf.len() + other.len() + other2.len());
  }

  #[test]
  #[allow(deprecated)]
  fn bufmutview_resize() {
    let new =
      || BufMutView::from(BytesMut::from(Vec::from_iter(0..100).as_slice()));
    let mut buf = new();
    assert_eq!(100, buf.len());
    buf.maybe_resize(200, 10).unwrap();
    assert_eq!(110, buf.len());

    let mut buf = new();
    assert_eq!(100, buf.len());
    buf.maybe_resize(200, 100).unwrap();
    assert_eq!(200, buf.len());

    let mut buf = new();
    assert_eq!(100, buf.len());
    buf.maybe_resize(200, 1000).unwrap();
    assert_eq!(200, buf.len());

    let mut buf = new();
    buf.advance_cursor(50);
    assert_eq!(50, buf.len());
    buf.maybe_resize(100, 100).unwrap();
    assert_eq!(100, buf.len());
    buf.reset_cursor();
    assert_eq!(150, buf.len());
  }

  #[test]
  #[allow(deprecated)]
  fn bufmutview_grow() {
    let new =
      || BufMutView::from(BytesMut::from(Vec::from_iter(0..100).as_slice()));
    let mut buf = new();
    assert_eq!(100, buf.len());
    buf.maybe_grow(200).unwrap();
    assert_eq!(200, buf.len());

    let mut buf = new();
    buf.advance_cursor(50);
    assert_eq!(50, buf.len());
    buf.maybe_grow(100).unwrap();
    assert_eq!(100, buf.len());
    buf.reset_cursor();
    assert_eq!(150, buf.len());
  }
}
