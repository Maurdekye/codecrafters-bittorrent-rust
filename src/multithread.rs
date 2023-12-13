#![allow(unused)]

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

pub struct SyncDoor {
    lock: Mutex<bool>,
    cvar: Condvar,
}

impl SyncDoor {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(false),
            cvar: Condvar::new(),
        }
    }

    pub fn close(&self) -> Result<(), std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>> {
        self.lock.lock().map(|mut lock| {
            *lock = true;
        })
    }

    pub fn open(&self) -> Result<(), std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>> {
        self.lock.lock().map(|mut lock| {
            *lock = false;
            self.cvar.notify_all();
        })
    }

    pub fn wait(&self) -> Result<(), std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>> {
        self.lock.lock().map(|mut lock| {
            while *lock {
                lock = self.cvar.wait(lock).unwrap();
            }
        })
    }

    pub fn is_closed(
        &self,
    ) -> Result<bool, std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>> {
        self.lock.lock().map(|lock| *lock)
    }
}
