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
use deno_core::WriteOutcome;
use deno_core::WriteResult;
use futures::FutureExt;
use tokio::io::ReadBuf;
use std::cell::RefCell;
use std::future::poll_fn;
use std::task::ready;
use std::task::Poll;

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
      loop {
        poll_fn(|cx| lock.poll_read_ready(cx)).await;
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = lock.read(&mut read_buf);
        break match res {
          ReadResult::PollAgain => continue,
          ReadResult::Ready => {
            Ok((read_buf.filled().len(), buf))
          },
          ReadResult::Err(err) => Err(err),
          ReadResult::EOF => Ok((0, buf)),
          ReadResult::ReadyBuf(buf) => unreachable!(),
          ReadResult::ReadyBufMut(buf) => unreachable!(),
          ReadResult::Future(..) => unreachable!()
        }
      }
    }.boxed_local()
  }

  fn write(self: std::rc::Rc<Self>, mut buf: BufView) -> deno_core::AsyncResult<deno_core::WriteOutcome> {
    async {
      let lock = RcRef::map(self, |this| &this.tx).borrow_mut().await;
      loop {
        poll_fn(|cx| lock.poll_write_ready(cx)).await;
        let nwritten = buf.len();
        if let Err(err_buf) = lock.write(buf) {
          buf = err_buf;
          continue;
        }
        break Ok(WriteOutcome::Full { nwritten });
      }
    }.boxed_local()
  }
}

#[op2]
#[serde]
pub fn op_pipe_create(op_state: &mut OpState) -> (ResourceId, ResourceId) {
  let tx1 = BoundedBufferChannel::default();
  let rx1 = tx1.clone();
  let tx2 = BoundedBufferChannel::default();
  let rx2 = tx2.clone();
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
