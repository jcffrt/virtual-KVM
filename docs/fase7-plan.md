# Fase 7: puente Bluetooth (`rawkvm-agent-bt`)

Brief para una sesión de código (VS Code). Decidido por el usuario el 2026-10-26 («añadamos BT de una vez»). Análisis y motivación: [puente-bluetooth-analisis.md](puente-bluetooth-analisis.md), capas **B** y **C**; la capa A es la fase 6 ([ADR 0030](adr/0030-varios-receptores-y-exposicion.md)), ya en producción. Lleva **un ADR** (0033). Lo que no esté decidido aquí, decídelo tú y déjalo en el ADR; no preguntes. Trabaja en un **worktree propio**: otra sesión puede estar en la copia principal.

## Objetivo

Un receptor del hub se expone a **un solo PC por Bluetooth**, sin agente ni cable en ese PC: el PC lo ve como un teclado y ratón Bluetooth normales. Se elige desde el widget con la exposición de la fase 6 («Exclusivo → *puente*») y el widget también empareja y olvida el PC.

Primer destino: la **Raspberry Pi 3B+** que hoy lleva el hub (Bluetooth Broadcom, BlueZ 5, Raspberry Pi OS Trixie arm64), con el agente en la misma placa conectado al hub por loopback. Debe funcionar igual en cualquier Linux con BlueZ (una Zero 2 W, una caja solo con dongle y Bluetooth: `listen = "127.0.0.1:4433"`, sin red).

## Hitos

1. **Prueba de concepto, primero y por separado (un día):** un binario que, con BlueZ, se anuncia como ratón Bluetooth con un descriptor fijo, se empareja con un portátil (el Mac o el GAMER) y mueve el puntero con informes sintéticos. Mide la tasa de informes y la latencia aproximada y **escríbelo en STATUS antes de seguir**. Si Windows o macOS no aceptan el dispositivo, para ahí y explica por qué.
2. El agente completo (abajo).
3. Widget y documentación.

## Diseño

### Perfil

**HID clásico (BR/EDR)** sobre BlueZ 5 con el crate `bluer` (D-Bus y sockets L2CAP, tokio). No BLE/HOGP: el clásico lleva el descriptor entero en el registro SDP y los dos canales son sockets normales.

- `ProfileManager1.RegisterProfile` con el registro SDP del HID: descriptor, clase de dispositivo `0x002540` (periférico, teclado y puntero), `HIDReconnectInitiate = true`, `HIDNormallyConnectable = true`, nombre «RawKVM (*nombre del hub*)».
- Sockets `SOCK_SEQPACKET` L2CAP en PSM `0x11` (control) y `0x13` (interrupción), en escucha; el anfitrión conecta tras emparejar. Informes de entrada: `0xA1` + report ID + datos por el canal de interrupción. `SET_REPORT`/`DATA` de salida (LEDs) llegan por los dos canales: se pasan al hub como `output`, como hace el gadget.
- `bluetoothd` con `--noplugin=input` para que no reclame el perfil; el `.deb` lo pone en un *override* de `bluetooth.service` y lo quita al desinstalar. `Discoverable`/`Pairable` solo mientras el widget lo pide (60 s); agente de emparejado `NoInputNoOutput`. **Un anfitrión vinculado a la vez**: emparejar otro olvida el anterior.
- **Reconexión iniciada por el dispositivo** cuando el anfitrión vinculado está presente y no conectado (vuelta de suspensión), con reintentos espaciados.

### Fusión de descriptores

Bluetooth admite **un descriptor por dispositivo**; el hub anuncia varios (teclado, ratón, y de cada receptor). El agente construye un descriptor único:

- toma los dispositivos del receptor exclusivo (y solo esos), asigna un **report ID** a cada colección de nivel superior (1, 2, 3…), inserta el ítem `Report ID` donde no lo hay y renumera los que ya lo tienen; antepone ese byte a cada informe de entrada y lo quita en los de salida. Usa `DescriptorInfo` de `proto` y la clasificación del [ADR 0031](adr/0031-gadget-con-mas-de-cuatro-interfaces.md); las interfaces de fabricante (HID++) **no se exponen**.
- cuando cambia el conjunto (otro `DeviceList`, otra exposición), el descriptor cambia; Windows y macOS **cachean** el descriptor por dispositivo emparejado, así que el agente cambia la **dirección o el nombre anunciado no**, sino que avisa en `BtStatus` de que hace falta olvidar y volver a emparejar en el PC. Documenta cómo se ve en cada sistema.
- tests de la fusión con los descriptores reales de `fixtures/` (C52B interfaces 0 y 1) y el sintético de `two-receivers.hidrec`: report IDs únicos, longitudes de informe, ida y vuelta de un informe de teclado y uno de ratón.

### Encaje con el hub (protocolo 11, versión 0.9.0)

- El agente es un `NodeKind::Agent` con `Os::Bridge` (nuevo valor; el hub no le aplica perfil de teclado HID++ por sistema, o aplica el del anfitrión si el widget lo indica; decide). No tiene pantallas: no manda `Screens`, no participa en bordes, y el hub no lo activa como nodo normal (`SwitchTo` hacia él se rechaza). Recibe entrada **solo** como destino exclusivo de uno o más receptores.
- Mensajes nuevos:
  - agente → hub → widgets: `BtStatus { bonded: Option<String>, connected: bool, discoverable_until: Option<u32 s>, repair_needed: bool }`.
  - widget → hub → agente: `BtDiscoverable { secs: u8 }`, `BtForget`.
- `--host-os` como en el gadget, para el perfil del teclado si se decide aplicarlo.

### Widget

En la pestaña Hub, cuando hay un puente conectado: su fila bajo «Equipos» con el estado (`BtStatus`), «Hacer visible 60 s» con cuenta atrás, «Olvidar PC», y el aviso de volver a emparejar. El selector de exposición de los receptores lista el puente como destino.

### Empaquetado y servicio

- Crate `crates/agent-bt`, binario `rawkvm-agent-bt`, `.deb` arm64/armhf en `debs.yml` y en la release, como el gadget: `rawkvm-agent-bt.service`, `/etc/rawkvm/agent-bt.toml` (hub, token, huella; `--pair` como los demás), `/etc/default/rawkvm-agent-bt`, override de `bluetooth.service`, dependencia de `bluez`.
- Instalación en el mismo equipo que el hub: `hub = "127.0.0.1:4433"`.

### Docs

- `docs/bluetooth-instalacion.md` (instalar, emparejar desde cada sistema, límites: tasa ~100‑130 informes/s, un PC, reemparejar si cambia el descriptor).
- `protocol.md` (11), ADR 0033, `widget-instalacion.md`, `hub-linux-instalacion.md` (caja mínima solo Bluetooth), STATUS con lo que probar en la RPi 3B+.

## Pruebas

- Unitarias: fusión de descriptores, prefijado y desprefijado de informes, generación del registro SDP, máquina de estados de emparejado y reconexión (con BlueZ simulado detrás de un trait), protocolo de ida y vuelta.
- En hardware (las hago yo con el usuario): emparejar el GAMER y el Mac, teclado y ratón, LEDs, suspensión y vuelta, cambio de exposición en caliente, dos receptores con uno exclusivo al puente.
- `cargo fmt`, `clippy -D warnings` (macOS, linux-gnu, windows-gnu para los crates de Windows), tests del workspace y del widget, `npm run build`.

## Fuera de esta fase

- BLE/HOGP.
- Varios anfitriones con conmutación tipo «Easy-Switch».
- Exponer la interfaz HID++ por Bluetooth.
