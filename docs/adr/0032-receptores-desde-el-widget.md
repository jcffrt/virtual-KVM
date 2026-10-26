# ADR 0032 — Añadir y quitar receptores desde el widget, sin reiniciar el hub

- Estado: aceptada (2026-10-26). Plan: [fase8-plan.md](../fase8-plan.md). Parte de la fase 6 ([ADR 0030](0030-varios-receptores-y-exposicion.md)).

## Contexto

Desde la fase 6 el hub reenvía varios receptores, pero solo los que tienen un bloque `[[receiver]]` en `hub.toml`. Para un dongle de otro fabricante había que editar el fichero por SSH y reiniciar el hub, y el reinicio corta la entrada de todos los equipos. Así se añadió a mano el Areson/Compx `25a7:fa09` en la prueba de la fase 6.

## Decisión

### El hub no adopta nada por su cuenta

Lo que el hub reenvía lo toma en exclusiva (`grab_evdev`). Adoptar cualquier HID que aparezca en la placa capturaría un teclado de mantenimiento enchufado para arreglar la placa. Por eso el hub solo **anuncia** los dispositivos que no reenvía, y añadir uno es siempre una acción explícita del usuario en el widget.

### «Sin configurar»

- En cada pasada (cada 2 s), el hilo de escaneo junta las interfaces `hidraw` que cumplen tres condiciones:
  - son USB;
  - no cuelgan de otro HID (se excluyen los hijos de `hid-logitech-dj` y similares);
  - ningún bloque las reconoce y el hub no las tiene abiertas. Una interfaz que se está cerrando aparece en la pasada siguiente.
- **Agrupación por dispositivo USB:** mismo `vid` y `pid`, y mismo número de serie o, si no lo hay, mismo puerto USB. El puerto es la parte de `phys` anterior a `/input`. Así dos dongles iguales en dos puertos son dos entradas.
- **Por cada grupo se anuncia:**
  - `vid` y `pid`;
  - el nombre: el más corto de sus interfaces, porque el kernel añade «Mouse» o «Consumer Control» a algunas;
  - el número de serie, si lo hay;
  - el número de interfaces;
  - qué contiene: teclado (Generic Desktop keyboard o keypad), ratón (mouse o pointer) u otro (cualquier otra colección). Es la clasificación del ADR 0031.
- El hub envía la lista a los widgets al conectar y cuando cambia (`UnclaimedReceivers`). `rawkvm-hub --list` la muestra también, para diagnosticar por SSH.

### Añadir

- `AddReceiver { vid, pid }` (de un widget; de otro nodo se ignora con aviso). El dispositivo debe estar en «sin configurar»: el hub no añade algo que no ve.
- **El bloque nuevo:**
  - `name` sale del nombre del producto: minúsculas, `[a-z0-9-]`, como mucho 32 caracteres, `receiver` si queda vacío, y único añadiendo `-2`, `-3`…;
  - `match = "receiver"`, `vid`, `pids = [pid]` y `grab_evdev = true`, con los valores por defecto del fabricante para lo demás (ADR 0030);
  - **`pids = [pid]` y no todo el fabricante**: el usuario ha visto ese producto, no otro. Así un segundo dongle de la misma marca sigue apareciendo en «sin configurar», y se evita solaparse con un bloque de ese fabricante.
- **Validación:** el texto resultante se carga con las reglas de la fase 6 (sin solape, nombres únicos, como mucho 16 bloques). Además, el número de huecos usados no puede llegar a 16 (ver índices).
- **Orden:** primero se escribe `hub.toml` y después cambia la memoria. Si la escritura falla, nada cambia y el widget recibe el motivo.
- **Después:**
  - el bloque entra en la lista del router con el siguiente índice libre;
  - el hilo de escaneo abre sus interfaces en su pasada siguiente (≤ 2 s), con ids `16 × receptor + interfaz`, y se anuncian con `DeviceList` a todos los nodos;
  - los widgets reciben `Settings` con la exposición nueva (`Network`) y la lista de «sin configurar» sin ese dispositivo;
  - el widget que lo pidió recibe `ReceiverResult { ok: true }`.

  Ni el hub ni los demás receptores se reinician.

### Quitar

- `RemoveReceiver { receiver }`. Se rechaza si el receptor no existe (o ya se quitó) y si es el único bloque.
- **Primero se escribe `hub.toml`, sin ese bloque.** Después:
  - se sueltan sus teclas en el equipo que recibía su entrada;
  - se borran su exposición (y con ella su entrada `[[exposure]]` de `settings.toml`) y su detector de escape;
  - sus dispositivos salen de la lista: `DeviceList` a todos;
  - se envían `Settings` y `Status`.
- **Cierre de las interfaces:** el hilo de escaneo ve el hueco y avisa a los lectores. Desde esta fase, los lectores esperan con `poll` (250 ms) y no con un `read` bloqueante, para poder cerrar la interfaz y soltar el `EVIOCGRAB`. Los informes que lleguen mientras tanto no van a ningún sitio. El dispositivo vuelve a aparecer en «sin configurar».

### Los índices se congelan

`HidDevice.receiver` es la posición del bloque. Quitar uno que no es el último renumeraría los siguientes mientras sus dispositivos están abiertos y los agentes tienen sus ids. Por eso **quitar deja un hueco**: el índice no se reutiliza mientras el hub siga en marcha, y añadir otro toma el siguiente. Al reiniciar, el hub lee `hub.toml`, ya sin el bloque, y numera desde cero sin huecos. Los ids cambian entonces, como en cualquier reinicio con otros receptores, y las exposiciones siguen porque se guardan por nombre. Es más cómodo que prohibir quitar cualquiera que no sea el último. El coste: con 16 huecos usados hay que reiniciar para añadir otro, y el hub lo dice.

### La escritura de `hub.toml`

- Con `toml_edit`: se conservan los comentarios, el orden y el formato del resto del fichero. El bloque nuevo va al final del array `[[receiver]]`, con un comentario «Added from the widget: vid:pid producto», y con `vid` y `pids` en hexadecimal.
- **Las formas antiguas se convierten:**
  - sin clave `receiver` (el bloque Logitech por defecto, implícito): se escribe primero ese bloque, porque si no el array nuevo lo sustituiría;
  - con una tabla única `[receiver]`: pasa a ser el primer elemento del array.
- **Al añadir o quitar, todos los bloques reciben su `name` escrito.** Si no, quitar `receiver-1` convertiría el `receiver-2` implícito en `receiver-1` al reiniciar, y su exposición, guardada por nombre, cambiaría de receptor.
- La escritura es atómica (`write_private`: temporal, `fsync`, `rename`, 0600) y se hace fuera del lock del router, serializada por su propio mutex. La memoria cambia después. El fichero manda: si alguien lo editó a mano con el hub en marcha, quitar un bloque que ya no está en el fichero se rechaza («restart the hub»), y añadir parte del fichero tal como está.

### HID++

Sigue siendo del primer receptor Logitech (ADR 0030). El router lo calcula al arrancar y lo actualiza cuando el hilo de escaneo engancha la interfaz HID++ de un receptor. Si se quita el receptor HID++, su lector se cierra y el hilo de HID++ se para. Un Logitech añadido después tiene HID++ si es el primer Logitech que queda.

### Protocolo 10

- `UnclaimedReceivers { receivers: Vec<UnclaimedReceiver { vid, pid, name, serial, interfaces, kinds: Vec<DeviceKind> }> }`, de hub a widgets.
- `AddReceiver { vid, pid }` y `RemoveReceiver { receiver }`, de widget a hub.
- `ReceiverResult { ok, message }`, de hub al widget que lo pidió. Los mensajes van en inglés, como los registros del hub, y el widget los muestra precedidos de «Hecho» o «El hub no lo hizo».
- `HubSettings.exposures` deja fuera los huecos.

### `.hidrec`

La reproducción ya no asigna los receptores una sola vez. En cada pasada sincroniza los dispositivos grabados con los bloques del router (`Replay::sync`):
- añade los que un bloque reconoce;
- quita los de un bloque quitado;
- anuncia los demás como «sin configurar».

`fixtures/two-receivers.hidrec`, con una configuración de solo el bloque Logitech, sirve como grabación con un dispositivo sin configurar. El test del router la usa para añadir y quitar el segundo receptor de punta a punta. No hace falta otra grabación.

## Consecuencias

- Un dongle nuevo se añade desde el widget en unos segundos, sin SSH y sin cortar la entrada de los demás.
- `PROTO_VERSION` 10 y versión 0.8.0: todos los nodos se actualizan a la vez.
- Editar un bloque existente (pids, nombre) sigue siendo a mano.

## Descartadas

- **Adoptar automáticamente** cualquier HID nuevo: capturaría un teclado de mantenimiento.
- **Prohibir quitar un bloque que no sea el último:** obliga a quitar en orden inverso.
- **Renumerar en caliente:** cambiaría los ids de dispositivos abiertos en todos los agentes.
- **`pids = []`** (todo el fabricante) en el bloque añadido: tomaría productos que el usuario no ha visto, y se solaparía con otros bloques de la marca.
- **Añadir el bloque como texto al final del fichero:** no sirve con la tabla `[receiver]` antigua ni sin clave `receiver`, y no permite quitar un bloque.
