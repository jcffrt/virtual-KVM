# HANDOFF — "RawKVM": switch KVM por software con reenvío HID crudo

Documento de traspaso para una sesión de Claude Code. Contiene decisiones ya tomadas, arquitectura, protocolo, componentes, fases y criterios de aceptación. **No reabrir las decisiones marcadas como cerradas salvo bloqueo técnico demostrado.**

---

## 1. Objetivo

Compartir **un teclado y un ratón Logitech (receptor Unifying)** entre tres ordenadores por Wi‑Fi, con **conmutación automática al llevar el puntero al borde de pantalla**, de modo que **cada SO reciba los informes HID crudos del dispositivo** y aplique él mismo su propio mapeo de modificadores (Cmd/Option/AltGr/Fn, etc.).

### Por qué no sirven las soluciones existentes
- **Logitech Easy‑Switch/Flow**: conmutación lenta y a veces el teclado no cambia.
- **Input Leap / Barrier / Synergy**: el host interpreta las teclas con su propio modelo (Windows o macOS) y las reinyecta como eventos del SO; se pierden/alteran modificadores entre Windows↔macOS.
- **USB/IP**: solo un host posee el bus; cada cambio implica reenumeración (1–2 s).

### Principio de diseño (cerrado)
Los informes HID viajan **sin interpretar** desde el receptor Unifying hasta un **dispositivo HID virtual en cada PC** que expone el **mismo report descriptor** que el dispositivo físico. El hub solo decide **a quién** enviar.

---

## 2. Hardware y entorno (cerrado)

| Rol | Equipo | SO | Notas |
|---|---|---|---|
| Hub | Orange Pi Zero 2W, 1 GB, Allwinner H618 (aarch64), Wi‑Fi/BT | Armbian (Debian/Ubuntu base, kernel ≥ 6.1) | Receptor Unifying en el USB‑C **OTG en modo host** (adaptador USB‑C→A). Alimentación por el otro USB‑C. |
| PC1 | Windows 10/11 x64 | — | Driver de kernel con **test‑signing** (uso personal). |
| PC2 | Windows 10/11 x64 | — | Ídem. |
| PC3 | Mac (Apple Silicon o Intel), macOS ≥ 13 | — | Dispositivo virtual vía **Karabiner‑DriverKit‑VirtualHIDDevice**. |
| PC4 (extra) | Linux, **Ubuntu 24.04 LTS, GNOME/Wayland** | — | Dispositivo virtual vía **`/dev/uhid`**. |

Red: todos por Wi‑Fi en la misma LAN. Objetivo de latencia extremo a extremo ≤ 15 ms p95.

---

## 3. Arquitectura

```
                 ┌──────────────── Orange Pi Zero 2W ────────────────┐
 Unifying ──USB──│ hidraw ─▶ kvm-hub (Rust)                           │
 (kbd+mouse)     │   • enumera receptor, descriptores, dispositivos   │
                 │   • grab exclusivo, lectura de informes            │
                 │   • HID++ (emparejar/listar dispositivos)          │
                 │   • router: destino activo                         │
                 │   • servidor QUIC (quinn) + mDNS                   │
                 └───────────────┬────────────────────────────────────┘
                                 │ Wi‑Fi (QUIC: datagrams = informes, streams = control)
        ┌────────────────────────┼─────────────────────────┬──────────────────────┐
        ▼                        ▼                         ▼                      ▼
  kvm-agent (Win)          kvm-agent (Win)           kvm-agent (macOS)      kvm-agent (Linux)
  user‑mode Rust            user‑mode Rust            Rust                    Rust
  ─▶ driver VHF (KMDF, C)   ─▶ driver VHF             ─▶ Karabiner VirtualHID ─▶ /dev/uhid
  edge: WH_MOUSE_LL         edge: WH_MOUSE_LL         edge: CGEventTap        edge: ext. GNOME / XFixes

  kvm-widget (Tauri 2, Rust + TS): bandeja/menubar, descubrimiento, layout de monitores, emparejamiento, estado.
```

Componentes en un único **workspace Cargo** (`rawkvm/`):

```
rawkvm/
├── Cargo.toml                # workspace
├── crates/
│   ├── proto/                # tipos de mensajes, serialización, versión de protocolo
│   ├── hub/                  # binario para Orange Pi (aarch64)
│   ├── agent-core/           # lógica común de agente: conexión, layout, edge logic, reconexión
│   ├── agent-windows/        # binario + servicio Windows; IOCTL hacia driver VHF
│   ├── agent-macos/          # binario + LaunchAgent; cliente Karabiner VirtualHID
│   ├── agent-linux/          # binario + systemd; uhid
│   └── hidpp/                # HID++ 1.0/2.0 para receptor Unifying (listar/emparejar)
├── drivers/
│   └── windows-vhf/          # proyecto KMDF (C), INF, script de firma test‑signing
├── widget/                   # app Tauri 2 (src-tauri en Rust, UI en TS/Svelte)
├── gnome-extension/          # extensión GNOME Shell para borde de pantalla (Wayland)
├── deploy/                   # systemd units, LaunchAgent plist, scripts Armbian, instaladores
└── docs/                     # este documento, protocolo, decisiones (ADR)
```

Lenguaje: **Rust** (edición 2021, toolchain stable) salvo el driver Windows (C/KMDF), la extensión GNOME (JS) y la UI del widget (TS).

---

## 4. Protocolo (crate `proto`)

### Transporte (cerrado)
- **QUIC** con `quinn`. Una conexión por agente.
  - **Datagrams QUIC** (no fiables, sin cabeza de línea) para `Report`.
  - **Streams bidireccionales** para control (`Hello`, `DeviceList`, `SwitchTo`, `Layout`, `Pairing*`).
- **Seguridad**: TLS 1.3 con certificado autofirmado del hub + **PSK/token de emparejamiento** mostrado en el widget la primera vez (TOFU: el agente guarda la huella del cert del hub). Los informes de teclado viajan cifrados siempre.
- **Descubrimiento**: mDNS/DNS‑SD, servicio `_rawkvm._udp.local.`, TXT con `ver`, `name`, `fp` (huella cert). Crate `mdns-sd`.

### Mensajes (serialización `postcard` o `bincode`, versión en `Hello`)

```rust
enum Control {
    Hello { proto_ver: u16, node_id: Uuid, node_name: String, os: Os, token: [u8; 32] },
    HelloAck { hub_id: Uuid, hub_name: String, assigned_index: u8 },
    DeviceList { devices: Vec<HidDevice> },        // hub -> agentes, al conectar y al cambiar
    Layout { screens: Vec<Screen>, edges: Vec<EdgeLink> }, // widget -> hub -> agentes
    SwitchTo { target: Uuid, enter_edge: Edge, pos_ratio: f32 }, // agente -> hub
    Activate { enter_edge: Option<Edge>, pos_ratio: Option<f32> }, // hub -> agente destino
    Deactivate,                                     // hub -> agente origen
    PairingStart { receiver: String, timeout_s: u8 }, PairingEvent(PairingEvent), PairingStop,
    Status(HubStatus), Ping(u64), Pong(u64),
}

struct HidDevice {
    id: u8,                       // índice estable mientras el hub viva
    name: String, vid: u16, pid: u16, serial: Option<String>,
    interface: u8,                // interfaz USB del receptor (kbd / mouse / vendor)
    report_descriptor: Vec<u8>,   // crudo, tal cual lo devuelve HIDIOCGRDESC
    uses_report_ids: bool,
}

// Datagram: [u8 dev_id][u8 flags][u16 seq][...report bytes incl. report id si procede]
```

### Semántica de conmutación
1. El agente activo detecta el puntero en un borde con enlace configurado → envía `SwitchTo{target, edge, pos_ratio}`.
2. El hub cambia `active = target`, envía `Deactivate` al origen y `Activate{enter_edge, pos_ratio}` al destino, y **antes** envía al origen un informe de "todas las teclas/botones liberados" para evitar teclas pegadas.
3. El destino coloca el puntero en el borde opuesto a la altura `pos_ratio` (0.0–1.0 del borde) y el origen aparca el puntero fuera del borde (o lo oculta) para evitar rebotes.
4. Histéresis: 150 ms mínimo entre conmutaciones; zona muerta de 2 px tras entrar.
5. **Atajo de emergencia** interpretado por el hub (no por los agentes): `Ctrl+Ctrl` (doble pulsación) + `1..4` cambia de destino aunque el agente activo esté caído. El hub solo parsea el informe de teclado para esto; no lo altera.
6. Si el agente activo se desconecta, el hub cambia al último destino vivo y lo notifica al widget.

---

## 5. Hub (`crates/hub`, Orange Pi)

- **Enumeración**: `udev` (crate `udev`) para localizar hidraw del receptor Logitech (VID `0x046D`, PID `0xC52B`/`0xC534`/`0xC539`…). El receptor expone **varias interfaces**: teclado (boot), ratón (boot), vendor HID++ (report IDs 0x10/0x11/0x20/0x21). Se reenvían **todas** las interfaces de entrada con su descriptor; la interfaz HID++ se usa internamente y también se reenvía (necesaria para multimedia/consumer y para que el SO vea exactamente el mismo dispositivo).
- **Lectura**: `read()` bloqueante por interfaz en hilos dedicados; `ioctl HIDIOCGRDESC`/`HIDIOCGRDESCSIZE` para descriptores; `HIDIOCGRAWINFO` para VID/PID. Grab exclusivo: el kernel ya no debe generar evdev para estos → **desvincular el driver `hid-logitech-dj`/`hid-generic`** del receptor (`unbind` por sysfs) o usar `udev` rule con `ENV{HID_DJ}` para que solo exista hidraw. Documentar en `deploy/armbian/`.
- **Router**: estado `active: Option<Uuid>`; envío de datagram al activo; cola limitada (descartar informes de ratón antiguos si hay congestión, nunca descartar teclado).
- **HID++ (`crates/hidpp`)**: listar dispositivos emparejados (registro 0xB5 "pairing info"), abrir emparejamiento (registro 0xB2), escuchar notificaciones de conexión/desconexión (0x41). Referencia de implementación: Solaar (`logitech_receiver/`). Fase 2.
- **Fallback físico**: el hub también puede exponerse como **USB gadget HID** por el otro USB‑C hacia un único PC (ConfigFS). Opcional, fase 3; útil si el Wi‑Fi cae.
- Despliegue: binario estático aarch64 (`cross build --target aarch64-unknown-linux-musl`), unit systemd `rawkvm-hub.service`, logs a journald, config en `/etc/rawkvm/hub.toml`.

---

## 6. Agente Windows (`crates/agent-windows` + `drivers/windows-vhf`)

### Driver (C, KMDF, **Virtual HID Framework**)
- Basado en el sample oficial `vhidmini2`/VHF de Microsoft (WDK). Crea un dispositivo HID virtual **por cada `HidDevice`** recibido, con el descriptor crudo tal cual.
- Interfaz de control (IOCTL sobre un control device `\\.\RawKvmVhf`):
  - `IOCTL_RAWKVM_CREATE { descriptor, vid, pid, name } -> handle`
  - `IOCTL_RAWKVM_REPORT { handle, bytes }` (buffered, ~1000/s)
  - `IOCTL_RAWKVM_DESTROY { handle }`
- `.inf` + `inf2cat` + `signtool` con certificado autofirmado. Script `drivers/windows-vhf/sign-test.ps1` (crea cert, instala en *Trusted Root* y *Trusted Publishers*, firma `.cat` y `.sys`). Requisitos documentados: `bcdedit /set testsigning on` + reinicio; comportamiento con Secure Boot.
- El driver no interpreta nada: los informes llegan a `hidclass` → `kbdhid`/`mouhid` como si fuera un Logitech real.

### Agente user‑mode (Rust)
- Servicio Windows (`windows-service` crate) + icono de bandeja opcional (lo da el widget).
- Conexión QUIC, aplica `DeviceList` → IOCTL CREATE; cada datagram → IOCTL REPORT.
- Edge: hook `WH_MOUSE_LL` (crate `windows`), coordenadas virtuales multi‑monitor (`GetSystemMetrics(SM_XVIRTUALSCREEN…)`), `SetCursorPos` al activar. Tener en cuenta DPI (`SetProcessDpiAwarenessContext`).

---

## 7. Agente macOS (`crates/agent-macos`)

- Dependencia externa: **Karabiner‑DriverKit‑VirtualHIDDevice** (pqrs‑org, MIT; ya firmado/notarizado con entitlements DriverKit). Instalar su `.pkg` y activar la extensión del sistema. Nuestro agente es un cliente de su socket Unix (`virtual_hid_device_service`).
- Mapeo raw → Karabiner: Karabiner define sus propios structs (`keyboard_input` = modificadores + array de usages HID; `pointing_input` = botones, dx, dy, rueda; `consumer_input`, `apple_vendor_keyboard_input`, `apple_vendor_top_case_input`). **Es una conversión 1:1 de usages HID (misma página/mismo usage), no una reinterpretación**: se parsea el informe con el descriptor (crate `hidparser`/propio) y se vuelcan los usages activos. macOS aplica entonces su propio tratamiento de modificadores sobre un dispositivo HID genuino. Documentar en un ADR por qué aquí no es byte‑a‑byte y por qué es equivalente.
- Edge: `CGEventTap` pasivo (`kCGEventMouseMoved`) o polling `CGEvent::location` a 120 Hz; `CGWarpMouseCursorPosition` + `CGAssociateMouseAndMouseCursorPosition` al activar; geometría de pantallas con `NSScreen`/`CGGetActiveDisplayList`.
- Permisos a solicitar: **Input Monitoring** y **Accessibility** (documentar en onboarding del widget).
- Empaquetado: LaunchAgent `cc.olitec.rawkvm.agent.plist`, binario firmado ad‑hoc (o con Developer ID si se dispone; no necesario para funcionar).

---

## 8. Agente Linux (`crates/agent-linux`, Ubuntu 24.04 GNOME/Wayland)

- Dispositivo virtual: `/dev/uhid` → `UHID_CREATE2` con el descriptor crudo (nombre, VID/PID, bus `BUS_USB`), `UHID_INPUT2` por informe. Crate `uhid-virt`. Regla udev `deploy/linux/99-rawkvm-uhid.rules` (`KERNEL=="uhid", GROUP="input", MODE="0660"`). El kernel crea hidraw/evdev; libinput/GNOME lo ven como Logitech real.
- Edge en **Wayland (GNOME)**: extensión GNOME Shell (`gnome-extension/`) que lee la posición del puntero (`global.get_pointer()`) a cada frame o usa barreras (`Meta.Barrier`) y notifica al agente por D‑Bus (`cc.olitec.RawKvm.Edge`); el agente expone un método D‑Bus `Warp(x,y)` que la extensión ejecuta con `Clutter`/`Meta` para posicionar el puntero al activar.
- Fallback **X11**: XFixes pointer barriers + `XWarpPointer` (crate `x11rb`).
- systemd user unit `rawkvm-agent.service`.

---

## 9. Widget (`widget/`, Tauri 2)

- **UI**: Svelte + TypeScript. Icono de bandeja/menubar en los tres SO; ventana "Settings" con pestañas:
  1. **Hub**: lista de hubs descubiertos por mDNS, emparejar (introducir token mostrado en el log/LED del hub la primera vez), estado de conexión y latencia (ping/pong), versión.
  2. **Dispositivos**: lo que reporta el hub: receptor Unifying detectado, dispositivos emparejados (nombre, tipo, batería si HID++ lo da), botón **"Emparejar nuevo dispositivo"** (fase 2) con cuenta atrás y resultado.
  3. **Monitores**: lienzo (Canvas/SVG) con un rectángulo por **pantalla de cada ordenador** (cada agente reporta sus monitores con resolución y posición relativa dentro de su propio escritorio). El usuario arrastra los bloques de cada ordenador para colocarlos relativamente; el widget calcula los `EdgeLink` (qué borde de qué pantalla conecta con qué borde de qué otra pantalla y con qué rango de `pos_ratio`) y los envía al hub con `Layout`. Snapping a bordes, alineación proporcional, vista previa del camino del puntero.
  4. **Atajos/Opciones**: histéresis, zona muerta, atajo de emergencia, arranque automático.
- El widget habla con el hub por el mismo protocolo (`proto`), como un nodo de tipo `Widget` (sin dispositivo virtual). Se instala preferentemente en el Mac; funciona igual en los otros.
- Comandos Tauri en Rust (`src-tauri`) reutilizan `agent-core` para descubrimiento/conexión; nada de lógica de protocolo en TS.

---

## 10. Fases y entregables

| Fase | Entregable | Criterio de aceptación |
|---|---|---|
| 0 | Workspace, `proto`, hub que enumera hidraw y vuelca informes por consola; agente Linux con uhid | Teclear en el Logitech conectado al Pi escribe en el Ubuntu por Wi‑Fi; `evtest` muestra el dispositivo con nombre Logitech. |
| 1 | Agente macOS (Karabiner) + agente Windows (driver VHF test‑signed) + `SwitchTo` por atajo de emergencia | Los tres PCs reciben; Cmd/Option en Mac y AltGr/Win en Windows funcionan nativamente; conmutación < 50 ms. |
| 2 | Widget Tauri: descubrimiento, layout de monitores, edge switching en los tres SO, extensión GNOME | Llevar el puntero al borde cambia de equipo y el puntero aparece a la altura correcta; sin teclas pegadas; sin rebotes. |
| 3 | HID++: listado y emparejamiento desde el widget; reconexión robusta; instaladores (`.pkg`, MSI/`winget`‑like script, `.deb`, imagen/script Armbian) | Emparejar un ratón nuevo desde el widget sin tocar el Pi. |
| 4 (opcional) | Gadget USB de respaldo desde el hub; cifrado con rotación de token; métricas | — |

Tests: unitarios en `proto` (round‑trip) y en edge‑logic (`agent-core`, tabla de casos con layouts multi‑monitor); integración con un hub simulado que reproduce informes grabados (`fixtures/*.hidrec`); medición de latencia con `Ping/Pong` y marcas de tiempo en datagrams.

---

## 11. Riesgos y decisiones abiertas

- **Wi‑Fi**: jitter puede superar 15 ms en redes cargadas. Mitigación: datagrams, sin retransmisión para ratón, cola con descarte, `WMM`/5 GHz recomendado. Medir en fase 0 antes de seguir.
- **Receptor Unifying multi‑interfaz**: confirmar en fase 0 qué interfaces produce el receptor concreto (`lsusb -v`, `/sys/class/hidraw/*/device/report_descriptor`) y que `hid-logitech-dj` queda desvinculado para que los informes lleguen crudos (sin la "virtualización" por dispositivo que hace ese driver). Alternativa: **mantener** `hid-logitech-dj` y reenviar los hidraw **por dispositivo** (ya separados teclado/ratón, descriptores sintetizados por el driver). Decidir con datos; preferencia inicial: **sin** `hid-logitech-dj`, descriptor del receptor tal cual.
- **macOS**: el descriptor lo fija Karabiner (no el nuestro). Verificar cobertura de teclas multimedia/Fn.
- **Windows**: test‑signing muestra marca de agua; con Secure Boot algunos equipos bloquean `testsigning`. Documentar desactivación o alternativa EV/attestation si se quisiera producción.
- **Wayland**: la extensión GNOME es específica de GNOME; KDE/wlroots fuera de alcance en esta versión.
- **Seguridad**: el hub acepta solo nodos con token; rechazar conexiones sin TLS; no loguear informes de teclado.

---

## 12. Convenciones para la sesión de Claude Code

- Idioma del código y comentarios: inglés; documentación de usuario: castellano.
- `cargo fmt`, `cargo clippy -D warnings`, tests en CI (GitHub Actions: build Linux x64, cross aarch64, macOS, Windows; driver KMDF se compila en runner Windows con WDK/EWDK).
- Cada decisión de arquitectura nueva → `docs/adr/NNNN-titulo.md`.
- Empezar por **Fase 0** y detenerse para validar latencia y descriptores reales antes de la Fase 1.
- Preguntar al usuario solo cuando haya un bloqueo real o una decisión que no esté en este documento.
