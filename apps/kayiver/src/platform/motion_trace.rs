//! Opt-in bounded native recordings. Normal capture performs no file I/O.
use kayiver_core::motion::{Location, Point, ReplaySample, Step, Topology};
use std::io::Write;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc::{sync_channel, SyncSender},
    OnceLock,
};
static RECORDER: OnceLock<Option<SyncSender<ReplaySample>>> = OnceLock::new();
static COUNT: AtomicUsize = AtomicUsize::new(0);
pub fn initialize() {
    RECORDER.get_or_init(|| {
        let path = std::env::var_os("KAYIVER_MOTION_TRACE")?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("motion recording unavailable: {e}");
                return None;
            }
        };
        let (tx, rx) = sync_channel::<ReplaySample>(256);
        std::thread::spawn(move || {
            let mut file = std::io::BufWriter::new(file);
            for sample in rx {
                if serde_json::to_writer(&mut file, &sample).is_err()
                    || file.write_all(b"\n").is_err()
                    || file.flush().is_err()
                {
                    break;
                }
            }
        });
        Some(tx)
    });
}
pub fn record(topology: &Topology, start: &Location, delta: Point, expected: &Step) {
    let Some(Some(tx)) = RECORDER.get() else {
        return;
    };
    if COUNT.fetch_add(1, Ordering::Relaxed) >= 10000 {
        return;
    }
    if tx
        .try_send(ReplaySample {
            topology: topology.clone(),
            start: start.clone(),
            delta,
            expected: expected.clone(),
        })
        .is_err()
    {
        // A recording may be incomplete under load, but input is never delayed.
        tracing::warn!("motion recording queue full; sample omitted");
    }
}
