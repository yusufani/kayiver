# Kayıver — marka rehberi

## İsim

**Kayıver** (ASCII/teknik bağlamda **kayiver**). Türkçe "kayıvermek"ten emir kipi:
*"karşıya kayıver"* — imlecin iki makine arasında yaptığı şeyin ta kendisi.

- Kısa, telaffuzu kolay (ka-yı-ver), CLI'da rahat: `kayiver`
- Yazılım/marka olarak internette kullanımı yok (2026-07 itibarıyla doğrulandı;
  yalnızca alakasız sosyal medya/`Kayvar` kozmetik markası mevcut).
- Değerlendirilen alternatifler: *drift* (çok jenerik, Drift.com çakışması),
  *Limen/Seam/Comet/Glide/Glisk/Kursor/Kavis/Yaka/Lope* (hepsinde mevcut
  yazılım/şirket çakışması), *Süzül/suzul* ve *Arakesit* (boş ama kısa listede
  ikinci/üçüncü sırada kaldılar).

## Kullanım

| Bağlam | Yazım |
|---|---|
| Ürün adı (TR) | Kayıver |
| Ürün adı (EN metin içinde) | Kayiver |
| Binary / CLI / paket | `kayiver` |
| Bundle ID (macOS) | `app.kayiver` |
| Config dizini | `kayiver/` |
| mDNS servisi | `_kayiver._tcp.local.` |

Slogan: **"Karşıya kayıver."** / EN: *"One keyboard, every screen."*

## Logo

Kavram: iki ekranın arasındaki **portal kenarından** süzülüp geçmiş bir imleç.
Dikey degrade çizgi = geçilen kenar; solda geride kalan hayalet izler,
sağda gelen beyaz imleç.

- Master dosyalar: `assets/logo/kayiver-icon.svg` (uygulama ikonu, koyu squircle),
  `kayiver-mark.svg` (şeffaf zemin, `currentColor` imleç — UI/doc başlıkları),
  `kayiver-menubar-template.svg` (macOS menü çubuğu template ikonu, siyah+alfa).
- Üretim: `scripts/gen-icons.sh` → `assets/icons/` (`Kayiver.icns`,
  `kayiver.ico`, `menubarTemplate[@2x].png`, PNG'ler).

## Renkler

| Rol | Değer |
|---|---|
| Accent (başlangıç) | `#5eead4` (teal) |
| Accent (bitiş) | `#34d399` (yeşil) |
| Zemin (koyu) | `#0a0d13` → `#1b2230` degrade |
| Metin (koyu tema) | `#eaedf3` |
| Uyarı / hata | `#fbbf24` / `#f87171` |

Accent her zaman degrade olarak (teal→yeşil, 135°) kullanılır; tek başına
gerekiyorsa `#34d399`.

## Ses tonu

Sade, teknik, kendine güvenen; Türkçe'de samimi ("kayıver"), İngilizce'de
kısa ve net. Abartılı pazarlama dili yok.
