# Fase 3: canal HID++ desde el hub (perfil del teclado, emparejamiento) y vía de vuelta (LED)

Brief del orquestador para la sesión de VS Code (2026-10-23). Estado de partida: Fase 2 completa
(tres SO con entrada nativa y cruce por borde; widget con Hub, Dispositivos, Monitores y Opciones).
Hub en la Orange Pi en modo A (sin DJ): `hid-generic` sobre las tres interfaces del receptor
`046D:C52B`, `hidraw0` teclado boot, `hidraw1` ratón/consumer/system, `hidraw2` HID++
(informes cortos `0x10` de 7 B y largos `0x11` de 20 B, ver `fixtures/unifying-c52b/`).
Hoy el hub solo **escribe** en `hidraw2` el comando DJ de modo normal (`receiver::normal_mode_report`)
y **lee** las tres interfaces para reenviarlas.

Orden recomendado: **A** (pequeño, lo pide el usuario ya), **B**, **C**, **D**. Cada bloque entrega por
separado: fmt, clippy `-D warnings`, tests, docs en castellano, un ADR por decisión nueva, y una nota en
`STATUS.md`. El orquestador despliega en la Pi, el Mac, el Windows (gadget) y el Ubuntu, y coordina
las pruebas con el usuario.

## A. Vía de vuelta: informes de salida del PC al teclado (LED de Bloq Mayús)

Problema: el LED de Bloq Mayús del MX Keys ya no se enciende en ningún PC. Los informes de salida
que el SO manda al teclado virtual se descartan en los agentes (riesgo 3 de la Fase 0, aplazado):
`agent-linux` (`uhid.rs`, `KernelEvent::Output` → "output report ignored"), `agent-gadget`
(`hidg.rs`, hilo `hidg-out` que vacía `/dev/hidgN`), `agent-macos` (sin camino conocido, ver abajo).

Diseño propuesto:

1. **Datagrama agente → hub** con el mismo formato (`proto::datagram`), nuevo flag
   `FLAG_OUTPUT = 0x02` en la cabecera; `dev_id` = la interfaz de destino (la misma `id` del
   `HidDevice`), `report` = bytes del informe de salida tal cual (con report ID si la interfaz los usa;
   el teclado boot de la interfaz 0 no lo usa: 1 byte de LEDs).
2. **Hub**: acepta informes de salida **solo del nodo activo**; comprueba que la longitud coincide con
   un informe de salida declarado en el descriptor de esa interfaz (`DescriptorInfo`; añadir la
   colección de informes de salida si el parser no la expone); limita el ritmo (p. ej. 50/s por
   interfaz) y escribe en el `hidraw` de la interfaz (`write()` sobre el `File` ya abierto en
   `devices.rs`; el kernel lo manda por interrupt OUT o SET_REPORT). Guarda el **último informe de
   salida por nodo e interfaz** y lo reaplica al activar ese nodo, para que el LED siga al PC activo.
3. **Agentes**:
   - Linux: `KernelEvent::Output` (y `SetReport` de tipo output) → datagrama. `rtype` indica
     report type; solo se reenvía el tipo output.
   - Gadget: el hilo `hidg-out` deja de descartar y manda cada lectura como datagrama con la
     interfaz de esa función (`/dev/hidg0` = interfaz 0).
   - macOS: comprobar si el daemon de Karabiner-DriverKit-VirtualHIDDevice 8.6.0 notifica al
     cliente el estado de LEDs del teclado virtual (buscar en el protocolo de cliente 7, ADR 0005).
     Si no hay camino, documentarlo como limitación del Mac en `macos-instalacion.md` y seguir.
4. Protocolo: el flag no rompe nada (los agentes nunca enviaban datagramas), pero B y D sí bumpean a
   **v4**; hacedlo una sola vez con A y dejad `docs/protocol.md` actualizado.

Verificación: en el Ubuntu, Bloq Mayús enciende el LED del MX Keys y lo apaga; lo mismo en el
Windows por el gadget; al cambiar de PC el LED refleja el estado del PC activo. Test del hub con un
`hidrec`/fake que compruebe filtro por nodo activo, longitud y reaplicación al activar.

## B. Transporte HID++ en el hub y lectura de `HOSTS_INFO` (0x1815)

Objetivo: que el hub hable HID++ 2.0 con los dispositivos emparejados a través de la interfaz 2,
con petición/respuesta correlacionada, sin romper el reenvío.

1. Módulo `hub::hidpp` con un trait `Transport` (escribir informe, recibir informes de la interfaz
   HID++) para probarlo con un transporte falso. El hilo lector de `hidraw2` ya existe: añadir un
   despacho que entregue a las peticiones pendientes las respuestas con su `device_index`,
   `feature_index` y `sw_id` (elegir una constante, p. ej. `0x0C`), con timeout (300 ms, 2 reintentos).
   Las respuestas a nuestras peticiones **no se reenvían a los agentes**; el resto de la interfaz
   sigue igual (hoy se reenvía entera como dispositivo 2).
2. Direccionamiento: índice de dispositivo 1..6 (ranura del receptor); `0xFF` es el receptor.
   Errores HID++ 1.0 (`0x8F`) y 2.0 (`0xFF`) con su código; ranura vacía o dispositivo apagado →
   error o timeout, tratarlo como "no disponible", nunca bloquear el arranque del hub.
3. Enumeración: por cada ranura, `IRoot.getFeature(0x1815)` (feature index 0, función 0). Si
   responde con índice ≠ 0, el dispositivo soporta `HOSTS_INFO`. Nombre y tipo por los registros
   HID++ 1.0 del receptor (`0xB5`, páginas `0x20+n` info de emparejamiento con wireless PID y tipo,
   `0x40+n` info extendida, `0x60+n` nombre). Contrastad los formatos con el código de Solaar
   (`hidpp10`, `hidpp20`) y del kernel (`hid-logitech-hidpp.c`), no de memoria.
4. `HOSTS_INFO` (funciones según Solaar `hidpp20`): `getHostsInfo()` → flags, `numHosts`,
   `currentHost`; `getHostInfo(host)`; `getHostOsVersion(host)` → `type` (0 desconocido, 1 iOS,
   2 macOS, 3 Android, 4 Windows, 5 Linux, 6 Chrome), `major`, `minor`, `build`;
   `setHostOsVersion(host, type, major, minor, build)`. Verificad los números de función en Solaar.
5. **CLI primero**, antes de tocar el router: `rawkvm-hub --hidpp-list` (ranuras, nombre, tipo,
   wireless PID, soporte de 0x1815, host actual y OS declarado) y
   `rawkvm-hub --hidpp-set-os <ranura> <windows|macos|linux>`. Con eso el orquestador valida en
   la Pi, con el hub parado, si `setHostOsVersion` sobre el host actual **cambia de verdad** la
   posición de Alt/Win del MX Keys (el usuario comprueba en el Windows). Guardad los intercambios
   reales en `fixtures/` para los tests.
6. Riesgo: que el teclado solo aplique el perfil al reconectar el host o que ignore la escritura
   (Fn+O / Fn+P lo cambia a mano). Si no cambia en caliente, se mantiene `swap_alt_gui` (ADR 0011)
   y queda documentado; no insistir.

## C. Perfil del teclado al conmutar (sustituye al parche Alt/GUI)

Solo si B.5 demuestra que funciona en caliente.

1. El router, al activar un nodo, llama `setHostOsVersion(currentHost, os_del_nodo)` en cada
   dispositivo con `HOSTS_INFO` (el `os` llega en `Hello`; Linux → 5, Windows → 4, macOS → 2).
   Fuera del camino de los informes (tarea aparte, no bloquea el reenvío); fallo → `warn` una vez
   por dispositivo, sin reintentos en bucle.
2. Config `[hidpp] os_profile = true|false` (por defecto `true` si B.5 fue bien) y
   `[[node_options]] os_profile = "windows"|"macos"|"linux"|"none"` para forzar un perfil distinto
   del SO real (p. ej. Linux con perfil Windows). `swap_alt_gui` sigue existiendo, el usuario lo
   quitará de su `hub.toml` cuando esto funcione.
3. Widget, pestaña Dispositivos: mostrar por dispositivo emparejado el perfil actual (del hub) —
   requiere D.1.
4. ADR: "cambio de perfil por HID++ desde el hub" (sustituye a 0011 en la práctica; 0011 queda como
   alternativa).

## D. Emparejamiento desde el widget

1. **Protocolo v4**: `Control::ReceiverDevices { devices: Vec<PairedDevice> }` hub → widgets (al
   conectar y al cambiar), con `slot`, `name`, `kind`, `wireless_pid`, `hosts_info: bool`,
   `os_profile: Option<Os>`; `Control::Unpair { slot }` widget → hub. `PairingStart/Event/Stop` ya
   existen en `proto` (`PairingEvent`: Started, DeviceFound, Paired, Failed, TimedOut) y el router
   responde hoy "pairing is not implemented yet" (`router.rs` ~560).
2. Hub: habilitar notificaciones del receptor (registro `0x00`, flag wireless), abrir
   emparejamiento con el registro `0xB2` (`0x01`, ranura/0, timeout en s), escuchar las
   notificaciones `0x4A` (estado del bloqueo de emparejamiento) y `0x41` (conexión de dispositivo,
   con wireless PID), leer el nombre por `0xB5` y emitir `PairingEvent`. `PairingStop` → `0xB2`
   `0x02`. Desemparejar → `0xB2` `0x03 <ranura>`. Formatos: contrastar con Solaar `pair.py` /
   `hidpp10`.
3. En modo A el emparejamiento **no cambia** `DeviceList` (los dispositivos son interfaces, no
   teclados); un teclado nuevo emite por las interfaces 0/1 sin más. Comprobadlo en el test del router.
4. Widget `Devices.svelte`: habilitar "Emparejar" (cuenta atrás, eventos), lista de dispositivos del
   receptor con "Olvidar", y el perfil de C.3. Confirmación antes de olvidar.
5. Seguridad: `PairingStart`, `Unpair` y `Settings` solo desde nodos `Widget` (ya emparejados con
   el token). Documentar en `protocol.md`.

## Entregables y verificación conjunta

- Protocolo v4 en un solo salto; el orquestador redespliega hub, gadget, agentes y widget a la vez
  (recordad la pausa de 60 s de los agentes ante `Reject` por versión).
- Tests: transporte HID++ falso (correlación, timeout, error), parseo de `HOSTS_INFO` y `0xB5`
  con intercambios reales grabados, router (A: filtro por activo y reaplicación; C: llamada al
  activar; D: eventos de emparejamiento y `DeviceList` intacto).
- Docs: `protocol.md` (v4), `gadget-instalacion.md` (quitar la nota de `swap_alt_gui` si C entra),
  `widget-instalacion.md` (Dispositivos), ADRs nuevos, `STATUS.md`.
- Fuera de alcance: HID++ desde los PCs hacia los dispositivos (Logitech Options en el PC seguirá
  sin ver el teclado), batería y DPI en el widget (posible Fase 4 con el mismo transporte).
