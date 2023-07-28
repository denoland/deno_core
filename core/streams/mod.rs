mod buffer_manager;
mod stream;

enum StreamPromise {
  // Stream 
  Read(usize),
  ReadBuf(usize, bytes::Bytes),
  Future(StreamTask)
}
