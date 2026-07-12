//! Start kayiver at login.
//!
//! macOS: LaunchAgent plist (`~/Library/LaunchAgents/app.kayiver.plist`)
//! with KeepAlive, so kayiver also restarts if it ever crashes.
//! Windows: `HKCU\...\Run` registry value (no admin rights needed).

use anyhow::Result;

pub fn apply(enable: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    imp::apply(enable, &exe)?;
    println!("autostart {}", if enable { "enabled" } else { "disabled" });
    Ok(())
}

/// Whether login autostart is currently configured.
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{Context, Result};
    use std::path::Path;

    const LABEL: &str = "app.kayiver";

    fn plist_path() -> Result<std::path::PathBuf> {
        Ok(dirs::home_dir()
            .context("no home dir")?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    pub fn apply(enable: bool, exe: &Path) -> Result<()> {
        // Clean up the LaunchAgent from the app's old name ("drift").
        if let Some(home) = dirs::home_dir() {
            let legacy = home.join("Library/LaunchAgents/dev.drift.kvm.plist");
            if legacy.exists() {
                let _ = std::process::Command::new("launchctl")
                    .args(["unload", "-w", legacy.to_str().unwrap()])
                    .output();
                let _ = std::fs::remove_file(&legacy);
            }
        }
        let path = plist_path()?;
        if enable {
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
                exe = exe.display()
            );
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, plist)?;
            let _ = std::process::Command::new("launchctl").args(["unload", path.to_str().unwrap()]).output();
            let out = std::process::Command::new("launchctl").args(["load", "-w", path.to_str().unwrap()]).output()?;
            anyhow::ensure!(out.status.success(), "launchctl load failed: {}", String::from_utf8_lossy(&out.stderr));
        } else if path.exists() {
            let _ = std::process::Command::new("launchctl").args(["unload", "-w", path.to_str().unwrap()]).output();
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn is_enabled() -> bool {
        plist_path().map(|p| p.exists()).unwrap_or(false)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use anyhow::Result;
    use std::path::Path;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Kayiver";

    pub fn apply(enable: bool, exe: &Path) -> Result<()> {
        // Clean up the Run value from the app's old name ("drift").
        let _ = std::process::Command::new("reg")
            .args(["delete", RUN_KEY, "/v", "DriftKVM", "/f"])
            .output();
        let out = if enable {
            std::process::Command::new("reg")
                .args(["add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &format!("\"{}\" run", exe.display()), "/f"])
                .output()?
        } else {
            std::process::Command::new("reg")
                .args(["delete", RUN_KEY, "/v", VALUE, "/f"])
                .output()?
        };
        anyhow::ensure!(
            out.status.success() || !enable, // delete of a missing value is fine
            "reg failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    pub fn is_enabled() -> bool {
        std::process::Command::new("reg")
            .args(["query", RUN_KEY, "/v", VALUE])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use anyhow::Result;
    use std::path::Path;

    pub fn apply(_enable: bool, _exe: &Path) -> Result<()> {
        anyhow::bail!("autostart is not implemented for this platform yet")
    }

    pub fn is_enabled() -> bool {
        false
    }
}
