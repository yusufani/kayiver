//! Keep the control stream ordered while bounding file production to one
//! queued/in-flight frame. The permit travels with the frame and is released
//! only after the serial encrypted writer has finished that frame.
use kayiver_core::proto::Msg;
use std::sync::Arc;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub(crate) struct Sender {
    tx: mpsc::UnboundedSender<Frame>,
    files: Arc<Semaphore>,
}
pub(crate) struct Frame {
    pub message: Msg,
    _file_permit: Option<OwnedSemaphorePermit>,
}
pub(crate) struct Receiver {
    rx: mpsc::UnboundedReceiver<Frame>,
    files: Arc<Semaphore>,
}
pub(crate) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let files = Arc::new(Semaphore::new(1));
    (
        Sender {
            tx,
            files: files.clone(),
        },
        Receiver { rx, files },
    )
}
impl Sender {
    pub fn send(&self, message: Msg) -> Result<(), mpsc::error::SendError<Msg>> {
        self.tx
            .send(Frame {
                message,
                _file_permit: None,
            })
            .map_err(|error| mpsc::error::SendError(error.0.message))
    }
    pub async fn send_file(&self, message: Msg) -> Result<(), mpsc::error::SendError<Msg>> {
        let permit = match self.files.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return Err(mpsc::error::SendError(message)),
        };
        self.tx
            .send(Frame {
                message,
                _file_permit: Some(permit),
            })
            .map_err(|error| mpsc::error::SendError(error.0.message))
    }
}
impl Receiver {
    pub async fn recv(&mut self) -> Option<Frame> {
        self.rx.recv().await
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        // Abort/disconnect wakes producers waiting for a frame's permit, even
        // if another sender clone still exists in a pending transfer task.
        self.files.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kayiver_core::proto::InputEvent;
    #[tokio::test]
    async fn file_backpressure_keeps_motion_ahead_of_unproduced_bulk() {
        let (tx, mut rx) = channel();
        let producer = tx.clone();
        let task = tokio::spawn(async move {
            for offset in 0..3 {
                producer
                    .send_file(Msg::QuickShareChunk {
                        id: 1,
                        offset,
                        data: vec![42; 8192],
                        is_eof: offset == 2,
                    })
                    .await
                    .unwrap();
            }
        });
        let first = rx.recv().await.unwrap();
        assert!(matches!(
            first.message,
            Msg::QuickShareChunk { offset: 0, .. }
        ));
        tx.send(Msg::Input(InputEvent::MouseMove { dx: 100, dy: 0 }))
            .unwrap();
        tx.send(Msg::Input(InputEvent::MouseMove { dx: -100, dy: 0 }))
            .unwrap();
        assert!(matches!(
            rx.recv().await.unwrap().message,
            Msg::Input(InputEvent::MouseMove { dx: 100, .. })
        ));
        assert!(matches!(
            rx.recv().await.unwrap().message,
            Msg::Input(InputEvent::MouseMove { dx: -100, .. })
        ));
        assert!(rx.rx.try_recv().is_err());
        assert!(!task.is_finished());
        drop(first); // Exactly the writer's completion point.
        for offset in 1..3 {
            let frame = rx.recv().await.unwrap();
            assert!(matches!(frame.message, Msg::QuickShareChunk {offset:n,..} if n==offset));
            drop(frame);
        }
        task.await.unwrap();
    }
    #[tokio::test]
    async fn disconnect_wakes_a_blocked_file_producer() {
        let (tx, mut rx) = channel();
        tx.send_file(Msg::Ping(1)).await.unwrap();
        let held = rx.recv().await.unwrap();
        let task = tokio::spawn(async move { tx.send_file(Msg::Ping(2)).await });
        drop(rx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        drop(held);
    }
    #[tokio::test]
    async fn cancelling_a_waiting_producer_does_not_consume_credit() {
        let (tx, mut rx) = channel();
        tx.send_file(Msg::Ping(1)).await.unwrap();
        let held = rx.recv().await.unwrap();
        let waiting = tx.clone();
        let task = tokio::spawn(async move { waiting.send_file(Msg::Ping(2)).await });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(held);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tx.send_file(Msg::Ping(3)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(rx.recv().await.unwrap().message, Msg::Ping(3)));
    }
}
