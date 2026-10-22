# ADR 0005 — Agente macOS sobre Karabiner-DriverKit-VirtualHIDDevice, cliente en Rust, como LaunchDaemon

- Estado: aceptada (2026-10-22). Protocolo verificado contra el código de pqrs-org/Karabiner-DriverKit-VirtualHIDDevice `ac836ed` (paquete 8.6.0, driver 1.8.0, protocolo de cliente 7), que coincide con lo instalado en el Mac.

## Contexto

macOS no permite a un proceso de usuario crear dispositivos HID arbitrarios. Karabiner-DriverKit-VirtualHIDDevice ofrece un teclado y un puntero virtuales ya firmados y notarizados. Su daemon (`Karabiner-VirtualHIDDevice-Daemon`) escucha en un socket Unix **stream** solo para root:

```
/Library/Application Support/org.pqrs/tmp/rootonly/karabiner_virtual_hid_device_service.sock
```

(La ruta `.../rootonly/vhidd_server/` de versiones antiguas, con datagramas, ya no existe.) El daemon **no arranca solo**: lo lanza quien lo use (Karabiner-Elements lo registra con SMAppService).

Protocolo (`pqrs/unix_domain_stream` + `virtual_hid_device_service/client.hpp`):

- Trama: `u32 BE longitud | u8 tipo | [u64 BE request_id] | payload`. Tipos: heartbeat 0, user_data 1, health_check 2/3, request 4, response 5. Payload máximo 1024 B.
- Petición del cliente: `u16 client_protocol_version (7) | u8 request | struct` con el `memcpy` de la struct C++ empaquetada. Si la versión no coincide, el daemon responde vacío y no hace nada.
- El daemon empuja el estado como *requests* (`(response, bool)` por pares: driver_activated, driver_connected, driver_version_mismatched, keyboard_ready, pointing_ready) y espera respuesta. Corta peers sin tráfico durante 30 s, así que hay que enviar heartbeat.
- **No hay verificación de firma ni de equipo del cliente**: la única barrera son los permisos del directorio `rootonly`.
- Structs: `keyboard_input` (67 B: id 1, modificadores con el mismo orden de bits que HID, reservado, 32 × u16), `consumer_input` (id 2), `apple_vendor_top_case_input` (3), `apple_vendor_keyboard_input` (4), `generic_desktop_input` (7) — 65 B cada una —, `pointing_input` (8 B: u32 botones, x, y, rueda vertical y horizontal como i8). Parámetros del teclado: tres `uint64_t` (VID, PID, country code).

## Decisión

1. **Cliente reimplementado en Rust** (`crates/agent-macos/src/karabiner.rs` y `vhid.rs`), no un shim C++. Lo que hay que reproducir es pequeño y fijo (framing de unas 20 líneas, structs planas sin padding), y queda cubierto por tests de tamaños y bytes contra las cabeceras. El shim exigiría C++20 y el árbol vendorizado de pqrs (asio, type_safe, nod, dispatcher con hilos propios) para enviar esos mismos bytes. El riesgo es un cambio de protocolo de Karabiner: está acotado por `CLIENT_PROTOCOL_VERSION = 7` (si sube, el daemon ignora las peticiones y el log lo muestra con `keyboard_ready` sin llegar), y los tests apuntan a las cabeceras a revisar.
2. **Mapeo 1:1 de usages** (`mapper.rs`): el descriptor del dispositivo virtual lo fija Karabiner, así que no se puede reenviar byte a byte. Cada informe se decodifica con el descriptor del receptor (`proto::descriptor::DescriptorInfo::decode`) y cada usage activo se copia a la misma página/usage de Karabiner. Teclado 0xE0..E7 → bits de modificadores (mismo orden); resto del teclado → `keys`; Consumer ≤ 0x300 → `consumer_input`; system control (0x81..0xB7) → `generic_desktop_input`; botones 1..32, X/Y/rueda y AC Pan → `pointing_input`. Los desplazamientos de 12 bits se **parten** en varios informes de ±127 en lugar de recortarse. HID++ (página vendor) se ignora. Es equivalente a enchufar el receptor: macOS recibe los mismos usages de un teclado HID genuino y aplica su propio tratamiento de Cmd/Option/Fn.
3. Un teclado y un puntero de Karabiner por agente (no por interfaz). El estado se guarda por (dispositivo, report ID) y se une, así funciona también con varios teclados (modo B). El teclado virtual se inicializa con el VID/PID del interfaz de teclado del hub (`046D:C52B`) para que macOS identifique el tipo de teclado como con el receptor real.
4. **El agente HID corre como root** (LaunchDaemon `cc.olitec.rawkvm.agent`), porque el socket es solo de root. Nuestro instalador también registra el daemon de Karabiner (`org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon`, mismo plist que el ejemplo de pqrs) salvo si está Karabiner-Elements, que lanza el suyo. Config del daemon: `/Library/Application Support/rawkvm/agent.toml` (el instalador copia el emparejamiento del usuario).

## Consecuencias / Fase 2

- La detección de borde (CGEventTap, `CGWarpMouseCursorPosition`) necesita la sesión gráfica del usuario y los permisos TCC (Input Monitoring, Accessibility), que un LaunchDaemon no tiene. Plan: un **helper de usuario** (LaunchAgent, o el propio widget Tauri) que hable con el daemon por un socket Unix local (`/var/run/rawkvm/agent.sock`, grupo `staff`), con mensajes `Screens`, `SwitchTo` (helper → daemon) y `Activate { edge, pos }` (daemon → helper). El daemon sigue siendo el único nodo QUIC hacia el hub. No implementado todavía; `HidSink::activate` solo registra.
- Las teclas Fn/Globe y las de brillo de Apple solo existen en las páginas Apple vendor; el receptor Logitech no las emite, así que no se pierde nada respecto a enchufarlo.
- LED de Bloq Mayús: Karabiner expone el informe de LED (id 6), pero no se reenvía al hub (igual que en Linux).
- `--dry-run` conecta al hub y registra los informes de Karabiner sin root ni daemon.
