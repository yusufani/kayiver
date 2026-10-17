//! Let the system mDNSResponder advertise for us instead of maintaining a
//! second multicast listener/parser on every network interface while idle.
use std::ffi::{c_char, c_void, CString};
use crate::{Error, Result};

type Ref = *mut c_void;
type Reply = unsafe extern "C" fn(Ref, u32, i32, *const c_char, *const c_char, *const c_char, Ref);
#[link(name = "System")]
extern "C" {
    fn DNSServiceRegister(service: *mut Ref, flags: u32, interface: u32, name: *const c_char,
        kind: *const c_char, domain: *const c_char, host: *const c_char, port: u16,
        txt_len: u16, txt: *const c_void, reply: Reply, context: Ref) -> i32;
    fn DNSServiceSetDispatchQueue(service: Ref, queue: Ref) -> i32;
    fn DNSServiceRefDeallocate(service: Ref);
}
extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: Ref) -> Ref;
    fn dispatch_sync_f(queue: Ref, context: Ref, work: unsafe extern "C" fn(Ref));
    fn dispatch_release(queue: Ref);
}
struct State { service: Ref }
pub struct Advertisement { state: *mut State, queue: Ref }
// The reference is accessed only on its serial dispatch queue after setup;
// dropping on another thread synchronously disposes it on that same queue.
unsafe impl Send for Advertisement {}

unsafe extern "C" fn reply(_: Ref, _: u32, error: i32, _: *const c_char,
    _: *const c_char, _: *const c_char, context: Ref) {
    if error != 0 {
        tracing::warn!(error, "Bonjour advertisement failed; configured addresses still work");
        let state = &mut *context.cast::<State>();
        if !state.service.is_null() {
            DNSServiceRefDeallocate(state.service);
            state.service = std::ptr::null_mut();
        }
    }
}
unsafe extern "C" fn dispose(context: Ref) {
    let state = Box::from_raw(context.cast::<State>());
    if !state.service.is_null() { DNSServiceRefDeallocate(state.service); }
}
impl Drop for Advertisement {
    fn drop(&mut self) { unsafe {
        // dns_sd.h requires deallocation on the queue used for callbacks.
        dispatch_sync_f(self.queue, self.state.cast(), dispose);
        dispatch_release(self.queue);
    } }
}

pub fn advertise(instance_name: &str, port: u16) -> Result<Advertisement> {
    let name = CString::new(instance_name).map_err(|_| Error::Protocol("invalid Bonjour instance name".into()))?;
    unsafe {
        let queue = dispatch_queue_create(c"app.kayiver.discovery".as_ptr(), std::ptr::null_mut());
        if queue.is_null() { return Err(Error::Protocol("Bonjour dispatch queue unavailable".into())); }
        let state = Box::into_raw(Box::new(State { service: std::ptr::null_mut() }));
        let error = DNSServiceRegister(&mut (*state).service, 0, 0, name.as_ptr(),
            c"_kayiver._tcp.".as_ptr(), c"local.".as_ptr(), std::ptr::null(), port.to_be(),
            0, std::ptr::null(), reply, state.cast());
        if error != 0 {
            drop(Box::from_raw(state));dispatch_release(queue);
            return Err(Error::Protocol(format!("Bonjour registration: {error}")));
        }
        let error = DNSServiceSetDispatchQueue((*state).service, queue);
        if error != 0 {
            dispose(state.cast());dispatch_release(queue);
            return Err(Error::Protocol(format!("Bonjour event queue: {error}")));
        }
        Ok(Advertisement { state, queue })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "live Bonjour registration, resolution and withdrawal on the local network"]
    fn native_advertisement_lifecycle() {
        use mdns_sd::{ServiceDaemon, ServiceEvent};
        use std::time::{Duration, Instant};
        let name=format!("kayiver-discovery-check-{}",std::process::id());
        let fullname=format!("{name}.{}",super::super::SERVICE_TYPE);
        let browser=ServiceDaemon::new().unwrap();
        let events=browser.browse(super::super::SERVICE_TYPE).unwrap();
        let advertisement=super::advertise(&name,24817).unwrap();
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Bonjour registration did not resolve") {
                ServiceEvent::ServiceResolved(info) if info.get_fullname()==&fullname => {
                    assert_eq!(info.get_port(),24817);assert!(!info.get_addresses().is_empty());break;
                }
                _=>{}
            }
        }
        drop(advertisement);
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Bonjour registration did not withdraw") {
                ServiceEvent::ServiceRemoved(_,removed) if removed==fullname=>break,
                _=>{}
            }
        }
        browser.shutdown().unwrap();
    }
}
