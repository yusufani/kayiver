# Kayıver Remote (Android)

Telefondan Kayıver'ı izleyip **ortak monitörü** el değiştirten companion uygulama.
Tam KVM istemcisi değildir (Android'de güvenilir input enjeksiyonu yok); durum +
kumanda içindir.

## Ne yapar

- Ana makinedeki Kayıver'ın durumunu gösterir (bağlı makineler, gecikme,
  imleç hangi makinede).
- Ortak monitörü tek dokunuşla bir makineden diğerine verir
  (`POST /api/shared`) — monitördeki fiziksel kaynak düğmesine basmadan önce
  telefondan "kayıver".

## Kurulum

1. Mac'te LAN API'yi aç:

   ```sh
   kayiver remote enable     # port 24819 + token üretir
   ```

   ve Kayıver'ı yeniden başlat (menü çubuğu → Çık, sonra tekrar aç).

2. Bu klasörü **Android Studio** ile aç (Studio, Gradle wrapper'ı kendisi
   oluşturur), `app` modülünü telefona kur.

3. Uygulamada ayarlar: **host** = Mac'in LAN IP'si, **port** = `24819`,
   **token** = `kayiver remote enable` çıktısındaki değer.

## Güvenlik

API varsayılan olarak kapalıdır (`remote.enabled = false`). Açıkken tüm
istekler `Authorization: Bearer <token>` ister; token config dosyasında
saklanır (`kayiver config-path`). Trafiği yalnızca güvendiğin LAN'da kullan.
