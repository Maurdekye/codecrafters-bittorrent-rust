use std::sync::{Condvar, Mutex};

pub struct Semaphore {
    value: Mutex<usize>,
    cvar: Condvar,
}

impl Semaphore {
    pub fn new(value: usize) -> Self {
        Self {
            value: Mutex::new(value),
            cvar: Condvar::new(),
        }
    }

    pub fn take_n(&self, n: usize) {
        let mut value = self.value.lock().unwrap();
        while *value < n {
            value = self.cvar.wait(value).unwrap();
        }
        *value -= n;
    }

    pub fn take(&self) {
        self.take_n(1)
    }

    pub fn put_n(&self, n: usize) {
        let mut value = self.value.lock().unwrap();
        *value += n;
        self.cvar.notify_all();
    }

    pub fn put(&self) {
        self.put_n(1)
    }
}