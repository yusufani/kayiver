//! Bridge for legacy KVM balloons converted to modern Windows notifications.
//! Reads only the notification identity registered to our installed KVM path.
//! No global notification setting, listener permission, or other app history.
fn connection_title(title: &str) -> bool {
    matches!(title.trim(), "Kayıver bağlandı" | "Kayiver baglandi" | "Kayıver bağlantısı koptu" | "Kayiver baglantisi koptu")
}
#[cfg(target_os="windows")]
mod win {
    use super::*;
    use std::time::{Duration,Instant};
    use windows::{core::{HSTRING,PCWSTR,PWSTR},UI::Notifications::ToastNotificationManager,
        Win32::System::{Registry::*,WinRT::{RoInitialize,RoUninitialize,RO_INIT_MULTITHREADED}}};
    struct Key(HKEY);
    impl Drop for Key {fn drop(&mut self){unsafe{let _=RegCloseKey(self.0);}}}
    fn wide(s:&str)->Vec<u16>{s.encode_utf16().chain(Some(0)).collect()}
    fn own_ids()->Vec<HSTRING> {
        let Some(root)=std::env::var_os("LOCALAPPDATA") else{return Vec::new()};
        let expected=std::path::PathBuf::from(root).join("kayiver/kayiver.exe").to_string_lossy().replace('/',"\\");
        let mut key=HKEY::default();let path=wide("Control Panel\\NotifyIconSettings");
        if unsafe{RegOpenKeyExW(HKEY_CURRENT_USER,PCWSTR(path.as_ptr()),None,KEY_READ,&mut key)}.0!=0{return Vec::new();}
        let key=Key(key);let value=wide("ExecutablePath");let mut ids=Vec::new();
        for index in 0..1024 {
            let mut name=[0u16;128];let mut n=name.len() as u32;
            let result=unsafe{RegEnumKeyExW(key.0,index,Some(PWSTR(name.as_mut_ptr())),&mut n,None,None,None,None)};
            if result.0==259{break;}if result.0!=0{continue;}
            let id=String::from_utf16_lossy(&name[..n as usize]);
            if id.is_empty()||!id.bytes().all(|b|b.is_ascii_digit()){continue;}
            let mut executable=[0u16;2048];let mut bytes=std::mem::size_of_val(&executable) as u32;
            if unsafe{RegGetValueW(key.0,PCWSTR(name.as_ptr()),PCWSTR(value.as_ptr()),RRF_RT_REG_SZ,None,Some(executable.as_mut_ptr().cast()),Some(&mut bytes))}.0!=0{continue;}
            let end=executable.iter().position(|&c|c==0).unwrap_or(executable.len());
            if String::from_utf16_lossy(&executable[..end]).eq_ignore_ascii_case(&expected){ids.push(HSTRING::from(format!("NotifyIconGeneratedAumid_{id}")));}
        }
        ids
    }
    pub struct Guard {ids:Vec<HSTRING>,last:Instant}
    impl Guard {
        pub fn new()->windows::core::Result<Self>{
            unsafe{RoInitialize(RO_INIT_MULTITHREADED)?;}
            Ok(Self{ids:own_ids(),last:Instant::now()-Duration::from_secs(1)})
        }
        pub fn clear_connection_banners(&mut self)->windows::core::Result<()> {
            if self.last.elapsed()<Duration::from_millis(500){return Ok(());}self.last=Instant::now();
            let history=ToastNotificationManager::History()?;
            for id in &self.ids {
                let notices=history.GetHistoryWithId(id)?;let count=notices.Size()?;
                if count==0 || count>128 {continue;}
                let mut all_connection=true;
                for index in 0..count {
                    let content=notices.GetAt(index)?.Content()?;
                    let texts=content.GetElementsByTagName(&HSTRING::from("text"))?;
                    if texts.Length()?==0 || !connection_title(&texts.Item(0)?.InnerText()?.to_string_lossy()) {all_connection=false;break;}
                }
                // History-backed ToastNotification objects cannot be hidden
                // with ToastNotifier.Hide. Clear only this validated sender,
                // and only when every record is a known connection banner.
                if all_connection {history.ClearWithId(id)?;tracing::warn!("cleared {count} own connection banner(s)");}
            }
            Ok(())
        }
    }
    impl Drop for Guard{fn drop(&mut self){unsafe{RoUninitialize();}}}
}
#[cfg(target_os="windows")]
pub use win::Guard;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn only_known_connection_titles_are_eligible(){
        assert!(connection_title("Kayıver bağlandı"));assert!(connection_title("Kayiver baglandi"));
        assert!(connection_title("Kayıver bağlantısı koptu"));
        assert!(!connection_title("Windows update"));assert!(!connection_title("Kayiver security warning"));assert!(!connection_title(""));
    }
}
