# drift — durum & devam notları (2026-07-12)

Bu dosya: **ne nerede**, **ne çalışıyor**, **ne kaldı**. Yeni bir oturumda buradan devam et.

---

## Nerede ne var

| Şey | Yer |
|---|---|
| **Proje kökü** | `~/Desktop/drift` (git repo, tüm iş commit'li) |
| Rust workspace | `crates/drift-core` (lib) + `apps/drift` (uygulama) |
| Platform kodu | `apps/drift/src/platform/{macos,windows,stub}.rs` |
| Motor (host/client) | `apps/drift/src/engine/{host,client,pairing}.rs` |
| UI (web, gömülü) | `apps/drift/src/ui/{mod.rs,index.html}` |
| Windows tray | `apps/drift/src/platform/tray_windows.rs` |
| **Mac binary** | `~/Desktop/drift/target/release/drift` — `drift` olarak `/opt/homebrew/bin`'e symlink'li (global çalışır) |
| **Mac config** | `~/Library/Application Support/drift/config.toml` |
| **Windows exe** | `%LOCALAPPDATA%\drift\drift.exe` |
| **Windows config** | `%APPDATA%\drift\config.toml` |
| Windows'a SSH anahtarı | `~/.ssh/drift_win_ed25519` |
| Docs | `docs/` (ARCHITECTURE, PROTOCOL, SECURITY, PLATFORMS, ROADMAP) |

Derleme: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` sonra `cargo build --release`.
Windows için cross-compile: `cargo build --release --target x86_64-pc-windows-gnu`.

---

## Çalışan kurulum (kalıcı)

- **Doğrudan Ethernet kablosu** Mac (`en8`, USB-LAN) ↔ Windows (Realtek). Link-local:
  Mac **169.254.47.23**, Windows **169.254.253.227**. Gateway YOK → internet Wi-Fi'de kalır.
  drift bu kablodan gidiyor: Windows config `addr = "169.254.47.23:24817"`.
  Ölçülen RTT: **~1.6ms medyan, max ~3ms, spike yok** (Wi-Fi 5.7ms/spike'lıydı).
- **Windows istemcisi** SYSTEM zamanlanmış görevi `"drift"` ile başlıyor:
  görev → `drift.exe launch-session` → `WTSQueryUserToken`+`CreateProcessAsUserW`
  (`lpDesktop=winsta0\default`) → `drift run` **görünür masaüstünde** (injection çalışsın diye).
  Trigger: AtLogon + on-demand. **SSH/normal görev enjeksiyon yapamıyor** (session 0 sorunu) —
  bu yüzden bu mekanizma şart.
- **Mac host:** `drift run` (Accessibility + Input Monitoring izni gerekli; izin başlatan
  bağlama bağlı — ad-hoc imzalı binary). Gömülü UI: `http://127.0.0.1:24818`.
- **Eşleştirme yapılı** (SPAKE2 PSK config'lerde). Layout: `mac.right -> yusuf` (yusuf = Windows).
- **Tray ikonu** (Windows), **app-pencere UI** (Chrome `--app`), **panic escape** (3×Esc).

### SSH ile Windows'u sürme (SYSTEM görevleriyle)
```sh
ssh -i ~/.ssh/drift_win_ed25519 yusuf@192.168.0.13 '<powershell>'
# istemciyi başlat: Start-ScheduledTask -TaskName "drift"
# oturumda bir komut çalıştır: DRIFT_LAUNCH_ARGS ile launch-session (bkz. geçmiş)
```

---

## Ortak monitör (Samsung LC32G5) — durum

- **DDC ile giriş değiştirme GÜVENİLMEZ** (Samsung Odyssey firmware bug'ı, web araştırmasıyla
  doğrulandı: yazma kabul ediliyor ama uygulanmıyor; okuma da tutarsız). `auto_switch=false` bırakıldı.
  Bulunan (ama güvenilmez) değerler: Mac index 2 → 18 (Windows'a); Windows index 0 → 5/3/9 (Mac'e).
- **Yeni yaklaşım (doğru yol): display disable/enable.** Pasif makinede paylaşımlı ekranı
  OS masaüstünden çıkar → cursor o görünmeyen ekrana kaçmaz.
  - **Mac tarafı ÇALIŞIYOR** (doğrulandı): `drift display disable 2` → masaüstü 5120→2560,
    `drift display enable 2` → 5120. `CGConfigureDisplayMirrorOfDisplay` (public API, geri alınabilir).
  - Paylaşımlı monitör = Mac'te **index 2**, Windows'ta **index 0**.
  - **Manuel workflow (şimdi kullanılabilir):** monitörü fiziksel tuşla Windows'a alınca
    `drift display disable 2`; Mac'e alınca `drift display enable 2`.

---

## KALAN İŞLER (TODO — öncelik sırası)

1. **Windows display disable/enable** — `apps/drift/src/platform/windows.rs::set_display_enabled`
   şu an stub. `ChangeDisplaySettingsExW` ile detach/attach (DEVMODE'u dosyaya kaydedip
   geri yüklemek gerek, çünkü enable/disable ayrı process'ler). Index→`\\.\DISPLAYn` eşlemesi.
2. **Ortak monitör otomasyonu** — "hangisi aktif?" seçimi:
   - UI'da (veya hotkey) kullanıcı "ortak monitör artık X'i gösteriyor" der.
   - drift o makinede display'i **enable**, diğerinde **disable** eder.
   - Host↔client **yeni mesaj** gerek (host, client'a "display'ini disable/enable et" desin).
   - **Hotkey + 5 sn onaylı prompt** ("geçireyim mi?").
3. **UI'da monitör-bazlı sürükleme** — şu an makine bütün olarak sürükleniyor; kullanıcı
   tek tek monitörleri sürükleyip **üst üste bırakınca "ortak" işaretlemeli**. (`ui/index.html`
   per-monitor drag; config'e shared-monitor işareti.)
4. **Mac menü-çubuğu ikonu** — Windows tray'in karşılığı (NSStatusItem). Dikkat: ana thread'de
   NSApplication run loop gerekir; şu an ana thread tokio `block_on`'da → yapı değişikliği lazım.
5. **3 saniyelik geçiş animasyonu** (daha önce istendi, ertelendi) — her ekranda yarı-saydam
   overlay pencere. macOS NSWindow / Windows layered window. Büyük GUI işi.
6. **Cleanup:** DDC `auto_switch` config'lerde false; `drift display set` komutu duruyor (manuel).
   Windows'ta test görevleri temizlendi (`drift-pos/caps/dlist/dset/injtest` vb. silindi, sadece
   `drift` görevi kaldı).

---

## Önemli teknik notlar (tekrar keşfetme)

- **macOS TCC:** izin binary'nin cdhash'ine bağlı → her rebuild izni geçersiz kılar. Binary
  ad-hoc imzalı (`codesign --force --sign -`). Rebuild sonrası host yeniden başlarken izin
  gerekebilir; process başlangıçta izinleri cache'liyor → gerekirse `drift run`'ı tekrar başlat.
- **Windows injection:** SADECE `winsta0\default` input desktop'ında çalışır. Zamanlanmış görev
  (Interactive bile olsa) yanlış desktop'a düşüyor → `launch-session` (SYSTEM + CreateProcessAsUser) şart.
- **DPI:** Windows'ta `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` — olmazsa ölçekli
  ekranda enjeksiyon yanlış yere gider.
- **Log:** `DRIFT_LOGFILE=<path>` env → flush'lı log dosyası (arka plan instance'ı için).
- **Cross-compile:** `snow` crate'te `ring`/`std` KAPALI (pure-Rust crypto), yoksa mingw derlemez.
