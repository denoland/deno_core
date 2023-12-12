// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use bencher::*;
use deno_core::OpSet;
use futures::stream::FuturesOrdered;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

const LOOPS: usize = 10;
const COUNT: usize = 10;

async fn task() {
  tokio::task::yield_now().await;
  let mut v = vec![];
  for _ in 0..3 {
    v.push(TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(
      Ipv4Addr::LOCALHOST,
      0,
    ))));
  }
  drop(v);
}

fn bench_opset(b: &mut Bencher) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let mut opset = OpSet::<(), ()>::default();
  b.iter(|| {
    runtime.block_on(async {
      for _ in 0..LOOPS {
        let mut expect = 0;
        for _ in 0..COUNT {
          if opset.insert_op(0, task(), |_, x| x).is_pending() {
            expect += 1;
          }
        }
        for _ in 0..expect {
          opset.ready(&mut ()).await;
        }
      }

      opset.reset()
    });
  });
}

fn bench_futures_unordered(b: &mut Bencher) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let mut futures = FuturesUnordered::default();
  b.iter(|| {
    runtime.block_on(async {
      for _ in 0..LOOPS {
        for _ in 0..COUNT {
          futures.push(task());
        }
        for _ in 0..COUNT {
          futures.next().await;
        }
      }
    });
  });
}

fn bench_futures_ordered(b: &mut Bencher) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let mut futures = FuturesOrdered::default();
  b.iter(|| {
    runtime.block_on(async {
      for _ in 0..LOOPS {
        for _ in 0..COUNT {
          futures.push_back(task());
        }
        for _ in 0..COUNT {
          futures.next().await;
        }
      }
    });
  });
}

fn bench_joinset(b: &mut Bencher) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let mut futures = JoinSet::default();
  b.iter(|| {
    runtime.block_on(async {
      for _ in 0..LOOPS {
        for _ in 0..COUNT {
          futures.spawn(task());
        }
        for _ in 0..COUNT {
          futures.join_next().await;
        }
      }
    });
  });
}

fn bench_unicycle(b: &mut Bencher) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let mut futures = unicycle::FuturesUnordered::new();
  b.iter(|| {
    runtime.block_on(async {
      for _ in 0..LOOPS {
        for _ in 0..COUNT {
          futures.push(task());
        }
        for _ in 0..COUNT {
          futures.next().await;
        }
      }
    });
  });
}

benchmark_main!(benches);

benchmark_group!(
  benches,
  bench_opset,
  bench_futures_ordered,
  bench_futures_unordered,
  bench_joinset,
  bench_unicycle,
);
