//! Quartz display callbacks plus kqueue file notifications. These observers
//! do no geometry/config work in callbacks and never acquire the motion gate.
use super::ChangeStamp;
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    sync::OnceLock,
};
use tokio::sync::watch;

type Callback = unsafe extern "C" fn(u32, u32, *mut libc::c_void);
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: Callback,
        context: *mut libc::c_void,
    ) -> i32;
    fn CGDisplayRemoveReconfigurationCallback(
        callback: Callback,
        context: *mut libc::c_void,
    ) -> i32;
}
static CHANGES: OnceLock<Option<watch::Sender<ChangeStamp>>> = OnceLock::new();
unsafe extern "C" fn display_changed(_: u32, flags: u32, _: *mut libc::c_void) {
    if flags & 1 != 0 {
        return;
    } // kCGDisplayBeginConfigurationFlag: not settled yet.
    if let Some(Some(tx)) = CHANGES.get() {
        tx.send_modify(|s| s.displays = s.displays.wrapping_add(1));
    }
}
struct Files {
    queue: OwnedFd,
    directory: File,
    file: Option<File>,
    path: PathBuf,
}
impl Files {
    fn add(&self, file: &File) -> std::io::Result<()> {
        let event = libc::kevent {
            ident: file.as_raw_fd() as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    fn new(path: PathBuf) -> std::io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let files = Self {
            queue: unsafe { OwnedFd::from_raw_fd(fd) },
            directory: File::open(
                path.parent()
                    .ok_or_else(|| std::io::Error::other("missing config directory"))?,
            )?,
            file: File::open(&path).ok(),
            path,
        };
        files.add(&files.directory)?;
        if let Some(file) = &files.file {
            files.add(file)?;
        }
        Ok(files)
    }
    fn run(mut self, tx: watch::Sender<ChangeStamp>) {
        loop {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    std::ptr::null(),
                )
            };
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            if result < 0
                || event.flags & libc::EV_ERROR != 0
                || (event.ident == self.directory.as_raw_fd() as usize
                    && event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0)
            {
                tx.send_modify(|s| s.failed = true);
                return;
            }
            // A save can replace the inode instead of modifying it. The
            // directory watch survives that and reattaches to the new file.
            if event.ident == self.directory.as_raw_fd() as usize
                || event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0
            {
                self.file = None; // Closing the old fd also removes its knote.
                self.file = File::open(&self.path).ok();
                if let Some(file) = &self.file {
                    if self.add(file).is_err() {
                        tx.send_modify(|s| s.failed = true);
                        return;
                    }
                }
            }
            tx.send_modify(|s| s.config = s.config.wrapping_add(1));
        }
    }
}
pub(super) fn subscribe() -> Option<watch::Receiver<ChangeStamp>> {
    CHANGES
        .get_or_init(|| {
            let files = Files::new(kayiver_core::config::Config::path()).ok()?;
            let (tx, _) = watch::channel(ChangeStamp::default());
            if unsafe {
                CGDisplayRegisterReconfigurationCallback(display_changed, std::ptr::null_mut())
            } != 0
            {
                return None;
            }
            let worker_tx = tx.clone();
            if std::thread::Builder::new()
                .name("kayiver-system-changes".into())
                .spawn(move || files.run(worker_tx))
                .is_err()
            {
                unsafe {
                    CGDisplayRemoveReconfigurationCallback(display_changed, std::ptr::null_mut());
                }
                return None;
            }
            Some(tx)
        })
        .as_ref()
        .map(|tx| tx.subscribe())
}
pub(super) fn active() -> bool {
    matches!(CHANGES.get(),Some(Some(tx)) if !tx.borrow().failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn file_watch_detects_writes_atomic_replacements_and_recreation() {
        let root = std::env::temp_dir().join(format!(
            "kayiver-file-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("config.toml");
        std::fs::write(&path, "initial").unwrap();
        let files = Files::new(path.clone()).unwrap();
        let (tx, mut rx) = watch::channel(ChangeStamp::default());
        let worker = std::thread::spawn(move || files.run(tx));
        for action in 0..4 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            rx.borrow_and_update();
            match action {
                0 => std::fs::write(&path, "edited").unwrap(),
                1 => {
                    let replacement = root.join("replacement");
                    std::fs::write(&replacement, "atomic").unwrap();
                    std::fs::rename(replacement, &path).unwrap();
                }
                2 => std::fs::remove_file(&path).unwrap(),
                _ => std::fs::write(&path, "recreated").unwrap(),
            }
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.changed())
                .await
                .expect("config event was lost")
                .unwrap();
            assert!(!rx.borrow().failed);
        }
        std::fs::remove_dir_all(&root).unwrap();
        worker.join().unwrap();
        assert!(
            rx.borrow().failed,
            "directory disappearance must enable polling fallback"
        );
    }
}
