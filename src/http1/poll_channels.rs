use std::ops::Drop;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, mpsc};
use std::task::{Context, Poll, Waker};

struct SharedWaker {
    mutex: Mutex<Waker>,
    refc: AtomicUsize,
}

impl SharedWaker {
    fn ref_dec(&self) -> usize {
        self.refc.fetch_sub(1, Ordering::Acquire)
    }

    fn ref_add(&self) -> usize {
        self.refc.fetch_add(1, Ordering::Acquire)
    }
}

pub(crate) struct PollRecv<T> {
    mutex: NonNull<SharedWaker>,
    recv: mpsc::Receiver<T>,
}

impl<T> PollRecv<T> {
    fn poll_recv(&self) {}
}

impl<T> Drop for PollRecv<T> {
    fn drop(&mut self) {
        // Safety: this is atomic and the pointer points to valid memory
        let count = unsafe { (*self.mutex.as_ptr()).ref_dec() };

        if count == 1 {
            // Safety: we are the only reference to this allocation
            unsafe {
                let victim = Box::from_raw(self.mutex.as_ptr());

                drop(victim);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct PollSender<T> {
    mutex: NonNull<SharedWaker>,
    snd: mpsc::Sender<T>,
}

impl<T> PollSender<T> {
    fn poll_send() {}
}

impl<T> Drop for PollRecv<T> {
    fn drop(&mut self) {
        // Safety: this is atomic and the pointer points to valid memory
        let count = unsafe { (*self.mutex.as_ptr()).ref_dec() };

        if count == 1 {
            // Safety: we are the only reference to this allocation
            unsafe {
                let victim = Box::from_raw(self.mutex.as_ptr());

                drop(victim);
            }
        }
    }
}

pub(crate) fn channel<T>() -> (PollSender<T>, PollRecv<T>) {
    let mutex = SharedWaker {
        mutex: Mutex::new(Waker::noop().clone()),
        refc: AtomicUsize::new(2),
    };

    let ptr = Box::into_raw(Box::new(mutex));

    // Safety: `ptr` is initialized.
    let n_ptr = unsafe { NonNull::new_unchecked(ptr) };

    let (snd, recv) = mpsc::channel();

    let poll_r = PollRecv { mutex: n_ptr, recv };

    let poll_s = PollSender { mutex: n_ptr, snd };

    (poll_s, poll_r)
}
