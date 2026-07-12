# Kayıver — durum & devam notları (2026-07-12, gece)

Bu dosya: **ne nerede**, **ne çalışıyor**, **ne kaldı**. Yeni oturumda buradan devam et.

> **İsim değişti:** drift → **Kayıver** (`kayiver`). Gerekçe ve marka rehberi:
> `docs/BRANDING.md`. Eski config'ler otomatik migrate edilir; eski LaunchAgent /
> Run-key / scheduled task kayıtları temizlendi.

---

## Nerede ne var

| Şey | Yer |
|---|---|
| **Proje kökü** | `~/Desktop/drift` (klasör adı eski; git repo, tüm iş commit'li) |
| Rust workspace | `crates/kayiver-core` (lib) + `apps/kayiver` (uygulama) |
| Platform kodu | `apps/kayiver/src/platform/{macos,windows,stub}.rs` |
| Motor (host/client) | `apps/kayiver/src/engine/{host,client,pairing}.rs` |
| UI (web, gömülü) | `apps/kayiver/src/ui/{mod.rs,index.html}` — Kayıver markalı, TR |
| macOS app kabuğu | `apps/kayiver/src/gui.rs` (tao + wry + tray-icon) |
| **Mac app** | `/Applications/Kayiver.app` (`packaging/macos/build-app.sh --install`) |
| Mac CLI | `/opt/homebrew/bin/kayiver` → app içindeki binary |
| **Mac config** | `~/Library/Application Support/kayiver/config.toml` |
| **Windows exe** | `C:\Users\yusuf\AppData\Local\kayiver\kayiver.exe` (ikon+versiyon gömülü) |
| **Windows config** | `%APPDATA%\kayiver\config.toml` (drift'ten migrate edildi) |
| Windows görevi | Scheduled task **"kayiver"** (SYSTEM, AtLogon + on-demand); eski "drift" görevi silindi |
| Windows'a SSH | `ssh -i ~/.ssh/drift_win_ed25519 yusuf@192.168.0.13 '<powershell>'` |
| Marka + ikonlar | `assets/logo/*.svg` (master), `assets/icons/` (`scripts/gen-icons.sh` üretir) |
| Android companion | `apps/android` (Compose; Android Studio ile derlenir) |
| Docs | `docs/` (ARCHITECTURE, PROTOCOL v2, SECURITY, PLATFORMS, ROADMAP, BRANDING) |

Derleme: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` → `cargo build --release`.
Windows cross-compile: `cargo build --release --target x86_64-pc-windows-gnu`.
Mac app paketi: `packaging/macos/build-app.sh [--install]`.

---

## Çalışan kurulum (kalıcı)

- **Doğrudan Ethernet** Mac (`en8`) ↔ Windows: Mac **169.254.47.23**, Windows
  **169.254.253.227**; Windows config `addr = "169.254.47.23:24817"`. RTT ~1.6 ms.
- **Windows istemcisi**: task "kayiver" → `kayiver.exe launch-session` (SYSTEM) →
  `WTSQueryUserToken` + `CreateProcessAsUserW` (`winsta0\default`) → görünür
  masaüstünde `kayiver run`. SSH'tan başlatma: `Start-ScheduledTask -TaskName kayiver`.
- **Mac host**: `/Applications/Kayiver.app` (menü çubuğu ikonu; motor arka planda).
  Gömülü editör: `http://127.0.0.1:24818`, menüden "Kayıver'ı Aç" → yerel pencere.
  Headless istersen: `kayiver run --no-gui`.
- Eşleştirme (SPAKE2 PSK) aynen taşındı; makine adları: mac tarafı hostname
  (`yusufs-macbook-pro`), Windows `yusuf`.

## Yeni: Ortak monitör otomasyonu (bu oturumda yazıldı)

- **Windows display detach/attach kodlandı**
  (`windows.rs::set_display_enabled`, `ChangeDisplaySettingsExW`; önceki mod
  config yanına `display_state_*.json` olarak kaydedilir, geri yüklenir).
- **Protokol v2**: `DisplayPower{index,on}` (host→client) + `DisplayPowerResult`.
- **Sahiplik akışı**: `kayiver monitor <makine|toggle|status>` · UI'daki düğmeler ·
  menü çubuğu · **Cmd/Ctrl+Alt+M** hotkey (host yakalar, iki modda da) ·
  Android app. Host, yeni sahibin ekranını attach eder, diğerininkini detach.
- **Config**: `[shared_monitor] local_index / peer / peer_index / hotkey`
  (editördeki "Ortak monitörü seç" akışı yazar; hot-reload).
- Açılışta sahip çıkarımı: mac'te panel mirror'daysa sahip = peer.
- DDC (`display.auto_switch`) Samsung firmware bug'ı yüzünden kapalı duruyor;
  `kayiver display set` manuel olarak duruyor.

## Yeni: LAN API + Android

- `kayiver remote enable` → 0.0.0.0:**24819**, tüm istekler
  `Authorization: Bearer <token>`. Varsayılan **kapalı**.
- `apps/android`: durum + ortak monitör kumandası (Compose). Studio ile derle.

---

## KALAN İŞLER / doğrulama

1. **[KULLANICI] macOS izin onayı** — Kayiver.app yeni binary olduğu için
   Erişilebilirlik + Giriş İzleme bir kez yeniden onaylanmalı (System Settings
   açık bekliyor). Onaylanınca motor kendiliğinden başlar.
2. **Uçtan uca ortak monitör testi** — izinden sonra: editörde "Ortak monitörü
   seç" (Mac index 2 ↔ Windows index 0 panel), sonra `kayiver monitor yusuf` /
   `kayiver monitor <mac-adı>` gidiş-dönüş; Windows detach/attach'i gerçek
   donanımda doğrula (kod cross-compile edildi ama canlıda denenmedi).
3. **3 sn geçiş animasyonu** (eski istek) — yarı saydam overlay pencere; büyük
   GUI işi, yapılmadı.
4. Android app'i Studio'da bir kez derleyip APK almak (SDK bu Mac'te yok).
5. İstersen repo klasörünü `~/Desktop/kayiver`e taşı (dokümanlarda path
   `~/Desktop/drift`; taşırsan `/opt/homebrew/bin/kayiver` symlink'ini güncelle).

---

## Önemli teknik notlar (tekrar keşfetme)

- **macOS TCC**: izin binary cdhash'ine bağlı → her rebuild/`--install` izni
  düşürebilir; `kayiver doctor` durumu gösterir.
- **Windows injection**: yalnız `winsta0\default`ta çalışır → SYSTEM
  `launch-session` mekanizması şart (görev: "kayiver").
- **Windows display detach**: indeks = attach'li ekranlar içindeki 0-tabanlı
  sıra; enable, state dosyasından (yoksa en iyi moddan) geri yükler; son
  ekran detach edilmez.
- **DPI**: `PER_MONITOR_AWARE_V2` şart (windows.rs::init).
- **Log**: `KAYIVER_LOGFILE=<path>`; uzaktan tek komut: `KAYIVER_LAUNCH_ARGS`.
- **Cross-compile**: `snow` pure-Rust modda (ring yok); winres, mingw
  `x86_64-w64-mingw32-windres/ar` kullanır (build.rs ayarlıyor).
- **mDNS**: servis tipi artık `_kayiver._tcp.local.`.
