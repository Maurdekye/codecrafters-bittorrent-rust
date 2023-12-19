use std::{
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread::Scope, ops::ControlFlow,
};

pub struct PoolMap<I, T, V> {
    iter: I,
    work_left: usize,
    out_recv: Receiver<ControlFlow<(), V>>,
    in_send: Sender<T>,
}

pub trait PoolMapExt<'a, 'b, 'c, I, T, F, V> {
    fn pool_map(self, scope: &'a Scope<'b, 'c>, workers: usize, f: F) -> PoolMap<I, T, V>
    where
        Self: Iterator<Item = T>,
        I: Iterator<Item = T>,
        F: Fn(T) -> ControlFlow<(), V>,
        F: Send + Sync + 'a,
        T: Send + 'a,
        V: Send + 'a,
        'a: 'b;
}

impl<'a, 'b, 'c, T, F, V, I> PoolMapExt<'a, 'b, 'c, I, T, F, V> for I
where
    I: Iterator<Item = T>,
{
    /// pool_map: map over an iterator with a parallel worker pool, returning in
    /// first-come first-served order.
    fn pool_map(mut self, scope: &'a Scope<'b, 'c>, workers: usize, f: F) -> PoolMap<I, T, V>
    where
        Self: Iterator<Item = T>,
        I: Iterator<Item = T>,
        F: Fn(T) -> ControlFlow<(), V>,
        F: Send + Sync + 'a,
        T: Send + 'a,
        V: Send + 'a,
        'a: 'b,
    {
        let (in_send, in_recv) = channel();
        let (out_send, out_recv) = channel();

        let in_recv = Arc::new(Mutex::new(in_recv));
        let out_send = Arc::new(Mutex::new(out_send));

        let f = Arc::new(f);

        for _ in 0..workers {
            let in_recv = in_recv.clone();
            let out_send = out_send.clone();
            let f = f.clone();
            scope.spawn(move || {
                while let Ok(item) = in_recv.lock().unwrap().recv() {
                    out_send.lock().unwrap().send(f(item)).unwrap();
                }
            });
        }

        let mut work_left = 0;
        for _ in 0..workers {
            let Some(value) = self.next() else { break };
            in_send.send(value).unwrap();
            work_left += 1;
        }

        PoolMap {
            iter: self,
            work_left,
            out_recv,
            in_send,
        }
    }
}

impl<'a, I, T, V> Iterator for PoolMap<I, T, V>
where
    I: Iterator<Item = T>,
    T: Send + 'a,
    V: Send + 'a,
{
    type Item = V;

    fn next(&mut self) -> Option<Self::Item> {
        if self.work_left == 0 {
            None
        } else {
            let ControlFlow::Continue(item) = self.out_recv.recv().unwrap() else { return None };
            if let Some(source) = self.iter.next() {
                self.in_send.send(source).unwrap();
            } else {
                self.work_left -= 1;
            }
            Some(item)
        }
    }
}
