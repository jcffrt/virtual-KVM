# Puente Bluetooth: exponer un dongle a un solo PC por BT (análisis)

Estado: **propuesta, sin decidir** (2026-10-26). Si se aprueba, cada fase lleva su ADR.

## Qué se pide

1. Que el hub lea **más de un receptor**, de **cualquier fabricante**, no solo el Unifying.
2. Que cada receptor pueda **exponerse** de dos maneras, elegidas desde el widget:
   - **Red local** (lo de hoy): sus dispositivos van al equipo activo y cambian de equipo por bordes, hotkey o widget.
   - **Bluetooth a un solo PC**: sus dispositivos se presentan a ese PC como un teclado/ratón Bluetooth normal, sin agente en el PC y sin cable.
3. Que funcione en una Pi Zero, una RPi o similar.

## Cómo encaja en lo que hay

El hub ya hace casi todo lo difícil del lado de entrada:

- Toma los **informes HID crudos** de las interfaces `hidraw` que casan con `[receiver]` de `hub.toml` (`vid`, `pids`, modo) y los reparte por `id` de dispositivo (`router.rs`). No entiende de Logitech para reenviar: solo lo accesorio (HID++: batería, nombres, emparejar, `force_normal_mode`) es Logitech.
- Los agentes reproducen los informes con un **`HidSink`** (`agent-core`): `create(dev)`, `input(dev_id, report)`, `destroy_all`, y un «ya están todos» para los sumideros que necesitan ver el conjunto entero (el gadget USB).
- El **agente gadget** (`rawkvm-agent-gadget`) es un proceso aparte que habla con el hub por red (en la Zero, por loopback) y vuelca en `configfs`/`hidg`. Es exactamente el molde para un agente Bluetooth.

Por tanto el puente BT es **un `HidSink` nuevo en un proceso nuevo**, `rawkvm-agent-bt`, no una rama del hub. Puede correr en la misma placa que el hub (RPi 3B+ con BT Broadcom) o en otra (una Zero haciendo solo de puente). El hub sigue siendo el único dueño de los `hidraw`.

Descartado: un proceso que lea el dongle por su cuenta y lo exponga por BT saltándose el hub. Es más corto de escribir, pero dos procesos no pueden compartir un `hidraw` limpiamente, se pierde el control desde el widget y no hay forma de devolver el dongle a la red sin tocar la placa.

## Diseño por capas

### A. Hub: varios receptores, de cualquier fabricante

- `hub.toml`: `[receiver]` pasa a `[[receiver]]` con `name`, `match`, `vid`, `pids` y las opciones Logitech (`force_normal_mode`, HID++) solo si el fabricante es Logitech; para otros se ignoran con aviso. Se mantiene compatibilidad con el bloque único.
- `HidDevice` gana `receiver: u8` (índice del bloque). **`PROTO_VERSION` 7 → 8**: todos los agentes y el widget se actualizan a la vez, como manda `docs/protocol.md`.
- El router guarda por receptor una **exposición**: `Network` (hoy) o `Bluetooth { agent: Uuid }`. Los dispositivos de un receptor en `Bluetooth` se entregan **siempre** al agente BT, ignoran bordes, hotkey y `SwitchTo`; los demás siguen como hasta ahora. Persistencia en `settings.toml` junto a la hotkey.
- Mensajes nuevos: `SetExposure { receiver, mode }` (widget → hub) y la lista de exposiciones dentro de `Settings` (hub → widgets).
- Sin cambios en los agentes de Mac, Ubuntu, Windows ni en el gadget: solo recompilan por el campo nuevo.

Esta capa vale por sí sola: permite un ratón o teclado de otra marca por red, hoy imposible porque solo hay un `vid`.

### B. `rawkvm-agent-bt`: el dispositivo HID Bluetooth

Perfil **HID clásico (BR/EDR)** sobre BlueZ 5, no BLE: el clásico lleva el descriptor tal cual en el registro SDP y los dos canales L2CAP son sockets normales; HID over GATT obliga a trocear el descriptor en características y no gana nada aquí.

- Crate `bluer` (BlueZ oficial en Rust, D-Bus + sockets L2CAP, tokio): `ProfileManager1.RegisterProfile` con el registro SDP del HID (descriptor, clase de dispositivo `0x002540`, teclado + puntero), sockets `SOCK_SEQPACKET` en PSM `0x11` (control) y `0x13` (interrupción). `bluetoothd` debe arrancar con `--noplugin=input` para que no se quede él el perfil; el `.deb` lo deja en un `override` de `bluetooth.service`.
- **Un solo descriptor.** USB permite una interfaz por dispositivo; BT solo un descriptor por dispositivo. El agente **fusiona** los descriptores de los dispositivos del receptor asignando un *report ID* a cada uno (insertando el ítem `Report ID` en los que no lo tienen y renumerando los que sí) y antepone ese byte a cada informe. La interfaz HID++ del Unifying no se expone. Es la parte con más aristas: Windows **cachea el descriptor** por dispositivo emparejado; si cambia (otro ratón añadido al receptor), hay que olvidar y volver a emparejar en el PC.
- **Emparejamiento**: agente D-Bus `NoInputNoOutput`, «visible y emparejable 60 s» a petición del widget, un solo anfitrión vinculado a la vez. El dispositivo **reinicia la conexión** él mismo cuando el PC vuelve de suspensión (lo permite el perfil HID).
- **Salida** (LEDs del teclado) llega por el canal de interrupción con cabecera `0xA2`; se pasa al hub como `output`, que ya existe para el gadget.
- Mensajes nuevos: `BtStatus { bonded_host, connected, discoverable_until }` (agente → hub → widgets), `BtDiscoverable { secs }`, `BtForget` (widget → hub → agente).
- Identidad en el hub: es un `NodeKind::Agent` con un `os` nuevo o una marca `bridge`, para que el widget lo liste como destino de exposición y no como pantalla.

### C. Widget

En la pestaña Hub, por receptor: selector «Red local (todos los equipos)» / «Bluetooth → *nombre del puente*», y bajo él el estado del puente (PC vinculado, conectado o no), botón «Hacer visible 60 s» y «Olvidar PC». Reutiliza el patrón de `PairingStart`/`PairingEvent` que ya tiene el emparejado Unifying.

## Hardware

| Placa | BT | Juicio |
|---|---|---|
| RPi 3B+ / 4 / Zero 2 W / Zero W | Broadcom/Cypress, bien soportado en BlueZ | Buena. Comparte antena con el Wi‑Fi; por Ethernet no molesta. |
| Orange Pi Zero 2W | Unisoc UWE5622, el mismo chip del Wi‑Fi que se cuelga | Evitar para esto. |
| Dongle USB BT 4.x (CSR, Realtek) | Genérico | Alternativa en cualquier placa. |

El puente puede ir en la misma placa que el hub o en otra: el agente solo necesita red hasta el hub y un adaptador BT.

## Límites que no se arreglan con código

- **Tasa de informes**: USB llega a 1000/s; HID clásico se queda en ~100‑130/s (intervalo de *sniff* 7,5‑11 ms). Para juego en el GAMER es un paso atrás respecto al gadget USB; para ofimática no se nota.
- **Latencia y jitter**: +10‑20 ms y variable, además de la red hasta el hub.
- **Un anfitrión**: cambiar de PC es olvidar y emparejar, no es un KVM. Por eso la exposición BT es «a un solo PC» y el cambio entre equipos lo sigue haciendo la red.
- **Teclado exclusivo**: un receptor en modo BT deja de atender la hotkey de emergencia y los bordes. Hay que decidir si la hotkey del propio teclado puede devolverlo a la red (vía de escape) o si solo se hace desde el widget.

## Trabajo estimado (VS Code)

| Fase | Contenido | Estimación |
|---|---|---|
| A | `[[receiver]]`, `HidDevice.receiver`, exposición en el router, `Settings`, proto 8, docs + ADR | 1,5 días |
| B | agente BT: `bluer`, SDP, fusión de descriptores, pairing agent, reconexión, salida, `.deb` + servicio + override de bluetoothd, pruebas en Windows/macOS/Ubuntu | 3‑4 días |
| C | widget: selector, estado, visible/olvidar | 1 día |

Más medio día por fase de pruebas en hardware desde aquí. Riesgos principales: la fusión de descriptores con dispositivos raros, la caché de descriptores de Windows, y la pila BT del PC objetivo (Windows es la más exigente con HID clásico).

## Propuesta

1. **Hacer A ya**, en paralelo con la migración a la RPi 3B+: es pequeña, sin hardware nuevo, y abre los dongles de otros fabricantes aunque B no se haga nunca.
2. **Decidir B tras una prueba de concepto de un día**: `bluer` + descriptor fijo de un ratón + emparejar con un portátil. Si Windows acepta el dispositivo y la tasa es tolerable, seguir con el resto de B y luego C. Si no, el análisis queda aquí.
3. Dejar la pregunta de la vía de escape (hotkey en modo BT) para el ADR de A, porque toca al router.
