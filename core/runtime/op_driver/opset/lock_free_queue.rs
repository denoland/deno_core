use super::pointers::IndirectPtr;
use super::pointers::LoadStore;
use super::pointers::Ptr;
use parking_lot::lock_api::RawMutex;

/// An intrusive queue, requiring storage within the underlying node type. The underlying node type `N`
/// must implement [`IntrusiveQueueStorage`].
pub struct IntrusiveQueue<N>
where
  N: IntrusiveQueueStorage<N>,
{
  mutex: parking_lot::RawMutex,
  head: IndirectPtr<N>,
  tail: IndirectPtr<N>,
}

impl<N> Default for IntrusiveQueue<N>
where
  N: IntrusiveQueueStorage<N>,
{
  #[inline(always)]
  fn default() -> Self {
    Self {
      mutex: parking_lot::RawMutex::INIT,
      head: Default::default(),
      tail: Default::default(),
    }
  }
}

pub trait IntrusiveQueueStorage<N> {
  unsafe fn queue_node(node: *mut N) -> *mut IntrusiveQueueNodeStorage<N>;
}

pub struct IntrusiveQueueNodeStorage<N> {
  next: IndirectPtr<N>,
}

impl<N> Default for IntrusiveQueueNodeStorage<N> {
  #[inline(always)]
  fn default() -> Self {
    Self {
      next: Default::default(),
    }
  }
}

#[inline(always)]
unsafe fn next<N>(node: Ptr<N>) -> *const IndirectPtr<N>
where
  N: IntrusiveQueueStorage<N>,
{
  let queue_node = N::queue_node(node.into());
  std::ptr::addr_of!((*queue_node).next)
}

impl<N> IntrusiveQueue<N>
where
  N: IntrusiveQueueStorage<N>,
{
  pub unsafe fn pop(queue: *const IntrusiveQueue<N>) -> Ptr<N> {
    let mutex = std::ptr::addr_of!((*queue).mutex);
    (*mutex).lock();
    let head_ptr = std::ptr::addr_of!((*queue).head);
    let head = head_ptr.load();
    if head.is_null() {
      (*mutex).unlock();
      return head.into();
    }

    let next = next(head).load();

    if next.is_null() {
      head_ptr.store(Ptr::null());
      let tail_ptr = std::ptr::addr_of!((*queue).tail);
      tail_ptr.store(Ptr::null());
    } else {
      head_ptr.store(next);
    }

    (*mutex).unlock();
    head
  }

  pub unsafe fn push(queue: *const IntrusiveQueue<N>, node: Ptr<N>) {
    next(node).store(Ptr::null());

    let mutex = std::ptr::addr_of!((*queue).mutex);
    (*mutex).lock();

    let tail_ptr = std::ptr::addr_of!((*queue).tail);
    let tail = tail_ptr.load();
    if tail.is_null() {
      let head_ptr = std::ptr::addr_of!((*queue).head);
      head_ptr.store(node);
      tail_ptr.store(node);
    } else {
      next(tail).store(node);
      tail_ptr.store(node);
    }

    (*mutex).unlock();
  }
}

// #[inline(always)]
// unsafe fn load<N>(ptr: *const AtomicPtr<N>) -> Ptr<N> {
//   (*ptr).load(Ordering::Acquire)
// }

// #[inline(always)]
// unsafe fn store<N>(ptr: *const AtomicPtr<N>, value: Ptr<N>) {
//   (*ptr).store(value, Ordering::Release)
// }

// #[inline(always)]
// unsafe fn cas<N>(
//   ptr: *const AtomicPtr<N>,
//   current: Ptr<N>,
//   new: Ptr<N>,
// ) -> bool {
//   (*ptr)
//     .compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire)
//     .is_ok()
// }

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Barrier, Mutex};

  use super::*;

  // Mock implementations for IntrusiveQueueStorage and IntrusiveQueueNodeStorage
  #[derive(Default)]
  struct Node {
    value: usize,
    queue_node: IntrusiveQueueNodeStorage<Node>,
  }

  impl Node {
    fn new(value: usize) -> Self {
      Self {
        value,
        ..Default::default()
      }
    }
  }

  #[derive(Default)]
  struct MockQueue {
    queue: IntrusiveQueue<Node>,
  }

  unsafe impl Send for MockQueue {}
  unsafe impl Sync for MockQueue {}

  impl IntrusiveQueueStorage<Node> for Node {
    unsafe fn queue_node(
      node: *mut Node,
    ) -> *mut IntrusiveQueueNodeStorage<Node> {
      std::ptr::addr_of_mut!((*node).queue_node)
    }
  }

  unsafe fn make_node(i: usize) -> Ptr<Node> {
    Ptr::from(Box::into_raw(Box::new(Node::new(i))))
  }

  unsafe fn free_node(ptr: Ptr<Node>) -> usize {
    let node = Box::<Node>::from_raw(ptr.into());
    let value = node.value;
    drop(node);
    value
  }

  #[test]
  fn test_queue_operations() {
    let mock_queue = MockQueue::default();
    let queue = &mock_queue.queue as *const _;

    unsafe {
      let node = make_node(0);
      assert!(IntrusiveQueue::pop(queue).is_null());
      IntrusiveQueue::push(queue, node);
      let popped_node = IntrusiveQueue::pop(queue);
      assert_eq!(popped_node, node);
      assert_eq!(free_node(popped_node), 0);
      assert!(IntrusiveQueue::pop(queue).is_null());
    }
  }

  #[test]
  fn test_queue_operations_same_node() {
    let mock_queue = MockQueue::default();
    let queue = &mock_queue.queue as *const _;

    unsafe {
      let node = make_node(0);
      assert!(IntrusiveQueue::pop(queue).is_null());
      IntrusiveQueue::push(queue, node);
      let popped_node = IntrusiveQueue::pop(queue);
      assert_eq!(popped_node, node);
      IntrusiveQueue::push(queue, node);
      let popped_node = IntrusiveQueue::pop(queue);
      assert_eq!(popped_node, node);
      assert_eq!(free_node(popped_node), 0);
      assert!(IntrusiveQueue::pop(queue).is_null());
    }
  }

  #[test]
  fn test_queue_operations_more() {
    let mock_queue = MockQueue::default();
    let queue = &mock_queue.queue as *const _;

    unsafe {
      let mut nodes = vec![];
      for i in 0..10 {
        nodes.push(make_node(i));
      }

      for node in &nodes {
        IntrusiveQueue::push(queue, *node);
      }

      for (i, ptr) in nodes.iter().enumerate() {
        let popped_node = IntrusiveQueue::pop(queue);
        assert_eq!(popped_node, *ptr);
        assert_eq!(free_node(popped_node), i);
      }
      assert!(IntrusiveQueue::pop(queue).is_null());
    }
  }

  #[test]
  fn test_queue_operations_multithreaded() {
    const THREADS: usize = 20;
    const COUNT: usize = 10;

    let mock_queue = Arc::new(MockQueue::default());

    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = vec![];
    let nodes1 = Arc::new(Mutex::new(vec![]));
    let nodes2 = Arc::new(Mutex::new(vec![]));

    for _ in 0..THREADS {
      let mock_queue = Arc::clone(&mock_queue);
      let barrier = Arc::clone(&barrier);
      let nodes1 = nodes1.clone();
      let nodes2 = nodes2.clone();
      let handle = std::thread::spawn(move || {
        let queue = &mock_queue.queue as *const _;
        barrier.wait();

        unsafe {
          for i in 0..COUNT {
            let node = make_node(i);
            nodes1.lock().unwrap().push(node);
            IntrusiveQueue::push(queue, node);
            loop {
              let popped_node = IntrusiveQueue::pop(queue);
              if popped_node.is_null() {
                std::thread::yield_now();
                continue;
              }
              nodes2.lock().unwrap().push(popped_node);
              break;
            }
          }
        }
      });

      handles.push(handle);
    }

    for handle in handles {
      handle.join().unwrap();
    }

    let mut v1 = Arc::into_inner(nodes1).unwrap().into_inner().unwrap();
    v1.sort();
    let mut v2 = Arc::into_inner(nodes2).unwrap().into_inner().unwrap();
    v2.sort();
    assert_eq!(v1, v2);

    for v1 in v1.into_iter() {
      unsafe {
        free_node(v1);
      }
    }
  }
}
