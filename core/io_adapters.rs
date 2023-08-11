use crate::error::bad_resource;
use crate::error::type_error;
use crate::error::AnyError;
use crate::futures::stream::Peekable;
use crate::io::ChannelBytesWrite;
use crate::io::StreamBytesRead;
use crate::io::TokioAsyncRead;
use crate::io::TokioAsyncWrite;
use crate::resources::ResourceHandle;
use crate::AsyncRefCell;
use crate::AsyncResult;
use crate::BufView;
use crate::CancelFuture;
use crate::CancelHandle;
use crate::CancelTryFuture;
use crate::RcLike;
use crate::RcRef;
use crate::Resource;
use crate::ResourceStreamRead;
use crate::ResourceStreamWrite;
use crate::WriteOutcome;
use futures::FutureExt;
use futures::StreamExt;
use log::debug;
use tokio::io::AsyncWriteExt;
use std::borrow::Cow;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use tokio::io::AsyncReadExt;

pub fn type_name_of_val<T: ?Sized>(_val: &T) -> &'static str {
  std::any::type_name::<T>()
}

macro_rules! io_debug {
  ($this:ident, $($arg:tt)*) => {
    eprint!("{}({}) ", type_name_of_val(& $this), $this.name);
    eprintln!($($arg)*);
  };
}

macro_rules! io_debug_new {
  ($this:ident, $name:ident) => {
    eprintln!("{}({})::new()", ::std::any::type_name::<$this>(), $name);
  };
}

/// An optimized stream that lives on top of a [`tokio::io::AsyncRead`].
pub struct DenoTokioAsyncReadResource<R, D>
where
  R: TokioAsyncRead,
  D: 'static,
{
  pub(crate) name: &'static str,
  pub(crate) on_shutdown: Option<fn(D) -> ()>,
  pub(crate) fd: Option<ResourceHandle>,
  pub(crate) reader: AsyncRefCell<R>,
  pub(crate) data: D,
  pub(crate) cancel_handle: CancelHandle,
}

impl<R, D> Resource for DenoTokioAsyncReadResource<R, D>
where
  R: TokioAsyncRead,
  D: 'static,
{
  fn name(&self) -> Cow<str> {
    Cow::Borrowed(self.name)
  }

  fn backing_handle(self: Rc<Self>) -> Option<ResourceHandle> {
    self.fd
  }

  crate::impl_readable_byob! {}

  fn close(self: Rc<Self>) {
    io_debug!(self, "close");
    self.cancel_handle.cancel()
  }
}

impl<R, D> DenoTokioAsyncReadResource<R, D>
where
  R: TokioAsyncRead,
  D: 'static,
{
  pub fn new(
    name: &'static str,
    on_shutdown: Option<fn(D) -> ()>,
    fd: Option<ResourceHandle>,
    reader: R,
    data: D,
  ) -> Self {
    io_debug_new!(Self, name);
    let cancel_handle = CancelHandle::new();
    Self {
      name,
      cancel_handle,
      on_shutdown,
      fd,
      reader: AsyncRefCell::new(reader),
      data,
    }
  }

  pub fn cancel_handle(self: &Rc<Self>) -> impl RcLike<CancelHandle> {
    RcRef::map(self, |s| &s.cancel_handle).clone()
  }

  async fn read(self: Rc<Self>, data: &mut [u8]) -> Result<usize, AnyError> {
    let cancel_handle = self.cancel_handle();
    let mut rd = RcRef::map(self, |s| &s.reader).borrow_mut().await;
    let nread = rd.read(data).try_or_cancel(cancel_handle).await?;
    Ok(nread)
  }
}


/// An optimized stream that lives on top of a [`tokio::io::AsyncRead`].
pub struct DenoTokioAsyncReadWriteResource<R, W, D>
where
  R: TokioAsyncRead,
  W: TokioAsyncWrite,
  D: 'static,
{
  pub(crate) name: &'static str,
  pub(crate) on_shutdown: Option<fn(D) -> ()>,
  pub(crate) fd: Option<ResourceHandle>,
  pub(crate) reader: AsyncRefCell<R>,
  pub(crate) writer: AsyncRefCell<W>,
  pub(crate) data: D,
  pub(crate) cancel_handle: CancelHandle,
}

impl<R, W, D> Resource for DenoTokioAsyncReadWriteResource<R, W, D>
where
  R: TokioAsyncRead,
  W: TokioAsyncWrite,
  D: 'static,
{
  fn name(&self) -> Cow<str> {
    Cow::Borrowed(self.name)
  }

  fn backing_handle(self: Rc<Self>) -> Option<ResourceHandle> {
    self.fd
  }

  crate::impl_readable_byob! {}
  crate::impl_writable! {}

  fn close(self: Rc<Self>) {
    io_debug!(self, "close");
    self.cancel_handle.cancel()
  }
}

impl<R, W, D> DenoTokioAsyncReadWriteResource<R, W, D>
where
  R: TokioAsyncRead,
  W: TokioAsyncWrite,
  D: 'static,
{
  pub fn new(
    name: &'static str,
    on_shutdown: Option<fn(D) -> ()>,
    fd: Option<ResourceHandle>,
    reader: R,
    writer: W,
    data: D,
  ) -> Self {
    io_debug_new!(Self, name);
    let cancel_handle = CancelHandle::new();
    Self {
      name,
      cancel_handle,
      on_shutdown,
      fd,
      reader: AsyncRefCell::new(reader),
      writer: AsyncRefCell::new(writer),
      data,
    }
  }

  pub fn cancel_handle(self: &Rc<Self>) -> impl RcLike<CancelHandle> {
    RcRef::map(self, |s| &s.cancel_handle).clone()
  }

  async fn read(self: Rc<Self>, data: &mut [u8]) -> Result<usize, AnyError> {
    let cancel_handle = self.cancel_handle();
    let mut rd = RcRef::map(self, |s| &s.reader).borrow_mut().await;
    let nread = rd.read(data).try_or_cancel(cancel_handle).await?;
    Ok(nread)
  }

  async fn write(self: Rc<Self>, buf: &[u8]) -> Result<usize, AnyError> {
    let cancel_handle = RcRef::map(self.clone(), |this| &this.cancel_handle);
    async {
      let write = RcRef::map(self, |this| &this.writer);
      let mut write = write.borrow_mut().await;
      Ok(Pin::new(&mut *write).write(buf).await?)
    }
    .try_or_cancel(cancel_handle)
    .await
  }
}

pub struct DenoStreamBytesReadResource<R, D>
where
  R: StreamBytesRead,
  D: 'static,
{
  pub(crate) name: &'static str,
  pub(crate) on_shutdown: Option<fn(D) -> ()>,
  pub(crate) reader: AsyncRefCell<Peekable<R>>,
  pub(crate) data: D,
  pub(crate) cancel_handle: CancelHandle,
}

impl<R, D> Resource for DenoStreamBytesReadResource<R, D>
where
  R: StreamBytesRead,
  D: 'static,
{
  fn name(&self) -> Cow<str> {
    Cow::Borrowed(self.name)
  }

  fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
    io_debug!(self, "read");
    Box::pin(DenoStreamBytesReadResource::<R, D>::read(self, limit))
  }

  fn close(self: Rc<Self>) {
    io_debug!(self, "close");
    // self.cancel_handle.cancel()
  }
}

impl<R, D> DenoStreamBytesReadResource<R, D>
where
  R: StreamBytesRead,
  D: 'static,
{
  pub fn new(
    name: &'static str,
    on_shutdown: Option<fn(D) -> ()>,
    reader: R,
    data: D,
  ) -> Self {
    io_debug_new!(Self, name);
    let cancel_handle = CancelHandle::new();
    Self {
      name,
      cancel_handle,
      on_shutdown,
      reader: AsyncRefCell::new(reader.peekable()),
      data,
    }
  }

  pub fn cancel_handle(self: &Rc<Self>) -> impl RcLike<CancelHandle> {
    RcRef::map(self, |s| &s.cancel_handle).clone()
  }

  async fn read(self: Rc<Self>, limit: usize) -> Result<BufView, AnyError> {
    let cancel_handle = self.cancel_handle();
    let peekable = RcRef::map(self, |this| &this.reader);
    let mut peekable = peekable.borrow_mut().await;
    match Pin::new(&mut *peekable)
      .peek_mut()
      .or_cancel(cancel_handle)
      .await?
    {
      None => Ok(BufView::empty()),
      // Take the actual error since we only have a reference to it
      Some(Err(_)) => Err(peekable.next().await.unwrap().err().unwrap()),
      Some(Ok(bytes)) => {
        if bytes.len() <= limit {
          // We can safely take the next item since we peeked it
          return Ok(BufView::from(peekable.next().await.unwrap()?));
        }
        // The remainder of the bytes after we split it is still left in the peek buffer
        let ret = bytes.split_to(limit);
        Ok(BufView::from(ret))
      }
    }
  }
}

pub struct DenoChannelBytesWriteResource<W, D>
where
  W: ChannelBytesWrite,
  D: 'static,
{
  pub(crate) name: &'static str,
  pub(crate) on_shutdown: Option<fn(D) -> ()>,
  pub(crate) writer: AsyncRefCell<Option<W>>,
  pub(crate) data: D,
  pub(crate) cancel_handle: CancelHandle,
}

impl<W, D> Resource for DenoChannelBytesWriteResource<W, D>
where
  W: ChannelBytesWrite,
  D: 'static,
{
  fn name(&self) -> Cow<str> {
    Cow::Borrowed(self.name)
  }

  fn write(self: Rc<Self>, buf: BufView) -> AsyncResult<WriteOutcome> {
    io_debug!(self, "write");
    let cancel_handle = self.cancel_handle();
    Box::pin(
      async move {
        let nwritten = buf.len();

        let tx = RcRef::map(self, |this| &this.writer).borrow().await;
        let tx = (*tx).as_ref();
        let tx = tx.ok_or(type_error("receiver not connected"))?;

        tx.send(buf)
          .await
          .map_err(|_| bad_resource("failed to write"))?;
        Ok(WriteOutcome::Full { nwritten })
      }
      .try_or_cancel(cancel_handle),
    )
  }

  fn write_error(self: Rc<Self>, error: anyhow::Error) -> AsyncResult<()> {
    io_debug!(self, "write_error");
    let cancel_handle = self.cancel_handle();
    Box::pin(
      async move {
        let tx = RcRef::map(self, |this| &this.writer).borrow().await;
        let tx = (*tx).as_ref();
        let tx = tx.ok_or(type_error("receiver not connected"))?;
        tx.send_error(error)
          .await
          .map_err(|_| bad_resource("failed to write"))?;
        Ok(())
      }
      .try_or_cancel(cancel_handle),
    )
  }

  fn shutdown(self: Rc<Self>) -> AsyncResult<()> {
    io_debug!(self, "shutdown");
    async move {
      let mut tx = RcRef::map(self, |this| &this.writer).borrow_mut().await;
      tx.take();
      Ok(())
    }
    .boxed_local()
  }

  fn close(self: Rc<Self>) {
    io_debug!(self, "close");
    self.cancel_handle.cancel()
  }
}

impl<W, D> DenoChannelBytesWriteResource<W, D>
where
  W: ChannelBytesWrite,
  D: 'static,
{
  pub fn new(
    name: &'static str,
    on_shutdown: Option<fn(D) -> ()>,
    writer: W,
    data: D,
  ) -> Self {
    io_debug_new!(Self, name);
    let cancel_handle = CancelHandle::new();
    Self {
      name,
      cancel_handle,
      on_shutdown,
      writer: AsyncRefCell::new(Some(writer)),
      data,
    }
  }

  pub fn cancel_handle(self: &Rc<Self>) -> impl RcLike<CancelHandle> {
    RcRef::map(self, |s| &s.cancel_handle).clone()
  }
}

pub struct DenoResourceStreamResource<S, D>
where
  S: ResourceStreamRead + ResourceStreamWrite + ?Sized + 'static,
  D: 'static,
{
  pub(crate) name: &'static str,
  pub(crate) on_shutdown: Option<fn(D) -> ()>,
  pub(crate) underlying: Rc<S>,
  pub(crate) data: D,
  pub(crate) cancel_handle: CancelHandle,
}

impl<S, D> Resource for DenoResourceStreamResource<S, D>
where
  S: ResourceStreamRead + ResourceStreamWrite + ?Sized + 'static,
  D: 'static,
{
  fn name(&self) -> Cow<str> {
    Cow::Borrowed(self.name)
  }

  fn backing_handle(self: Rc<Self>) -> Option<ResourceHandle> {
    self.underlying.clone().backing_handle()
  }

  fn close(self: Rc<Self>) {
    io_debug!(self, "close");
  }

  fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
    self.underlying.clone().read(limit)
  }

  fn read_byob(
    self: Rc<Self>,
    buf: crate::BufMutView,
  ) -> AsyncResult<(usize, crate::BufMutView)> {
    self.underlying.clone().read_byob(buf)
  }

  fn read_byob_sync(
    self: Rc<Self>,
    data: &mut [u8],
  ) -> Result<usize, anyhow::Error> {
    self.underlying.clone().read_byob_sync(data)
  }

  fn write(self: Rc<Self>, buf: BufView) -> AsyncResult<WriteOutcome> {
    self.underlying.clone().write(buf)
  }

  fn write_all(self: Rc<Self>, view: BufView) -> AsyncResult<()> {
    self.underlying.clone().write_all(view)
  }

  fn write_sync(self: Rc<Self>, data: &[u8]) -> Result<usize, anyhow::Error> {
    self.underlying.clone().write_sync(data)
  }
}

impl<S, D> DenoResourceStreamResource<S, D>
where
  S: ResourceStreamRead + ResourceStreamWrite + ?Sized + 'static,
  D: 'static,
{
  pub fn new(
    name: &'static str,
    on_shutdown: Option<fn(D) -> ()>,
    underlying: Rc<S>,
    data: D,
  ) -> Self {
    io_debug_new!(Self, name);
    let cancel_handle = CancelHandle::new();
    Self {
      name,
      cancel_handle,
      on_shutdown,
      underlying,
      data,
    }
  }

  pub fn cancel_handle(self: &Rc<Self>) -> impl RcLike<CancelHandle> {
    RcRef::map(self, |s| &s.cancel_handle).clone()
  }
}
