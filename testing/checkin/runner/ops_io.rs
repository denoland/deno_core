use deno_core::op2;
use deno_core::BufMutView;
use deno_core::BufView;
use deno_core::ReadContext;
use deno_core::ReadResult;
use deno_core::Resource;
use futures::channel::mpsc::Receiver;
use futures::StreamExt;
use std::cell::RefCell;
use std::task::ready;
use std::task::Poll;
use tokio::sync::mpsc::Sender;

struct PipeResource {
  tx: RefCell<Sender<BufView>>,
  rx: RefCell<Receiver<BufView>>,
}

impl Resource for PipeResource {
  fn poll_read(
    &self,
    cx: &mut std::task::Context,
    read_context: &ReadContext,
    preferred_buffer: &mut BufMutView,
  ) -> Poll<ReadResult> {
    let Some(next) = ready!(self.rx.borrow_mut().poll_next_unpin(cx)) else {
      return Poll::Ready(ReadResult::ReadySync(Ok(BufMutView::default())));
    };
    Poll::Ready(ReadResult::ReadySync(Ok(next)))
  }

  fn poll_write(
    &self,
    cx: &mut std::task::Context,
    write_context: &WriteContext,
    buffer: deno_core::BufView,
  ) -> Poll<WriteResult> {
  }
}

#[op2]
#[serde]
pub fn create_pipe() -> (ResourceId, ResourceId) {}
