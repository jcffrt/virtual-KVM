# ADR 0034 — Gadget USB en una ESP32-S3 por el enlace ligero

- Estado: aceptada (2026-11-13). Rama `gadget-esp32`. Firmware 0.2.2 probado en GAMER a través del hub: conmutación, teclado, ratón, trackball y LED. Actualización por el enlace (0.3.0 → 0.3.1) probada en la Zero: 584 KB en 7 s.

## Contexto

El gadget de Linux (ADR 0007) necesita una placa con Linux, y la Zero 2W ya hace de hub. Una ESP32-S3 (USB OTG de velocidad completa, WiFi de 2,4 GHz, unos 5 €) puede hacer de gadget para un PC: se conecta al hub por WiFi y presenta el receptor por USB.

La placa no puede hablar QUIC: no hay una pila QUIC con TLS 1.3 razonable en `no_std`. Con 512 KB de RAM, el canal tiene que ser pequeño.

## Decisión

### Enlace ligero (`crates/light`, `no_std` + `alloc`)

- **Transporte:** TCP al puerto **4434** del hub (`light_listen` en `hub.toml`, activo por defecto; quitar la clave lo desactiva). `TCP_NODELAY` en el hub.
- **Apertura:**
  - el nodo envía `RKL1` y un nonce de 16 bytes;
  - el hub responde `RKL1` y su propio nonce.
- **Claves:** HKDF-SHA256 sobre el **token de emparejamiento del hub** (el mismo de todos los agentes), con los dos nonces como sal. Sale una clave por sentido.
- **Tramas:** `u16` LE de longitud, más ChaCha20-Poly1305 del mensaje en `postcard`. El nonce del AEAD es un contador por sentido.
- **Garantías:**
  - quien no tiene el token no produce una trama que el hub acepte;
  - una sesión grabada no se puede reproducir, porque cada lado aporta un nonce nuevo;
  - una trama no se puede devolver al otro lado, porque cada sentido tiene su clave.
  No hay huella del hub fijada (TOFU): quien suplante al hub también necesita el token.
- **Mensajes:**
  - nodo → hub: `Hello { version, node_id, name, os, firmware, max_interfaces }`, `Output`, `Ping`, `Pong`;
  - hub → nodo: `Welcome`, `Reject`, `Gadget`, `Report`, `Ping`, `Pong`, `Update`, `UpdateData`, `UpdateEnd`;
  - nodo → hub, versión 2: `UpdateResult`.
- **Versión propia** (`light::VERSION`), independiente de `PROTO_VERSION`. La 2 añade la actualización de firmware; el hub sigue aceptando placas en la 1 (`MIN_VERSION`), sin actualizarlas.

### En el hub (`hub/src/light.rs`)

- Cada placa es un nodo `Agent` normal: tiene índice, entra en el atajo y en la conmutación, y aparece en el widget. El módulo traduce entre la placa y el router; el router no cambia.
- **El hub decide el gadget.**
  - Aplica a la `DeviceList` las reglas del gadget de Linux (ADR 0031), ahora en `proto::gadget`, con el límite que anuncia la placa.
  - Envía `Gadget` con el VID/PID y los textos del receptor. Por cada interfaz van el descriptor, la subclase y el protocolo boot, el informe más largo y los informes de «nada pulsado».
  - Las interfaces que no caben van al router como `Unexposed`, igual que con el gadget de Linux.
  - Con un PC Linux no se expone la interfaz HID++ (ADR 0007).
- **Informes:**
  - los datagramas pasan a `Report`;
  - la copia redundante (ADR 0004) se descarta por `seq`, porque en TCP no sirve;
  - si la cola de 256 se llena, se pierden informes, como hace quinn con los datagramas viejos.
- **Salidas:** `Output` (LED) va a `Router::on_output`.
- **Ping cada 1 s.** Si la placa pasa 5 s sin enviar nada, se la da por caída, como con QUIC.

### En la placa (`firmware/rawkvm-esp-gadget`)

- **USB:**
  - el dispositivo se construye al arrancar a partir del último `Gadget` (`usb.rs`), en el núcleo 1;
  - cada interfaz tiene solo un endpoint IN, y los LED llegan por SET_REPORT en el endpoint de control;
  - caben **cuatro interfaces**, el mismo límite que el gadget de Linux.
- **Un `Gadget` nuevo:**
  - si dura 500 ms sin cambiar (el hub anuncia una lista por cada interfaz que abre), se guarda en la RAM del RTC y la placa se reinicia;
  - al arrancar, antes de encender el núcleo 1, también se escribe en flash (último sector de `nvs`): escribir la flash con el núcleo 1 en marcha cuelga la placa;
  - un `Gadget` vacío se ignora, y se conserva el USB actual.
- **Sin ningún gadget guardado,** la placa arranca sin USB, espera el primero del hub y se reinicia con él.
- **Si se pierde el hub,** la placa escribe los informes de «nada pulsado» y vuelve a conectar a 1, 2, 4 y 5 s.
- **Ajustes en `firmware/wifi.env`,** que no está en git: `WIFI_SSID`, `WIFI_PASS`, `HUB_TOKEN`, `HUB_ADDR` (por defecto 192.168.1.39), `NODE_NAME` y `NODE_OS` (por defecto `windows`, que el hub usa para el perfil del teclado, ADR 0014).
- **Identidad del nodo:** `RAWKVM-ESP` seguido de la MAC de la placa. Es estable y no se guarda en ningún sitio. Con `NODE_ID` en `wifi.env` la placa toma el id de un nodo que el hub ya conoce, con su índice de atajo, sus pantallas en la disposición y su helper de bordes: así sustituye al gadget de la Zero en GAMER sin tocar el PC. El agente que reemplaza tiene que estar parado.
- **El puerto 7790 queda para servicio:** ping y registro. No acepta firmware.

### Actualización del firmware (versión 2)

- **El hub reparte el firmware** que haya en `{state_dir}/esp-gadget.bin` (la imagen de `espflash save-image`). Lo lee en cada conexión: cambiar el fichero no requiere reiniciar el hub, pero la placa lo recibe en su siguiente conexión.
- Si la versión de la imagen (la de `esp_app_desc`, que es la del `Cargo.toml`) **es distinta** de la que anuncia la placa, el hub le envía `Update` tras `Welcome`. Distinta, no mayor: poner una imagen anterior sirve para volver atrás.
- La placa **se reinicia en modo actualización** (sin USB y sin núcleo 1, para poder escribir la flash) y conecta de nuevo con `max_interfaces = 0`. El hub no la registra como nodo: le manda `Update`, la imagen en `UpdateData` de 1 KB y `UpdateEnd`.
- La placa escribe la partición OTA que no está en uso, **comprueba el SHA-256** anunciado, la activa y contesta `UpdateResult`. Después se reinicia con el firmware nuevo.
- **Autenticación:** las tramas van cifradas y autenticadas con el token. Quien no lo tenga no puede cargar un firmware.
- **Tres intentos** por placa y versión de imagen; después el hub deja de ofrecerla hasta que se reinicie. Un fallo no deja la placa sin arrancar: la partición activa no cambia hasta que la imagen está entera y verificada.
- El modo actualización dura como mucho 2 minutos; después la placa vuelve a arrancar normal.
- **Memoria:** el driver WiFi toma sus búferes del heap (por defecto hasta 32 de recepción y 32 de envío, de unos 1,6 KB). Con los 72 KB de RAM recuperada, el tráfico del ratón lo agotaba. La placa usa además 96 KB de RAM normal y limita los búferes a 16 y 12; el hub junta en una sola escritura las tramas que tiene en cola.
- **Arranque seguro:** si la ejecución anterior acabó en panic, la placa arranca en modo actualización (WiFi sin USB, 2 minutos), para poder cargar un firmware corregido sin cable.
- **embassy-usb** necesita `max-handler-count-8`: cada interfaz HID registra un handler, más el de eventos del dispositivo.

## Consecuencias

- Un PC por cable sin instalar nada, con una placa de pocos euros y sin una segunda Raspberry.
- **Latencia:** RTT de unos 5 ms de mediana por WiFi, más 1 ms de sondeo USB. Es parecida a la del gadget de la Zero.
- **Un cambio de receptores en el hub** (añadir o quitar uno) hace que la placa se reinicie y que el PC vuelva a enumerarla. Con el gadget de Linux también se re-enumeraba.
- **El token de emparejamiento queda grabado en el firmware** de la placa. Es lo mismo que guarda cualquier agente en su configuración.

## Pendiente

- **Descubrimiento por mDNS** en lugar de `HUB_ADDR`.
- **El widget** no distingue la placa de otro agente. Mostrar su firmware y su señal WiFi requiere ampliar `NodeInfo`, es decir, un cambio de `PROTO_VERSION` para todos los agentes: queda para el próximo.
- **El paquete del hub** no incluye el firmware: hoy la imagen se copia a mano a `/var/lib/rawkvm/esp-gadget.bin`.
