//! Quick Share / Handoff engine.
//!
//! Tracks shareable clipboard items (URLs and local files). When the user crosses
//! the screen boundary to another computer, an offer is sent across the wire so
//! a subtle, non-intrusive action bubble / popup can appear on that screen.
//! Clicking the action either opens the URL in the browser or transfers the file
//! over the encrypted connection directly to the Downloads folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kayiver_core::proto::{Msg, QuickShareOffer, QuickSharePayload};
use tokio::io::AsyncReadExt;

use tracing::{debug, info, warn};

/// Maximum age of a copied item to be offered upon crossing (10 minutes).
const OFFER_MAX_AGE: Duration = Duration::from_secs(600);
/// Short frames limit the wire interval before the next control event.
const CHUNK_SIZE: usize = super::outbound::DATA_CHUNK_SIZE;

static NEXT_OFFER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct QuickShareInner {
    latest_candidate: Option<(QuickShareOffer, Instant)>,
    offered_ids: HashSet<u64>,
}

struct InboundTransfer {
    temp_path: PathBuf,
    file_name: String,
    expected_size: u64,
    received_bytes: u64,
    file: Option<std::fs::File>,
}

#[derive(Clone)]
pub struct QuickShareEngine {
    inner: Arc<Mutex<QuickShareInner>>,
    inbound: Arc<Mutex<std::collections::HashMap<(String,u64,u64),Arc<Mutex<InboundTransfer>>>>>,
}

static ENGINE: OnceLock<QuickShareEngine> = OnceLock::new();

pub fn engine() -> &'static QuickShareEngine {
    ENGINE.get_or_init(|| QuickShareEngine {
        inner: Arc::new(Mutex::new(QuickShareInner::default())),
        inbound: Arc::new(Mutex::new(std::collections::HashMap::new())),
    })
}

/// Helper to format file sizes nicely (e.g. "12.4 MB").
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Detect whether text or an explicit clipboard file path constitutes a shareable item.
pub fn detect_payload(text: Option<&str>, file_path: Option<&str>) -> Option<QuickSharePayload> {
    // 1. Explicit file path (e.g. Finder / Explorer copied file)
    if let Some(fp) = file_path {
        let p = Path::new(fp);
        if p.is_file() {
            if let Ok(meta) = p.metadata() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    return Some(QuickSharePayload::File {
                        name: name.to_string(),
                        size: meta.len(),
                        source_path: fp.to_string(),
                    });
                }
            }
        }
    }

    // 2. Text payload: could be a URL or a file path
    if let Some(t) = text {
        let trimmed = t.trim();
        // File path check
        let p = Path::new(trimmed);
        if p.is_file() {
            if let Ok(meta) = p.metadata() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    return Some(QuickSharePayload::File {
                        name: name.to_string(),
                        size: meta.len(),
                        source_path: trimmed.to_string(),
                    });
                }
            }
        }

        // Web URL check
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Some(QuickSharePayload::Url {
                url: trimmed.to_string(),
                title: None,
            });
        }
    }

    None
}

impl QuickShareEngine {
    /// Record a newly observed shareable item on this machine.
    pub fn update_candidate(&self, payload: QuickSharePayload) {
        let id = NEXT_OFFER_ID.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();

        // Check if identical to current candidate to avoid resetting ID needlessly
        if let Some((current, _)) = &inner.latest_candidate {
            if current.payload == payload {
                return;
            }
        }

        debug!("quickshare: new candidate available");
        inner.latest_candidate = Some((QuickShareOffer { id, payload }, Instant::now()));
    }

    /// Retrieve an un-offered candidate upon cursor crossing.
    pub fn get_offer_for_crossing(&self) -> Option<QuickShareOffer> {
        let mut inner = self.inner.lock().unwrap();
        let (offer, created_at) = inner.latest_candidate.clone()?;

        if created_at.elapsed() > OFFER_MAX_AGE {
            return None;
        }

        if inner.offered_ids.contains(&offer.id) {
            return None;
        }

        inner.offered_ids.insert(offer.id);
        Some(offer)
    }

    /// Retrieve local source path for a file transfer by offer ID.
    pub fn find_source_file(&self, id: u64) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        if let Some((offer, _)) = &inner.latest_candidate {
            if offer.id == id {
                if let QuickSharePayload::File { source_path, .. } = &offer.payload {
                    return Some(source_path.clone());
                }
            }
        }
        None
    }

    /// Stream an accepted file transfer to the peer in bounded chunks.
    pub async fn stream_file(
        &self,
        id: u64,
        source_path: String,
        out_tx: super::outbound::Sender,
    ) -> Result<()> {
        let mut file = tokio::fs::File::open(&source_path)
            .await
            .with_context(|| format!("cannot open source file {source_path}"))?;

        let meta = file.metadata().await?;
        let total_size = meta.len();
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK_SIZE];

        info!("quickshare: streaming {total_size} bytes for offer #{id}");

        loop {
            let limit = total_size.saturating_sub(offset).min(CHUNK_SIZE as u64) as usize;
            let n = file.read(&mut buf[..limit]).await?;
            if n == 0 {
                if offset < total_size { anyhow::bail!("source file was truncated during transfer"); }
                // If the file was 0 bytes, send an empty EOF chunk
                if offset == 0 {
                    out_tx.send_data(Msg::QuickShareChunk {
                        id,
                        offset: 0,
                        data: Vec::new(),
                        is_eof: true,
                    }).await.map_err(|_| anyhow::anyhow!("peer connection closed during file transfer"))?;
                }
                break;
            }

            offset += n as u64;
            let is_eof = offset >= total_size;

            let chunk_msg = Msg::QuickShareChunk {
                id,
                offset: offset - n as u64,
                data: buf[..n].to_vec(),
                is_eof,
            };

            if out_tx.send_data(chunk_msg).await.is_err() {
                warn!("quickshare: peer connection closed during file transfer #{id}");
                anyhow::bail!("peer connection closed during file transfer");
            }

            if is_eof {
                break;
            }

            // Yield briefly to ensure input events are never starved by file streaming
            tokio::task::yield_now().await;
        }

        info!("quickshare: file stream completed for offer #{id}");
        Ok(())
    }

    /// Prepare to receive an incoming file for an accepted offer.
    pub fn prepare_inbound_transfer(
        &self,
        peer:&str, session:u64, id: u64,
        file_name: &str,
        expected_size: u64,
    ) -> Result<()> {
        if file_name.is_empty() || file_name.contains('/') || file_name.contains('\\') || file_name=="." || file_name==".." {
            anyhow::bail!("invalid offered file name");
        }
        let key=(peer.to_string(),session,id);
        let mut inner = self.inbound.lock().unwrap();
        if inner.contains_key(&key) {return Ok(());}
        inner.insert(
            key,
            Arc::new(Mutex::new(InboundTransfer {
                temp_path:PathBuf::new(),
                file_name: file_name.to_string(),
                expected_size,
                received_bytes: 0,
                file:None,
            })),
        );

        Ok(())
    }

    /// Process an incoming file chunk; returns `Some(final_path)` when completed.
    pub fn handle_inbound_chunk(
        &self,
        peer:&str, session:u64, id: u64,
        offset: u64,
        data: &[u8],
        is_eof: bool,
    ) -> Result<Option<PathBuf>> {
        use std::io::Write;

        let key=(peer.to_string(),session,id);
        let state=self.inbound.lock().unwrap().get(&key).cloned().context("file transfer was not accepted for this session")?;
        let mut transfer=state.lock().unwrap();
        if offset!=transfer.received_bytes {anyhow::bail!("file offset out of order");}
        if data.len() as u64 > transfer.expected_size.saturating_sub(transfer.received_bytes) {anyhow::bail!("file exceeds declared size");}
        if is_eof && transfer.received_bytes+data.len() as u64 != transfer.expected_size {anyhow::bail!("incomplete file at EOF");}

        #[cfg(feature="sim")]
        crate::platform::simulate_file_write_delay();
        if transfer.file.is_none() {
            let downloads=download_directory();
            std::fs::create_dir_all(&downloads)?;
            // Never truncate a previous partial transfer or an unrelated file.
            for nonce in 0..1000u32 {
                let path=downloads.join(format!("{}.part_{session}_{id}_{nonce}",transfer.file_name));
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file)=>{transfer.temp_path=path;transfer.file=Some(file);break;},
                    Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{},
                    Err(e)=>return Err(e.into()),
                }
            }
            if transfer.file.is_none() {anyhow::bail!("no available partial file destination");}
        }
        if !data.is_empty() {
            transfer.file.as_mut().unwrap().write_all(data)?;
            transfer.received_bytes += data.len() as u64;
            if transfer.expected_size > 0 {
                let _pct = (transfer.received_bytes as f64 / transfer.expected_size as f64 * 100.0) as u32;
            }
        }

        if is_eof {
            transfer.file.as_mut().unwrap().flush()?;
            self.inbound.lock().unwrap().remove(&key);
            transfer.file.take();

            let downloads=download_directory();

            let final_path = unique_destination(&downloads, &transfer.file_name);
            std::fs::rename(&transfer.temp_path, &final_path).with_context(|| {
                format!(
                    "failed to rename {} to {}",
                    transfer.temp_path.display(),
                    final_path.display()
                )
            })?;

            info!(
                "quickshare: file transfer #{id} complete: saved to {}",
                final_path.display()
            );
            Ok(Some(final_path))
        } else {
            Ok(None)
        }
    }
    pub fn close_session(&self,peer:&str,session:u64) {
        self.inbound.lock().unwrap().retain(|(p,s,_),_|p!=peer || *s!=session);
    }

}

fn download_directory()->PathBuf {
    #[cfg(feature="sim")]
    {return kayiver_core::config::Config::path().parent().unwrap().join("test-downloads");}
    #[cfg(not(feature="sim"))]
    {dirs::download_dir().or_else(dirs::desktop_dir).unwrap_or_else(||PathBuf::from("."))}
}

/// Generate a unique destination path in `dir` by appending `(1)`, `(2)`, etc. if needed.
fn unique_destination(dir: &Path, file_name: &str) -> PathBuf {
    let target = dir.join(file_name);
    if !target.exists() {
        return target;
    }

    let p = Path::new(file_name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    let ext = p.extension().and_then(|e| e.to_str());

    for i in 1..1000 {
        let new_name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = dir.join(new_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_works() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(10485760), "10.0 MB");
        assert_eq!(format_size(1073741824), "1.0 GB");
    }

    #[test]
    fn detect_url_payload() {
        let p = detect_payload(Some("https://kayiver.app"), None);
        assert!(matches!(p, Some(QuickSharePayload::Url { url, .. }) if url == "https://kayiver.app"));

        let p_http = detect_payload(Some("http://localhost:3000/docs"), None);
        assert!(matches!(p_http, Some(QuickSharePayload::Url { .. })));

        let not_url = detect_payload(Some("hello world"), None);
        assert!(not_url.is_none());
    }

    #[test]
    fn detect_file_payload() {
        let cargo_toml = Path::new("Cargo.toml");
        assert!(cargo_toml.exists());
        let p = detect_payload(Some("Cargo.toml"), None);
        assert!(matches!(p, Some(QuickSharePayload::File { name, size, .. }) if name == "Cargo.toml" && size > 0));
    }

    #[test]
    fn offer_dedup_on_multiple_crossings() {
        let engine = QuickShareEngine {
            inner: Arc::new(Mutex::new(QuickShareInner::default())),
        inbound: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };

        engine.update_candidate(QuickSharePayload::Url {
            url: "https://example.com".into(),
            title: None,
        });

        // First crossing gets the offer
        let first = engine.get_offer_for_crossing();
        assert!(first.is_some());

        // Second crossing without new candidate is deduped (None)
        let second = engine.get_offer_for_crossing();
        assert!(second.is_none());

        // Updating with a new candidate permits a new offer
        engine.update_candidate(QuickSharePayload::Url {
            url: "https://example.com/2".into(),
            title: None,
        });
        let third = engine.get_offer_for_crossing();
        assert!(third.is_some());
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(data: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!("kayiver-stream-test-{}-{}",std::process::id(),NEXT_OFFER_ID.fetch_add(1,Ordering::Relaxed)));
            std::fs::write(&path,data).unwrap(); Self(path)
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _=std::fs::remove_file(&self.0); } }
    fn isolated_engine() -> QuickShareEngine {
        QuickShareEngine { inner: Arc::new(Mutex::new(QuickShareInner::default())), inbound:Arc::new(Mutex::new(std::collections::HashMap::new())) }
    }
    #[tokio::test]
    async fn bounded_file_stream_preserves_bytes_offsets_and_eof() {
        for length in [0, CHUNK_SIZE * 17 + 13] {
            let expected: Vec<u8> = (0..length).map(|n| (n.wrapping_mul(71) % 251) as u8).collect();
            let fixture = Fixture::new(&expected);
            let path = fixture.0.to_string_lossy().to_string();
            let (tx, mut rx) = super::super::outbound::channel();
            let receipts=tx.clone();
            let task = tokio::spawn(async move { isolated_engine().stream_file(17,path,tx).await });
            let mut actual = Vec::new();
            loop {
                let frame = tokio::time::timeout(Duration::from_secs(2),rx.recv()).await.unwrap().unwrap();
                let eof = match &frame.message {
                    Msg::FileFrame {id,offset,data,is_eof,..} => {
                        assert_eq!(*id,17); assert_eq!(*offset,actual.len() as u64);
                        assert!(data.len()<=CHUNK_SIZE); actual.extend_from_slice(data); *is_eof
                    }, _ => panic!("unexpected stream message"),
                };
                if let Msg::FileFrame {token,..}=frame.message {receipts.acknowledge(token,true);}
                if eof {break;}
            }
            task.await.unwrap().unwrap();
            assert_eq!(actual,expected);
        }
    }
    #[tokio::test]
    async fn disconnected_stream_reports_failure_instead_of_completion() {
        let fixture=Fixture::new(&vec![1;CHUNK_SIZE*3]);
        let path=fixture.0.to_string_lossy().to_string();
        let (tx,mut rx)=super::super::outbound::channel();
        let task=tokio::spawn(async move {isolated_engine().stream_file(19,path,tx).await});
        let held=rx.recv().await.unwrap();
        drop(rx);
        assert!(tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap().is_err());
        drop(held);
    }
    #[test]
    fn file_acceptance_is_scoped_and_invalid_offsets_never_open_a_file() {
        let engine=isolated_engine();
        engine.prepare_inbound_transfer("peer-a",1,7,"fixture.bin",100).unwrap();
        assert!(engine.handle_inbound_chunk("peer-b",1,7,0,&[1],false).is_err());
        assert!(engine.handle_inbound_chunk("peer-a",2,7,0,&[1],false).is_err());
        assert!(engine.handle_inbound_chunk("peer-a",1,7,1,&[1],false).is_err());
        assert!(engine.handle_inbound_chunk("peer-a",1,7,0,&[1],true).is_err());
        assert!(engine.inbound.lock().unwrap().values().all(|f|f.lock().unwrap().file.is_none()));
        engine.close_session("peer-a",1);
        assert!(engine.inbound.lock().unwrap().is_empty());
        assert!(engine.prepare_inbound_transfer("peer-a",1,8,"../escape",100).is_err());
    }

}
