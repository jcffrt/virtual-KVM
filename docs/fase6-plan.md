# Fase 6: varios receptores, de cualquier fabricante, con exposición por receptor

Brief para la sesión de código (VS Code). Contexto y motivación: [puente-bluetooth-analisis.md](puente-bluetooth-analisis.md), capa **A**. Esta fase no toca Bluetooth: deja el hub preparado para que un receptor pueda entregarse en exclusiva a un agente puente (fase 7, si se decide) y, de paso, admite dongles de otros fabricantes.

Decidido el 2026-10-26 por el usuario. Lleva **un ADR** (0030) con las decisiones de abajo; si alguna se cambia al implementar, se documenta en el ADR, no se pregunta.

## Contexto operativo (2026-10-26)

- El hub ya corre en una **Raspberry Pi 3B+** por Ethernet (`rawkvm-hub.local`, 192.168.1.39, Raspberry Pi OS Trixie arm64); la Zero 2W solo lleva `rawkvm-agent-gadget`. Detalles y hallazgos en la última sección de `docs/STATUS.md`. Las pruebas de hardware del vigilante quedan en segundo plano: en las dos placas la ruta va por Ethernet y el vigilante está en reposo por diseño.
- Hallazgo de empaquetado, **sin asignar todavía**: el postinst arranca el hub al instalar y, sin estado previo, genera identidad y token nuevos y los anuncia por mDNS; al migrar hubo que pararlo y restaurar el estado encima. Candidatos: `rawkvm-hub --import-state <tar>` (para, vuelca, arranca) o que el postinst no arranque si existe una marca de migración. Se decide después de esta fase.
- Al terminar: commit en `main`, fmt/clippy/tests limpios, y el despliegue en la RPi 3B+ lo hace la sesión de Claude Desktop.

## Objetivo

1. `hub.toml` admite **varios bloques `[[receiver]]`**, cada uno con su fabricante; el bloque único `[receiver]` sigue funcionando.
2. Cada dispositivo HID que el hub anuncia dice **a qué receptor pertenece**.
3. El router mantiene por receptor una **exposición**: `network` (lo de hoy) o `exclusive` hacia un agente concreto. Los dispositivos de un receptor `exclusive` van siempre a ese agente, pasen los bordes, la hotkey o el widget por donde pasen.
4. El widget puede **consultar y cambiar** la exposición, y la ve en `Settings`. La interfaz del widget para elegirla es parte de esta fase solo si cabe en el tiempo; si no, queda para la fase 7 con el resto de la pantalla del puente.

## Cambios

### `crates/hub/src/config.rs`

- `ReceiverMatch` gana `name: String` (por defecto `"receiver-N"`) y pasa a vivir en `receivers: Vec<ReceiverMatch>`. Deserializar `[receiver]` (tabla única) o `[[receiver]]` (lista): un `#[serde(deserialize_with)]` que acepta ambos, o un campo `receiver` legado que se vuelca en la lista al cargar. El ejemplo `hub.toml.example` y la configuración generada en el primer arranque usan la lista con un solo bloque Logitech.
- `force_normal_mode` y todo lo HID++ (`hidpp.rs`, `receiver_ctl.rs`, `receiver.rs`) solo se aplican a receptores con `vid == 0x046d`. Para otro fabricante con `force_normal_mode = true` el hub avisa una vez y lo ignora. El widget solo recibe `ReceiverDevices`, emparejado, `Unpair`, velocidad de puntero y `SetFnSwap` del receptor Logitech; si hay varios Logitech, del primero (dejar constancia en el ADR; el soporte HID++ multirreceptor no es de esta fase).
- Validación al cargar: dos bloques no pueden solapar (`vid` igual y `pids` vacíos o con intersección); error claro al arrancar.

### `crates/hub/src/devices.rs`

- `scan_loop` recorre la lista de receptores; `is_forwarded` devuelve el índice del bloque que casa. `assign_id` sigue dando ids `u8` únicos globales (el id es el primer byte de cada datagrama y no cambia de formato).
- `to_hid_device` rellena el campo nuevo `receiver`.

### `crates/proto/src/lib.rs`

- `HidDevice.receiver: u8` (índice del bloque en la configuración del hub). **`PROTO_VERSION` 7 → 8.** Actualizar `docs/protocol.md`.
- `Control` nuevo:
  - `SetExposure { receiver: u8, mode: Exposure }` (widget → hub).
  - `HubSettings` gana `exposures: Vec<ReceiverExposure { receiver: u8, name: String, mode: Exposure }>`; el hub lo manda en `Settings` como hasta ahora (al conectar y al cambiar). Así no hace falta otro mensaje de vuelta.
  - `enum Exposure { Network, Exclusive { agent: Uuid } }`.
- `Control::name()` y los tests de ida y vuelta con postcard.

### `crates/hub/src/router.rs`

- Estado `exposures: HashMap<u8, Exposure>`, persistido en `settings.toml` junto a la hotkey (`with_settings_file`). Al cargar, un receptor sin entrada es `Network`.
- `on_report(dev_id, report)`: el destino es el agente exclusivo si el receptor del dispositivo está en `Exclusive` **y ese agente está conectado**; si no está conectado, los informes de ese receptor se **descartan** (no van al activo: la exclusividad es la razón de ser del modo) y el hub lo dice en `Status` para que el widget lo muestre. Los receptores `Network` siguen la lógica actual sin cambios.
- La hotkey de emergencia y la detección de bordes **no** actúan sobre informes de un receptor `Exclusive`. Vía de escape: la hotkey (modificador, modificador, `Esc`) en un teclado exclusivo devuelve **ese receptor** a `Network`, además de soltar las teclas. Es la única excepción y va al ADR.
- `DeviceList` sigue yendo a todos los nodos (no cambia qué dispositivos ve cada agente; cambia solo quién recibe los informes). `Activate`/`Deactivate` no cambian.
- Al desconectarse el agente exclusivo no se cambia el modo (volverá él).
- `SetExposure` con un `agent` que no es `NodeKind::Agent` o no se conoce se rechaza con `Status`/log, sin cambiar nada.

### Agentes

- Mac, Ubuntu, Windows, gadget, edge helpers: recompilar por el campo nuevo y la versión 8. Sin lógica nueva. El gadget ya tolera cambios de `DeviceList`.

### Widget

- `Settings` con `exposures` llega al frontend; si entra en la fase, un selector por receptor en la pestaña Hub: «Red local (todos los equipos)» / «Exclusivo → *agente*» (lista de agentes conectados). Si no entra, al menos que no rompa al recibir el campo.

### Docs y empaquetado

- `docs/hub-linux-instalacion.md`: sección «Varios receptores» con un ejemplo de dos bloques (Logitech + otro fabricante por `lsusb`), y la nota de que lo HID++ es solo Logitech.
- `docs/protocol.md`: versión 8, mensajes nuevos, semántica de `Exclusive`.
- ADR 0030.
- `.deb`: la actualización conserva `hub.toml`; comprobar que el bloque `[receiver]` viejo carga sin tocarlo.

## Pruebas

- Unitarias: deserializar `[receiver]` y `[[receiver]]`; solape detectado; `is_forwarded` con dos bloques; router: informes de un receptor exclusivo van solo al agente aunque otro esté activo; descarte sin agente; hotkey `Esc` devuelve a `Network`; persistencia en `settings.toml`.
- `.hidrec`: una grabación con dos receptores (`D` con `vid` distintos) reproducida en el hub simulado.
- `cargo fmt`, `clippy -D warnings`, tests, en todo el workspace, incluidos los targets de Windows y macOS que se compilen desde aquí.

## Fuera de esta fase

- El agente Bluetooth (fase 7), la fusión de descriptores y su UI.
- HID++ para más de un receptor Logitech.
- Cambiar el formato del datagrama.

## Defecto del gadget con más de cuatro interfaces (2026-10-26, corregido en 0.7.0)

Encontrado en hardware con dos receptores (Logitech C52B, 3 interfaces, y Areson/Compx `25a7:fa09`, 2 interfaces): el gadget de la Zero 2W falla al crear la quinta función, `Mkdir("/sys/kernel/config/usb_gadget/rawkvm/functions/hid.usb17"): No such device`, y el PC se queda sin ningún `/dev/hidg*`. Causa: `usb_f_hid` admite como máximo `HIDG_MINORS = 4` instancias. Con `--no-hidpp` (cuatro interfaces: ids 0, 1, 16, 17) enlaza bien.

Pedido a la sesión de código:

1. Cuando haya más de cuatro interfaces, el gadget elige cuáles entran por utilidad: teclado y ratón de cada receptor antes que interfaces de fabricante/HID++; el HID++ del Logitech solo si cabe. Avisa en el log y, si hay vía, en el widget (por ejemplo una marca «no expuesta en el gadget» por dispositivo en `Status` o `DeviceList`).
2. Un `mkdir` fallido de una función no tira todo el gadget: se construye con las que quepan.
3. Documentar el límite en `gadget-instalacion.md` y en las consecuencias del ADR 0030.
4. Test del sink con cinco dispositivos y un límite simulado.

**Corregido en 0.7.0** ([ADR 0031](adr/0031-gadget-con-mas-de-cuatro-interfaces.md)). El gadget elige por prioridad: teclado y ratón, después otras colecciones estándar y al final las de fabricante. Si un `mkdir` falla, aprende cuántas funciones caben y reconstruye con esas. Avisa en el log y en el widget, con el protocolo 9 (`Unexposed`, `NodeInfo.unexposed`).
