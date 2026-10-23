# Robustez y Fase 4: batería y DPI, atajos configurables, widget para Windows

Brief del orquestador para la sesión de VS Code (2026-10-23). Estado de partida: Fase 3 completa y
validada en hardware (protocolo v4, fbf5349 + 2f4923f): LED de Bloq Mayús en los tres PCs, perfil del
MX Keys por MULTIPLATFORM en cada cruce, gadget con `--host-os`, lista de dispositivos del receptor en
el widget. El emparejamiento desde el widget queda **pendiente de probar** (sin dispositivo libre).

Orden fijado por el usuario: **R** (robustez), **F4.1** (batería y DPI), **F4.2** (atajos),
**F4.3** (widget para Windows). Cada bloque se entrega por separado con fmt, clippy `-D warnings`,
tests, docs en castellano, un ADR por decisión nueva y nota en `STATUS.md`. Cualquier cambio de
`proto` bumpea la versión una sola vez por bloque (postcard no es autodescriptivo: añadir un campo ya
rompe), y el orquestador redespliega todo a la vez.

## R. Robustez (lo que ha salido en el despliegue v4)

### R1. Firma estable del widget y los binarios del Mac

Problema: la `RawKVM.app` va firmada ad hoc y cada versión tiene otro hash, así que macOS no reconoce
la app como la misma y la concesión de **Red local** deja de aplicarse (síntoma: `No route to host`
hacia la Pi mientras el agente del mismo Mac conecta; lanzada desde una shell con permiso, conecta).
Lo mismo amenaza a **Monitorización de entrada** del agente (LED) en cada reinstalación.

Diseño propuesto:
- `deploy/macos/make-signing-identity.sh`: crea una vez, en el llavero de inicio de sesión del
  usuario, un certificado autofirmado de firma de código "RawKVM Local" (`openssl req` con EKU
  codeSigning + `security import` + `security add-trusted-cert -r trustRoot -p codeSign`, este último
  con sudo); idempotente.
- `install.sh`, `install-edge.sh` y la instalación del widget (`docs/widget-instalacion.md`, y un
  `install-widget.sh` nuevo que copie la `.app` a `~/Applications` y la relance) firman con esa
  identidad si existe (`codesign --force --deep --sign "RawKVM Local"`), y ad hoc si no, avisando.
  El requisito designado pasa a ser por certificado y las concesiones sobreviven a las versiones.
- CI sigue produciendo artefactos ad hoc; la firma estable es local.
- Verificación: `codesign -dr - RawKVM.app` muestra el requisito por certificado; tras reinstalar
  una versión nueva, el widget conecta sin tocar Ajustes. ADR.

### R2. Caída de la Orange Pi (2026-10-23 11:16)

Sin causa conocida: el Windows que la alimenta siguió encendido; sin ping ni ARP; reset manual. El
journal era volátil (Armbian/Orange Pi montan `/var/log` en zram), así que no quedó nada.

Hecho ya por el orquestador en la Pi: `/etc/systemd/journald.conf.d/rawkvm.conf`
(`Storage=persistent`, `SystemMaxUse=64M`) y la unidad `var-log-journal.mount` (bind de
`/var/log.hdd/journal` sobre `/var/log/journal`, `After=orangepi-ramlog.service`). Verificado:
`journalctl --header` escribe en la SD y el historial de hoy sigue ahí.

Encargo: llevar eso a `deploy/hub-linux/` (fichero del drop-in, la unidad detectando el nombre del
servicio ramlog, `armbian-ramlog` u `orangepi-ramlog`, y su instalación en `install.sh`), y un
`rawkvm-hub --diagnose` o sección en `hub-linux-instalacion` con lo que mirar tras una caída
(`journalctl -b -1`, `dmesg` del arranque anterior). Sin watchdog de hardware por ahora.

### R3. Pequeños

- El instalador del Mac ya comprueba sus plists (2f4923f). Añadir lo mismo a `install-edge.sh`.
- `docs/macos-instalacion.md`: el selector de Ajustes no entra en `/usr/local/bin`; documentar el
  arrastre desde el Finder (`open /usr/local/bin`) como método principal.
- El `rawkvm-edge-windows-*.zip` lo monta el orquestador a mano; que el workflow `windows.yml`
  produzca ese paquete (exe + ps1 + LEEME + COMMIT) como artefacto.

## F4.1. Batería y DPI en el widget

- Transporte HID++ del hub ya existe (`hub::hidpp`, `receiver_ctl`). Features: `UNIFIED_BATTERY`
  0x1004 (MX Keys, MX Ergo: nivel por tramos y, si lo hay, porcentaje; estado de carga) con
  `BATTERY_STATUS` 0x1000 como alternativa; `ADJUSTABLE_DPI` 0x2201 (MX Ergo: lista o rango de DPI,
  `getSensorDpi`/`setSensorDpi`). Formatos contrastados con Solaar `hidpp20` y grabados en
  `fixtures/hidpp/` con `--hidpp-trace` (el orquestador los captura en la Pi si hace falta).
- Hub: sondeo de batería al conectar el dispositivo (notificación 0x41) y cada 5 min; evento de
  batería 0x1004 por notificación si el dispositivo lo emite. Nunca en el camino de los informes.
- Protocolo: `PairedDevice` gana `battery: Option<Battery { percent: Option<u8>, level: Level,
  charging: bool }>` y `dpi: Option<Dpi { current: u16, choices: Vec<u16> }>`; `Control::SetDpi
  { slot, dpi }` widget → hub. Un bump de versión.
- Widget, pestaña Dispositivos: batería con icono y porcentaje, DPI con selector; CLI
  `--hidpp-list` los muestra también.
- Verificación: valores coherentes con los de Logi Options en otro PC; cambiar el DPI del MX Ergo se
  nota al instante.

## F4.2. Atajos configurables

Hoy: Ctrl, Ctrl, 1..4 fijo (`hotkey: bool` en `HubSettings` y en `[switching]` del hub).

- `HubSettings.hotkey` pasa a `Hotkey { modifier: Ctrl|Alt|Gui|Shift, enabled }` (doble pulsación
  del modificador y después el dígito); `previous: bool` para "Ctrl, Ctrl, 0" = volver al anterior;
  `release_keys: bool` para "Ctrl, Ctrl, Esc" = Liberar teclas. Todo en el hub (`hotkey.rs`), con
  tests sobre informes boot y DJ como los actuales, y sin que el dígito ni el Esc se reenvíen.
- Widget, pestaña Opciones: selector del modificador, interruptores, y el listado "Ctrl, Ctrl, N"
  por equipo en Hub/bandeja actualizado al modificador elegido.
- Persistido en `settings.toml` del hub. Bump de protocolo compartido con F4.1 si van juntos.

## F4.3. Widget para Windows

- El mismo `widget/` (Tauri 2 + Svelte) compilado para Windows: bandeja con el número activo, menú de
  cambio rápido, pestañas Hub/Dispositivos/Monitores/Opciones. Autostart con el plugin existente
  (clave Run de HKCU), config en `%LOCALAPPDATA%\rawkvm\widget.toml` con el mismo formato.
- CI: `widget.yml` gana un job `windows-latest` que produce `RawKVM-windows.zip` (exe + LEEME).
  Firma: ninguna (exe sin firmar aceptado por el usuario; SmartScreen avisará una vez).
- El helper de borde de Windows sigue aparte (proceso ligero, sin UI). No fusionarlos.
- Verificación en el GAMER: bandeja, cambio rápido, Monitores muestra las 4 pantallas y Aplicar
  funciona; `docs/widget-instalacion.md` con la sección de Windows.

## Fuera de alcance por ahora

Watchdog de hardware en la Pi, widget para Linux (GNOME tiene su extensión; el agente ya lleva el
helper), emparejamiento Bluetooth, soporte de otros receptores (Bolt).
