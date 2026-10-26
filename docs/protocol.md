# Protocolo RawKVM (versión 10)

Fuente de verdad: `crates/proto`. Este documento lo resume.

## Transporte

- Una conexión **QUIC** (quinn) por nodo, puerto UDP **4433** por defecto, ALPN `rawkvm/1`, solo TLS 1.3.
- El hub usa un certificado autofirmado (`rcgen`, SAN `rawkvm-hub`) guardado en `/var/lib/rawkvm/hub-cert.der` y `hub-key.der`. Su huella es el SHA‑256 del DER en hexadecimal (64 caracteres).
- Los agentes **no** validan el certificado contra una CA: fijan la huella (TOFU). La primera conexión aceptada con un token válido guarda la huella en `hub_fingerprint`; a partir de ahí cualquier otro certificado se rechaza y el agente se detiene (error fatal, no reintenta). La firma del handshake TLS 1.3 sí se verifica.
- Keep‑alive QUIC cada 1 s, timeout de inactividad 5 s: un hub o agente caído se detecta en ≤ 5 s.

## Descubrimiento

mDNS / DNS‑SD, servicio `_rawkvm._udp.local.`, instancia = nombre del hub, host `rawkvm-<nombre>.local.` (distinto del de avahi para no competir). TXT:

| Clave | Valor |
|---|---|
| `ver` | versión de protocolo (`3`) |
| `name` | nombre del hub |
| `fp` | huella SHA‑256 del certificado |
| `id` | UUID del hub |

El agente sin `hub` configurado escucha 3 s y elige el hub cuya `fp` coincide con la fijada, o el único encontrado si aún no hay ninguna. Con varios hubs y ninguno fijado, pide `--hub`.

## Canal de control

El nodo abre **un** stream bidireccional. Cada mensaje es `u32` little‑endian con la longitud + `Control` serializado con `postcard` (máx. 1 MiB).

Secuencia de entrada:

```
nodo -> hub   Hello { proto_ver, node_id, node_name, os, kind, token }
hub  -> nodo  HelloAck { hub_id, hub_name, assigned_index }   |  Reject { reason } y cierre
hub  -> nodo  DeviceList { devices }
hub  -> nodo  Layout {..}             (si hay uno)
hub  -> nodo  Screens {..}            (solo widgets, uno por agente)
hub  -> nodo  Activate {..}           (si este agente pasa a ser el activo)
hub  -> todos Status(HubStatus)       (en cada cambio)
```

- `assigned_index` 1..4 para agentes (dígito del atajo de emergencia; se conserva al reconectar mientras viva el hub), 0 para widgets. Un quinto agente recibe `Reject { TooManyNodes }`.
- `Reject { BadToken | VersionMismatch }` es definitivo: el agente se detiene. `TooManyNodes` se reintenta.
- `kind`: `Agent`, `Widget` o `EdgeHelper { for_node }` (v2). Un helper vigila el puntero en nombre del agente `for_node`, que no puede hacerlo (gadget USB, daemon root fuera de la sesión gráfica). No recibe índice ni informes; sus `Screens` cuentan como del agente, puede enviar `SwitchTo` mientras ese agente es el activo, y recibe sus `Activate`/`Deactivate`.
- **Versiones**: hub y nodos deben tener la misma `PROTO_VERSION`; si no, `Reject { VersionMismatch }` y el nodo se detiene con un mensaje que pide actualizar. `Hello` y las variantes hasta `Reject` no cambian nunca de forma, para que un nodo de otra versión siempre reciba ese rechazo (test `handshake_stays_decodable_across_versions`). Historia: v1, fases 0 y 1; v2 añade `EdgeHelper`, `screen` en `SwitchTo`/`Activate` y `options` en `Layout`; v3 (widget) añade `Entry` opcional, `canvas` en `Layout`, `Settings`, `version` en `HubStatus` y `screens` en `NodeInfo`, y fija el significado de `Screen::scale`; v4 (Fase 3) añade los datagramas de salida agente → hub (`FLAG_OUTPUT`), `ReceiverDevices` y `Unpair`; v5 (Fase 4.1) añade `battery`, `pointer_speed` y `fn_swap` a `PairedDevice`, y `SetPointerSpeed` y `SetFnSwap`; v6 (Fase 4.2) cambia `HubSettings.hotkey` de `bool` a `Hotkey { enabled, modifier, previous, release_keys }`. v7 (Fase 5.3b) añade `Shutdown` y `ShuttingDown` (apagado limpio del hub, ADR 0025); v8 (Fase 6) añade varios receptores: `HidDevice.receiver`, `HubSettings.exposures`, `SetExposure` y `HubStatus.held` (ADR 0030); v9 añade `Unexposed` y `NodeInfo.unexposed` (gadget con más interfaces de las que caben, ADR 0031); v10 (fase 8) añade `UnclaimedReceivers`, `AddReceiver`, `RemoveReceiver` y `ReceiverResult` (receptores desde el widget, ADR 0032).
- Si el mismo `node_id` se reconecta, la conexión nueva sustituye a la vieja.

Mensajes en régimen:

| Mensaje | Dirección | Efecto |
|---|---|---|
| `Ping(t)` / `Pong(t)` | ambos | eco; el agente envía `Ping` cada 1 s con microsegundos desde el inicio de sesión y calcula el RTT (EWMA 0.2) |
| `SwitchTo { target, entry: Option<Entry> }` | agente activo, su helper o widget → hub | cambia el destino. `Entry { screen, edge, pos_ratio }` dice dónde aparece el puntero en `target` (`pos_ratio` ya traducido por los rangos del enlace). Sin `entry` (menú del widget) el destino conserva su puntero. El de un widget no respeta la histéresis. **Un widget que pide el nodo ya activo** («Liberar teclas») hace que el hub envíe a todos los agentes un datagrama `RELEASE_ALL` por dispositivo, repetido a los `resend_ms` |
| `Screens` | agente o helper → hub → widgets | monitores de la máquina (el hub reescribe `node` con el agente) |
| `Layout { screens, edges, options, canvas }` | hub → nodos; widget → hub | enlaces de bordes en vigor y lienzo del widget (ver Layout) |
| `Settings(HubSettings { hysteresis_ms, hotkey })` | hub → widgets (al conectar y al cambiar); widget → hub | ajustes de conmutación; el hub los aplica y guarda en `/var/lib/rawkvm/settings.toml`, que manda sobre `[switching]` de `hub.toml`. v6: `hotkey` es `Hotkey { enabled, modifier: Ctrl\|Shift\|Alt\|Gui, previous, release_keys }` (modificador dos veces, luego 1..4, 0 = anterior, Esc = liberar teclas). v8: `exposures: Vec<ReceiverExposure { receiver, name, mode }>`, una por bloque `[[receiver]]` del hub; la rellena el hub y en un `Settings` de widget se ignora (ver «Receptores y exposición») |
| `SetExposure { receiver, mode: Exposure }` | widget → hub | v8. `Network` o `Exclusive { agent }`. Aceptado: el hub suelta las teclas de ese receptor en su destino anterior, lo guarda en `settings.toml` y envía `Settings` a todos los widgets y un `Status`. Rechazado (receptor inexistente, o `agent` que no es un agente conectado ni de `nodes.toml`): aviso en el registro y `Settings` sin cambios solo al que lo pidió |
| `DeviceList` | hub → nodos | al conectar y cuando cambia el conjunto de dispositivos |
| `Activate { entry: Option<Entry> }` / `Deactivate { keep }` | hub → agente y sus helpers | este agente empieza / deja de recibir. Sin `entry` (atajo, caída del activo, menú del widget) el helper no mueve el puntero. v8: `keep` son los ids de dispositivos de receptores exclusivos, que el agente **no** suelta al desactivarse (los helpers lo ignoran) |
| `PairingStart { timeout_s }` / `PairingStop` | widget → hub | abre (registro `0xB2` `01 00 <s>`) o cierra (`02`) el emparejamiento del receptor. Sin la interfaz HID++ abierta, `PairingEvent(Failed)` |
| `PairingEvent` | hub → widgets | `Started { timeout_s }`, `DeviceFound { name }` (notificación `0x41` de una ranura nueva), `Paired { name, kind }` o `TimedOut` (notificación `0x4A` de cierre), `Failed { reason }` (error en `0x4A` o en la petición) |
| `ReceiverDevices { devices: Vec<PairedDevice> }` | hub → widgets | v4. Dispositivos emparejados con el receptor (`slot`, `name`, `kind`, `wireless_pid`, `online`, `os_profiles`, `os_profile`), leídos por HID++; al conectar y al cambiar (dispositivo que se conecta, duerme, se empareja o se olvida; disposición cambiada) |
| `Unpair { slot }` | widget → hub | v4. Desemparejar la ranura `slot` del receptor (registro `0xB2` `03 <slot>`) |
| `SetPointerSpeed { slot, value }` | widget → hub | v5. Velocidad del puntero (HID++ `POINTER_SPEED` 0x2205, en 1/256: 256 = 1×). El hub la limita a 0x2E..=0x1FF, el rango que admite Solaar |
| `SetFnSwap { slot, on }` | widget → hub | v5. `on`: F1…F12 dan su función especial sin Fn (HID++ 0x40A3, en el host actual) |
| `Status(HubStatus)` | hub → nodos | en cada cambio. v8: `held` = receptores exclusivos cuyo agente no está conectado (su entrada se descarta). v9: `nodes[].unexposed` = ids de dispositivos que ese agente no puede presentar a su PC |
| `UnclaimedReceivers { receivers }` | hub → widgets | v10. Al conectar y al cambiar: los dispositivos USB HID de la placa que ningún bloque `[[receiver]]` reconoce, uno por dispositivo USB (`vid`, `pid`, `name`, `serial`, `interfaces`, `kinds`: `Keyboard`, `Mouse`, `Other`). El hub nunca los toma por su cuenta |
| `AddReceiver { vid, pid }` | widget → hub | v10. Añadir ese dispositivo «sin configurar» como receptor: el hub escribe un bloque `[[receiver]]` en `hub.toml` (`pids = [pid]`) y abre sus interfaces en ≤ 2 s, sin reiniciar. Responde con `ReceiverResult`; si sale bien, además `Settings` a los widgets y `DeviceList` a todos |
| `RemoveReceiver { receiver }` | widget → hub | v10. Cerrar el receptor y borrar su bloque y su exposición. Se rechaza si es el único. El índice queda libre sin reutilizarse hasta reiniciar el hub. Responde con `ReceiverResult`; si sale bien, `DeviceList`, `Settings` y `Status` |
| `ReceiverResult { ok, message }` | hub → el widget que lo pidió | v10. Resultado de `AddReceiver` / `RemoveReceiver`; `message` (en inglés) dice el motivo de un rechazo: solape con otro bloque, último receptor, no está en «sin configurar», error al escribir `hub.toml` |
| `Unexposed { devices: Vec<u8> }` | agente → hub | v9. Ids del `DeviceList` que el agente no puede presentar a su PC (el gadget USB solo admite cuatro interfaces HID, ADR 0031). Se envía cuando el conjunto cambia, también a vacío; el hub lo pone en `NodeInfo.unexposed` y envía un `Status`. De nodos que no son agentes se ignora |
| `Shutdown` | widget → hub | v7. Apagar la placa del hub: libera las teclas en todos los agentes, envía `ShuttingDown` a todos, para el gadget, `sync` y `systemctl poweroff` (ADR 0025). Un segundo `Shutdown` durante el apagado solo recibe `ShuttingDown` |
| `ShuttingDown { reason }` | hub → nodos | v7. El hub se apaga (`reason`: «requested from GAMER»). Los nodos cierran la sesión y reintentan cada 30 s en vez del backoff normal hasta que vuelve. Para el widget que lo pidió es la confirmación |

`PairedDevice` (v5) añade:
- `battery: Option<Battery { percent, next_percent, charge }>`, de `BATTERY_STATUS` 0x1000. El nivel va por tramos (p. ej. 100, 50, 20, 5 %). `charge` es `Discharging`, `Charging`, `AlmostFull`, `Full`, `SlowCharging` o `Error`.
- `pointer_speed: Option<PointerSpeed { value, min, max }>`.
- `fn_swap: Option<bool>`.

Cada uno vale `None` si el dispositivo no tiene esa función o no ha respondido.

**Solo widgets**: el hub atiende `PairingStart`, `PairingStop`, `Unpair`, `SetPointerSpeed`, `SetFnSwap`, `Shutdown`, `Settings`, `SetExposure`, `AddReceiver`, `RemoveReceiver` y `Layout` únicamente de nodos `Widget` (que ya presentaron el token); de un agente o un helper los ignora. `ReceiverDevices`, `PairingEvent`, `UnclaimedReceivers` y `ReceiverResult` solo van a widgets.

## Datagramas (informes HID)

```
+--------+--------+-----------------+------------------------------+
| dev_id | flags  |  seq (u16 LE)   | informe crudo (con report ID) |
+--------+--------+-----------------+------------------------------+
```

- `dev_id` = `HidDevice.id`. En modo `receiver` es `16 × receptor + número de interfaz USB`: para el primer receptor, 0 teclado, 1 ratón/consumer/system, 2 HID++/DJ; para el segundo, 16, 17…
- `seq` es por dispositivo **y por agente**; el agente cuenta pérdidas con `SeqTracker` y **descarta** duplicados y datagramas más antiguos que el último recibido.
- `flags & 0x01` (`RELEASE_ALL`): informe sintético "nada pulsado".
- `flags & 0x02` (`OUTPUT`, v4): **agente → hub**, un informe de salida que el PC envió a su dispositivo virtual (LEDs del teclado), con los bytes tal como los produce su pila HID: report ID delante solo si la interfaz usa IDs (el teclado boot de la interfaz 0 no: 1 byte de LEDs, bit 0 Bloq Num, bit 1 Bloq Mayús, bit 2 Bloq Despl). Es el único datagrama que aceptan los hubs de nodo a hub. Ver «Informes de salida».
- **Redundancia**: todo informe que no sea de ratón se envía dos veces, la segunda a los `resend_ms` (15 ms por defecto). Así una pérdida aislada no deja teclas pegadas. El duplicado lleva el mismo `seq` y el agente lo descarta. Ver ADR 0004.
- Congestión: el buffer de envío de datagramas es de 64 KiB; quinn descarta los más antiguos al llenarse.

## Informes de salida (LEDs)

v4, ADR 0012. Camino de vuelta del PC al teclado real:

- Agentes: Linux reenvía los `UHID_OUTPUT` (y los `SET_REPORT` de tipo salida, quitando el byte de número de informe que añade el kernel cuando es 0); el gadget, cada lectura de `/dev/hidgN` (el endpoint OUT entrega el informe tal cual); macOS, el estado de los LED que el driver de Karabiner devuelve como informe de entrada (página 0x08), convertido al byte del teclado boot. Cada informe va dos veces (la segunda a los 30 ms, mismo `seq`). El agente recuerda el último por dispositivo y report ID y lo reenvía al empezar cada sesión, porque el PC solo los manda cuando cambian.
- Hub: solo de **agentes**; descarta duplicados y atrasados por `seq`; solo informes **declarados** por el descriptor de esa interfaz (mismo report ID y longitud) y de páginas estándar (nunca HID++: esos son del hub). Guarda el último por agente, dispositivo y report ID (también de agentes inactivos y entre reconexiones), y solo **escribe** en el `hidraw` los del agente **activo**, como mucho 50 por segundo e interfaz, desde un hilo por interfaz (`write()` con el número de informe delante, 0 sin IDs).
- Al activar un agente, el hub escribe su último estado; si nunca mandó uno, todo a cero (LEDs apagados). Lo mismo al reconectar el receptor.

## Receptores y exposición

v8, ADR 0030. El hub puede leer varios receptores (`[[receiver]]` en `hub.toml`, de cualquier fabricante; HID++ solo con el primer Logitech). `HidDevice.receiver` es el índice del bloque, desde 0. Cada receptor tiene una exposición:

- `Network`: sus informes van al agente activo y siguen la conmutación normal.
- `Exclusive { agent }`: sus informes van siempre a `agent`, sea cual sea el activo; si no está conectado se **descartan** (y el receptor aparece en `HubStatus.held`). Ni el atajo ni los bordes actúan sobre ellos: el hub ignora los `SwitchTo` de un agente (o su helper) cuyo puntero movió por última vez un ratón exclusivo. Excepción: modificador, modificador, `Esc` en un teclado exclusivo devuelve ese receptor a `Network` (aunque el atajo esté desactivado) y suelta sus teclas; el `Esc` no se reenvía.
- Al cambiar de agente activo, `RELEASE_ALL` y los LEDs solo afectan a los receptores `Network`, y `Deactivate { keep }` lista los dispositivos exclusivos para que el agente no los suelte. Los informes de salida de un dispositivo exclusivo solo se escriben si vienen de su agente.
- `DeviceList` es la misma para todos los nodos: cada agente crea todos los dispositivos, y la exposición solo decide quién recibe los informes.
- Persistencia: `settings.toml`, `[[exposure]]` con `receiver = "<name>"` y `exclusive = "<uuid>"`, por nombre de receptor.
- v10: los receptores se pueden añadir y quitar desde el widget con el hub en marcha (ADR 0032). Un receptor quitado deja un hueco: su índice no se reutiliza hasta reiniciar el hub, y `HubSettings.exposures` no lo incluye.

## Conmutación

1. El agente activo (o un widget) envía `SwitchTo`. Un agente inactivo no puede robar la entrada.
2. Histéresis: se ignora un `SwitchTo` a menos de `hysteresis_ms` (150 ms) del cambio anterior. El atajo y la caída del agente activo no la respetan.
3. El hub envía al origen un datagrama `RELEASE_ALL` por cada informe de entrada de páginas estándar de los receptores `Network` (las colecciones vendor ≥ 0xFF00, como HID++ o el ID 8 de página 0xFFBC, se omiten), luego `Deactivate { keep }`. Al recibir `Deactivate` el agente **también** sintetiza localmente esos informes, salvo para los dispositivos de `keep`, porque los datagramas pueden perderse.
4. El destino y sus helpers reciben `Activate { entry }` (`pos_ratio` acotado a 0..1); los helpers del origen, `Deactivate`.
5. Si el agente activo se desconecta, pasa a ser activo el último agente que lo fue y sigue conectado (o el de índice menor).

## Pantallas y unidades

- `Screen { node, index, x, y, w, h, scale }`: `x`, `y`, `w`, `h` van **siempre en las unidades en que el helper lee y coloca el puntero**: puntos en macOS; píxeles físicos en Windows, que es consciente del DPI por monitor. El helper nunca convierte.
- `scale` = unidades nativas por unidad lógica: 1.0 en macOS, DPI/96 en Windows. Solo sirve para dibujar el lienzo del widget, que divide por ella. El usuario puede corregirla por nodo (`Placement::scale`).
- `index` 0 es la pantalla principal; las demás van de izquierda a derecha.
- `HubStatus.nodes[].screens`: número de pantallas que el hub conoce de cada nodo. Un agente con 0 no tiene helper de borde.

## Layout (bordes)

- El hub envía `Layout { screens, edges, options }` al conectar cada nodo y cada vez que cambia: `screens` son los monitores que han reportado agentes y helpers (datos vivos), `edges` los `EdgeLink` en vigor en **los dos sentidos**, `options` = `EdgeOptions { push_ms, corner_pct, dead_zone_px }`, `canvas` = `Vec<Placement { node, x, y, scale }>`: posición de cada bloque de pantallas en el lienzo del widget, en unidades lógicas. Va vacío con la semilla; los helpers lo ignoran.
- Fuente de los enlaces, por prioridad: (1) el último `Layout` de un widget, que el hub guarda en `/var/lib/rawkvm/layout.toml` (enlaces, opciones y lienzo) y aplica al instante. Un `Layout` de widget con `edges` y `canvas` vacíos significa «volver a la semilla»: el hub borra `layout.toml`. Un lienzo real siempre tiene bloques; (2) ese fichero al arrancar; (3) la semilla `[[layout]]` de `hub.toml`, por índice de agente, resuelta contra los `Screens` recibidos. Si el destino de un enlace no ha reportado pantallas, la ida se resuelve con `to_screen` (0 por defecto) y la vuelta espera a sus pantallas. Los helpers no guardan layout propio.
- CLI: `rawkvm-hub --dump-layout` imprime el layout de `layout.toml` (o la semilla); `rawkvm-hub --load-layout <fichero>` lo valida e instala, y `systemctl reload rawkvm-hub` (SIGHUP) lo aplica sin reiniciar. Borrar `layout.toml` y recargar vuelve a la semilla.
- Reglas de detección del lado del helper: `agent-core::edge` (ADR 0009).

## Atajo de emergencia

Interpretado por el hub sobre el informe de teclado, con el layout que da el descriptor (`KeyboardLayout`: boot de 8 bytes o los 7 bytes con report ID de los descriptores DJ). Dos pulsaciones limpias de Ctrl (izquierdo o derecho, ≤ 400 ms cada una, ≤ 500 ms entre ambas, sin otra tecla) arman el atajo durante 1,5 s; la siguiente tecla 1..4 (fila superior o teclado numérico) cambia a ese agente. Cualquier otra tecla o modificador lo desarma. El informe con el dígito **no se reenvía** a nadie; el resto de informes se reenvían sin alterar.
