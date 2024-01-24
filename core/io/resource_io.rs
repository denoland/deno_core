// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::BufMutViewWhole;
use crate::io::BufMutView;
use crate::io::BufView;
use crate::WriteOutcome;
use anyhow::Error;
use bytes::Buf;
use bytes::BufMut;
use futures::Future;
use futures::FutureExt;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;

#[derive(Default)]
pub struct ReadState {
  partial: Option<BufView>,
}

impl ReadState {
  pub fn write(
    &mut self,
    slice: &mut [u8],
    mut buf: BufView,
  ) -> Result<usize, Error> {
    let buf_len = buf.len();
    let slice_len = slice.len();
    if buf_len > slice_len {
      buf.copy_to_slice(slice);
      self.partial = Some(buf);
      return Ok(slice_len);
    } else {
      buf.copy_to_slice(&mut slice[..buf_len]);
      return Ok(buf_len);
    }
  }
}

// We don't want `OpaqueReadFuture` leaking.
#[allow(private_interfaces)]
pub enum ReadResult<'a> {
  /// The read operation returned an error.
  Err(Error),
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

pub enum ReadResultSync {
  /// The read operation returned an error.
  Err(Error),
  /// The stream is at the end-of-file and is therefore complete. It is an error to
  /// return an empty result via the `Ready` enumeration value to prevent coding errors.
  EOF,
  Ready,
  ReadyBufMut(BufMutView),
  ReadyBuf(BufView),
}

impl ReadResultSync {
  pub fn into_read_byob_result(
    self,
    mut buf: BufMutView,
    read_size: usize,
  ) -> Result<(usize, BufMutView), Error> {
    match self {
      ReadResultSync::Ready => Ok((read_size, buf)),
      ReadResultSync::Err(err) => Err(err),
      ReadResultSync::EOF => Ok((0, buf)),
      ReadResultSync::ReadyBuf(new_buf) => {
        buf[..new_buf.len()].copy_from_slice(&new_buf);
        Ok((new_buf.len(), buf))
      }
      ReadResultSync::ReadyBufMut(new_buf) => {
        buf[..new_buf.len()].copy_from_slice(&new_buf);
        Ok((new_buf.len(), buf))
      }
    }
  }
}

// We don't want `OpaqueWriteFuture` leaking.
#[allow(private_interfaces)]
pub enum WriteResult<'a> {
  /// The write operation returned an error.
  Err(Error),
  EOF,
  Ready(usize),
  Future(OpaqueWriteFuture<'a>),
}

pub enum WriteResultSync {
  /// The write operation returned an error.
  Err(Error),
  /// No more bytes can be written and the buffer was returned.
  EOF(BufView),
  /// The buffer was fully written and consumed.
  Ready(usize),
  /// The buffer was fully or partially written and returned.
  ReadyReturn(usize, BufView),
}

impl WriteResultSync {
  pub fn into_write_result(self) -> Result<WriteOutcome, Error> {
    match self {
      WriteResultSync::EOF(view) => Ok(WriteOutcome::Partial {
        nwritten: 0,
        view,
      }),
      WriteResultSync::Ready(nwritten) => {
        Ok(WriteOutcome::Full { nwritten })
      }
      WriteResultSync::ReadyReturn(nwritten, view) => Ok(WriteOutcome::Partial {
        nwritten,
        view,
      }),
      WriteResultSync::Err(err) => Err(err),
    }
  }
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

  pub fn poll_writer<W: AsyncWrite + Unpin>(
    &self,
    cx: &mut Context<'_>,
    w: &mut W,
  ) -> Poll<WriteResult<'a>> {
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

  pub fn new_from_read_byob(buf: &'a mut BufMutView) -> Self {
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
      ReadContextBuf::Empty => panic!("ReadContext is no longer valid"),
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
