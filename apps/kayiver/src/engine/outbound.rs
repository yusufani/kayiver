//! One receiver-consumed data frame per connection. Control events share the
//! ordered encrypted writer, but never wait for data credits or disk I/O.
//! Unknown/stale receipts cannot release a pending frame; uncertain cancelled
//! or timed-out transfers close data credit until reconnect.
use kayiver_core::proto::Msg;
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
pub(crate) const DATA_CHUNK_SIZE: usize = 8 * 1024;

#[derive(Clone)]
pub(crate) struct Sender {
    tx: mpsc::UnboundedSender<Frame>,
    files: Arc<Semaphore>,
    pending: Arc<std::sync::Mutex<Option<Receipt>>>,
    sequence: Arc<std::sync::atomic::AtomicU64>,
    runtime: tokio::runtime::Handle,
}
struct Receipt {
    token: u64,
    reply: tokio::sync::oneshot::Sender<bool>,
}
struct PendingGuard {
    files: Arc<Semaphore>,
    token: u64,
    pending: Arc<std::sync::Mutex<Option<Receipt>>>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().unwrap();
        if pending.as_ref().is_some_and(|r| r.token == self.token) {
            pending.take();
            self.files.close(); // An unconsumed cancelled frame cannot grant new credit.
        }
    }
}
pub(crate) struct Frame {
    pub message: Msg,
}
pub(crate) struct Receiver {
    rx: mpsc::UnboundedReceiver<Frame>,
    files: Arc<Semaphore>,
    pending: Arc<std::sync::Mutex<Option<Receipt>>>,
}
pub(crate) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let files = Arc::new(Semaphore::new(1));
    let pending = Arc::new(std::sync::Mutex::new(None));
    (
        Sender {
            tx,
            files: files.clone(),
            pending: pending.clone(),
            sequence: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            runtime: tokio::runtime::Handle::current(),
        },
        Receiver { rx, files, pending },
    )
}
impl Sender {
    pub fn send(&self, message: Msg) -> Result<(), mpsc::error::SendError<Msg>> {
        self.tx
            .send(Frame { message })
            .map_err(|e| mpsc::error::SendError(e.0.message))
    }
    pub async fn send_data(&self, message: Msg) -> Result<(), &'static str> {
        let _permit = self
            .files
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "data channel unavailable")?;
        let token = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply, result) = tokio::sync::oneshot::channel();
        *self.pending.lock().unwrap() = Some(Receipt { token, reply });
        let _guard = PendingGuard {
            files: self.files.clone(),
            token,
            pending: self.pending.clone(),
        };
        let frame = match message {
            Msg::QuickShareChunk {
                id,
                offset,
                data,
                is_eof,
            } if data.len() <= 8192 => Msg::FileFrame {
                token,
                id,
                offset,
                data,
                is_eof,
            },
            Msg::ClipboardImageChunk { id, offset, data }
                if !data.is_empty() && data.len() <= 8192 =>
            {
                Msg::ImageFrame {
                    token,
                    id,
                    offset,
                    data,
                }
            }
            _ => return Err("invalid data frame"),
        };
        self.send(frame).map_err(|_| "data channel unavailable")?;
        // Data alone waits for receiver consumption. Movement/keys never take
        // this permit or wait for a receipt. Bound both kernel and peer backlog.
        match tokio::time::timeout(std::time::Duration::from_secs(30), result).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err("receiver rejected data frame"),
            Ok(Err(_)) => Err("data channel unavailable"),
            Err(_) => {
                self.files.close();
                Err("data receipt timed out")
            }
        }
    }
    /// Called only from the dedicated native clipboard thread, never the
    /// async control router or a platform capture callback.
    pub fn send_image_blocking(&self, message: Msg) -> Result<(), &'static str> {
        self.runtime.block_on(self.send_data(message))
    }
    pub fn acknowledge(&self, token: u64, accepted: bool) {
        let mut pending = self.pending.lock().unwrap();
        if pending.as_ref().is_some_and(|r| r.token == token) {
            let receipt = pending.take().unwrap();
            let _ = receipt.reply.send(accepted);
        }
    }
}
impl Receiver {
    pub async fn recv(&mut self) -> Option<Frame> {
        self.rx.recv().await
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.files.close();
        self.pending.lock().unwrap().take();
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
                    .send_data(Msg::QuickShareChunk {
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
        assert!(matches!(first.message, Msg::FileFrame { offset: 0, .. }));
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
        if let Msg::FileFrame { token, .. } = first.message {
            tx.acknowledge(token, true);
        }
        for offset in 1..3 {
            let frame = rx.recv().await.unwrap();
            assert!(matches!(frame.message, Msg::FileFrame {offset:n,..} if n==offset));
            if let Msg::FileFrame { token, .. } = frame.message {
                tx.acknowledge(token, true);
            }
        }
        task.await.unwrap();
    }

    fn file() -> Msg {
        Msg::QuickShareChunk {
            id: 9,
            offset: 0,
            data: vec![7; 8192],
            is_eof: false,
        }
    }
    #[tokio::test]
    async fn disconnect_wakes_a_blocked_file_producer() {
        let (tx, mut rx) = channel();
        let task = tokio::spawn(async move { tx.send_data(file()).await });
        let _frame = rx.recv().await.unwrap();
        drop(rx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    #[tokio::test]
    async fn old_or_cancelled_receipts_cannot_release_new_credit() {
        let (tx, mut rx) = channel();
        let p = tx.clone();
        let first = tokio::spawn(async move { p.send_data(file()).await });
        let Msg::FileFrame { token: old, .. } = rx.recv().await.unwrap().message else {
            panic!()
        };
        tx.acknowledge(old, true);
        assert!(first.await.unwrap().is_ok());
        let p = tx.clone();
        let next = tokio::spawn(async move { p.send_data(file()).await });
        let Msg::FileFrame { token: new, .. } = rx.recv().await.unwrap().message else {
            panic!()
        };
        tx.acknowledge(old, true);
        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        tx.acknowledge(new, true);
        assert!(next.await.unwrap().is_ok());
        tx.acknowledge(new, true); // Duplicate receipt is harmless.
    }
    #[tokio::test]
    async fn receiver_credit_bounds_real_tcp_bulk_ahead_of_motion() {
        use kayiver_core::secure::{handshake_initiator, handshake_responder};
        use tokio::sync::oneshot;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let (mut reader, mut writer) = handshake_responder(stream, &[13; 32]).await.unwrap();
            let Msg::FileFrame { token, data, .. } = reader.recv().await.unwrap() else {
                panic!()
            };
            let bytes = data.len();
            ready_tx.send(()).unwrap();
            // Deliberately withhold the disk/consumption receipt. The source
            // must still deliver both directions without another bulk frame.
            let mut motion = Vec::new();
            for _ in 0..2 {
                match reader.recv().await.unwrap() {
                    Msg::Input(InputEvent::MouseMove { dx, .. }) => motion.push(dx),
                    _ => panic!("bulk advanced before receiver consumed its frame"),
                }
            }
            writer
                .send(&Msg::DataReceipt {
                    token,
                    accepted: true,
                })
                .await
                .unwrap();
            (bytes, motion)
        });
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.set_nodelay(true).unwrap();
        let (mut reader, mut writer) = handshake_initiator(stream, &[13; 32]).await.unwrap();
        let (tx, mut rx) = channel();
        let receipts = tx.clone();
        let ack = tokio::spawn(async move {
            let Msg::DataReceipt { token, accepted } = reader.recv().await.unwrap() else {
                panic!()
            };
            receipts.acknowledge(token, accepted);
        });
        let writer_task = tokio::spawn(async move {
            let mut moves = 0;
            while let Some(frame) = rx.recv().await {
                writer.send(&frame.message).await.unwrap();
                if matches!(frame.message, Msg::Input(_)) {
                    moves += 1;
                    if moves == 2 {
                        break;
                    }
                }
            }
        });
        let producer = tx.clone();
        let file = tokio::spawn(async move {
            for _ in 0..20 {
                if producer.send_data(file()).await.is_err() {
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
        let (bytes, motion) = tokio::time::timeout(std::time::Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, 8192);
        assert_eq!(motion, vec![100, -100]);
        writer_task.await.unwrap();
        ack.await.unwrap();
        file.await.unwrap();
        println!("Receipt withheld: at most {bytes} bulk bytes ahead; both motion events delivered before receipt");
    }
    #[tokio::test]
    async fn cancelled_inflight_data_closes_credit_but_controls_still_flow() {
        let (tx, mut rx) = channel();
        let p = tx.clone();
        let task = tokio::spawn(async move { p.send_data(file()).await });
        let Msg::FileFrame { token, .. } = rx.recv().await.unwrap().message else {
            panic!()
        };
        task.abort();
        let _ = task.await;
        tx.acknowledge(token, true);
        assert!(tx.send_data(file()).await.is_err());
        tx.send(Msg::Input(InputEvent::MouseMove { dx: 100, dy: 0 }))
            .unwrap();
        assert!(matches!(rx.recv().await.unwrap().message, Msg::Input(_)));
    }
}
