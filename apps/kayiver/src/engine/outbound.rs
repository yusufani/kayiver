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
    #[tokio::test]
    #[ignore = "explicit slow-receiver probe; documents kernel buffering outside the application budget"]
    async fn encrypted_socket_probe_reports_bulk_already_ahead_of_motion() {
        use kayiver_core::secure::{handshake_initiator, handshake_responder};
        use tokio::sync::{oneshot, Notify};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (begin_tx, begin_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let (mut reader, _writer) = handshake_responder(stream, &[13; 32]).await.unwrap();
            begin_rx.await.unwrap();
            let mut bulk_bytes = 0;
            let mut motion = Vec::new();
            while motion.len() < 2 {
                match reader.recv().await.unwrap() {
                    Msg::QuickShareChunk { data, .. } => bulk_bytes += data.len(),
                    Msg::Input(InputEvent::MouseMove { dx, .. }) => motion.push(dx),
                    _ => panic!("unexpected probe message"),
                }
            }
            (bulk_bytes, motion)
        });
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.set_nodelay(true).unwrap();
        let (_reader, mut writer) = handshake_initiator(stream, &[13; 32]).await.unwrap();
        let (tx, mut rx) = channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let resume = Arc::new(Notify::new());
        let gate = resume.clone();
        let writer_task = tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut files = 0;
            let mut moves = 0;
            while let Some(frame) = rx.recv().await {
                writer.send(&frame.message).await.unwrap();
                match frame.message {
                    Msg::QuickShareChunk { .. } => {
                        files += 1;
                        if files == 4 {
                            ready.take().unwrap().send(()).unwrap();
                            gate.notified().await;
                        }
                    }
                    Msg::Input(_) => {
                        moves += 1;
                        if moves == 2 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        });
        let producer = tx.clone();
        let file = tokio::spawn(async move {
            for offset in 0..20 {
                if producer
                    .send_file(Msg::QuickShareChunk {
                        id: 8,
                        offset,
                        data: vec![7; 8192],
                        is_eof: false,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx)
            .await
            .unwrap()
            .unwrap();
        tx.send(Msg::Input(InputEvent::MouseMove { dx: 100, dy: 0 }))
            .unwrap();
        tx.send(Msg::Input(InputEvent::MouseMove { dx: -100, dy: 0 }))
            .unwrap();
        begin_tx.send(()).unwrap();
        resume.notify_one();
        let (bytes, motion) = tokio::time::timeout(std::time::Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
        writer_task.await.unwrap();
        file.await.unwrap();
        assert_eq!(motion, vec![100, -100]);
        assert_eq!(bytes, 4 * 8192);
        println!("Kernel/peer buffering ahead of newly queued motion: {bytes} bytes despite a one-frame application budget");
    }
}
