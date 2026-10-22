# Estado del proyecto RawKVM

Última actualización: 2026-10-22. Spec completa en [HANDOFF.md](HANDOFF.md).

## Decisiones tomadas en la sesión inicial (no están en el handoff)

- **El workspace Cargo es la raíz del repositorio** (`/Users/javiercancelaco/Projects/virtual-KVM`), no un subdirectorio `rawkvm/`. Los crates se llaman `rawkvm-proto`, `rawkvm-hub`, `rawkvm-agent-core`, `rawkvm-agent-linux` (alias `proto`, `agent-core` en el workspace).
- **Cross-compilación sin Docker**: en el Mac no hay `cross`; se instalaron `zig` (brew), `cargo-zigbuild` y los targets `aarch64-unknown-linux-musl` y `x86_64-unknown-linux-gnu`. Para la Orange Pi: `cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub`.
- **`proto` añade a los mensajes del handoff**: `NodeKind { Agent, Widget }` en `Hello`, `Reject { reason }` (hub → nodo), `Screens` (agente → hub con sus monitores), y `Screen.scale`. El `Control` enum completo está en `crates/proto/src/lib.rs`.
- **`proto::descriptor`** es un parser mínimo de report descriptors (short items, push/pop, varios report IDs). Da: `uses_report_ids`, tamaño de cada input report (para sintetizar "release all"), y la posición del byte de modificadores y del array de teclas (para el atajo de emergencia del hub). No interpreta nada más.
- **Sin repo git todavía.** Crear con `git init` cuando se decida (pendiente de confirmar con el usuario).

## Riesgos detectados que afectan a la Fase 0 (pendientes de decidir con datos)

1. **Agente Linux y `hid-logitech-dj`**: si el dispositivo uhid se crea con `BUS_USB` y VID/PID 046D:C52B, el kernel de Ubuntu le asociará `hid-logitech-dj`, que pondrá el "receptor" en modo DJ, consultará los dispositivos emparejados por HID++ (SET_REPORT/GET_REPORT hacia uhid) y **descartará los informes de teclado/ratón crudos** si no hay dispositivos DJ enumerados. Opciones: (a) crear el uhid con `BUS_VIRTUAL` para que solo se asocie `hid-generic` (preferida, documentar en ADR), (b) emular el protocolo DJ en el agente. Verificar en el Ubuntu real.
2. **Hub: hidraw desaparece si se desvincula el driver**. El nodo `/dev/hidrawN` lo crea el driver HID al arrancar el dispositivo; "unbind" de `hid-logitech-dj` sin "bind" a `hid-generic` deja el receptor sin hidraw. Plan: blacklist `hid_logitech_dj` y `hid_logitech_hidpp` en el Pi (`/etc/modprobe.d/`) para que `hid-generic` tome las tres interfaces, y que el hub haga `EVIOCGRAB` sobre los `/dev/input/event*` del receptor para que el Pi no consuma las teclas localmente.
3. **Receptor conectado y leído (2026-10-22)**. Receptor `046D:C52B` en el bus USB 4 (`/sys/bus/usb/devices/4-1`), tres dispositivos emparejados: **MX Ergo** (406F), **K230** (400D), **MX Keys** (408A). Con `hid_logitech_dj` cargado el receptor está en **modo DJ**: la interfaz 2 la tiene `logitech-djreceiver` (hidraw0) y cada dispositivo emparejado aparece como HID hijo con descriptor sintetizado por el kernel y driver `logitech-hidpp-device` (hidraw1..3, evdev event3..5). Las interfaces 0 y 1 del receptor quedan **sin driver y sin hidraw**. Descriptores guardados en `fixtures/unifying-c52b/` (ver su README) y parseados por `crates/proto/tests/fixtures.rs`: el parser los entiende (teclado sintetizado: ID 1, modificadores byte 0, teclas bytes 1..6, sin byte reservado).
   - **Decisión pendiente, a tomar con datos**: (A) sin DJ (blacklist): reenviar las 3 interfaces del receptor tal cual (preferencia del handoff), o (B) con DJ: reenviar los hidraw sintetizados por dispositivo (identidad por dispositivo en cada PC, pero descriptores del kernel y dependencia de `hid-logitech-dj`). Para capturar los descriptores de las interfaces 0/1 en modo normal hace falta ejecutar en el Pi (requiere sudo con contraseña, no automatizable desde aquí hasta tener NOPASSWD):
     ```
     printf 'blacklist hid_logitech_dj\nblacklist hid_logitech_hidpp\n' | sudo tee /etc/modprobe.d/rawkvm.conf
     sudo rmmod hid_logitech_hidpp hid_logitech_dj
     echo 0 | sudo tee /sys/bus/usb/devices/4-1/authorized; sleep 1; echo 1 | sudo tee /sys/bus/usb/devices/4-1/authorized
     ```
     y después volver a volcar `/sys/bus/hid/devices/0003:046D:C52B.000{1,2,3}/report_descriptor`.
4. ~~`sudo` en el Pi pide contraseña~~ Resuelto 2026-10-22: `orangepi` tiene NOPASSWD; las sesiones de Claude pueden desplegar por SSH.
5. **Modo normal capturado (2026-10-22)**: con `/etc/modprobe.d/rawkvm.conf` (blacklist `hid_logitech_dj` y `hid_logitech_hidpp`, **ya aplicado en el Pi**) y reset USB, `hid-generic` toma las tres interfaces con hidraw0/1/2 y evdev. Descriptores en `fixtures/unifying-c52b/*.nodj.bin`: interfaz 0 teclado boot de 8 B sin report ID; interfaz 1 ratón ID 2 / consumer ID 3 / system ID 4 / vendor 0xFFBC ID 8; interfaz 2 HID++. El parser los procesa. **Modo A (sin DJ) queda confirmado como viable y es el que está activo en el Pi.** El hub debe hacer `EVIOCGRAB` sobre event3..6 para que el Pi no consuma las teclas.
6. **El modo DJ persiste en el receptor** aunque se descargue el driver y se resetee el USB por `authorized`: las interfaces 0/1 siguen mudas y todo llega por la interfaz 2 como informes DJ (`20 <idx> <tipo> …`). Solo lo quita un corte de alimentación o el comando de cambio de modo. **Verificado 2026-10-22**: escribir en hidraw de la interfaz 2 el informe DJ corto de 15 bytes `20 FF 80 00 00 00 00 00 00 00 00 00 00 00 00` (report 0x20, índice receptor 0xFF, comando SWITCH 0x80, bitfield de dispositivos en modo DJ = 0x00, timeout 0) devuelve el receptor a modo normal al instante y el teclado vuelve a emitir por la interfaz 0 (8 B boot). El kernel entra en DJ con el mismo comando y bitfield 0x3F (`logi_dj_recv_switch_to_dj_mode`, drivers/hid/hid-logitech-dj.c). **El hub debe enviar este comando al arrancar y tras cada reconexión del receptor**, antes de leer; así no depende de que el receptor venga "limpio".
7. **Lectura cruda validada (2026-10-22)**: en modo normal, interfaz 0 emite informes boot de 8 B al teclear en el MX Keys; interfaz 1 emite ~70 informes/s con ID 2 (7 B) al mover el MX Ergo. Los dispositivos tienen además canales Bluetooth hacia otros dos PCs (Easy‑Switch): para RawKVM deben estar en el canal del receptor del Pi.

## Orange Pi (hub)

- IP `192.168.1.28`, usuario `orangepi`, **acceso por clave SSH ya instalada** (`ssh orangepi@192.168.1.28`, sin contraseña).
- Armbian, kernel `6.1.31-sun50iw9`, aarch64. Módulos `hid_logitech_dj` y `hid_logitech_hidpp` cargados por defecto.

## Hecho

- Workspace, `rustfmt.toml`, `.gitignore`. CI en `.github/workflows/ci.yml` (fmt, clippy `-D warnings`, test en Linux x64 y macOS, build aarch64‑musl con cargo‑zigbuild; sin ejecutar todavía porque no hay repo).
- `crates/proto`: protocolo completo. Cambios en esta sesión: `SeqTracker::observe` devuelve `None` para duplicados/tardíos (hay que descartarlos, ADR 0004), `cert_fingerprint`, `token_hex`, `CERT_SERVER_NAME`.
- `crates/hub` (lib `rawkvm_hub` + bin):
  - `config`: `/etc/rawkvm/hub.toml`, genera `hub_id` y token la primera vez (0600). `[receiver] match = "receiver"` (modo A, por defecto) o `"dj-children"` (modo B), `grab_evdev`, `force_normal_mode`. `[switching] hysteresis_ms = 150`, `resend_ms = 15`, `hotkey`.
  - `hidraw` + `devices` (solo Linux): ioctls `HIDIOCGRDESC/RAWINFO/RAWNAME/RAWPHYS` con `nix`; sysfs para driver, `bInterfaceNumber`, HID padre y evdev. Escaneo cada 2 s (enchufar en caliente), hilo lector por interfaz, `EVIOCGRAB` de todos sus evdev, `id` = número de interfaz en modo A. Al abrir la interfaz HID++ envía `20 FF 80 00…` (riesgo 6).
  - `router`: nodos, índices 1..4 persistentes por `node_id`, activo, histéresis, `RELEASE_ALL` + `Deactivate`, vuelta al último activo si cae, reenvío redundante de informes no‑ratón, relé de `Screens`/`Layout`.
  - `hotkey`: Ctrl, Ctrl, 1..4 (fila o teclado numérico), con el layout del descriptor; el dígito no se reenvía.
  - `server`: QUIC (quinn 0.11, rustls ring, solo TLS 1.3), cert rcgen persistido en `state_dir`, mDNS `_rawkvm._udp` con TXT `ver/name/fp/id`.
  - `hidrec`: formato de grabación; `--dump --record f.hidrec` y `--replay f.hidrec` (hub simulado en cualquier SO). `fixtures/unifying-c52b-devices.hidrec` = los tres descriptores reales, sin informes.
  - CLI: `--list [--save DIR]`, `--dump [--record F]`, `--replay F`, `--show-token`.
- `crates/agent-core`: config con TOFU (`hub_fingerprint` se fija solo tras un `HelloAck`, nunca con token malo), descubrimiento mDNS (elige el hub con la huella fijada o el único), cliente QUIC con `ServerCertVerifier` por huella (verifica la firma TLS 1.3), `Ping` 1 s + RTT EWMA, log de estadísticas cada 10 s, `HidSink`, reconexión con backoff 0,5–10 s; token malo, versión distinta y certificado cambiado son fatales. Tests extremo a extremo contra el servidor real del hub en localhost.
- `crates/agent-linux`: módulo `uhid` propio sin `unsafe` (bytes en los offsets del ABI, 4376 B), responde GET/SET_REPORT, un dispositivo por `HidDevice`, `--bus virtual|usb` (virtual por defecto), `--hub`, `--token`, `--unpin`.
- `deploy/armbian`: `rawkvm-hub.service` (root), `modprobe-rawkvm.conf`, `install.sh`. `deploy/linux`: `99-rawkvm-uhid.rules`, `rawkvm-uhid.conf` (modules-load), `rawkvm-agent.service` (user), `install.sh`.
- `docs/`: `protocol.md`, ADR 0001–0004, `fase0-validacion.md`.
- Verificado en el Mac: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` (macOS y `--target x86_64-unknown-linux-gnu` con el `zigcc` de cargo-zigbuild como `CC`), `cargo test --workspace` (54 tests), build release aarch64‑musl del hub, prueba de humo del hub con `--replay`.

## Pendiente de la Fase 0

1. **Validar en hardware** siguiendo `docs/fase0-validacion.md`: desplegar el hub en el Pi, el agente en el Ubuntu, `evtest`, RTT y pérdidas. Parar antes de la Fase 1.
2. No verificado aún: el código exclusivo de Linux solo se ha compilado y pasado clippy, no ejecutado (`--list`, `--dump`, grab, uhid, comando DJ desde el hub). Los tests de `hidraw` solo corren en Linux (CI); los de `uhid` son de bytes y corren también en el Mac.
3. Conocido y aplazado: informes de salida del host (LED Bloq Mayús, HID++ desde el PC) no se reenvían al hub; emparejamiento (`PairingStart`) responde "no implementado".
4. `git init` y primer commit cuando el usuario lo confirme.

## Convenciones

Ver sección 12 del handoff: código y comentarios en inglés, docs de usuario en castellano, `cargo fmt` y `cargo clippy -D warnings` limpios, un ADR por decisión nueva.
