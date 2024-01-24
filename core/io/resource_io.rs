// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::BufMutViewWhole;
use super::ResourceHandle;
use crate::error::not_supported;
use crate::io::BufMutView;
use crate::io::BufView;
use crate::AsyncResult;
use anyhow::Error;
use bytes::BufMut;
use futures::Future;
use futures::FutureExt;
use std::any::type_name;
use std::any::Any;
use std::any::TypeId;
use std::borrow::Cow;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;

// We don't want `OpaqueReadFuture` leaking.
#[allow(private_interfaces)]
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

pub(crate) enum ReadResult2 {
  Err(Error),
  EOF,
  Ready(usize),
  ReadyBufMut(BufMutView),
  ReadyBuf(BufView),
}

// We don't want `OpaqueWriteFuture` leaking.
#[allow(private_interfaces)]
pub enum WriteResult<'a> {
  /// The write operation returned an error.
  Err(Error),
  /// The write operation could not complete, but is requesting a re-poll at a later time
  /// (potentially even right away). This is useful in certain cases where there may be internal
  /// buffering and it is simply easier to retry to write.
  PollAgain,
  EOF,
  Ready(usize),
  Future(OpaqueWriteFuture<'a>),
}

pub(crate) struct OpaqueReadFuture<'a> {
  f: Pin<Box<dyn Future<Output = ReadResult<'a>> + 'a>>,
}

impl<'a> OpaqueReadFuture<'a> {
  pub(crate) fn poll_read(&mut self, cx: &mut Context) -> Poll<ReadResult<'a>> {
    self.f.poll_unpin(cx)
  }
}

pub(crate) struct OpaqueWriteFuture<'a> {
  f: Pin<Box<dyn Future<Output = WriteResult<'a>> + 'a>>,
}

impl<'a> OpaqueWriteFuture<'a> {
  pub(crate) fn poll_write(
    &mut self,
    cx: &mut Context,
  ) -> Poll<WriteResult<'a>> {
    self.f.poll_unpin(cx)
  }
}

pub struct WriteContext<'a> {
  buf: &'a mut BufView,
}

impl<'a> WriteContext<'a> {
  pub(crate) fn new(buf: &'a mut BufView) -> Self {
    Self { buf }
  }

  pub fn buf_copy(&mut self) -> BufView {
    // self.buf.clone()
    unimplemented!()
  }

  pub fn buf_owned(&mut self) -> BufView {
    self.buf.split_off(0)
  }

  pub fn poll_writer<W: AsyncWrite + Unpin>(&self, cx: &mut Context<'_>, w: &mut W) -> Poll<WriteResult<'a>> {
    Poll::Ready(match ready!(Pin::new(w).poll_write(cx, &self.buf)) {
      Err(err) => WriteResult::Err(err.into()),
      Ok(n) => WriteResult::Ready(n),
    })
  }
}

pub enum ReadContextBuf<'a> {
  Buf(&'a mut [u8]),
  BufRead(ReadBuf<'a>),
  Empty,
}

pub struct ReadContext<'a> {
  pub(crate) buf: ReadContextBuf<'a>,
}

impl<'a> ReadContext<'a> {
  pub fn new(buf: &'a mut BufMutViewWhole) -> Self {
    Self {
      buf: ReadContextBuf::Buf(buf.as_mut()),
    }
  }
  /// Use this buffer holder to poll a reader.
  pub fn poll_reader<R: AsyncRead + Unpin>(
    &mut self,
    cx: &mut Context,
    r: &mut R,
  ) -> Poll<ReadResult<'a>> {
    let buf = self.preferred_buffer();
    Poll::Ready(match ready!(Pin::new(r).poll_read(cx, buf)) {
      Err(err) => ReadResult::Err(err.into()),
      Ok(_) if buf.filled().len() == 0 => ReadResult::EOF,
      Ok(_) => ReadResult::Ready,
    })
  }

  pub fn read_future<F: Future<Output = ReadResult<'a>> + 'a>(
    &mut self,
    f: impl (FnOnce(ReadBufHolder<'a>) -> F) + 'a,
  ) -> Poll<ReadResult<'a>> {
    let buffer = ReadContext {
      buf: std::mem::replace(&mut self.buf, ReadContextBuf::Empty),
    };
    Poll::Ready(ReadResult::Future(OpaqueReadFuture {
      f: Box::pin((f)(ReadBufHolder { buffer })),
    }))
  }

  pub fn read_length_hint(&self) -> usize {
    match &self.buf {
      ReadContextBuf::Buf(buf) => buf.len(),
      ReadContextBuf::BufRead(buf) => buf.remaining_mut(),
      ReadContextBuf::Empty => panic!("ReadContext is no longer valid")
    }
  }

  pub fn preferred_buffer(&mut self) -> &mut ReadBuf<'a> {
    let buf_ptr = &mut self.buf;
    if let ReadContextBuf::BufRead(buf) = buf_ptr {
      return buf;
    };
    let buf = std::mem::replace(buf_ptr, ReadContextBuf::Empty);
    if let ReadContextBuf::Buf(buf) = buf {
      _ =
        std::mem::replace(buf_ptr, ReadContextBuf::BufRead(ReadBuf::new(buf)));
    }
    if let ReadContextBuf::BufRead(buf) = buf_ptr {
      return buf;
    };
    panic!("ReadContext is no longer valid")
  }

  pub(crate) fn take(self, _future: Option<OpaqueReadFuture<'a>>) {}
}

pub struct ReadBufHolder<'a> {
  buffer: ReadContext<'a>,
}

impl<'a> ReadBufHolder<'a> {
  pub fn read_length_hint(&self) -> usize {
    self.buffer.read_length_hint()
  }

  /// Use this buffer for some other purpose.
  pub fn with_buf<T>(&mut self, f: impl Fn(&mut ReadBuf<'a>) -> T) -> T {
    f(&mut self.buffer.preferred_buffer())
  }

  /// Use this buffer holder to poll a reader.
  pub fn poll_reader<R: AsyncRead + Unpin>(
    &mut self,
    cx: &mut Context,
    r: &mut R,
  ) -> Poll<std::io::Result<()>> {
    Poll::Ready(ready!(
      Pin::new(r).poll_read(cx, &mut self.buffer.preferred_buffer())
    ))
  }
}
