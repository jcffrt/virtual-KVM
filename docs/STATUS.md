# Estado del proyecto RawKVM

Última actualización: 2026-10-22. Spec completa en [HANDOFF.md](HANDOFF.md).

## Decisiones tomadas en la sesión inicial (no están en el handoff)

- **El workspace Cargo es la raíz del repositorio** (`/Users/javiercancelaco/Projects/virtual-KVM`), no un subdirectorio `rawkvm/`. Los crates se llaman `rawkvm-proto`, `rawkvm-hub`, `rawkvm-agent-core`, `rawkvm-agent-linux` (alias `proto`, `agent-core` en el workspace).
- **Cross-compilación sin Docker**: en el Mac no hay `cross`; se instalaron `zig` (brew), `cargo-zigbuild` y los targets `aarch64-unknown-linux-musl` y `x86_64-unknown-linux-gnu`. Para la Orange Pi: `cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub`.
- **`proto` añade a los mensajes del handoff**: `NodeKind { Agent, Widget }` en `Hello`, `Reject { reason }` (hub → nodo), `Screens` (agente → hub con sus monitores), y `Screen.scale`. El `Control` enum completo está en `crates/proto/src/lib.rs`.
- **`proto::descriptor`** es un parser mínimo de report descriptors (short items, push/pop, varios report IDs). Da: `uses_report_ids`, tamaño de cada input report (para sintetizar "release all"), y la posición del byte de modificadores y del array de teclas (para el atajo de emergencia del hub). No interpreta nada más.
- **Repo git** creado el 2026-10-22 (rama `main`, commit inicial de la Fase 0). Sin remoto todavía.

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
- `deploy/hub-linux`: `rawkvm-hub.service` (root), `modprobe-rawkvm.conf`, `install.sh`. `deploy/linux`: `99-rawkvm-uhid.rules`, `rawkvm-uhid.conf` (modules-load), `rawkvm-agent.service` (user), `install.sh`.
- `docs/`: `protocol.md`, ADR 0001–0004, `fase0-validacion.md`.
- Verificado en el Mac: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` (macOS y `--target x86_64-unknown-linux-gnu` con el `zigcc` de cargo-zigbuild como `CC`), `cargo test --workspace` (54 tests), build release aarch64‑musl del hub, prueba de humo del hub con `--replay`.

## Pendiente de la Fase 0

1. **Validar en hardware** siguiendo `docs/fase0-validacion.md`: desplegar el hub en el Pi, el agente en el Ubuntu, `evtest`, RTT y pérdidas. Parar antes de la Fase 1.
2. No verificado aún: el código exclusivo de Linux solo se ha compilado y pasado clippy, no ejecutado (`--list`, `--dump`, grab, uhid, comando DJ desde el hub). Los tests de `hidraw` solo corren en Linux (CI); los de `uhid` son de bytes y corren también en el Mac.
3. Conocido y aplazado: informes de salida del host (LED Bloq Mayús, HID++ desde el PC) no se reenvían al hub; emparejamiento (`PairingStart`) responde "no implementado".
4. Añadir un remoto (GitHub) para que corra el CI.

## Despliegue actual (2026-10-22)

- **Hub instalado y en marcha en la Orange Pi** como servicio `rawkvm-hub.service` (binario `/usr/local/bin/rawkvm-hub`, build `cargo zigbuild --release --target aarch64-unknown-linux-musl`). Verificado en el Pi: `--list` muestra las 3 interfaces `[forward]` con `hid-generic`; al arrancar el servicio abre hidraw0/1/2, manda el comando de salida de DJ ("DJ mode switched off"), hace `EVIOCGRAB` de event3..6, anuncia mDNS y escucha en UDP 4433.
- Config en `/etc/rawkvm/hub.toml` (token de emparejamiento generado ahí, `--show-token` lo imprime); certificado en `/var/lib/rawkvm`, huella `71a6f657…ecac` (también en el TXT mDNS, campo `fp`).
- Desde el Mac: `dns-sd -L orangepizero2w _rawkvm._udp local.` resuelve `rawkvm-orangepizero2w.local.:4433`. RTT ICMP Wi‑Fi Mac→Pi: media 4,5 ms, máx 8,4 ms (30 pings).
- **Cambio de orden (decisión del usuario, 2026-10-22)**: no hay Ubuntu disponible, así que la validación del agente Linux queda aplazada. Se sigue con el **agente macOS** (este Mac, macOS 27.0, es el PC3) y después el **agente Windows**. El agente Linux se validará cuando haya un Ubuntu.
- Preparado en este Mac: `~/Library/Application Support/rawkvm/agent.toml` con `hub = "192.168.1.28:4433"` y el token del hub (0600). El `node_id` se genera en el primer arranque.
- **Karabiner-DriverKit-VirtualHIDDevice 8.6.0 instalado en este Mac (2026-10-22)**: pkg firmado y notarizado por pqrs-org; extensión `org.pqrs.Karabiner-DriverKit-VirtualHIDDevice` 1.8.0 en estado `activated enabled` en macOS 27.0; el dext aparece en IOKit (`org_pqrs_Karabiner_DriverKit_VirtualHIDDeviceRoot` con las clases VirtualHIDKeyboard/VirtualHIDPointing). **El daemon `Karabiner-VirtualHIDDevice-Daemon` no arranca solo**: lo tiene que lanzar el instalador de nuestro agente (LaunchDaemon como root) para que exista el socket `/Library/Application Support/org.pqrs/tmp/rootonly/karabiner_virtual_hid_device_service.sock` (stream; la ruta `vhidd_server/` era de versiones antiguas, ver ADR 0005).
- Aplazado: instalar `rawkvm-agent` en el Ubuntu (`deploy/linux/install.sh`), emparejar con el token, y validar con `evtest` que al teclear en el MX Keys escribe en el Ubuntu; medir RTT y pérdidas con las estadísticas del agente.

## Fase 1 — agente macOS (2026-10-22)

- `crates/agent-macos` (binario `rawkvm-agent-macos`, instalado como `/usr/local/bin/rawkvm-agent`): cliente Rust del daemon de Karabiner (protocolo de cliente 7, ADR 0005), mapeo 1:1 de usages (`mapper.rs`), `--dry-run`, LaunchDaemon como root. `proto::descriptor` gana `fields` y `decode()` (decodificador general de informes) con tests sobre los `.nodj.bin`.
- `deploy/macos/`: `cc.olitec.rawkvm.agent.plist`, plist del daemon de Karabiner (no se instala si hay Karabiner-Elements), `install.sh` (firma ad hoc, copia el emparejamiento del usuario a `/Library/Application Support/rawkvm/agent.toml`), `uninstall.sh`. Guía: `docs/macos-instalacion.md`.
- Verificado: tests (16 del crate, incluido un daemon falso por socket Unix), clippy en macOS y Linux. `--dry-run` contra el hub real: conecta, fija la huella `71a6f657…ecac`, recibe 3 dispositivos, activo, RTT 6 ms.
- **Pendiente**: instalar con `sudo deploy/macos/install.sh` y validar con el daemon real (requiere sudo del usuario): `KeyboardReady`/`PointingReady`, escribir y mover el puntero, Cmd/Option, teclas multimedia.
- Fase 2 (no empezada): helper de usuario para bordes (CGEventTap) que hable con el daemon por socket local.

## Validación del agente macOS (2026-10-22, sesión VS Code + usuario)

- Instalado con `deploy/macos/install.sh` (sudo del usuario): LaunchDaemons `cc.olitec.rawkvm.agent` y `org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon`, emparejamiento en `/Library/Application Support/rawkvm/agent.toml`.
- Log: DriverActivated, DriverConnected (0,6 s), KeyboardReady/PointingReady (~4 s en el primer arranque, ~1 s después); hub `index=1`, teclado virtual 046d:c52b.
- Confirmado por el usuario con MX Keys y MX Ergo en el canal del Pi: escritura, Cmd+C/V, Option+A → «å», volumen, movimiento, botones, clic derecho y rueda. **Cmd/Option nativos: criterio de la Fase 1 cumplido en macOS.**
- Sin teclas pegadas al matar el agente (`launchctl kickstart -k`), vuelve en <1 s. Commit 612b4d1 añade limpieza en SIGTERM (SIGINT ya la tenía); **hace falta reinstalar el agente con sudo para tener ese binario**.
- ~10 min de uso: 7.654 informes, 0 perdidos; RTT 4–10 ms habitual, pico 24 ms, 30–80 ms solo en los primeros 30 s tras arrancar.
- Revisión del orquestador: protocolo y structs contrastados byte a byte con pqrs 8.6.0; sin hallazgos.

## Fase 1 — Windows (2026-10-22)

- `drivers/windows-vhf`: driver KMDF + VHF (`rawkvm_vhf.c`, `RawKvmVhf.inf`, `.vcxproj`), interfaz `rawkvm_ioctl.h` sobre `\\.\RawKvmVhf`, `sign-test.ps1`. Base: sample HIDInjector de Windows-IoT-Samples (`vhidmini2` no usa VHF). Diseño y firma: ADR 0006.
- `crates/agent-windows` (bin `rawkvm-agent-windows`, se distribuye como `rawkvm-agent.exe`): codificación de IOCTLs, `VhfSink` (reabre el driver y recrea los dispositivos si falla, release-all antes de DESTROY), servicio Windows `RawKvmAgent` (LocalSystem), `driver-install` (crea `Root\RawKvmVhf` con SetupAPI, como devcon), `configure`, `run`. Config y log en `%ProgramData%\rawkvm`.
- `deploy/windows/install.ps1`, `uninstall.ps1`; guía `docs/windows-instalacion.md` (viaja en el zip como `LEEME.md`).
- CI: job `windows` en `windows-2022` (trae el WDK; `windows-2025` no), compila agente (MSVC) y driver, firma con un certificado nuevo por build y publica el artefacto **rawkvm-windows**.
- Verificado: tests del crate (9) en macOS/Linux, clippy para `x86_64-pc-windows-gnu` con zig, el C del driver compila y enlaza en CI. **Pendiente: prueba en los dos PCs Windows por el usuario** (test-signing + reinicio, Secure Boot desactivado).

## Revisión del orquestador: Windows (2026-10-22)

- Verificado en el Mac sobre 87fb803: fmt, clippy -D warnings, 81 tests. CI run 39814129211 en verde (linux, hub-aarch64, macos, windows 7m44s) con artefacto `rawkvm-windows` (driver .sys/.cat firmados, .cer, .inf, rawkvm-agent.exe, install/uninstall.ps1, LEEME.md).
- Driver revisado: VhfCreate/VhfStart/VhfDelete en cola PASSIVE, `EvtFileCleanup` destruye los VHF del agente caído, WriteReport completa al instante, Get/SetFeature → STATUS_NOT_SUPPORTED, report ID = primer byte solo con `RAWKVM_FLAG_REPORT_IDS`. install.ps1 revisado: comprueba TESTSIGNING por registro, importa el .cer y retira los antiguos, ACL de ProgramData\rawkvm, servicio con reinicio. Sin hallazgos.
- **Pendiente: prueba en un PC Windows real por el usuario** (`LEEME.md`): test-signing (requiere Secure Boot desactivado; BitLocker pedirá clave de recuperación), reinicio, `install.ps1 -Hub 192.168.1.28 -Token <token>`, Administrador de dispositivos, teclear y usar el trackball, AltGr y tecla Windows nativos, conmutación con el atajo Ctrl,Ctrl,1/2 entre Mac y Windows.
- Nota menor: el token pasa por la línea de comandos de PowerShell y queda en el historial de PSReadLine del administrador; aceptable para uso personal, documentar o pedirlo interactivamente en una versión futura.

## Nuevo componente: agente gadget USB (decisión del usuario, 2026-10-22)

- **Objetivo**: un agente en una Raspberry Pi (u otra placa con USB OTG) que recibe los informes del hub por Wi‑Fi y los presenta por USB al PC como si fuera el receptor Unifying (ConfigFS HID gadget con los mismos VID/PID y descriptores crudos). El PC no necesita driver ni software: elimina test‑signing en Windows y Karabiner en macOS para ese PC. Cable USB‑A a USB‑A (o a USB‑C) entre la Pi y el PC.
- **Hardware**: Pi 3 B/B+ **no** sirven (OTG cableado al hub LAN9514; puertos solo host). Sirven Pi 3 A+, Pi Zero / Zero 2 W, Pi 4 (USB‑C), Orange Pi Zero 2W (segundo USB‑C). Pendiente: el usuario confirma qué modelo tiene.
- La prueba en Windows del driver VHF queda aplazada por el usuario; el gadget puede hacerla innecesaria para PCs con puerto USB libre.
- **Intercambio de roles (decisión del usuario, 2026-10-22)**: la Raspberry Pi 3 B (Ethernet, 4×USB host) pasará a ser el **hub** con el receptor Unifying, y la **Orange Pi Zero 2W** será el **gadget**. Comprobado por SSH en la Orange Pi (Armbian 6.1.31): nodo OTG `usb@5100000` con `status=okay`, `dr_mode=peripheral`; UDC `musb-hdrc.5.auto` presente; configfs montado; `CONFIG_USB_CONFIGFS_F_HID=y`, `CONFIG_USB_MUSB_DUAL_ROLE=y`, `libcomposite` como módulo. **No hace falta overlay**: el gadget funciona en la Orange Pi tal como está. Pendiente: identificar cuál de los dos USB‑C es el OTG (el musb) y si admite alimentación desde el PC por ese mismo cable.
- Pendiente para el hub en la Pi 3 B: SO (Raspberry Pi OS 64 bits → `aarch64-unknown-linux-musl`; 32 bits → `armv7-unknown-linux-musleabihf`), acceso SSH con clave, blacklist de `hid_logitech_dj`, `deploy/hub-linux/install.sh` (vale para Raspberry Pi OS; renombrar o generalizar).
- Crate previsto: `crates/agent-gadget` (Linux, aarch64 y armv7), reutiliza `agent-core`; HidSink sobre ConfigFS (`/sys/kernel/config/usb_gadget/rawkvm`), un `hid.usbN` por HidDevice, informes a `/dev/hidgN`. Detalles en el brief enviado a la sesión de VS Code.

## Agente gadget USB: implementado (2026-10-22)

- `crates/agent-gadget` (lib + bin `rawkvm-agent-gadget`): `configfs.rs` (árbol del gadget como lista de operaciones, funciones `hid.usb<id>` enlazadas en orden, `report_length` = mayor input + ID: 8/8/32 con los `.nodj.bin`, subclase/protocolo boot 1/1, 1/2, 0/0), `hidg.rs` (`/dev/hidgN` vía `dev` + `/sys/dev/char`, EAGAIN: 8 ms ratón / 50 ms resto y descarte, hilo que drena los OUT del host), `sink.rs` (`GadgetSink`; conserva el gadget si la DeviceList no cambia, release-all en destroy_all, desmontaje al salir y al arrancar). `agent-core` gana `HidSink::devices_ready()`.
- CLI: `--hub/--token/--unpin`, `--configure-only`, `--udc`, `--no-hidpp` (hosts Linux), `--serial`. Config `/etc/rawkvm/agent.toml`.
- `deploy/gadget/install.sh` (overlay dwc2 en Pi, modules-load, ConfigFS, servicio), `rawkvm-agent-gadget.service`; `docs/gadget-instalacion.md`; ADR 0007.
- CI: job `gadget` (aarch64-musl, armv7-musleabihf) con artefactos `rawkvm-agent-gadget-aarch64` / `-armv7`.
- Verificado en el Mac: 8 tests (ConfigFS simulado en directorio temporal), clippy, cross-build de ambos targets. **Pendiente: prueba en una placa real** (el usuario confirma el modelo).

## Gadget USB probado en hardware (2026-10-22, orquestador)

- Orange Pi Zero 2W con hub y gadget a la vez (gadget → `127.0.0.1:4433`), USB‑C de alimentación/OTG conectado al Mac (la placa se alimenta del Mac). Gadget `046d:c52b` enlazado a `musb-hdrc.5.auto` con 3 interfaces, UDC `configured`; macOS enumera "Logitech USB Receiver" (serie = node_id) con teclado, ratón y vendor 0xFF00, como un receptor real.
- Incidencias resueltas: (1) `mkdir` de los grupos automáticos de ConfigFS (corregido en 8c06b4a); (2) tras el reinicio, `hid_logitech_dj` volvió a cargarse porque el **initramfs de Armbian no tenía la blacklist**: ejecutado `update-initramfs -u` en el Pi; el instalador del hub debe hacerlo (pendiente VS Code); (3) el gadget se reenlaza por cada DeviceList incremental (3 binds en 80 ms): pendiente estabilización de ~500 ms (VS Code).
- Tras el reinicio el hub reasignó índices: gadget = 1 (activo), Mac/Karabiner = 2. Mejora propuesta: persistir índices y último activo en el state_dir.
- **Confirmado por el usuario y por los logs (2026-10-22 21:01)**: escritura y trackball por USB en el Mac vía gadget (5.588 informes, 0 perdidos); **atajo de emergencia validado**: Ctrl,Ctrl,2 → Mac/Karabiner y Ctrl,Ctrl,1 → gadget, cuatro pulsaciones reconocidas por el hub ("emergency hotkey index=N" + "switched"), agentes pasando a active/inactive y recibiendo informes en consecuencia, sin teclas pegadas. Con esto el criterio de la Fase 1 "conmutación por atajo de emergencia" queda cumplido (Mac + gadget; Windows pendiente de prueba).
- 6a7c018 desplegado en la Orange Pi: bind único del gadget 500 ms tras la DeviceList, verificado.
- **6c66734 desplegado y verificado (21:03)**: `/var/lib/rawkvm/nodes.toml` con gadget = índice 1 y Mac = índice 2 (fijos desde ahora; cambiar con `--forget-node` con el hub parado); al reiniciar el hub espera hasta 10 s al último activo y lo reactiva al reconectar. Log de diagnóstico del atajo disponible con `RUST_LOG=info,rawkvm_hub::hotkey=debug`.

## Correcciones tras la prueba del gadget (2026-10-22, VS Code)

- `deploy/hub-linux/install.sh` ejecuta `update-initramfs -u` (si existe) tras instalar la blacklist; documentado en `docs/fase0-validacion.md`, `docs/gadget-instalacion.md` y ADR 0003.
- Hub: `[receiver] require_hid_generic = true` (por defecto). En modo A no abre interfaces con otro driver y lo registra como error (una vez por interfaz y driver) con la causa probable (initramfs).
- `agent-core`: `HidSink::settle_time()` (por defecto cero). `devices_ready()` se llama cuando la DeviceList lleva ese tiempo sin cambios; el gadget usa 500 ms, también al arrancar. ADR 0007, revisión.
- Pendiente de desplegar en la Orange Pi: binarios aarch64 de hub y gadget.

## Índices persistentes y log del atajo (2026-10-22, VS Code)

- `crates/hub/src/nodes.rs`: `NodeStore` sobre `<state_dir>/nodes.toml` (node_id → índice, nombre, `last_active`). El router lo usa (`Router::with_store`). Índice reservado aunque el nodo esté desconectado, nodo nuevo al primer libre no reservado, último activo restaurado si conecta en 10 s (`RESTORE_GRACE`, `end_restore()`). ADR 0008.
- CLI del hub: `--list-nodes`, `--forget-node <uuid|índice>` (con el hub parado).
- `hotkey.rs`: log `debug` de la máquina de estados sin códigos de tecla (`RUST_LOG=info,rawkvm_hub::hotkey=debug`). Documentado en `docs/fase0-validacion.md` §6.

## Arquitectura definitiva (decisión del usuario, 2026-10-22)

| Equipo | Papel | Cómo recibe la entrada |
|---|---|---|
| Orange Pi Zero 2W | **Hub** (receptor Unifying en el USB‑C host) **y gadget** (USB‑C OTG) | — |
| PC Windows | Conectado por cable USB al OTG de la Orange Pi | Ve un receptor Unifying real: **sin driver, sin test‑signing** |
| Mac | Agente macOS (Karabiner) por Wi‑Fi | Dispositivos virtuales Karabiner |
| Ubuntu | Agente Linux (uhid) por Wi‑Fi | uhid (pendiente de validar cuando haya máquina) |

- **Descartados**: la Raspberry Pi 3 como hub, y el driver VHF + agente Windows (quedan en el repo como alternativa, sin seguimiento; el job de CI `windows` debería pasar a manual para no costar 8 min por push).
- **Riesgo de alimentación**: el USB‑C OTG de la Orange Pi es también su entrada de alimentación. Conectado al Windows, el hub se apaga o reinicia con el PC (suspensión, apagado, BIOS con ErP) y todos los agentes pierden el hub. Soluciones: alimentar la placa por los pines 5V/GND del conector de 40 pines con una fuente propia (el OTG sigue funcionando solo para datos), o asegurarse de que el PC mantiene 5 V en USB en S3/S5. **Decisión del usuario: el PC Windows alimenta la Orange Pi por el cable USB; el hub se apaga y arranca con el PC.** Consecuencias ya cubiertas por el diseño: los agentes reconectan solos con backoff y el hub restaura el último activo; el receptor se reinicia con la placa (sale de DJ por sí mismo) y la blacklist está en el initramfs.
- **Fase 2 con este esquema**: el PC Windows no ejecuta ningún agente, así que no puede detectar el borde de pantalla por sí solo. Hace falta un *helper* de usuario en Windows sin driver (solo hook de ratón + QUIC al hub) que envíe `SwitchTo` en nombre del nodo gadget; el código de `agent-windows` (servicio, config, cliente) se puede reutilizar sin la parte del driver. En Mac el helper de borde va en sesión de usuario junto al daemon; en Linux la extensión GNOME.

## Ajustes a la arquitectura definitiva (2026-10-22, VS Code)

- CI: el job `windows` pasa a `.github/workflows/windows.yml` (solo con cambios en `drivers/windows-vhf`, `crates/agent-windows`, `deploy/windows`, o a mano con *Run workflow*). El job `linux` mantiene clippy de `agent-windows` cruzado a `x86_64-pc-windows-gnu` (MinGW).
- Docs: driver VHF marcado como alternativa aparcada (`windows-instalacion.md`, ADR 0006); en `gadget-instalacion.md`, el gadget como vía recomendada para Windows, alimentación por el conector de 40 pines y nota sobre Logitech Options+/G HUB.
- Gadget: `--udc-wait <s>` (60 por defecto; sustituye al `ExecStartPre` de la unit) y `HostState`: si el PC deja de leer (ESHUTDOWN o EAGAIN agotado), se registra una vez, se escribe sin esperar y se avisa al volver.
- `docs/fase2-plan.md`: plan de Fase 2 para revisar con el usuario (sin implementar).

## Fase 2: respuestas del usuario (2026-10-22) y arranque

1. **Disposición física, de izquierda a derecha**: Mac pantalla integrada → Mac monitor externo → PC Windows → Ubuntu. Dos monitores en el Mac, uno en Windows, uno en Linux.
2. El PC Windows está en la misma LAN que la Orange Pi.
3. **Cambio inmediato** al tocar el borde; si da problemas, pasar a "empujar".
4. Acepta un `.exe` sin firmar en Windows que arranque al iniciar sesión.

Layout inicial para `hub.toml` (índices de `nodes.toml`: gadget/Windows = 1, Mac = 2; Linux cuando exista): borde derecho del monitor externo del Mac ↔ borde izquierdo del Windows; borde derecho del Windows ↔ borde izquierdo del Linux. Orden de implementación aprobado: protocolo y hub, lógica de borde, helper macOS, helper Windows; después GNOME y widget.
- **Requisito del usuario (2026-10-22)**: el widget debe permitir cambiar la posición relativa de monitores y PCs en cualquier momento. El layout escrito a mano en `hub.toml` es solo arranque: el hub debe tratar el `Layout` recibido del widget como fuente de verdad, persistirlo (p. ej. `state_dir/layout.toml`), reenviarlo a todos los helpers al instante y preferirlo sobre `hub.toml` en el siguiente arranque. Los helpers no deben cachear nada que no venga del hub.

## Fase 2, pasos 1 y 2: protocolo v2, layout en el hub, lógica de borde (2026-10-22, VS Code)

- **Protocolo v2** (`PROTO_VERSION = 2`):
  - `NodeKind::EdgeHelper { for_node }`.
  - `SwitchTo { target, screen, enter_edge, pos_ratio }`, `Activate { screen, enter_edge, pos_ratio }` y `Layout { screens, edges, options: EdgeOptions }`.
  - `Hello` y `Reject` están congelados: un nodo v1 recibe `VersionMismatch` y se detiene con un mensaje claro. Hay test con los bytes de un `Hello` v1.
- **Hub, helpers**: los helpers no tienen índice; `Screens` y `SwitchTo` cuentan por su agente, y reciben `Activate`/`Deactivate`.
- **Hub, layout** (`layout.rs`):
  - Semilla `[[layout]]` en `hub.toml` por índice. La regla de pantalla por defecto es la más exterior en la dirección del borde.
  - El layout de un widget se persiste en `/var/lib/rawkvm/layout.toml`, se aplica al instante y gana a la semilla en los arranques siguientes.
  - CLI `--dump-layout` / `--load-layout <f>`; `ExecReload` (SIGHUP) recarga sin reiniciar.
  - Opciones `[edge] push_ms = 0, corner_pct = 3, dead_zone_px = 2`.
- **`agent-core::edge::EdgeSwitcher`**:
  - Detección en el último píxel, con exclusión de esquinas y umbral `push_ms`.
  - Un `SwitchTo` por contacto, zona muerta al entrar y aparcado en el centro.
  - Tests de tabla con el escritorio del usuario (geometrías inventadas y marcadas).
- **Docs**: `protocol.md` (v2, Layout) y ADR 0009.
- **Despliegue**: hub, gadget y agente macOS deben actualizarse a la vez (orquestador). El agente Linux también, cuando se instale.
- **Siguiente**: helper macOS (paso 3), pendiente de visto bueno.

## Fase 2, paso 3: helper macOS (2026-10-22, VS Code)

- `agent-core::helper` (`run_helper` + trait `EdgePlatform`). Test e2e: dos agentes y dos helpers contra el hub real con la semilla `[[layout]]`. El borde derecho del primero activa el segundo, que coloca el puntero en su borde izquierdo, y el primero aparca.
- `crates/edge-macos` (bin `rawkvm-edge-macos`, instalado como `/usr/local/bin/rawkvm-edge`):
  - CoreGraphics sin permisos;
  - `--show`;
  - `--from-agent` (copia hub, token, certificado y `for_node` del agent.toml de root);
  - `--for-node/--hub/--token/--unpin/--configure-only`.
- `deploy/macos/install-edge.sh` (sudo; LaunchAgent `cc.olitec.rawkvm.edge` en `gui/<uid>`, log `~/Library/Logs/rawkvm-edge.log`); `uninstall.sh` también lo quita. Documentado en `docs/macos-instalacion.md` §6 y ADR 0009.
- Pantallas reales de este Mac (`--show`): 0 = integrada 1470×956 @2x en (0,0); 1 = externa 1720×1440 @1x en (1470,−251).
- Errores fatales (versión, token, certificado): los agentes y el helper registran el error y, si corren como servicio (sin terminal), esperan 60 s antes de salir. El hub registra cada rechazo repetido de un mismo nodo y motivo una vez por minuto.
- **Pendiente de probar en hardware**: el warp (supresión de eventos con el ratón de Karabiner) y el cambio real Mac → gadget.

## Incidente: apagón de la Orange Pi tras un despliegue (2026-10-22 21:35–21:44)

- La Orange Pi se apagó bruscamente (cambio del cable USB al Windows) poco después de instalar hub v2, unit nueva y la semilla del layout. Al arrancar, la SD había **perdido las escrituras posteriores a las 21:13**: binario del hub corrupto (SEGV incluso con `--version`, hash distinto), `hub.toml` sin la semilla, unit antigua. El hub quedó en bucle de reinicios (15 intentos) y ningún equipo tenía teclado.
- Resuelto reinstalando hub, gadget, unit y semilla con `sync` explícito y comprobando hashes. `dmesg` no muestra errores de ext4; es escritura diferida sin flush antes del corte.
- **Medidas**: todos los scripts de despliegue hacen `sync` al final; el orquestador comprueba el hash tras instalar; y tras un despliegue hay que esperar unos segundos antes de cortar la alimentación. Como la Orange Pi se alimenta del PC Windows, cualquier apagado del PC es un corte brusco: valorar `commit=1` en `/etc/fstab` para la raíz, o alimentación por GPIO.
- Tras el arreglo: hub v2 con semilla de layout, gadget activo (índice 1) enumerado por el Windows y funcionando allí (confirmado por el usuario), agente macOS v2 registrado como índice 2.
- **Causa raíz confirmada**: la raíz de Armbian estaba montada con `commit=600` (10 min de caché de escritura). Aplicado `commit=5` en `/etc/fstab` (copia en `/etc/fstab.bak-rawkvm`) y remontado en caliente el 2026-10-22. Hub y gadget 0482dd1 desplegados (escrituras durables con fsync, `sync` en instaladores, commit en el log de arranque), hashes comprobados.

## Tras el apagón: escrituras durables y versión en el log (2026-10-22, VS Code)

- `config::write_private` (hub) y `AgentConfig::save` (agentes) escriben a un temporal, hacen `fsync`, renombran y hacen `fsync` del directorio. Lo usan `hub.toml`, `nodes.toml`, `layout.toml`, el certificado y `agent.toml`.
- Todos los `install.sh` terminan con `sync`; `install.ps1`, con `Write-VolumeCache`.
- `rawkvm-hub` y `rawkvm-agent-gadget`: `--version` incluye el commit (`build.rs`), y la primera línea del log de cada arranque dice `starting version=… pid=…`.
- Docs: «Cortes de alimentación y la tarjeta SD» en `gadget-instalacion.md` (Armbian monta con `commit=600`: recomendado `commit=5` o `1`, o alimentar por GPIO) y entrada en problemas conocidos de `fase0-validacion.md`.

## Fase 2, paso 4: helper Windows (2026-10-22, VS Code)

- `crates/edge-windows` (bin `rawkvm-edge.exe`, subsistema GUI):
  - `EnumDisplayMonitors`/`GetDpiForMonitor`, `GetCursorPos`/`SetCursorPos`, DPI por monitor v2;
  - log en `%LOCALAPPDATA%\rawkvm\edge.log` (se trunca al pasar de 4 MiB);
  - config `%APPDATA%\rawkvm\edge.toml`;
  - `--show`, `--hub/--token/--for-node/--unpin/--configure-only`.
- `deploy/windows/install-edge.ps1` / `uninstall-edge.ps1`: sin administrador; clave `HKCU\...\Run`; `Unblock-File`.
- CI: job `edge-windows` (MinGW en Ubuntu) con artefacto `rawkvm-edge-windows` (exe + scripts + LEEME). clippy cruzado en el job linux. El workflow `windows` (driver) ya no se dispara con los scripts del helper.
- Docs: `docs/windows-borde.md` y ADR 0009 (sección Helper Windows).
- Verificado: clippy con `x86_64-pc-windows-gnu` y el `.exe` compilado (PE32+ GUI). **No probado en Windows.**

## Fase 2, paso 3: helper macOS en hardware (2026-10-22 22:15)

- `rawkvm-edge` instalado como LaunchAgent del usuario (`cc.olitec.rawkvm.edge`), configurado por `--from-agent` con el node_id del agente macOS. Conecta al hub como `EdgeHelper` (índice 0), reporta las dos pantallas reales y el hub las registra. Sin permisos TCC.
- **Resuelto con d69e767** (enlace de ida con pantalla provisional) y **primer salto por borde validado (2026-10-22 22:24)**: el helper detectó el borde derecho de la externa en (3189, 384), pos_ratio 0,44, el hub conmutó al gadget/Windows, el Mac quedó inactivo con el puntero aparcado en el centro de la externa; vuelta con Ctrl,Ctrl,2. Confirmado por el usuario ("ahora sí pasa"). Siguiente: helper de Windows (2dc5694, CI verde) para el enlace de vuelta.
- ~~Bloqueo~~ (histórico): `layout links=0`. La semilla `2 right → 1` no se resuelve porque el nodo 1 (gadget/Windows) no reporta pantallas. Pedido a VS Code: resolver enlaces hacia nodos sin pantallas con una pantalla provisional (el destino no necesita colocar el puntero). Hasta entonces no se puede probar el salto por el borde.

## Fase 2: ida y vuelta Mac ↔ Windows por el borde, validada (2026-10-22 22:33)

- Helper de Windows (`2dc5694`, sin driver ni administrador) instalado por el usuario con `install-edge.ps1`: nodo "GAMER (edge)" registrado como EdgeHelper del gadget, **2 pantallas reportadas** (el PC Windows tiene dos monitores; la regla de pantalla más exterior eligió la izquierda sin configurar nada). El hub pasó a `layout screens=4 links=2` automáticamente.
- Tres cambios consecutivos por borde en 7 s: Windows→Mac (entrada por el borde derecho de la externa, pos 0,61, puntero colocado en (3187, 627)), Mac→Windows (0,55), Windows→Mac (0,66). Sin atajo. Sin teclas pegadas reportadas.
- **Criterio de aceptación de la Fase 2 cumplido para Mac ↔ Windows**: llevar el puntero al borde cambia de equipo y aparece a la altura correcta. Pendientes de la Fase 2: Ubuntu (GNOME) cuando haya máquina, widget Tauri (descubrimiento, layout gráfico editable, estado), y afinado (rebotes, esquinas, opción "empujar") según uso real.

## Widget Tauri: aprobado (2026-10-22)

Decisiones del usuario/orquestador sobre `docs/widget-plan.md`: (1) protocolo v3 con la disposición del lienzo (`Layout.canvas`) persistida en el hub; (2) primera entrega solo para el Mac, Windows después; (3) Svelte; (4) cambio rápido de equipo desde la bandeja. Orden: agent-core::widget + canvas, proto v3, esqueleto Tauri + pestaña Hub, Monitores, Dispositivos/Opciones, empaquetado + CI. Al pasar a v3 habrá que redesplegar hub, gadget, agente y helpers del Mac y helper del Windows a la vez.

## Widget, paso 1: agent-core::canvas y agent-core::widget (2026-10-22, VS Code)

- `canvas`:
  - bloques rígidos por nodo en unidades lógicas (un factor por nodo: escala de su principal o corrección manual) y marcador de 1920×1080 para nodos sin pantallas;
  - `links()`: lados que se tocan entre nodos distintos, con tolerancia de 1 unidad, dan un enlace en cada sentido con los rangos del tramo solapado;
  - `snap()`: imán a lados y alineado de bordes superiores;
  - `initial()`: disposición inicial a partir de los enlaces en vigor;
  - tests con el escritorio real del Mac (Windows inventado y marcado).
- `widget`: `WidgetState` (estado, pantallas vivas por nodo, layout, dispositivos, RTT) y `run_widget` (sesión `Widget` con reconexión y comandos `Control`). Test e2e contra el hub real: el widget ve pantallas, estado, dispositivos y RTT, envía un `Layout`, y un segundo widget lo recibe.
- `proto::Placement` añadido. Aún no viaja en ningún mensaje: no cambia el protocolo.
- Propuesta de protocolo v3 en `docs/widget-plan.md`, pendiente de revisión del orquestador antes de tocar los agentes.

## Widget, paso 2: protocolo v3 (2026-10-22, VS Code)

- `PROTO_VERSION = 3`:
  - `proto::Entry`, con `SwitchTo { target, entry: Option<Entry> }` y `Activate { entry: Option<Entry> }`;
  - `Layout.canvas`;
  - `HubStatus.version`, `NodeInfo.screens`;
  - `Control::Settings(HubSettings)`;
  - `Screen::scale` = unidades nativas por unidad lógica: el helper macOS manda 1.0.
- Hub:
  - `layout.toml` guarda también el lienzo;
  - `settings.toml` (`settings.rs`) manda sobre `[switching]`; la histéresis y el atajo se leen del estado;
  - el `SwitchTo` de un widget no respeta la histéresis;
  - widgets reciben `Settings` al conectar;
  - `Status` tras cada `Screens`.
- `agent-core`: helper y agentes adaptados (el `HidSink` no cambia). `WidgetState` lleva `canvas` y `settings`.
- Docs: `protocol.md` (v3, sección «Pantallas y unidades»), ADR 0010.
- Redespliegue conjunto pendiente: hub y gadget en la Pi, agente y helper del Mac, `.exe` del helper Windows (artefacto `rawkvm-edge-windows`).

## Protocolo v3 desplegado y validado (2026-10-22 23:0x)

- Hub y gadget 0f83e4e en la Orange Pi (hashes comprobados, sync), agente y helper del Mac reinstalados por el usuario, helper del Windows reinstalado. Hub: `layout screens=4 links=2`. Cruce por el borde en ambos sentidos confirmado por el usuario con v3 (Entry dentro de SwitchTo/Activate).
- **Encargado a VS Code (2026-10-22)**: intercambio opcional Alt↔GUI por nodo destino en el hub (parche para el perfil Mac del MX Keys en Windows; opt-in, solo interfaz de teclado, documentado como desviación del reenvío crudo). **Primer objetivo de la Fase 3**: cambio de perfil del teclado por HID++ (HOSTS_INFO 0x1815, setHostOsVersion) al conmutar, que sustituirá al parche. Mientras tanto, parche manual: Fn+O (Windows) / Fn+P (macOS) en el MX Keys.

## Widget, paso 3: esqueleto Tauri, bandeja y pestaña Hub (2026-10-22, VS Code)

- `widget/`: Svelte 5 + Vite 8 y `src-tauri` (Tauri 2) en su propio workspace (`exclude = ["widget"]` en la raíz).
  - `view.rs` (puro, con tests) deriva lo que dibuja la UI: nodos con rol, helper de quién, pantallas, `has_helper`, activo, RTT y versión del hub.
  - `lib.rs`:
    - la sesión `run_widget` se reinicia al emparejar u olvidar;
    - bandeja (icono plantilla, número del equipo activo como título, menú con los agentes y cambio rápido `SwitchTo { entry: None }`; el menú solo se reconstruye cuando cambian conexión, equipos o activo, no con cada RTT);
    - ventana de ajustes que se oculta al cerrarla;
    - `ActivationPolicy::Accessory` (sin icono en el Dock).
  - Comandos: `get_view`, `discover` (mDNS, marca los hubs de otra versión de protocolo), `pair` (TOFU), `forget`, `switch_to`.
- Pestaña Hub: emparejar (buscar o escribir dirección + token), estado, latencia, versión, certificado, y tabla de equipos con los agentes sin helper en gris y el botón «Usar».
- Config local `~/Library/Application Support/rawkvm/widget.toml` (hub, token, huella); log `~/Library/Logs/rawkvm-widget.log`.
- `[profile.release.build-override] strip = false`: con el `strip` por defecto, las dylib de proc-macros fallan al cargarse en este macOS («mis-aligned LINKEDIT string pool»).
- CI: `.github/workflows/widget.yml` (macos-14; solo con cambios en `widget/`, `agent-core` o `proto`, o a mano). Artefacto `rawkvm-widget-macos` (`RawKVM.app.zip` firmada ad hoc + LEEME).
- Docs: `docs/widget-instalacion.md`.
- Verificado: `.app` de 9,4 MB; arranca y escribe el log («not paired»). **Sin probar contra el hub** (el emparejamiento lo hace el usuario con el token).

## Intercambio Alt/GUI por agente (2026-10-22, VS Code)

- `[[node_options]] index = N, swap_alt_gui = true` en `hub.toml` (por defecto no).
- `KeyboardLayout::swap_alt_gui` (proto) intercambia los bits 0x04↔0x08 y 0x40↔0x80 y los usages 0xE2↔0xE3 y 0xE6↔0xE7, solo en informes de la interfaz de teclado.
- El router lo aplica solo al agente activo con la opción, después del atajo. La copia redundante va igual; el release-all no cambia.
- Tests con informes boot de 8 B y con el layout DJ de 7 B (K230), y test del router (solo el destino configurado; ratón y release-all intactos).
- ADR 0011; nota en `gadget-instalacion.md`. **Primer objetivo de la Fase 3**: cambio de perfil por HID++ `HOSTS_INFO` 0x1815 `setHostOsVersion` (anotado en `fase2-plan.md`).

## Widget paso 3 en el Mac y llegada del Ubuntu (2026-10-23 00:00)

- RawKVM.app (4be4002) instalada en `~/Applications`, pre-emparejada por el orquestador (widget.toml con hub y token), permiso de Red local concedido por el usuario; conectada al hub como nodo Widget (índice 0), huella fijada. Pendiente de la prueba de bandeja y pestaña Hub por el usuario.
- Hub dc62461 con `[[node_options]] index = 1, swap_alt_gui = true` activo en la Orange Pi (ADR 0011).
- **El usuario ya dispone del Ubuntu**: siguiente, instalar el agente Linux (uhid, nunca probado en hardware) con `deploy/linux/install.sh`, validar con `evtest`, y después la extensión GNOME (paso 5 de la Fase 2). Binario x86-64 compilado con `cargo zigbuild --target x86_64-unknown-linux-gnu`.

## Widget, paso 4: pestaña Monitores (2026-10-22, VS Code)

- `widget/src-tauri/src/canvas_view.rs` (puro, con tests) calcula todo lo que dibuja el SVG:
  - **qué equipos**: los agentes de `Status`, más los nodos con pantallas o con bloque guardado aunque estén apagados;
  - **disposición**: la del hub, completando los nodos nuevos a la derecha, o una propuesta (`canvas::initial`) mientras el hub usa la semilla;
  - **bloques** con escala informada y corregida, **rectángulos** con su tamaño nativo y **tramos de enlace** (uno por par);
  - **acciones**: `drop_block` (imán de 40 unidades), `set_scale` y `layout_for` (enlaces + opciones + lienzo).
- Comandos: `canvas_view`, `canvas_drop`, `canvas_scale`, `canvas_apply`, `canvas_reset`.
- `Monitors.svelte`: SVG que se arrastra con el puntero (traslado local mientras se arrastra; el imán y los enlaces los calcula Rust al soltar), líneas de enlace, botones Aplicar / Descartar / Volver a la semilla, tabla de escala con corrección manual.
- Hub: un `Layout` de widget con `edges` y `canvas` vacíos vuelve a la semilla y borra `layout.toml`. Sin cambio de protocolo, pero **hay que redesplegar el hub** para que «Volver a la semilla» funcione: el hub `dc62461` lo guardaría como layout vacío.
- **Sin probar en la app real** (geometría cubierta por tests; la interfaz solo compilada).

## Widget, paso 5: Dispositivos y Opciones (2026-10-22, VS Code)

- `view.rs`: `devices` (interfaces del receptor con su contenido según las colecciones del descriptor: teclado, ratón, multimedia, control del sistema, HID++) y, en `View`, `settings`, `options` y `custom_layout`. Test con los descriptores de referencia.
- Comandos:
  - `set_settings` (`Control::Settings`, 0–5000 ms);
  - `set_edge_options` (reenvía el layout en vigor con las opciones nuevas; si el hub usa la semilla, envía la disposición actual de Monitores);
  - `get_autostart` / `set_autostart` (`tauri-plugin-autostart`, LaunchAgent).
- `Devices.svelte` (emparejar HID++ deshabilitado hasta la Fase 3) y `Options.svelte`.
- Sin cambios en hub ni protocolo. **Sin probar en la app real.**

## Widget paso 4 validado (2026-10-23)

- Hub 75944b5 en la Orange Pi ("Volver a la semilla" por Layout vacío). RawKVM.app 75944b5 en el Mac. Usuario: la propuesta del lienzo aparece (Mac a la izquierda, GAMER a la derecha), **Aplicar** guarda el layout en el hub, y al mover un bloque y aplicar **el cambio por borde sigue la nueva disposición sin reiniciar nada**. Requisito del usuario (layout editable en caliente desde el widget) cumplido.

## Fase 2, paso 5: GNOME (extensión + helper en el agente Linux) (2026-10-22, VS Code)

- `gnome-extension/rawkvm@olitec.cc` (GNOME 46, ESM): objeto D-Bus `cc.olitec.RawKvm.Shell` con:
  - `Monitors`, `Watch` (arrendamiento de 3 s) y la señal `Pointer` (cada 8 ms, solo si el puntero se movió);
  - `GetPointer`, `Warp`, `MonitorsChanged` y la propiedad `Version`.
- `crates/agent-linux/src/edge.rs`:
  - zbus 5 (tokio) y una caché sincronizada por una tarea puente, sobre la que se implementa `EdgePlatform`;
  - `Watch` solo mientras el helper pide el puntero;
  - config derivada `~/.config/rawkvm/edge.toml` (node_id UUID v5 del agente, mismo hub, token y huella, `for_node` = el agente);
  - espera a que el agente esté emparejado y la extensión presente (aviso una vez, reintento cada 10 s);
  - `--no-edge` lo desactiva.
- `deploy/linux/install.sh` instala la extensión y la añade a `enabled-extensions` (hay que volver a iniciar sesión).
- `docs/gnome-plan.md`: instalación y comandos de verificación (`gdbus`) para copiar tal cual.
- Verificado: tests (conversión de monitores, derivación de config, caché del puntero) y clippy en macOS y Linux. **Sin probar en el Ubuntu.**

## Incidente de la coma repetida en el Mac (2026-10-23) y endurecimiento

- Causa más probable, según el usuario: una pulsación física sin querer. Con la distribución española, además, la «,» puede venir del «.» del teclado numérico (usage 0x63), no de 0x36.
- No se encontró ningún fallo en `SeqTracker` ni en la unión del mapper. Test nuevo con secuencias reales: press, release y copia tardía del press; release antes que el press; informe de otra interfaz en medio; reconexión con `clear()` y `SeqTracker` nuevos. No queda nada pulsado.
- Fallo latente corregido en `agent-macos`: si una publicación en Karabiner fallaba, el mapper ya había dado el estado por enviado (una suelta perdida podía dejar la tecla pulsada) y el resto de salidas se descartaba. Ahora se marca `resync` y se reenvía el estado completo (`Mapper::full_state`) en el siguiente informe o en el siguiente tick.
- Watchdog: `HidSink::tick` (cada segundo) avisa una vez si una tecla o un modificador lleva más de 10 s pulsado sin ningún informe de teclado nuevo. No dice qué tecla y no suelta nada.
- «Liberar teclas»: en la bandeja del widget y en Opciones. Es un `SwitchTo` del widget al nodo ya activo; el hub envía `RELEASE_ALL` por dispositivo a todos los agentes, dos veces. Sin cambio de protocolo; requiere el hub nuevo.

## Agente Linux en hardware (2026-10-23 01:34, Ubuntu "Linux-JCF")

- Instalación manual por el usuario con el paquete `rawkvm-linux.zip` (f66d895). Primer arranque sin permiso en `/dev/uhid` (grupo `input` sin re-login, como avisa el instalador); tras reiniciar sesión, **uhid crea los cuatro dispositivos** (teclado, ratón, consumer, system control) con nombre "Logitech USB Receiver" en bus 0006 (virtual), `event14..17`. **Confirmado por el usuario (2026-10-23): teclado y trackball funcionan en el Ubuntu tras Ctrl,Ctrl,3. Criterio de la Fase 0 para Linux cumplido; los tres SO reciben entrada nativa (Fase 1 completa).**
- Nodo "Linux-JCF" registrado como índice 3. **El Ubuntu es 26.04.1 LTS con GNOME Shell 50.1** (no 24.04/46): la extensión queda en estado OUT OF DATE por `shell-version ["46"]`. Encargado a VS Code adaptar y verificar la extensión contra GNOME 50 y ampliar las versiones declaradas; después, paquete solo con la extensión y re-login en el Ubuntu.

## Extensión GNOME: de la 45 a la 51 (2026-10-23, VS Code)

- El Ubuntu del usuario es 26.04.1 LTS con GNOME Shell 50.1: la extensión salía `OUT OF DATE` porque solo declaraba la 46.
- APIs comprobadas en el código fuente (mutter y gnome-shell, tags 45.0 … 51.0 y 50.1): `global.get_pointer`, `global.display` (n/primario/geometría/escala de monitores), `ClutterSeat.warp_pointer`, `'monitors-changed'` y la clase ESM `Extension`.
- Única diferencia: cómo llegar al seat. `Clutter.get_default_backend()` desaparece en 51 y `actor.get_context()` existe desde la 47. Se detecta en tiempo de ejecución (`global.stage.get_context?.()?.get_backend?.() ?? Clutter.get_default_backend?.()`), sin ramas por versión.
- `shell-version` de la 45 a la 51.
- `install.sh` avisa si `gnome-shell --version` no está en la lista; `gnome-plan.md` explica `OUT OF DATE`, `INITIALIZED` y `ERROR`.
- Verificado en el Ubuntu: ver la sección siguiente.

## Borde GNOME verificado en el Ubuntu (2026-10-23 03:54, Linux-JCF)

- Extensión 720f975 copiada por SSH a `~/.local/share/gnome-shell/extensions/rawkvm@olitec.cc`; tras cerrar y abrir sesión (Wayland solo relee `metadata.json` al arrancar) `gnome-extensions info` pasa de `OUT OF DATE` a **ACTIVE** en GNOME Shell 50.1.
- D-Bus comprobado a mano: `Monitors` → `[(0, 0, 1536, 960, 1.667, primario)]`, `GetPointer` → `(768, 480)` (puntero aparcado por el helper). Ojo: la interfaz se llama `cc.olitec.RawKvm.Shell1` (con el `1`); el nombre de bus y la ruta son `cc.olitec.RawKvm.Shell` / `/cc/olitec/RawKvm/Shell`. Los comandos de `gnome-plan.md` ya usan `Shell1` en `--method`; `verificar-gnome.sh` no está en el repositorio (es del paquete del usuario) y debe hacer lo mismo.
- El agente (f66d895, ya en marcha) la detectó solo a los 10 s: "GNOME extension found version=1", "edge helper connected", pantalla 1536×960 escala 1.0, layout links=3, puntero aparcado al quedar inactivo.
- Hub: "Linux-JCF (edge)" registrado como EdgeHelper, `screens reported … screens=1`, **layout screens=4 links=8** (2 Mac + Windows + Ubuntu). Antes de conectar el helper ya se conmutaba *hacia* Linux por el borde superior de GAMER (03:49 y 03:53); salir de Linux por el borde requiere el helper, ahora presente.
- Lienzo actual en `layout.toml`: Linux encima de GAMER (y = -920) y pegado a la derecha del monitor externo del Mac; no es la disposición Mac → Windows → Linux en fila que pidió el usuario. Se recoloca desde el widget (Monitores → Aplicar) sin tocar nada más.
- **Confirmado por el usuario (2026-10-23): el cruce por el borde entre Windows y Ubuntu funciona en ambos sentidos con el puntero.** Fase 2, paso 5 cerrado: los tres SO conmutan por borde. Pendiente: reinstalar el agente macOS 5464fb4; recolocar el bloque Linux en el widget si se quiere la fila Mac → Windows → Linux.

## Fase 3 encargada (2026-10-23)

- Brief en `docs/fase3-plan.md` (ddd14a8), aprobado por el usuario y enviado a VS Code. Bloques: A) informes de salida PC → teclado (**el LED de Bloq Mayús del MX Keys no funciona en ningún PC**, pedido por el usuario), B) transporte HID++ en el hub y CLI para validar `HOSTS_INFO` 0x1815 en la Pi, C) perfil del teclado al conmutar (sustituye a `swap_alt_gui` si B funciona en caliente), D) emparejamiento desde el widget. Protocolo v4 en un salto; el orquestador redespliega todo a la vez.
- Pendiente del Mac: reinstalar el agente 5464fb4.

## Fase 3 A: informes de salida y LED de Bloq Mayús (2026-10-23, VS Code)

- Protocolo **v4** (un solo salto para toda la Fase 3): `FLAG_OUTPUT = 0x02` en datagramas agente → hub; `ReceiverDevices`, `PairedDevice` y `Unpair` ya definidos (D). `protocol.md` actualizado, ADR 0012.
- `proto`: el parser de descriptores expone `outputs` y `output_for`.
- Hub: `Router::on_output` (solo agentes, descarta duplicados/atrasados por `seq`, solo informes declarados y de páginas estándar, nunca HID++), último estado por agente, se escribe solo el del activo y se reaplica al activar (ceros si nunca mandó) y al reconectar el receptor; 50/s por interfaz. `HidrawWriter`: hilo por interfaz, cola de 16, `write()` con el número de informe delante (0 sin IDs). El servidor lee datagramas de los nodos.
- `agent-core`: `HidSink::take_outputs`, cada informe va dos veces (30 ms), último estado reenviado al empezar cada sesión.
- Linux: `UHID_OUTPUT` y `SET_REPORT` de salida → hub. Gadget: el hilo `hidg-out` reenvía en vez de descartar. macOS: Karabiner 8.6.0 **no** notifica LEDs al cliente; el agente lee el eco del driver (página 0x08) abriendo el teclado virtual con IOHIDManager, como Karabiner-Elements. Necesita **Monitorización de entrada** para `/usr/local/bin/rawkvm-agent` (`macos-instalacion.md` §3).
- Formatos contrastados con el código: `hidraw.rst`/`hidraw.c`/usbhid (byte 0 en escrituras sin IDs), `uhid.h`/`uhid.c`/`hid-core.c`/`hid-input.c` (LEDs por `UHID_OUTPUT`), `f_hid.c` (read con endpoint OUT), Karabiner-DriverKit-VirtualHIDDevice v8.6.0 y Karabiner-Elements.
- Verificado: fmt, clippy `-D warnings` (macOS, Linux con zig, Windows gnu), tests del workspace (router: filtro por activo, longitud, HID++, reaplicación, límite; e2e agente → hub → escritor). **Sin probar en hardware**: Bloq Mayús en el Ubuntu, el Windows (gadget) y el Mac, y que el LED siga al PC activo. Hay que redesplegar todo a la vez (v4).

## Fase 3 B: transporte HID++ y CLI de perfil de SO (2026-10-23, VS Code)

- `hub::hidpp`: correlación petición/respuesta como Solaar y el kernel (B1..B3, más B4 en 0xB5), errores 1.0/2.0, software id 0x0C, 300 ms y 2 reintentos, transporte falso con guion para los tests. En el hub en marcha las respuestas a nuestras peticiones no se reenvían a los agentes; el resto de la interfaz HID++ igual que antes (`Router::hidpp()` para C y D).
- Ranuras por `0xB5` (`0x20+n-1`, `0x30+n-1`, `0x40+n-1`), ping, `getFeature`. Perfil de SO: **Solaar no implementa** `getHostOsVersion`/`setHostOsVersion` de 0x1815; sus números (7 y 8) y tipos (1 Windows, 3 Linux, 6 macOS) vienen de OpenLogi (ingeniería inversa de Logi Options+), sin verificar en hardware. Se añadió `MULTIPLATFORM` 0x4531 (Solaar «Set OS» y parche del kernel), que es lo que cambia Fn+P/Fn+O, y `DUALPLATFORM` 0x4530. ADR 0013.
- CLI: `--hidpp-list`, `--hidpp-set-os <ranura> <windows|macos|linux> [--hidpp-via ...]`, `--hidpp-call`, `--hidpp-trace`. Guía en `docs/hidpp-validacion.md`.
- Verificado: tests (7 de `hidpp`, con bytes de Solaar/kernel), clippy macOS y Linux, build aarch64-musl. **Pendiente: validación en la Pi** con el hub parado y el usuario mirando el Windows; según el resultado, bloque C o se queda ADR 0011.

## Fase 3 B validada en la Pi: el MX Keys cambia de perfil en caliente (2026-10-23 05:10)

- Hub fedb591 instalado aparte como `/usr/local/bin/rawkvm-hub-v4` (el servicio sigue con el hub v3 hasta el redespliegue conjunto). Con el servicio parado, `--hidpp-list --hidpp-trace`: ranura 1 MX Ergo (HID++ 4.5, HOSTS_INFO), ranura 2 K230 (dormido, error 1.0 0x09), ranura 3 **MX Keys** (HID++ 4.5, HOSTS_INFO 0x1815 con host actual 0 y SO 6 = macOS 27.0, `can set OS`; **MULTIPLATFORM 0x4531** modificable, plataformas Windows/macOS/iOS/Android, actual macOS), 4–6 vacías.
- `--hidpp-set-os 3 windows`: escribe por MULTIPLATFORM (plataforma 0); el teclado emite al instante la notificación de cambio (`11 03 10 00 00 00 03`) y la relectura confirma. Con el hub arrancado y `swap_alt_gui = false`, **el usuario confirma en el Windows que Alt y Win están en su sitio sin el parche**. `--hidpp-set-os 3 macos` (plataforma 1): notificación y relectura OK; **el usuario confirma que en el Windows vuelven a cruzarse y en el Mac Cmd/Option van bien**. Cambio en caliente, sin reconectar el teclado. HOSTS_INFO fn 8 no se ha probado (no hizo falta).
- Trazas en `fixtures/hidpp/` (`hidpp-list.txt`, `hidpp-set-windows.txt`, `hidpp-set-macos.txt`).
- Estado dejado: `hub.toml` con `swap_alt_gui = true` restaurado y el hub v3 en marcha (7 nodos). **Vía libre al bloque C** (perfil al conmutar por MULTIPLATFORM, con 0x1815 como alternativa) y al D. Al entregar C, redespliegue conjunto v4 (hub, gadget, agentes, widget) y retirada de `swap_alt_gui` del `hub.toml` de la Pi.
- Fase 3 A (LED) sigue sin probar en hardware: se probará con el redespliegue v4.

## Fase 3 C y D: disposición del teclado al conmutar y emparejamiento desde el widget (2026-10-23, VS Code)

- C (ADR 0014): hilo `receiver_ctl`, dueño del canal HID++. Lee las ranuras al abrir la interfaz y activa las notificaciones del receptor (`0x00` = `00 09 00`). En cada conmutación pone `MULTIPLATFORM` en la plataforma del SO del agente activo: Linux → Windows en el MX Keys, que no tiene plataforma Linux; `Other` → no tocar. No escribe si ya está; un dispositivo dormido la recibe al conectarse; los fallos se avisan una vez. `[hidpp] os_profile` (por defecto `true`) y `[[node_options]] os_profile = "windows"|"macos"|"linux"|"none"`. El hub avisa si `swap_alt_gui` sigue activo.
- D (ADR 0015): `ReceiverDevices` a widgets (ranura, nombre, tipo, PID, conectado, si admite disposición, disposición actual). `PairingStart`/`PairingStop`/`Unpair` solo de widgets: registro `0xB2` y notificaciones `0x41`/`0x4A`/`0x40`, con eventos `Started`, `DeviceFound`, `Paired`, `Failed` y `TimedOut`. `DeviceList` no cambia al emparejar (test).
- Widget: la pestaña Dispositivos lista los emparejados, con Olvidar (doble clic), Emparejar (30 s, cuenta atrás) y disposición actual. `proto::PairedDevice`: `hosts_info` pasa a `os_profiles` y se añade `online` (v4 aún sin desplegar).
- Tests: `receiver_ctl` reproduce las **trazas reales** de `fixtures/hidpp/` (lista de ranuras, Linux → Windows, escritura única, vuelta a macOS). Además: sesión de emparejamiento completa con cancelación, errores y silencio del receptor; teclado dormido que despierta; router (disposición por SO y por opción, órdenes solo de widgets, listas solo a widgets).
- Verificado: fmt, clippy `-D warnings` (macOS, Linux con zig, Windows gnu), tests del workspace y del widget, build del frontend sin avisos. **Sin probar en hardware**: el cambio al conmutar con el hub en servicio, el emparejamiento real y la pestaña Dispositivos. Al desplegar v4, quitar `swap_alt_gui` de `hub.toml`.

## Redespliegue v4 (Fase 3 A, C y D en hardware) (2026-10-23 11:35)

- Desplegado fbf5349 en todo: hub y gadget en la Pi (hashes comprobados, `swap_alt_gui` retirado de `hub.toml`, copia en `hub.toml.bak-v3`), agente en el Ubuntu, agente + helper de borde + widget en el Mac (carpeta `target/mac-v4`, instalados por el usuario con sudo), helper de borde en el Windows (`rawkvm-edge-windows-v4.zip`).
- Incidentes del despliegue: (1) el instalador del Mac aborta si no tiene al lado los plists de `deploy/macos` → el daemon de Karabiner quedó parado (`bootout` sin `bootstrap`) y el Mac sin teclado hasta reejecutarlo con la carpeta completa; conviene que `install.sh` compruebe los ficheros antes de tocar launchd. (2) La Pi se cayó a las 11:16 (sin ping; el Windows seguía encendido); reset manual del usuario, binarios íntegros al volver. Causa sin determinar. (3) El widget nuevo pierde el permiso de Red local al cambiar la firma ad hoc: hay que volver a concederlo.
- **Validado por el usuario**: LED de Bloq Mayús funciona en Mac, Windows y Ubuntu (bloque A); en el Mac el agente registra "watching the keyboard LEDs" tras conceder Monitorización de entrada por arrastre desde el Finder (el selector de Ajustes no entra en `/usr/local/bin`). El perfil del MX Keys cambia en cada cruce por borde en <50 ms (`keyboard layout set … want=MacOs platform=1` / `want=Linux platform=0` para el gadget; bloque C). Cruce por borde Windows↔Mac y Ubuntu operativo con los helpers v4.
- Detalle para VS Code: el nodo gadget se registra con `os=Linux` (corre en la Pi) y el hub lo mapea a plataforma Windows por la regla Linux=Windows; funciona, pero el gadget debería anunciar el SO del PC al que está enchufado o llevar `os_profile = "windows"` en la doc de instalación.
- Pendiente: permiso de Red local al widget y prueba de la pestaña Dispositivos (lista del receptor, emparejar/olvidar; bloque D).

## Cierre de la Fase 3 y encargo de robustez + Fase 4 (2026-10-23 12:25)

- Gadget 2f4923f en la Pi con `--host-os` por defecto Windows: el hub lo registra `os=Windows`. Carpeta `target/mac-v4` rehecha con el instalador que comprueba sus plists.
- Widget: con el permiso de Red local "activado" en cuatro entradas, la app nueva seguía sin ruta a la Pi; lanzada desde una shell conecta. Causa: concesión ligada a la firma ad hoc anterior. Queda en marcha lanzada desde la shell; arreglo de fondo (firma estable) encargado como R1.
- Journal de la Pi ahora **persistente** en la SD (`journald.conf.d/rawkvm.conf` + `var-log-journal.mount` bind sobre `/var/log.hdd/journal`, tras `orangepi-ramlog.service`); verificado. La caída de las 11:16 queda sin causa (journal volátil entonces).
- Emparejamiento desde el widget (bloque D): **pendiente de probar**, sin dispositivo Unifying libre.
- Brief de robustez y Fase 4 en `docs/fase4-plan.md`, orden del usuario: R (firma estable, journal en deploy, pequeños), F4.1 batería y DPI, F4.2 atajos configurables, F4.3 widget para Windows. Enviado a VS Code.

## Fase 4 en hardware: F4.1 validado, v6 en Pi y Ubuntu, R1 verificado (2026-10-23 16:25)

- **R1 (firma estable)**: `make-signing-identity.sh` crea "RawKVM Local"; agente, helper y widget del Mac firmados con certificado (`codesign -dr` muestra identifier + certificate leaf). Fallo corregido por el orquestador (fea2600): `install_signed` creaba la copia temporal en el `$TMPDIR` privado de root, intransitable para el usuario que firma; ahora en `/tmp`. Tras el cambio ad hoc → certificado hubo que conceder una última vez Red local (widget) y Monitorización de entrada (agente). La prueba de que sobreviven a una actualización es el despliegue v6.
- **F4.1 validado por el usuario** (v5, 7c2ab35): batería, deslizador de velocidad del MX Ergo y casilla Fn del MX Keys funcionan desde Dispositivos. Trazas reales: `fixtures/hidpp/mxergo-2205-0.txt` (POINTER_SPEED = 256, 1,00×), `mxkeys-40a3-0.txt`, `hidpp-list-3/4.txt`. Con los dos dispositivos cargando, BATTERY_STATUS devuelve nivel 0 (desconocido) y estado "recharging": el widget debe mostrar "cargando" sin porcentaje.
- Protocolo **v6** (778f633, atajos configurables) instalado en la Pi (hub y gadget) y en el Ubuntu (agente + extensión GNOME v2 con `unlock-dialog`; helper conectado). Mac y Windows pendientes del usuario: `target/mac-v6`, `rawkvm-edge-windows-v6.zip` y el widget de Windows (`~/Downloads/RawKVM-windows`, artefacto del CI de d67c7c5, F4.3).
- Comprobado antes: con el Ubuntu bloqueado la extensión v1 quedaba INACTIVE (`LockedHint=yes`); motivo del cambio a v2.
- Pendiente de validar: F4.2 (modificadores, 0 = anterior, Esc = liberar; columna Atajo y bandeja), extensión v2 con pantalla bloqueada (tras re-login), F4.3 en el GAMER, y que los permisos del Mac sobrevivan a la actualización v6.

## v6 desplegado en todo; R1 confirmado; widget de Windows instalado (2026-10-23 17:05)

- Mac v6: agente, helper y widget firmados con "RawKVM Local". **El widget conectó al instante tras la actualización sin volver a conceder Red local: R1 cumple su objetivo.** Monitorización de entrada del agente sigue "not permitted"; sin saber aún si se perdió con la firma o nunca se volvió a dar tras v5.
- Windows: helper v6 y widget (d67c7c5, F4.3) instalados por el usuario; el hub registra "GAMER" como Widget. Sin rechazos de protocolo en el hub.
- Pendiente de probar por el usuario: F4.2 (modificador, 0 = anterior, Esc = liberar, columna Atajo, bandeja), widget de Windows (bandeja con número, clics, cambio rápido, Monitores con 4 pantallas, Aplicar, autostart), extensión GNOME v2 (re-login y bloqueo), LED del Mac tras volver a dar el permiso.

## Batería sin nivel mientras carga (2026-10-23)

- Widget: el texto de la columna Batería se construye en `view.rs` (`BatteryView.text`, con test). Si el dispositivo da nivel 0 (desconocido, lo hace mientras carga) muestra solo "cargando", sin "?" ni porcentaje. Antes salía "?, cargando". Sin cambio de protocolo ni del hub: solo hay que reinstalar el widget. Sin probar en hardware.

## R1: Monitorización de entrada tras pasar a «RawKVM Local» (2026-10-23)

- Hallazgo del orquestador en el Mac: tras pasar el agente de ad hoc a «RawKVM Local», TCC conserva el requisito viejo y Monitorización de entrada queda denegada con el interruptor encendido; al ser un daemon, no hay diálogo. Solución única: `sudo tccutil reset ListenEvent`, volver a arrastrar el binario y `launchctl kickstart -k`. Documentado en `macos-instalacion.md` y ADR 0016. `install.sh` avisa de esos pasos cuando el binario anterior estaba firmado ad hoc y el nuevo no (`signed_adhoc` en `signing.sh`). Solo comprobado con `sh -n` y `signed_adhoc` sobre binarios locales; sin ejecutar `install.sh` con sudo. Confirmado por el orquestador: tras el reset, volver a arrastrar el binario y `kickstart`, el agente registra `watching the keyboard LEDs`. El aviso de `install.sh` se queda.

## Fase 4 validada en hardware (2026-10-23 17:30)

- **Confirmado por el usuario**: LED de Bloq Mayús en el Mac tras `tccutil reset ListenEvent` + volver a añadir el agente (R1); extensión GNOME v2 sigue activa con la pantalla bloqueada (F4.2 extra); widget de Windows en el GAMER (bandeja con número, clics, cambio rápido, Monitores y Aplicar, F4.3); atajos configurables (modificador, 0 = anterior, Esc = liberar, columna Atajo y bandeja, F4.2). Con F4.1 validado antes, **la Fase 4 queda completa en hardware** (hub y gadget 778f633 en la Pi; agentes y helpers v6; widgets d67c7c5 en Mac y Windows).
- Pendiente menor: redesplegar los widgets con a9918fa ("cargando" sin "?") en el próximo ciclo; emparejar desde el widget (D de la Fase 3) sin probar por falta de dispositivo libre.
- Limpieza: `TOKEN.txt` de `~/Downloads/RawKVM-windows` borrado en el Mac; el usuario debe borrar su copia en el Windows.

## Disposición del MX Keys tras un sondeo de batería fallido (2026-10-23)

- Fallo visto en la Pi (hub 778f633): un sondeo de batería falló con el MX Keys dormido y el hub lo marcó como desconectado sin avisar. No llegó ninguna `0x41`, así que dejó de cambiarle la disposición al conmutar Mac↔Windows (Alt/Win cruzadas).
- Corregido en `receiver_ctl.rs`:
  - Al conmutar se intenta también con los dispositivos desconectados.
  - El sondeo también lee los desconectados y vuelve a leer las ranuras dormidas en el arranque.
  - Un dispositivo que responde vuelve a conectado y recibe la disposición que se perdió.
  - Registro: warn al pasar a desconectado por el sondeo, info al volver.
- Test `a_missed_battery_poll_does_not_stop_the_layout_switches`. Revisión en ADR 0014.
- Sin cambio de protocolo; hay que redesplegar el hub. Sin probar en hardware.

## Fase 5.1: paquetes .deb del hub y del gadget, releases, versión 0.5.0 (2026-10-24)

- Versión **0.5.0** en todo el workspace y en el widget. `release.yml` publica una release al recibir una etiqueta `v*` (comprueba que la etiqueta coincide con la versión y añade `SHA256SUMS`). ADR 0021.
- Paquetes `rawkvm-hub` y `rawkvm-agent-gadget` (`arm64` y `armhf`) con `cargo-deb`. ADR 0022.
  - Instalan en `/usr/bin` y `/lib/systemd/system`.
  - `hub.toml` no va en el paquete: el hub lo crea al arrancar, y hay un ejemplo en `/usr/share/doc/rawkvm-hub/` comprobado por un test.
  - Las opciones del gadget van en `/etc/default/rawkvm-agent-gadget`.
  - Pasan solos de la instalación manual, conservando la configuración, los ficheros de ajuste y el estado.
  - `purge` borra solo lo de cada paquete y avisa de que los agentes tendrán que volver a emparejarse.
- `rawkvm-agent-gadget --pair`: empareja y sale, sin Ctrl+C. El token se pide sin eco o se lee con `--token-file`. Está en `agent-core` (`pairing.rs`) para los demás agentes; hay test contra el hub real.
- CI:
  - `debs.yml`: compila, pasa lintian (solo avisos) y prueba instalación, migración, reinstalación, `remove`, `purge` e instalación limpia en un contenedor Debian `arm64` con QEMU.
  - Job `shellcheck` para todos los scripts.
  - Los jobs `hub-aarch64` y `gadget` de `ci.yml` desaparecen: `debs.yml` los sustituye.
- Docs: `docs/instalacion.md` (página de entrada), `README.md`, y `hub-linux-instalacion.md` y `gadget-instalacion.md` con el paquete primero y la instalación manual como anexo.
- Repositorio **público** desde el 2026-10-24 (decisión del usuario, historial revisado; ADR 0021). La descarga principal es `curl` anónimo de `releases/latest` comprobando `SHA256SUMS`, con `gh` y `scp` como alternativas. `releases/latest` da 404 hasta la etiqueta `v0.5.0`.
- Sin probar en hardware: la migración en la Pi real, con el servicio bajo systemd, el reenchufado del receptor y el montaje del journal; `--pair` del gadget contra el hub real.

## Fase 5.2: agente de Ubuntu, paquete .deb y `rawkvm-agent setup` (2026-10-24)

- `rawkvm-agent_0.5.0_amd64.deb` (glibc 2.35, Ubuntu 22.04 en adelante). Instala el binario, la regla udev de `/dev/uhid` con `uaccess` (el usuario de la sesión gráfica no necesita el grupo `input`), la unidad de usuario en `/usr/lib/systemd/user` y la extensión en `/usr/share`. El postinst quita los ficheros de `/etc` de la instalación manual y, al actualizar, reinicia los agentes en marcha. ADR 0023.
- `rawkvm-agent setup`, como el usuario: retira la instalación manual de `~`, ofrece los hubs que ve por mDNS, pide el token sin eco, empareja, activa el servicio de usuario y la extensión, y comprueba. `rawkvm-agent --check` solo lee y da una línea por pieza. `--pair` como en el gadget.
- `deploy/linux/setup-linux.sh`: `curl … | sh` descarga la release, comprueba `SHA256SUMS`, instala con sudo y lanza `setup`.
- CI: build del agente, lintian y prueba de instalación en Ubuntu 22.04 y 24.04 (binario, `--check` sin configuración, `setup` rechaza root, `remove`/`purge`).
- Docs: `ubuntu-instalacion.md`; `instalacion.md`, `fase0-validacion.md` y `gnome-plan.md` remiten a ella.
- Sin probar en el Ubuntu real: `setup` (mDNS, systemd de usuario, gsettings, migración desde `~/.local`), `uaccess`, el reinicio al actualizar y `--check` con la extensión. En 192.168.1.157 el `apt install` lo hace el usuario; `setup` puede hacerlo el orquestador por SSH con la sesión gráfica abierta.

## Fase 5.3: instalador de Windows, `setup.ps1` (2026-10-24)

- `deploy/windows/setup.ps1`, sin administrador. Instala el helper de borde y el widget en `%LOCALAPPDATA%\Programs\RawKVM`, sin la marca de descargado de Internet, con las claves `Run` de HKCU (`RawKvmEdge`, `RawKVM`) y la entrada en el menú Inicio. Retira la instalación manual anterior. Empareja una vez y comprueba. ADR 0024.
- Origen del zip: `-From`, la carpeta del script, o la release (`Invoke-WebRequest` anónimo; `gh` si falla), comprobado con `SHA256SUMS`.
- Emparejamiento:
  - `setup.ps1` pide el hub (Enter: mDNS) y el token sin mostrarlo (`Read-Host -AsSecureString`, fichero temporal que borra al terminar), o lo toma de `-TokenFile`. No tiene `-Token`.
  - El helper empareja con `rawkvm-edge --pair`: conecta una vez como widget con un id desechable, lee la lista de nodos y elige solo el agente de Windows (el gadget). Si hay varios, `setup.ps1` pregunta; `-ForNode` lo fuerza. Sin cambio de protocolo.
  - El widget toma el emparejamiento de `edge.toml` al arrancar (`adopt_pairing`).
- `setup.ps1 -Check` y `rawkvm-edge --check`: una línea por pieza (versión, configuración, `for_node`, hub, widget, inicio de sesión, procesos, cortafuegos). `uninstall.ps1` (`-Purge` borra también la configuración y los logs).
- `agent_core::setup` reúne lo común de los `setup` y `--check` de Linux y Windows.
- Los scripts del driver VHF pasan a `install-driver.ps1` / `uninstall-driver.ps1` y su guía a `windows-driver-vhf.md`. `install-edge.ps1`, `uninstall-edge.ps1` y `windows-borde.md` desaparecen: todo está en la nueva `windows-instalacion.md`.
- CI:
  - workflow `windows-zip`: clippy y tests del helper y del widget, el zip, y una prueba de instalación y desinstalación en el runner;
  - `release.yml` adjunta el zip;
  - job `psscriptanalyzer` en `ci.yml`.
- Sin probar en GAMER: la descarga, el emparejamiento real (mDNS, token oculto, elección del gadget), que el widget tome el emparejamiento, el arranque al iniciar sesión sin SmartScreen y `-Check` con todo en marcha.

## Convenciones

Ver sección 12 del handoff: código y comentarios en inglés, docs de usuario en castellano, `cargo fmt` y `cargo clippy -D warnings` limpios, un ADR por decisión nueva.

## Encargos tras el redespliegue v4 (2026-10-23)

- `deploy/macos/install.sh` comprueba que existen (y son válidos, `plutil -lint`) los dos plists antes de cualquier `bootout`; si falta alguno sale sin tocar launchd. Sin verificar con sudo en el Mac (solo `sh -n` y `plutil` local).
- Gadget: nueva opción `--host-os windows|macos|linux` (por defecto `windows`): el nodo anuncia en Hello el sistema del PC enchufado, no el de la placa, y el hub elige la disposición por eso. Avisa si `--host-os linux` va sin `--no-hidpp`. Sin cambio de protocolo; requiere redesplegar el binario del gadget para que el hub vea `os=Windows`.

## Bloque R (robustez) (2026-10-23)

- **R1, firma estable en el Mac** (ADR 0016):
  - `deploy/macos/make-signing-identity.sh` crea una vez el certificado autofirmado «RawKVM Local» en el llavero de inicio de sesión y lo hace de confianza para firma de código (sudo).
  - `signing.sh` es compartido por `install.sh`, `install-edge.sh` y el nuevo `install-widget.sh` (que instala en `~/Applications` y relanza). Firma con esa identidad, o ad hoc avisando.
  - Identificadores fijos: `cc.olitec.rawkvm.agent` y `cc.olitec.rawkvm.edge`.
  - El artefacto del widget incluye los tres scripts.
  - Verificado en el Mac de desarrollo, en un llavero temporal: `codesign` firma con un certificado así y el requisito designado pasa a `certificate leaf = H"…"`; el camino ad hoc de `signing.sh` también funciona.
  - **Sin verificar**: `make-signing-identity.sh` en el llavero real (pide sudo y el diálogo «Permitir siempre»), y que Red local y Monitorización de entrada sobrevivan a una reinstalación.
- **R2, journal persistente** (ADR 0017):
  - `deploy/hub-linux/install.sh` instala `journald-rawkvm.conf` y, si detecta `armbian-ramlog` u `orangepi-ramlog`, `var-log-journal.mount`, generado desde `.mount.in`.
  - Lo que mirar tras una caída está en `docs/hub-linux-instalacion.md`.
  - **Sin ejecutar en la Pi.** La unidad está escrita de nuevo: hay que compararla con la que dejó el orquestador.
- **R3**:
  - `install-edge.sh` comprueba su plist y `signing.sh` antes de tocar launchd.
  - Monitorización de entrada se da arrastrando desde el Finder (`open /usr/local/bin`).
  - El paquete del helper de Windows ya lo produce CI (job `edge-windows` de `ci.yml`, artefacto `rawkvm-edge-windows`). En `windows-borde.md` está cómo bajarlo con `gh`.

## Fase 4.1: batería y ajustes por HID++ (2026-10-23)

- Ajustado a lo que anuncian los dispositivos reales (ADR 0018): batería por `BATTERY_STATUS` 0x1000, velocidad del puntero por `POINTER_SPEED` 0x2205 (MX Ergo) y F1…F12 por 0x40A3 (MX Keys). No hay 0x1004 ni 0x2201. `BACKLIGHT2` queda fuera.
- **Protocolo v5**: `PairedDevice.battery`, `pointer_speed` y `fn_swap`; `SetPointerSpeed` y `SetFnSwap`, solo de widgets.
- **Hub**:
  - `hidpp.rs` lee las tres features en la lectura de cada ranura.
  - `receiver_ctl` lee la batería al conectar el dispositivo, cada 5 minutos y en los eventos de batería, y aplica los dos ajustes.
  - `--hidpp-list` muestra las tres.
- **Widget**: columna Batería (⚠︎ con 10 % o menos) y bloque «Ajustes de los dispositivos» (deslizador de velocidad con 1×, casilla de Fn).
- **Verificado**:
  - fmt, clippy `-D warnings` (macOS, Linux con zig, Windows gnu), tests del workspace y del widget, `npm run build`.
  - Test nuevo `batteries_pointer_speed_and_fn_swap`, con las respuestas reales de batería (`bat-slot*-1000.txt`).
- **Sin verificar en hardware**:
  - lectura y escritura de 0x2205 y 0x40A3 (los valores del test son inventados);
  - el evento de batería;
  - si el dispositivo guarda los ajustes al apagarse.
  - Para grabarlos: `--hidpp-call 1 0x2205 0`, `--hidpp-call 3 0x40a3 0 00` y `--hidpp-list` con `--hidpp-trace`.

## Fase 4.2: atajo configurable (2026-10-23)

- **Protocolo v6** (ADR 0019): `HubSettings.hotkey` es `Hotkey { enabled, modifier: Ctrl|Shift|Alt|Gui, previous, release_keys }`.
- **Hub**:
  - `hotkey::Detector` devuelve `Agent(n)`, `Previous` (0, el agente más reciente según `history`) o `ReleaseKeys` (Esc, `release_everywhere`). Las teclas que lo completan no se reenvían.
  - Claves nuevas en `[switching]`: `hotkey_modifier`, `hotkey_previous` y `hotkey_release_keys`.
  - El `settings.toml` de v5 (`hotkey = bool`) se sigue leyendo.
- **Widget**:
  - Opciones: tecla y dos casillas, con aviso para Alt y Cmd/Win en Windows.
  - Hub: columna «Atajo».
  - Barra: una línea con el atajo.
- **Verificado**:
  - fmt, clippy `-D warnings` (macOS, Linux con zig, Windows gnu), tests del workspace y del widget, `npm run build`.
  - Tests nuevos: `previous_and_release_keys`, `other_modifiers` y `hotkey_previous_and_release_keys` (router, con informes boot), y la lectura del `settings.toml` de v5.
- **Sin verificar en hardware**: el atajo con cada modificador en los tres PCs, y que el menú de la barra y la columna se actualicen al cambiarlo.
- Extensión de GNOME v2: `session-modes` con `unlock-dialog`, para que el helper de borde de Linux siga activo con la pantalla bloqueada (antes gnome-shell la dejaba INACTIVE). Hay que reinstalarla y cerrar sesión una vez. Sin verificar en el Ubuntu.

## Fase 4.3: widget para Windows (2026-10-23)

- El mismo `widget/` compila para Windows (ADR 0020). Sin cambio de protocolo.
- **Bandeja**: cuadro azul con el número activo, dibujado en tiempo de ejecución (`tray_icon.rs`). Clic izquierdo abre la ventana, clic derecho el menú.
- **Ficheros**: config y log en `%LOCALAPPDATA%\rawkvm\`. Inicio automático con la clave `Run` de HKCU.
- **Una sola instancia**, en los dos sistemas.
- **Textos**: «Este PC» en Windows.
- **CI**: job `windows` en `widget.yml`, artefacto `RawKVM-windows` (`RawKVM.exe`, LEEME y COMMIT).
- **Verificado en el Mac**: clippy `-D warnings` del widget para macOS y para `x86_64-pc-windows-gnu` (solo comprobación, sin enlazar), tests (incluido `tray_icon`) y `npm run build`. El enlazado y el exe los hace el job de CI.
- **Sin verificar en el GAMER**: bandeja y número, clics, cambio rápido, pestaña Monitores con las 4 pantallas y Aplicar, inicio automático, aviso del cortafuegos.

## Incidencia: el perfil del teclado deja de cambiar (2026-10-23 20:00)

- **Síntoma**: a veces, al cruzar Mac ↔ Windows, el MX Keys se queda con el perfil anterior (Alt y Win cruzadas). El cruce de puntero y teclado sí funciona.
- **Evidencia** (journal de la Pi, UTC): hasta las 00:53 cada `switched` Mac ↔ Windows va seguido de `keyboard layout set`; desde las 00:55:59, tres cruces sin él y sin ningún aviso de `receiver_ctl`. Tras reiniciar el hub (01:01), el primer cruce vuelve a aplicar `want=MacOs`.
- **Causa** (`receiver_ctl.rs`): el sondeo de batería cada 5 min marca el slot `online=false` si el teclado está dormido (solo a nivel debug); `apply()` no hace nada con un slot offline; el sondeo no vuelve a tocar los slots offline; la única recuperación es una notificación 0x41 de enlace, que no llega porque para el receptor el enlace nunca cayó. Trampa de un solo sentido.
- **Remedio inmediato**: `sudo systemctl restart rawkvm-hub` en la Pi (el survey lo devuelve a online).
- **Encargado a VS Code**: reintentar el cambio de perfil aunque el slot esté offline, volver a sondear los slots offline, avisar a nivel warn/info y test. Sin cambio de protocolo.
- En la Pi queda un drop-in `rawkvm-hub.service.d/debug.conf` con `RUST_LOG=info,rawkvm_hub::receiver_ctl=debug`; quitarlo al desplegar la corrección.
- **Corrección desplegada** (2026-10-23 20:09): hub e148526 en la Pi (`sha256 eefb9f8e…`), drop-in de debug retirado, servicio activo. Un slot que no contesta al sondeo de batería ya recibe el cambio de perfil en cada cruce y vuelve a online cuando contesta; aviso a nivel warn en el journal. Pendiente de ver en uso real que no se repite.

## Fase 5 encargada: instaladores desde cero (2026-10-24)

- Encargo del usuario: instalador completo para Mac (Karabiner, permisos, agente, edge, widget), para Windows (edge + widget), para Ubuntu (agente + extensión) y paquete `.deb` para el hub y el gadget.
- Brief en [fase5-plan.md](fase5-plan.md): GitHub Releases con `SHA256SUMS` y versión 0.5.0, `.deb` con `cargo-deb` (hub, gadget, agente Linux), `rawkvm-agent setup` en Ubuntu, `setup.ps1` en Windows con `-ForNode` automático, `setup.sh` en macOS idempotente y reanudable (Karabiner, firma local, emparejamiento con `--pair`, permisos TCC guiados), `--pair` y `--check` en todos los agentes, `docs/instalacion.md` como entrada única.
- Orden: hub/gadget `.deb` → Ubuntu → Windows → macOS. Validación en hardware: la Pi y el Ubuntu los pruebo yo; el GAMER y el Mac con el usuario.
- **Repositorio público** (2026-10-24): el usuario decidió hacerlo público para que los instaladores descarguen de las releases sin credenciales. Revisado el historial antes: el token real del hub no aparece en ningún commit, sin claves de API ni certificados; los únicos 64 hex fuera de `Cargo.lock` son ejemplos de prueba. Quedan en la documentación direcciones de la red local (192.168.1.x) y nombres de máquina, aceptados. La API de GitHub responde anónimamente; `releases/latest` dará 404 hasta la primera etiqueta `v0.5.0`.

## Fase 5.1 validada en la Pi: .deb del hub y del gadget (2026-10-24 12:50)

- `apt-get install ./rawkvm-hub_0.5.0_arm64.deb ./rawkvm-agent-gadget_0.5.0_arm64.deb` (CI e36ce7d, run 40117083614) sobre la instalación manual, sin ningún diálogo de dpkg.
- Migración: borrados `/usr/local/bin/rawkvm-{hub,agent-gadget}` y las unidades de `/etc/systemd/system`; unidades nuevas en `/usr/lib/systemd/system`, `enabled` y `active`; conservados `hub.toml` (con `[[node_options]]`), los `.bak`, `agent.toml`, `/var/lib/rawkvm` (certificado, clave, layout, nodes); `/etc/default/rawkvm-agent-gadget` creado sin opciones (el GAMER usa el valor por defecto). El montaje del journal siguió activo sin reinicio de journald.
- Hub `0.5.0 (e36ce7d)`: DJ apagado, 3 interfaces, layout de 4 enlaces; gadget 0.5.0 conectado y `bound` con 3 interfaces; los seis nodos (dos agentes, dos widgets, dos edges) se registraron a los 7 s sin reemparejar; el Mac volvió a activo y el perfil del teclado se aplicó.
- Sin verificar: `rawkvm-agent-gadget --pair` contra el hub real (el gadget ya estaba emparejado), el paquete armhf y `release.yml` (se probará con la etiqueta v0.5.0 al cerrar la fase).
- El drop-in de debug ya no existía (retirado el 23 a las 20:09), así que esa parte de la migración no se ejercitó.

## Incidencia: la Pi se apaga a los pocos segundos de arrancar (2026-10-24 20:00)

- El usuario reinició la Pi (`reboot`). Desde entonces, en cada arranque el hub y el gadget levantaban, los seis nodos se registraban y el perfil del teclado cambiaba, y a los 15-40 s la placa entera desaparecía de la red: sin mensajes de apagado, journal «corrupted or uncleanly shut down», reloj retrocedido a la hora guardada. Tres arranques seguidos iguales (20:00, 20:12, 20:17).
- Diagnóstico: corte de alimentación, no software. Con un alimentador de pared en el USB‑C OTG la placa aguantó sin perder un ping (aunque sin GAMER, gadget `not attached`). En **otro puerto USB del GAMER** también aguanta (desde las 20:28, gadget `configured`). El puerto que usaba hasta ahora dejó de dar corriente suficiente al arrancar con Wi‑Fi y receptor; no se sabe por qué cambió.
- Revisión tras los cortes: ext4 sin errores, `commit=5` ya en `/etc/fstab`, `dpkg -V` de los dos paquetes limpio, hub sin avisos, temperatura 67 °C.
- Consecuencias: (1) el usuario pide un apagado limpio del hub desde el widget y al apagar el PC que lo alimenta → **bloque 3b de la fase 5** en [fase5-plan.md](fase5-plan.md) (protocolo v7). (2) Sigue vigente la recomendación de `gadget-instalacion.md`: alimentación propia por el conector de 40 pines; el 3b protege la SD pero no evita que el hub se vaya con el PC.

## Fase 5.3b: apagado limpio del hub, protocolo v7 (2026-10-25)

- `Control::Shutdown` (widget → hub) y `Control::ShuttingDown { reason }` (hub → todos); `PROTO_VERSION` 7. ADR 0025.
- Hub, al recibir `Shutdown`:
  - solo de un widget; registra un `warn` con quién lo pidió;
  - libera las teclas en todos los agentes y avisa a todos;
  - `poweroff::serve`: 0,5 s, `systemctl stop rawkvm-agent-gadget` (máx. 3 s), `sync`, `systemctl poweroff`;
  - si falla, `error` en el journal y sigue en marcha.
- Agentes, helpers y widgets, con `ShuttingDown`:
  - cierran la sesión y reintentan cada 30 s hasta que el hub vuelve (`agent_core::Reconnect`, común a los tres);
  - log: `the hub is powering off (…)` y `hub off; reconnecting in 30.0 s`.
- Widget:
  - **Apagar hub** con confirmación;
  - estado «Hub apagado», con el aviso de esperar al LED;
  - bandeja «hub apagado».
- Windows: opción **Apagar el hub cuando este PC se apague** (desactivada por defecto; `power_off_hub_with_pc` en `widget.toml`).
  - Una ventana oculta en su propio hilo atiende `WM_QUERYENDSESSION`/`WM_ENDSESSION`; ignora `ENDSESSION_LOGOFF`.
  - Envía `Shutdown` y espera la confirmación o 3 s.
  - Registra la hora y el `lParam` de cada aviso, también con la opción desactivada.
  - Suspender queda fuera.
- Despliegue: todo a la vez (hub y gadget en la Pi, agentes de Mac y Ubuntu, helper y widget del GAMER con `setup.ps1`).
- Sin probar en hardware:
  - `systemctl poweroff` desde el servicio en la Pi;
  - los avisos de Windows en un apagado y un reinicio reales (con Inicio rápido incluido);
  - el margen que da el GAMER entre el aviso a las aplicaciones y el corte de los 5 V.

## Fase 5.3: Microsoft Defender bloquea el helper (2026-10-25)

- En el GAMER, el despliegue v7 funcionó salvo `rawkvm-edge.exe` 0.5.0 (MSVC, sha256 `8f1ecc46…`): `Trojan:Win32/Bearfoos.A!ml`, falso positivo de la detección automática. El widget (con recursos de versión) y el helper v6 (mingw) no se detectaron.
- Mitigación (ADR 0024):
  - recurso de versión, manifiesto e icono en el helper (`build.rs`, solo MSVC; el CI comprueba que están);
  - `setup.ps1` y `-Check` detectan el bloqueo y explican cómo avisar a Microsoft o permitirlo;
  - documentado en `windows-instalacion.md`.
- Pendiente de ver en el GAMER si el binario nuevo pasa.

## Fase 5.3b validada en la Pi; journal perdido en los apagados limpios (2026-10-25)

- «Apagar hub» desde el widget del Mac (Pi en alimentador): el Mac registró `the hub is powering off (requested from Air-Javier)`, la Pi se apagó de forma ordenada (`last -x`: shutdown) unos 6 s después, y al darle corriente el Mac reconectó solo. La unit endurecida no impide `systemctl poweroff`.
- Hallazgo, encontrado por el orquestador: el `rsync --delete` de `orangepi-ramlog` al parar pisaba el journal de la SD con la copia de la zram (ADR 0017 corregido).
  - Corregido a mano en la Pi (excludes en `/etc/default/orangepi-ramlog`).
  - En el paquete, el postinst del hub los pone y `postrm purge` los quita (ADR 0022). Probado en el contenedor del CI.
  - Pendiente: confirmar con el siguiente apagado limpio que `journalctl -b -1` lo conserva.


## Despliegue v7 y validación de 5.3 y 5.3b en hardware (2026-10-24 21:10 – 22:05)

- **Todo a la vez por el protocolo 7** (99a1485): Pi con `apt-get install` de los dos `.deb` (hub y gadget 0.5.0, run 40152167206); Mac con `install.sh`, `install-edge.sh` (sudo del usuario) e `install-widget.sh` (artefacto `rawkvm-widget-macos`); Ubuntu con el `.deb` amd64 (apt del usuario) y `rawkvm-agent setup` por SSH; GAMER con el zip de `windows-zip` y `setup.ps1`. Nueve nodos registrados en el hub al final.
- **Bloque 2 validado en el Ubuntu real** (26.04, GNOME Shell 50): `setup` retiró la instalación manual de `~`, conservó el emparejamiento, activó el servicio de usuario y la extensión empaquetada; `--check` todo `ok`. Funciona por SSH con la sesión gráfica abierta.
- **Bloque 3 validado en GAMER**: `setup.ps1` retiró la instalación manual, el widget tomó el emparejamiento del edge sin pedir token, `-ForNode` automático eligió el gadget, autoarranque y procesos `ok`, `-Check` completo en verde. **Incidencia Defender**: el `rawkvm-edge.exe` de 99a1485 (MSVC, sin recursos) fue bloqueado como `Trojan:Win32/Bearfoos.A!ml` (heurística ML, falso positivo); el usuario lo permitió en el historial de protección para seguir. Corrección f28de57 (recurso de versión, manifiesto e icono en el edge; `setup.ps1` reconoce el bloqueo y explica qué hacer; ADR 0024): el edge nuevo (sha256 4799eade…) **no dispara Defender** en el mismo PC y se registró como `GAMER (edge)`.
- **Bloque 3b, botón «Apagar hub»** (Pi en alimentador): el Mac registró «the hub is powering off (requested from Air-Javier)» y «hub off; reconnecting in 30.0 s»; la Pi dejó de responder 6 s después; `last -x` muestra `shutdown system down`, apagado ordenado; al dar corriente arrancó limpia y los nodos reconectaron solos. La unidad endurecida (`ProtectSystem=strict`, `NoNewPrivileges`) no impide `systemctl poweroff`. LED en la Orange Pi Zero 2W: el **verde** deja de parpadear al terminar; el **rojo** es alimentación y sigue mientras haya cable (el texto del widget debe decir «cuando el LED verde deje de parpadear»).
- **Hallazgo: ramlog borra el journal en los apagados limpios.** `/var/log` es zram (`orangepi-ramlog`); al arrancar copia `/var/log.hdd` a RAM (journal incluido) y luego nuestro `var-log-journal.mount` tapa `/var/log/journal`; al apagar, el mount se desmonta antes de que pare ramlog y `rsync --delete` pisa el journal del disco con la copia vieja. Por eso el journal del arranque apagado desde el widget no existe y el siguiente dice «system.journal … uncleanly shut down». Corregido a mano en la Pi: `XTRA_RSYNC_FROM=(--exclude journal)` y `XTRA_RSYNC_TO=(--exclude journal)` en `/etc/default/orangepi-ramlog` (copia `.bak-rawkvm`); pendiente comprobar en el próximo apagado limpio y meterlo en el postinst del hub (encargado a VS Code).
- **Sin medir todavía**: el margen entre el aviso de apagado de Windows y el corte de los 5 V (prueba 7), porque la Pi sigue en el alimentador: en el cable de GAMER muere al enumerar el gadget (ver incidencia de las 20:00). Requiere alimentación por el conector de 40 pines o probar de nuevo en el puerto que aguantó.

## Fase 5.4: instalador de macOS, `setup.sh` (2026-10-25)

- `deploy/macos/setup.sh`, como usuario (sudo solo donde hace falta), idempotente y reanudable desde `/usr/local/share/rawkvm`. ADR 0026.
  - Programas desde `--from`, la carpeta del script o la release, con `SHA256SUMS`.
  - Karabiner-DriverKit-VirtualHIDDevice 8.6.0 fijado (SHA-256 y firma del autor), con la activación y la espera a la aprobación; si macOS pide reiniciar, sale con 2.
  - Identidad «RawKVM Local».
  - Agente y emparejamiento con token oculto (`rawkvm-agent --pair --token-file`).
  - Monitorización de entrada guiada: panel, Finder con el binario, espera en el log y `tccutil reset` con aviso.
  - Helper, y widget con arranque al iniciar sesión y Red local guiada.
  - Comprobación final.
- `--check`: una línea por pieza. `rawkvm-agent --check` y `rawkvm-edge --check` nuevos; `agent_core::setup::current_run` lee solo la ejecución actual del log.
- `uninstall.sh` completo (`--purge`, `--karabiner`; nunca Karabiner por defecto).
- El widget de macOS toma también el emparejamiento del helper.
- CI: workflow `macos-tar` (tarball y prueba de `setup.sh`/`uninstall.sh` en macos-14); `release.yml` lo adjunta.
- Probado en este Mac sin instalar nada: `setup.sh --check` con los binarios nuevos (`RAWKVM_BIN`), `rawkvm-agent --check` y `rawkvm-edge --check` contra los logs reales.
- Sin probar: la instalación desde cero (la hace el orquestador con el usuario: `uninstall.sh --purge` y `setup.sh`).
