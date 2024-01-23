use deno_core::op2;
use deno_core::AsyncRefCell;
use deno_core::BoundedBufferChannel;
use deno_core::BufView;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::ReadContext;
use deno_core::ReadResult;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::WriteContext;
use deno_core::WriteResult;
use futures::FutureExt;
use tokio::io::ReadBuf;
use std::cell::RefCell;
use std::future::poll_fn;
use std::task::ready;
use std::task::Poll;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

struct PipeResource {
  tx: AsyncRefCell<BoundedBufferChannel>,
  rx: AsyncRefCell<BoundedBufferChannel>,
}

impl Resource for PipeResource {
  fn read_byob(
      self: std::rc::Rc<Self>,
      mut buf: deno_core::BufMutView,
    ) -> deno_core::AsyncResult<(usize, deno_core::BufMutView)> {
    async {
      let lock = RcRef::map(self, |this| &this.rx).borrow_mut().await;
      poll_fn(|cx| lock.poll_read_ready(cx)).await;
      let mut buf = ReadBuf::new(&mut buf);
      lock.read(&mut buf);
      unimplemented!()
    }.boxed_local()
  }

  fn write(self: std::rc::Rc<Self>, buf: BufView) -> deno_core::AsyncResult<deno_core::WriteOutcome> {
    async {
      let lock = RcRef::map(self, |this| &this.tx).borrow_mut().await;

      unimplemented!()
    }.boxed_local()

  }
//   fn poll_read<'a>(
//     &self,
//     cx: &mut std::task::Context,
//     read_context: &mut ReadContext<'a>,
//   ) -> Poll<ReadResult<'a>> {
//     let Some(next) = ready!(self.rx.borrow_mut().poll_recv(cx)) else {
//       return Poll::Ready(ReadResult::EOF);
//     };
//     Poll::Ready(ReadResult::ReadyBuf(next))
//   }

//   fn poll_write<'a>(
//     &self,
//     cx: &mut std::task::Context,
//     write_context: &mut WriteContext<'a>,
//   ) -> Poll<WriteResult<'a>> {
//     let buf = write_context.buf_owned();
//     let len = buf.len();
//     match self.tx.borrow_mut().send(buf) {
//       Err(err) => Poll::Ready(WriteResult::EOF),
//       Ok(_) => Poll::Ready(WriteResult::Ready(len)),
//     }
//   }
}

#[op2]
#[serde]
pub fn op_pipe_create(op_state: &mut OpState) -> (ResourceId, ResourceId) {
  let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
  let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
  let rid1 = op_state.resource_table.add(PipeResource {
    tx: tx1.into(),
    rx: rx2.into(),
  });
  let rid2 = op_state.resource_table.add(PipeResource {
    tx: tx2.into(),
    rx: rx1.into(),
  });
  (rid1, rid2)
}
