use deno_core::op2;
use deno_core::BufView;
use deno_core::OpState;
use deno_core::ReadContext;
use deno_core::ReadResult;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::WriteContext;
use deno_core::WriteResult;
use std::cell::RefCell;
use std::task::ready;
use std::task::Poll;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

struct PipeResource {
  tx: RefCell<UnboundedSender<BufView>>,
  rx: RefCell<UnboundedReceiver<BufView>>,
}

impl Resource for PipeResource {
  fn poll_read<'a>(
    &self,
    cx: &mut std::task::Context,
    read_context: &mut ReadContext<'a>,
  ) -> Poll<ReadResult<'a>> {
    let Some(next) = ready!(self.rx.borrow_mut().poll_recv(cx)) else {
      return Poll::Ready(ReadResult::EOF);
    };
    Poll::Ready(ReadResult::ReadyBuf(next))
  }

  fn poll_write<'a>(
    &self,
    cx: &mut std::task::Context,
    write_context: &mut WriteContext<'a>,
  ) -> Poll<WriteResult<'a>> {
    let buf = write_context.buf_owned();
    let len = buf.len();
    match self.tx.borrow_mut().send(buf) {
      Err(err) => Poll::Ready(WriteResult::EOF),
      Ok(_) => Poll::Ready(WriteResult::Ready(len)),
    }
  }
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
