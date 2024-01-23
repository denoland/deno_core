// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::AnyError;
use crate::io::ReadResult;
use crate::BufView;
use deno_unsync::UnsyncWaker;
use std::cell::RefCell;
use std::cell::RefMut;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;
use tokio::io::ReadBuf;

// How many buffers we'll allow in the channel before we stop allowing writes.
const BUFFER_CHANNEL_SIZE: u16 = 1024;

// How much data is in the channel before we stop allowing writes.
const BUFFER_BACKPRESSURE_LIMIT: usize = 64 * 1024;

// Optimization: prevent multiple small writes from adding overhead.
//
// If the total size of the channel is less than this value and there is more than one buffer available
// to read, we will allocate a buffer to store the entire contents of the channel and copy each value from
// the channel rather than yielding them one at a time.
const BUFFER_AGGREGATION_LIMIT: usize = 1024;

struct BoundedBufferChannelInner {
  buffers: [MaybeUninit<BufView>; BUFFER_CHANNEL_SIZE as _],
  ring_producer: u16,
  ring_consumer: u16,
  error: Option<AnyError>,
  current_size: usize,
  // TODO(mmastrac): we can math this field instead of accounting for it
  len: usize,
  closed: bool,

  read_waker: UnsyncWaker,
  write_waker: UnsyncWaker,

  _unsend: PhantomData<std::sync::MutexGuard<'static, ()>>,
}

impl Default for BoundedBufferChannelInner {
  fn default() -> Self {
    Self::new()
  }
}

impl std::fmt::Debug for BoundedBufferChannelInner {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_fmt(format_args!(
      "[BoundedBufferChannel closed={} error={:?} ring={}->{} len={} size={}]",
      self.closed,
      self.error,
      self.ring_producer,
      self.ring_consumer,
      self.len,
      self.current_size
    ))
  }
}

impl BoundedBufferChannelInner {
  pub fn new() -> Self {
    const UNINIT: MaybeUninit<BufView> = MaybeUninit::uninit();
    Self {
      buffers: [UNINIT; BUFFER_CHANNEL_SIZE as _],
      ring_producer: 0,
      ring_consumer: 0,
      len: 0,
      closed: false,
      error: None,
      current_size: 0,
      read_waker: UnsyncWaker::default(),
      write_waker: UnsyncWaker::default(),
      _unsend: PhantomData,
    }
  }

  /// # Safety
  ///
  /// This doesn't check whether `ring_consumer` is valid, so you'd better make sure it is before
  /// calling this.
  #[inline(always)]
  unsafe fn next_unsafe(&mut self) -> &mut BufView {
    self
      .buffers
      .get_unchecked_mut(self.ring_consumer as usize)
      .assume_init_mut()
  }

  /// # Safety
  ///
  /// This doesn't check whether `ring_consumer` is valid, so you'd better make sure it is before
  /// calling this.
  #[inline(always)]
  unsafe fn take_next_unsafe(&mut self) -> BufView {
    let res = std::ptr::read(self.next_unsafe());
    self.ring_consumer = (self.ring_consumer + 1) % BUFFER_CHANNEL_SIZE;

    res
  }

  fn drain(&mut self, mut f: impl FnMut(BufView)) {
    while self.ring_producer != self.ring_consumer {
      // SAFETY: We know the ring indexes are valid
      let res = unsafe { std::ptr::read(self.next_unsafe()) };
      self.ring_consumer = (self.ring_consumer + 1) % BUFFER_CHANNEL_SIZE;
      f(res);
    }
    self.current_size = 0;
    self.ring_producer = 0;
    self.ring_consumer = 0;
    self.len = 0;
  }

  pub fn read(&mut self, buf: &mut ReadBuf) -> ReadResult<'static> {
    // Empty buffers will return the error, if one exists, or None
    if self.len == 0 {
      if let Some(error) = self.error.take() {
        return ReadResult::Err(error);
      } else {
        if self.closed {
          return ReadResult::EOF;
        } else {
          // Spurious wakeup
          return ReadResult::PollAgain;
        }
      }
    }

    // If we have less than the aggregation limit AND we have more than one buffer in the channel,
    // aggregate and return everything in a single buffer.
    if self.current_size <= BUFFER_AGGREGATION_LIMIT
      && self.current_size <= buf.remaining()
      && self.len > 1
    {
      self.drain(|slice| {
        buf.put_slice(slice.as_ref());
      });

      // We can always write again
      self.write_waker.wake();

      return ReadResult::Ready;
    }

    // SAFETY: We know this exists
    let buf = unsafe { self.take_next_unsafe() };
    self.current_size -= buf.len();
    self.len -= 1;

    // If current_size is zero, len must be zero (and if not, len must not be)
    debug_assert!(
      !((self.current_size == 0) ^ (self.len == 0)),
      "Length accounting mismatch: {self:?}"
    );

    // We may be able to write again if we have buffer and byte room in the channel
    if self.can_write() {
      self.write_waker.wake();
    }

    ReadResult::ReadyBuf(buf)
  }

  pub fn write(&mut self, buffer: BufView) -> Result<(), BufView> {
    let next_producer_index = (self.ring_producer + 1) % BUFFER_CHANNEL_SIZE;
    if next_producer_index == self.ring_consumer {
      // Note that we may have been allowed to write because of a close/error condition, but the
      // underlying channel is actually closed. If this is the case, we return `Ok(())`` and just
      // drop the bytes on the floor.
      return if self.closed || self.error.is_some() {
        Ok(())
      } else {
        Err(buffer)
      };
    }

    self.current_size += buffer.len();

    // SAFETY: we know the ringbuffer bounds are correct
    unsafe {
      *self.buffers.get_unchecked_mut(self.ring_producer as usize) =
        MaybeUninit::new(buffer)
    };
    self.ring_producer = next_producer_index;
    self.len += 1;
    debug_assert!(self.ring_producer != self.ring_consumer);
    self.read_waker.wake();
    Ok(())
  }

  pub fn write_error(&mut self, error: AnyError) {
    self.error = Some(error);
    self.read_waker.wake();
  }

  #[inline(always)]
  pub fn can_read(&self) -> bool {
    // Read will return if:
    //  - the stream is closed
    //  - there is an error
    //  - the stream is not empty
    self.closed
      || self.error.is_some()
      || self.ring_consumer != self.ring_producer
  }

  #[inline(always)]
  pub fn can_write(&self) -> bool {
    // Write will return if:
    //  - the stream is closed
    //  - there is an error
    //  - the stream is not full (either buffer or byte count)
    let next_producer_index = (self.ring_producer + 1) % BUFFER_CHANNEL_SIZE;
    self.closed
      || self.error.is_some()
      || (next_producer_index != self.ring_consumer
        && self.current_size < BUFFER_BACKPRESSURE_LIMIT)
  }

  pub fn poll_read_ready(&mut self, cx: &mut Context) -> Poll<()> {
    if !self.can_read() {
      self.read_waker.register(cx.waker());
      Poll::Pending
    } else {
      Poll::Ready(())
    }
  }

  pub fn poll_write_ready(&mut self, cx: &mut Context) -> Poll<()> {
    if !self.can_write() {
      self.write_waker.register(cx.waker());
      Poll::Pending
    } else {
      Poll::Ready(())
    }
  }

  pub fn close(&mut self) {
    self.closed = true;
    // Wake up reads and writes, since they'll both be able to proceed forever now
    self.write_waker.wake();
    self.read_waker.wake();
  }
}

/// A unidirectional channel that provides a read and write end to collect [`V8Slice`]s
/// and provide them to read operations.
///
/// The channel optimizes reads after small writes where possible.
#[repr(transparent)]
#[derive(Clone, Default)]
pub struct BoundedBufferChannel {
  inner: Rc<RefCell<BoundedBufferChannelInner>>,
}

impl BoundedBufferChannel {
  // TODO(mmastrac): in release mode we should be able to make this an UnsafeCell
  #[inline(always)]
  fn inner(&self) -> RefMut<BoundedBufferChannelInner> {
    self.inner.borrow_mut()
  }

  pub fn read<'a>(&self, buf: &mut ReadBuf<'a>) -> ReadResult<'static> {
    self.inner().read(buf)
  }

  pub fn write(&self, buffer: BufView) -> Result<(), BufView> {
    self.inner().write(buffer)
  }

  pub fn write_error(&self, error: AnyError) {
    self.inner().write_error(error)
  }

  pub fn can_write(&self) -> bool {
    self.inner().can_write()
  }

  pub fn poll_read_ready(&self, cx: &mut Context) -> Poll<()> {
    self.inner().poll_read_ready(cx)
  }

  pub fn poll_write_ready(&self, cx: &mut Context) -> Poll<()> {
    self.inner().poll_write_ready(cx)
  }

  pub fn closed(&self) -> bool {
    self.inner().closed
  }

  #[cfg(test)]
  pub fn byte_size(&self) -> usize {
    self.inner().current_size
  }

  pub fn close(&self) {
    self.inner().close()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::future::poll_fn;
  use std::sync::atomic::AtomicUsize;
  use std::time::Duration;

  fn create_buffer(byte_length: usize) -> BufView {
    BufView::from(vec![0; byte_length])
  }

  #[test]
  fn test_bounded_buffer_channel() {
    let channel = BoundedBufferChannel::default();

    for _ in 0..BUFFER_CHANNEL_SIZE - 1 {
      channel.write(create_buffer(1024)).unwrap();
    }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn test_multi_task() {
    let channel = BoundedBufferChannel::default();
    let channel_send = channel.clone();

    // Fast writer
    let a = deno_core::unsync::spawn(async move {
      for _ in 0..BUFFER_CHANNEL_SIZE * 2 {
        poll_fn(|cx| channel_send.poll_write_ready(cx)).await;
        channel_send
          .write(create_buffer(BUFFER_AGGREGATION_LIMIT))
          .unwrap();
      }
    });

    // Slightly slower reader
    let b = deno_core::unsync::spawn(async move {
      for _ in 0..BUFFER_CHANNEL_SIZE * 2 {
        tokio::time::sleep(Duration::from_micros(100)).await;
        poll_fn(|cx| channel.poll_read_ready(cx)).await;
        let mut buf = [0; BUFFER_AGGREGATION_LIMIT];
        let mut buf = ReadBuf::new(&mut buf);
        channel.read(&mut buf);
      }
    });

    a.await.unwrap();
    b.await.unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn test_multi_task_small_reads() {
    let channel = BoundedBufferChannel::default();
    let channel_send = channel.clone();

    let total_send = Rc::new(AtomicUsize::new(0));
    let total_send_task = total_send.clone();
    let total_recv = Rc::new(AtomicUsize::new(0));
    let total_recv_task = total_recv.clone();

    // Fast writer
    let a = deno_core::unsync::spawn(async move {
      for _ in 0..BUFFER_CHANNEL_SIZE * 2 {
        poll_fn(|cx| channel_send.poll_write_ready(cx)).await;
        channel_send.write(create_buffer(16)).unwrap();
        total_send_task.fetch_add(16, std::sync::atomic::Ordering::SeqCst);
      }
      // We need to close because we may get aggregated packets and we want a signal
      channel_send.close();
    });

    // Slightly slower reader
    let b = deno_core::unsync::spawn(async move {
      for _ in 0..BUFFER_CHANNEL_SIZE * 2 {
        poll_fn(|cx| channel.poll_read_ready(cx)).await;
        // We want to make sure we're aggregating at least some packets
        while channel.byte_size() <= 16 && !channel.closed() {
          tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let mut buf = [0; 1024];
        let mut buf = ReadBuf::new(&mut buf);
        let len = match channel.read(&mut buf) {
          ReadResult::Ready => buf.filled().len(),
          ReadResult::EOF => break,
          ReadResult::ReadyBuf(buf) => buf.len(),
          _ => unreachable!(),
        };
        total_recv_task.fetch_add(len, std::sync::atomic::Ordering::SeqCst);
      }
    });

    a.await.unwrap();
    b.await.unwrap();

    assert_eq!(
      total_send.load(std::sync::atomic::Ordering::SeqCst),
      total_recv.load(std::sync::atomic::Ordering::SeqCst)
    );
  }
}
