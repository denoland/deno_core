// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use deno_core::AsyncRefCell;
use deno_core::BufView;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::WriteOutcome;
use futures::FutureExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::DuplexStream;
use tokio::io::WriteHalf;
use tokio::io::ReadHalf;

struct PipeResource {
  tx: AsyncRefCell<WriteHalf<DuplexStream>>,
  rx: AsyncRefCell<ReadHalf<DuplexStream>>,
}

impl Resource for PipeResource {
  fn read_byob(
    self: std::rc::Rc<Self>,
    mut buf: deno_core::BufMutView,
  ) -> deno_core::AsyncResult<(usize, deno_core::BufMutView)> {
    async {
      let mut lock = RcRef::map(self, |this| &this.rx).borrow_mut().await;
      // Note that we're holding a slice across an await point, so this code is very much not safe
      let res = lock.read(&mut buf).await?;
      Ok((res, buf))
    }.boxed_local()
  }

  fn write(
    self: std::rc::Rc<Self>,
    buf: BufView,
  ) -> deno_core::AsyncResult<deno_core::WriteOutcome> {
    async {
      let mut lock = RcRef::map(self, |this| &this.tx).borrow_mut().await;
      let nwritten = lock.write(&buf).await?;
      Ok(WriteOutcome::Partial { nwritten, view: buf })
    }
    .boxed_local()
  }
}

#[op2]
#[serde]
pub fn op_pipe_create(op_state: &mut OpState) -> (ResourceId, ResourceId) {
  let (s1, s2) = tokio::io::duplex(1024);
  let (rx1, tx1) = tokio::io::split(s1);
  let (rx2, tx2) = tokio::io::split(s2);
  let rid1 = op_state.resource_table.add(PipeResource {
    rx: AsyncRefCell::new(rx1),
    tx: AsyncRefCell::new(tx1),
  });
  let rid2 = op_state.resource_table.add(PipeResource {
    rx: AsyncRefCell::new(rx2),
    tx: AsyncRefCell::new(tx2),
  });
  (rid1, rid2)
}
