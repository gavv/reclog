use lockfree_object_pool::{LinearObjectPool, LinearOwnedReusable};
use ringbuffer::{AllocRingBuffer, RingBuffer};
use std::sync::{Arc, Condvar, Mutex};

/// Buffer is a mutable string + a reference to owning buffer pool.
pub type Buffer = LinearOwnedReusable<String>;

/// Thread-safe buffer pool.
pub struct BufferPool {
    obj_pool: Arc<LinearObjectPool<String>>,
}

impl BufferPool {
    /// Construct buffer pool.
    pub fn new() -> Self {
        BufferPool {
            obj_pool: Arc::new(LinearObjectPool::new(
                || String::new(),
                |s| {
                    s.clear();
                },
            )),
        }
    }

    // Allocate empty buffer.
    // Returns a wrapped String.
    // Returned string has zero size (but typically non-zero capacity).
    // When returned struct is dropped, the string is automatically
    // returned to the pool.
    pub fn alloc(&self) -> Buffer {
        self.obj_pool.pull_owned()
    }
}

/// Thread-safe bounded buffer queue.
pub struct BufferQueue {
    state: Mutex<BufferQueueState>, // protected state
    cond: Condvar,
}

struct BufferQueueState {
    ringbuf: AllocRingBuffer<Buffer>,
    closed: bool,
}

impl BufferQueue {
    /// Construct queue with specified maxium size.
    pub fn new(queue_size: usize) -> Self {
        BufferQueue {
            state: Mutex::new(BufferQueueState {
                ringbuf: AllocRingBuffer::new(queue_size),
                closed: false,
            }),
            cond: Condvar::new(),
        }
    }

    /// Read buffer from queue.
    /// Blocks until queue is non-empty or is empty and closed.
    /// Returns None if queue is empty and closed.
    pub fn read(&self) -> Option<Buffer> {
        loop {
            let mut locked_state = self.state.lock().unwrap();

            match locked_state.ringbuf.dequeue() {
                Some(buf) => return Some(buf),
                None => {
                    if locked_state.closed {
                        // Queue empty and closed.
                        return None;
                    } else {
                        // Queue empty, but not closed.
                        drop(self.cond.wait(locked_state).unwrap());
                        continue;
                    }
                }
            };
        }
    }

    /// Write buffer to queue.
    /// Wakes up blocked reads.
    pub fn write(&self, buf: Buffer) {
        let mut locked_state = self.state.lock().unwrap();

        if locked_state.closed {
            return;
        }

        locked_state.ringbuf.enqueue(buf);
        self.cond.notify_all();
    }

    /// Closes queue.
    pub fn close(&self) {
        let mut locked_state = self.state.lock().unwrap();

        if locked_state.closed {
            return;
        }

        locked_state.closed = true;

        self.cond.notify_all();
    }
}
