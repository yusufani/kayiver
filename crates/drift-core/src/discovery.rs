//! Zero-config discovery over mDNS (`_drift-kvm._tcp.local.`).
//!
//! The host advertises itself; clients browse for the instance whose name
//! matches the paired host. Discovery is best-effort: clients always try a
//! configured static address first (VPNs and many corporate networks drop
//! multicast), and mDNS fills in when addresses change on a normal LAN.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::{Error, Result};

pub const SERVICE_TYPE: &str = "_drift-kvm._tcp.local.";

/// Advertise this machine as a drift host. Keep the returned daemon alive
/// for as long as the advertisement should stay up.
pub fn advertise(instance_name: &str, port: u16) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().map_err(|e| Error::Protocol(format!("mdns: {e}")))?;
    let hostname = format!("{}.local.", crate::config::machine_name());
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        &hostname,
        "",
        port,
        HashMap::<String, String>::new(),
    )
    .map_err(|e| Error::Protocol(format!("mdns service info: {e}")))?
    .enable_addr_auto();
    daemon
        .register(info)
        .map_err(|e| Error::Protocol(format!("mdns register: {e}")))?;
    Ok(daemon)
}

/// Browse for a drift host named `instance_name`. Returns the first resolved
/// address, or None after `timeout`.
pub async fn resolve(instance_name: &str, timeout: Duration) -> Option<SocketAddr> {
    let daemon = ServiceDaemon::new().ok()?;
    let receiver = daemon.browse(SERVICE_TYPE).ok()?;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let _ = daemon.shutdown();
            return None;
        }
        match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                let name_matches = info
                    .get_fullname()
                    .strip_suffix(&format!(".{SERVICE_TYPE}"))
                    .map(|n| n == instance_name)
                    .unwrap_or(false);
                if name_matches {
                    if let Some(ip) = info.get_addresses().iter().next() {
                        let addr = SocketAddr::new(*ip, info.get_port());
                        let _ = daemon.shutdown();
                        return Some(addr);
                    }
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => {
                let _ = daemon.shutdown();
                return None;
            }
        }
    }
}
