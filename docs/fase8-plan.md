# Fase 8: añadir receptores desde el widget, sin reiniciar el hub

Brief para una sesión de código (VS Code). Decidido por el usuario el 2026-10-26. Se numera 8 porque la 7 es el puente Bluetooth ([puente-bluetooth-analisis.md](puente-bluetooth-analisis.md)), que puede hacerse antes o después; las dos son independientes. Lleva **un ADR** (0032). Lo que no esté decidido aquí, decídelo tú y déjalo en el ADR; no preguntes.

## Problema

Desde la fase 6 ([ADR 0030](adr/0030-varios-receptores-y-exposicion.md)) el hub reenvía varios receptores, pero solo los que tienen un bloque `[[receiver]]` en `hub.toml`. Un dongle de un fabricante nuevo exige editar el fichero por SSH y reiniciar el hub. En la prueba de hoy (Areson/Compx `25a7:fa09`) se hizo a mano. Debe poder hacerse desde el widget, y sin cortar la entrada de los demás receptores.

Lo que **no** cambia: el hub nunca adopta un HID por su cuenta. Toma en exclusiva (`grab_evdev`) lo que casa, y adoptar cualquier HID que aparezca en la placa capturaría un teclado de mantenimiento. La adopción es siempre una acción explícita del usuario en el widget.

## Objetivo

1. El hub **anuncia a los widgets** los dispositivos USB HID presentes en la placa que no casan con ningún bloque `[[receiver]]` («receptores sin configurar»).
2. El widget los muestra bajo la tabla «Receptores» con un botón **«Añadir como receptor»**.
3. Al añadirlo, el hub **escribe el bloque** en `hub.toml`, lo **abre en caliente** (sus interfaces se anuncian a todos los nodos con `DeviceList`, con ids `16 × receptor + interfaz`) y reenvía `Settings` con la exposición nueva (`Network`). Sin reiniciar y sin interrumpir los receptores ya abiertos.
4. **«Quitar»** un receptor desde el widget: cierra sus interfaces, borra el bloque y su exposición, y avisa a los nodos. No se puede quitar el último.

## Diseño

### Hub

- `scan_loop` (`devices.rs`) ya enumera las interfaces `hidraw`. Hoy recibe la lista de receptores por valor; pasa a leerla de un estado **compartido y mutable** (`Arc<RwLock<Vec<ReceiverMatch>>>` o equivalente, dentro del `Router` o junto a él), para que añadir o quitar un bloque surta efecto en la siguiente pasada sin reiniciar el hilo. El índice del protocolo (`HidDevice.receiver`) sigue siendo la posición del bloque; quitar un bloque que no es el último renumera los siguientes, así que: **o se prohíbe quitar uno que no sea el último, o los índices se congelan** (el bloque quitado deja un hueco que no se reutiliza hasta el reinicio). Elige y documéntalo; lo segundo es más cómodo para el usuario.
- **Sin configurar**: en cada pasada, las interfaces USB (`bus == BUS_USB`, sin `parent_hid`, cualquier driver) que no casan con ningún bloque, **agrupadas por dispositivo USB** (mismo `vid`, `pid` y, si lo hay, `serial`; si no, el padre USB en sysfs). Para cada grupo: `vid`, `pid`, nombre del producto, `serial` opcional, número de interfaces y una pista de qué hay (teclado, ratón, otro) sacada del descriptor con `DescriptorInfo`, la misma clasificación del [ADR 0031](adr/0031-gadget-con-mas-de-cuatro-interfaces.md). Se excluyen las interfaces que el hub ya tiene abiertas y los hijos `dj-children`.
- **Añadir**: valida con las reglas de la fase 6 (sin solape con los bloques existentes, nombre único, como mucho 16). El bloque nuevo lleva `name` derivado del nombre del producto (minúsculas, `[a-z0-9-]`, único añadiendo `-2`, `-3`…), `match = "receiver"`, `vid`, `pids = [pid]` (solo ese producto, no todo el fabricante), `grab_evdev = true` y los valores por defecto del fabricante (ADR 0030). Se añade al final de `hub.toml` **sin reescribir el resto del fichero**: usa `toml_edit` para conservar comentarios y formato, o añade el bloque como texto al final si el fichero termina en una tabla de array; en cualquier caso, un test que carga el resultado y comprueba que los bloques anteriores no cambiaron. Si la escritura falla, no se añade en memoria y se responde con error.
- **Quitar**: cierra las interfaces (como cuando se desenchufa), borra el bloque del fichero y la entrada `[[exposure]]` de `settings.toml` si la tenía, y anuncia `DeviceList` y `Settings`. Con una sola entrada, se rechaza.
- `rawkvm-hub --list` muestra también los «sin configurar», para diagnosticar por SSH.

### Protocolo (versión 10; `Cargo.toml` del workspace y widget a 0.8.0)

- hub → widgets, al conectar y al cambiar: `UnclaimedReceivers { receivers: Vec<UnclaimedReceiver { vid: u16, pid: u16, name: String, serial: Option<String>, interfaces: u8, kinds: Vec<DeviceKind> }> }`, con `DeviceKind` = `Keyboard | Mouse | Other`.
- widget → hub: `AddReceiver { vid, pid }` y `RemoveReceiver { receiver: u8 }`.
- hub → el widget que lo pidió: `ReceiverResult { ok: bool, message: String }`, para que el widget muestre el motivo de un rechazo (solape, nombre, último bloque, error al escribir). El éxito llega además por `Settings` y `DeviceList` a todos.
- Un `AddReceiver` de un nodo que no es widget se ignora con aviso, como `SetExposure`.
- `docs/protocol.md` y los tests de ida y vuelta.

### Widget

- Pestaña Hub, bajo «Receptores»: lista «Sin configurar» con nombre, `vid:pid`, interfaces y pista (teclado, ratón), y el botón «Añadir como receptor». Vacía, una línea «Ningún receptor sin configurar». Tras añadir, la tabla «Receptores» se actualiza sola con el `Settings` nuevo.
- En cada fila de «Receptores», un botón «Quitar» con confirmación; deshabilitado si es el único.
- Mensaje de error del hub visible junto a la lista.

### Docs

- `hub-linux-instalacion.md`, sección «Varios receptores»: la vía del widget primero; la edición a mano queda como alternativa.
- `widget-instalacion.md`: los dos botones.
- ADR 0032: por qué no se adopta nada automáticamente, cómo se congelan o no los índices, `pids = [pid]` en vez de todo el fabricante, y la escritura del fichero.
- `docs/STATUS.md`: apartado al terminar, con lo que hay que probar en la RPi 3B+ (enchufar un dongle nuevo, añadirlo desde el widget, ver sus ids en todos los agentes, quitarlo, reiniciar el hub y comprobar que el bloque sigue).

## Pruebas

- Unitarias: agrupación de interfaces sin configurar; nombre derivado y único; rechazo por solape y por último bloque; añadir y quitar en caliente con el estado compartido (ids de las interfaces nuevas, `DeviceList` enviado); escritura del `hub.toml` que conserva los bloques y comentarios anteriores; `settings.toml` sin la exposición tras quitar; protocolo de ida y vuelta.
- `.hidrec`: una grabación con un dispositivo que no casa con ningún bloque, para que el hub simulado anuncie «sin configurar».
- `cargo fmt`, `clippy -D warnings` (macOS, linux-gnu y los crates de Windows en windows-gnu), tests del workspace y del widget, `npm run build`.

## Fuera de esta fase

- Adoptar automáticamente.
- Editar un bloque existente desde el widget (cambiar `pids`, nombre).
- Receptores que no sean USB (Bluetooth: fase 7).
