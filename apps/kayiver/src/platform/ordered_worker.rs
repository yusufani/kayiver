//! One outstanding command, reusable completion storage, strict FIFO.
//! Native input must stay off threads that own GUI windows or hooks.
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

struct Completion<R> { result: Option<R>, closed: bool }
type Shared<R> = Arc<(Mutex<Completion<R>>, Condvar)>;

struct ExitSignal<R>(Shared<R>);
impl<R> Drop for ExitSignal<R> {
    fn drop(&mut self) {
        let (state, ready) = &*self.0;
        state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        ready.notify_all();
    }
}

pub(super) struct OrderedWorker<C, R> {
    tx: Option<mpsc::SyncSender<C>>,
    completion: Shared<R>,
    thread: Option<JoinHandle<()>>,
}
impl<C: Send + 'static, R: Send + 'static> OrderedWorker<C, R> {
    pub(super) fn spawn<F, H>(name: &str, factory: F) -> std::io::Result<Self>
    where F: FnOnce() -> H + Send + 'static, H: FnMut(C) -> R {
        let (tx, rx) = mpsc::sync_channel::<C>(1);
        let completion = Arc::new((Mutex::new(Completion { result: None, closed: false }), Condvar::new()));
        let worker_completion = completion.clone();
        let thread = thread::Builder::new().name(name.into()).spawn(move || {
            let _exit = ExitSignal(worker_completion.clone());
            let mut handler = factory();
            while let Ok(command) = rx.recv() {
                let result = handler(command);
                let (state, ready) = &*worker_completion;
                state.lock().unwrap_or_else(|e| e.into_inner()).result = Some(result);
                ready.notify_one();
            }
            // Handler drops before ExitSignal: native cleanup completes before
            // callers observe shutdown, including on panic unwinding.
        })?;
        Ok(Self { tx: Some(tx), completion, thread: Some(thread) })
    }
    pub(super) fn run(&mut self, command: C) -> Option<R> {
        let (state, ready) = &*self.completion;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed { return None; }
        state.result = None;
        if self.tx.as_ref()?.send(command).is_err() { return None; }
        while state.result.is_none() && !state.closed {
            state = ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.result.take()
    }
}
impl<C, R> Drop for OrderedWorker<C, R> {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_commands_are_consumed_once_without_merging_reversals() {
        let mut worker = OrderedWorker::spawn("ordered-test", || {
            let mut sequence = 0;
            let mut pos = 0;
            move |(expected, delta)| {
                assert_eq!(sequence, expected);
                sequence += 1;
                pos += delta;
                (sequence, pos)
            }
        }).unwrap();
        for n in 0..10_000 {
            assert_eq!(worker.run((n, if n % 2 == 0 { 100 } else { -100 })),
                Some((n + 1, if n % 2 == 0 { 100 } else { 0 })));
        }
    }
    #[test]
    fn worker_failure_wakes_caller_and_future_calls_fail() {
        let mut worker = OrderedWorker::spawn("panic-test", || |_: ()| -> bool { panic!("controlled failure") }).unwrap();
        assert_eq!(worker.run(()), None);
        assert_eq!(worker.run(()), None);
    }
    #[test]
    fn drop_waits_for_native_cleanup() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Cleanup(Arc<AtomicBool>);
        impl Drop for Cleanup { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }
        let cleaned = Arc::new(AtomicBool::new(false));
        let flag = cleaned.clone();
        let mut worker = OrderedWorker::spawn("cleanup-test", move || {
            let cleanup = Cleanup(flag);
            move |()| { let _ = &cleanup; true }
        }).unwrap();
        assert_eq!(worker.run(()), Some(true));
        drop(worker);
        assert!(cleaned.load(Ordering::SeqCst));
    }
    #[test]
    #[ignore = "explicit transport benchmark; reports timing without a flaky CI threshold"]
    fn benchmark_reusable_completion_against_per_command_reply_channel() {
        use std::time::Instant;
        let mut reused = Vec::new();
        let mut legacy = Vec::new();
        for _ in 0..5 {
            let mut worker = OrderedWorker::spawn("reused-bench", || |n: u32| n).unwrap();
            let start = Instant::now();
            for n in 0..10_000 { assert_eq!(worker.run(n), Some(n)); }
            reused.push(start.elapsed().as_micros());
            drop(worker);
            let (tx, rx) = mpsc::channel::<(u32, mpsc::SyncSender<u32>)>();
            let old = thread::spawn(move || {
                while let Ok((n, reply)) = rx.recv() { reply.send(n).unwrap(); }
            });
            let start = Instant::now();
            for n in 0..10_000 {
                let (reply, result) = mpsc::sync_channel(1);
                tx.send((n, reply)).unwrap();
                assert_eq!(result.recv().unwrap(), n);
            }
            legacy.push(start.elapsed().as_micros());
            drop(tx);
            old.join().unwrap();
        }
        reused.sort(); legacy.sort();
        println!("10000 round-trips, median of 5: reused={} us; legacy={} us", reused[2], legacy[2]);
    }

}
