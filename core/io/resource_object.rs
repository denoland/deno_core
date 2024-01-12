// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::resource::OpaqueReadFuture;
use super::resource::ReadContext;
use super::resource::ReadResult;
use super::BufMutViewWhole;
use crate::buffer_strategy::AdaptiveBufferStrategy;
use crate::error::not_supported;
use crate::io::BufMutView;
use crate::io::BufView;
use crate::AsyncRefCell;
use crate::Resource;
use crate::ResourceHandle;
use crate::io::resource;
use anyhow::bail;
use anyhow::Error;
use bytes::Buf;
use bytes::BytesMut;
use cooked_waker::ViaRawPointer;
use std::borrow::Cow;
use std::fs::File;
use std::future::poll_fn;
use std::io::ErrorKind;
use std::io::Read;
use std::marker::PhantomData;
use std::net::TcpStream;
use std::os::fd::FromRawFd;
use std::pin::Pin;
use std::ptr::NonNull;
use std::rc::Rc;
use std::task::Context;
use std::task::ready;
use std::task::Poll;
use tokio::io::ReadBuf;

#[derive(Default)]
struct ReadState {
  partial: Option<BufView>,
}

pub struct ResourceObject<T: Resource + ?Sized> {
  resource: Box<dyn Resource>,
  close_fn: fn(Box<dyn Resource>),
  backing_handle: Option<ResourceHandle>,
  async_read: Option<NonNull<dyn tokio::io::AsyncRead>>,
  async_write: Option<NonNull<dyn tokio::io::AsyncWrite>>,
  read_lock: Rc<AsyncRefCell<ReadState>>,
  write_lock: Rc<AsyncRefCell<()>>,
  _type: PhantomData<T>,
}

impl<T: Resource + ?Sized> ResourceObject<T> {
  /// Resources backed by a file descriptor or socket handle can let ops know
  /// to allow for low-level optimizations. If a backing handle is provided, the
  /// read and write methods are skipped.
  pub fn backing_handle(&self) -> Option<ResourceHandle> {
    self.backing_handle
  }

  pub fn name(&self) -> Cow<str> {
    self.resource.name()
  }

  pub fn close(self) {
    (self.close_fn)(self.resource)
  }

  pub async fn write(&self, buf: BufView) -> Result<usize, Error> {
    unimplemented!()
  }

  pub async fn read(&self, buf: &mut BufMutViewWhole) -> Result<usize, Error> {
    let mut read_state = self.read_lock.borrow_mut().await;
    if let Some(mut r) = self.async_read {
      let res = poll_fn(|cx| unsafe {
        let mut buf = ReadBuf::new(buf);
        let res =
          ready!(Pin::new_unchecked(r.as_mut()).poll_read(cx, &mut buf));
        Poll::Ready(res.map(|_| buf.filled().len()))
      })
      .await;
      res.map_err(|e| e.into())
    } else {
      if let Some(mut retbuf) = read_state.partial.take() {
        let len = retbuf.len();
        retbuf.copy_to_slice(buf);
        if len > buf.len() {
          read_state.partial = Some(retbuf.split_off(buf.len()));
          return Ok(buf.len());
        } else {
          return Ok(len);
        }
      }
      let read_context = &ReadContext {  };
      let mut maybe_future: Option<OpaqueReadFuture> = read_context.x();
      let mut read_buf = read_context.y(buf);

      fn do_poll<'a, 'b, 'c, T: Resource + ?Sized>(cx: &mut Context, resource: &T, read_context: &'a ReadContext, maybe_future: &'c mut Option<OpaqueReadFuture<'a>>, read_buf: &mut ReadBuf<'b>) -> Poll<ReadResult<'a>> {
        loop {
          let res: ReadResult<'a> = ready!(if let Some(future) = maybe_future.as_ref() {
            future.poll_read(cx, read_context, read_buf)
          } else {
            resource.poll_read(
              cx,
              &read_context,
              read_buf
            )
          });

          break match res {
            ReadResult::PollAgain => { continue; },
            ReadResult::Future(future) => {
              *maybe_future = Some(future);
              continue;
            },
            res => Poll::Ready(res),
          };
        }
      }

      let res = poll_fn(|cx| {
        do_poll(cx, self.resource.as_ref(), read_context, &mut maybe_future, &mut read_buf)
      }).await;
      drop(maybe_future);
      match res {
        ReadResult::EOF => Ok(0),
        ReadResult::Ready => Ok(read_buf.filled().len()),
        ReadResult::Err(err) => Err(err),
        ReadResult::ReadyBuf(mut retbuf) => {
          drop(read_buf);
          let len = retbuf.len();
          retbuf.copy_to_slice(buf);
          if len > buf.len() {
            read_state.partial =
              Some(retbuf.split_off(buf.len()));
            Ok(buf.len())
          } else {
            Ok(len)
          }
        }
        ReadResult::ReadyBufMut(mut retbuf) => {
          let len = retbuf.len();
          retbuf.copy_to_slice(buf);
          if len > buf.len() {
            read_state.partial =
              Some(retbuf.split_off(buf.len()).into_view());
            Ok(buf.len())
          } else {
            Ok(len)
          }
        },
        _ => unreachable!()
      }
    }
  }

  pub async fn write_all(&self, buf: BufView) -> Result<(), Error> {
    unimplemented!()
  }

  pub async fn read_all(&self) -> Result<BytesMut, Error> {
    let (min, maybe_max) = self.resource.size_hint();
    let mut buffer_strategy =
      AdaptiveBufferStrategy::new_from_hint_u64(min, maybe_max);
    let mut buf = BufMutView::new(buffer_strategy.buffer_size());

    // loop {
    //   #[allow(deprecated)]
    //   buf.maybe_grow(buffer_strategy.buffer_size()).unwrap();

    //   let (n, new_buf) = resource.clone().read_byob(buf).await?;
    //   buf = new_buf;
    //   buf.advance_cursor(n);
    //   if n == 0 {
    //     break;
    //   }

    //   buffer_strategy.notify_read(n);
    // }

    // let nread = buf.reset_cursor();
    // // If the buffer is larger than the amount of data read, shrink it to the
    // // amount of data read.
    // buf.truncate(nread);

    // Ok(buf.maybe_unwrap_bytes().unwrap())
    unimplemented!()
  }

  pub fn read_sync(&self, mut buf: BufMutView) -> Result<usize, Error> {
    let Some(_lock) = self.read_lock.try_borrow() else {
      bail!("resource locked");
    };
    if let Some(handle) = self.backing_handle {
      match handle {
        ResourceHandle::Fd(fd) => {
          let mut f = unsafe { File::from_raw_fd(fd) };
          let res = f.read(&mut buf);
          std::mem::forget(f);
          match res {
            Ok(n) => Ok(n),
            Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(0),
            Err(err) => Err(err.into()),
          }
        }
        ResourceHandle::Socket(sock) => {
          let mut s = unsafe { TcpStream::from_raw_fd(sock) };
          let res = s.read(&mut buf);
          std::mem::forget(s);
          match res {
            Ok(n) => Ok(n),
            Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(0),
            Err(err) => Err(err.into()),
          }
        }
      }
    } else {
      Err(not_supported())
    }
  }

  pub fn write_sync(&self, buf: BufView) -> Result<usize, Error> {
    unimplemented!()
  }

  pub async fn shutdown(&self) -> Result<(), Error> {
    Resource::shutdown(self.resource.as_ref()).await
  }
}

impl<T: Resource> std::ops::Deref for ResourceObject<T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    // SAFETY: We know this is a T
    unsafe { &*(NonNull::from(self.resource.as_ref()).as_ptr() as *const T) }
  }
}

impl<T: Resource> ResourceObject<T> {
  pub fn new(t: T) -> Self {
    let (r, w) = if let Some(backing_handle) = t.backing_handle() {
      match backing_handle {
        ResourceHandle::Fd(h) => unsafe {
          let f = Box::new(tokio::fs::File::from_raw_fd(h)).into_raw();
          let r =
            (Box::from_raw(f) as Box<dyn tokio::io::AsyncRead>).into_raw();
          let w =
            (Box::from_raw(f) as Box<dyn tokio::io::AsyncWrite>).into_raw();
          (NonNull::new(r), NonNull::new(w))
        },
        ResourceHandle::Socket(h) => unsafe {
          let f = Box::new(
            tokio::net::TcpStream::from_std(std::net::TcpStream::from_raw_fd(
              h,
            ))
            .unwrap(),
          )
          .into_raw();
          let r =
            (Box::from_raw(f) as Box<dyn tokio::io::AsyncRead>).into_raw();
          let w =
            (Box::from_raw(f) as Box<dyn tokio::io::AsyncWrite>).into_raw();
          (NonNull::new(r), NonNull::new(w))
        },
      }
    } else {
      (None, None)
    };

    // TODO(dupe)
    let backing_handle = t.backing_handle();

    ResourceObject {
      resource: Box::new(t) as _,
      backing_handle,
      close_fn: |b| unsafe {
        let t = *Box::from_raw(b.into_raw() as *mut T);
        t.close();
      },
      async_read: r,
      async_write: w,
      read_lock: Default::default(),
      write_lock: Default::default(),
      _type: PhantomData,
    }
  }

  pub fn into_inner(self) -> T {
    // SAFETY: We know this is a T
    unsafe { *Box::from_raw(self.resource.into_raw() as *mut T) }
  }

  pub fn as_dyn(self) -> ResourceObject<dyn Resource> {
    // SAFETY: We store a dyn Resource so the representation is identical
    unsafe { std::mem::transmute(self) }
  }

  pub fn as_rc_dyn(self: Rc<Self>) -> Rc<ResourceObject<dyn Resource>> {
    // SAFETY: We can safely transmute to a `dyn Resource` because it has the same representation
    unsafe { Rc::from_raw(self.into_raw() as _) }
  }
}

impl ResourceObject<dyn Resource> {
  pub fn downcast<T: Resource>(self) -> Result<ResourceObject<T>, Self> {
    if self.resource.is::<T>() {
      // SAFETY: We can safely transmute from a `dyn Resource` because it has the same representation
      Ok(unsafe { std::mem::transmute(self) })
    } else {
      Err(self)
    }
  }

  pub fn downcast_rc<'a, T: Resource>(
    self: &'a Rc<Self>,
  ) -> Option<&'a Rc<ResourceObject<T>>> {
    if self.resource.is::<T>() {
      // SAFETY: We can safely transmute to a `dyn Resource` because it has the same representation
      Some(unsafe { std::mem::transmute(self) })
    } else {
      None
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{cell::RefCell, ops::DerefMut};
  use tokio::io::AsyncRead;

  use super::*;

  struct FileResource(File);
  impl Resource for FileResource {
    fn backing_handle(&self) -> Option<ResourceHandle> {
      Some(ResourceHandle::from_fd_like(&self.0))
    }
  }

  struct FileResourcePoll(RefCell<tokio::fs::File>);
  impl Resource for FileResourcePoll {
    fn poll_read<'a>(
      &self,
      cx: &mut std::task::Context,
      _read_context: &'a ReadContext,
      preferred_buffer: &mut ReadBuf,
    ) -> Poll<ReadResult<'a>> {
      match ready!(
        Pin::new(&mut *self.0.borrow_mut()).poll_read(cx, preferred_buffer)
      ) {
        Ok(_) => Poll::Ready(ReadResult::Ready),
        Err(err) => Poll::Ready(ReadResult::Err(err.into())),
      }
    }
  }

  struct FileResourcePollOwnBuf(RefCell<tokio::fs::File>);
  impl Resource for FileResourcePollOwnBuf {
    fn poll_read<'a>(
      &self,
      cx: &mut std::task::Context,
      _read_context: &'a ReadContext,
      preferred_buffer: &mut ReadBuf,
    ) -> Poll<ReadResult<'a>> {
      // Copy the incoming capacity
      let mut buf = BufMutView::new(preferred_buffer.capacity());
      let mut read_buf = ReadBuf::new(&mut buf);
      match ready!(
        Pin::new(&mut *self.0.borrow_mut()).poll_read(cx, &mut read_buf)
      ) {
        Ok(n) => {
          let n = read_buf.filled().len();
          buf.reset_cursor();
          buf.truncate(n);
          Poll::Ready(ReadResult::ReadyBufMut(buf))
        },
        Err(err) => Poll::Ready(ReadResult::Err(err.into())),
      }
    }
  }

  struct FileResourcePollFuture(Rc<RefCell<tokio::fs::File>>);
  impl Resource for FileResourcePollFuture {
    fn poll_read<'a>(
      &self,
      cx: &mut std::task::Context,
      read_context: &'a ReadContext,
      preferred_buffer: &mut ReadBuf,
    ) -> Poll<ReadResult<'a>> {
      let f = self.0.clone();
      read_context.read_future(|mut buf| async move {
        let mut f = f.borrow_mut();
        let res = poll_fn(|cx| {
          buf.poll_reader(cx, f.deref_mut())
        }).await;
        ReadResult::Ready
      })
    }
  }

  #[tokio::test]
  async fn test_resource_read() {
    let mut expected = [0; 1024];
    File::open("Cargo.toml")
      .unwrap()
      .read_exact(&mut expected)
      .unwrap();

    let resource = FileResource(File::open("Cargo.toml").unwrap());
    let resource = ResourceObject::new(resource);

    let mut buf = BufMutViewWhole::new(expected.len());
    let res = resource.read(&mut buf).await;
    assert_eq!(1024, res.unwrap());
    assert_eq!(expected.as_ref(), buf.as_ref());
  }

  #[tokio::test]
  async fn test_resource_read_poll() {
    let mut expected = [0; 1024];
    File::open("Cargo.toml")
      .unwrap()
      .read_exact(&mut expected)
      .unwrap();

    let resource = FileResourcePoll(RefCell::new(tokio::fs::File::from_std(
      File::open("Cargo.toml").unwrap(),
    )));
    let resource = ResourceObject::new(resource);

    let mut buf = BufMutViewWhole::new(expected.len());
    let res = resource.read(&mut buf).await;
    assert_eq!(1024, res.unwrap());
    assert_eq!(expected.as_ref(), buf.as_ref());
  }

  #[tokio::test]
  async fn test_resource_read_poll_ownbuf() {
    let mut expected = [0; 1024];
    File::open("Cargo.toml")
      .unwrap()
      .read_exact(&mut expected)
      .unwrap();

    let resource = FileResourcePollOwnBuf(RefCell::new(tokio::fs::File::from_std(
      File::open("Cargo.toml").unwrap(),
    )));
    let resource = ResourceObject::new(resource);

    let mut buf = BufMutViewWhole::new(expected.len());
    let res = resource.read(&mut buf).await;
    assert_eq!(1024, res.unwrap());
    assert_eq!(expected.as_ref(), buf.as_ref());
  }
}
