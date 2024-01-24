// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use deno_core::AsyncRefCell;
use deno_core::BoundedBufferChannel;
use deno_core::BufView;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::ReadContext;
use deno_core::ReadState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::WriteOutcome;
use futures::FutureExt;
use std::future::poll_fn;
use tokio::io::ReadBuf;

struct PipeResource {
  tx: AsyncRefCell<BoundedBufferChannel>,
  rx: AsyncRefCell<(BoundedBufferChannel, ReadState)>,
}

impl Resource for PipeResource {
  fn read_byob(
    self: std::rc::Rc<Self>,
    mut buf: deno_core::BufMutView,
  ) -> deno_core::AsyncResult<(usize, deno_core::BufMutView)> {
    async {
      let lock = RcRef::map(self, |this| &this.rx).borrow_mut().await;
      lock.1.poll(&mut buf)
      poll_fn(|cx| lock.poll_read_ready(cx)).await;
      let mut read_buf = ReadBuf::new(&mut buf);
      let res = lock.read(&mut read_buf);
      let len = read_buf.filled().len();
      res.into_read_byob_result(buf, len)
    }
    .boxed_local()
  }

  fn write(
    self: std::rc::Rc<Self>,
    buf: BufView,
  ) -> deno_core::AsyncResult<deno_core::WriteOutcome> {
    async {
      let lock = RcRef::map(self, |this| &this.tx).borrow_mut().await;
      poll_fn(|cx| lock.poll_write_ready(cx)).await;
      let res = lock.write(buf);
      res.into_write_result()
    }
    .boxed_local()
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
    rx: (rx2, ReadState::default()).into(),
  });
  let rid2 = op_state.resource_table.add(PipeResource {
    tx: tx2.into(),
    rx: (rx1, ReadState::default()).into(),
  });
  (rid1, rid2)
}
