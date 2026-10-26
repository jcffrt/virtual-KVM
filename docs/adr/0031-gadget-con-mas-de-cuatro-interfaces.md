# ADR 0031 — Gadget USB con más interfaces de las que admite el kernel

- Estado: aceptada (2026-10-26). Defecto: [fase6-plan.md](../fase6-plan.md), «Defecto del gadget con más de cuatro interfaces».

## Contexto

Con dos receptores en el hub (ADR 0030), el Logitech C52B con 3 interfaces y el Areson/Compx `25a7:fa09` con 2, el hub anuncia 5 dispositivos. El agente gadget creaba una función `hid.usb<id>` por dispositivo, y la quinta fallaba: `Mkdir(".../functions/hid.usb17"): No such device`. `usb_f_hid` reserva sus números menores al crear cada instancia, y solo tiene **`HIDG_MINORS = 4`** (`drivers/usb/gadget/function/f_hid.c`). El error tiraba todo el gadget: el PC se quedaba sin teclado ni ratón. El apaño en la Zero fue `--no-hidpp`: 4 interfaces (ids 0, 1, 16, 17).

El límite es del módulo, no del gadget: cuenta las instancias de **todos** los gadgets de la placa. Otro gadget HID puede dejar menos de cuatro libres.

## Decisión

### Qué entra

Cuando hay más interfaces candidatas que sitio, el gadget elige por **prioridad**, según las colecciones de aplicación del report descriptor:

| Prioridad | Interfaz | Ejemplos |
|---|---|---|
| 0 | Alguna colección Generic Desktop de teclado (`0x01/0x06`), keypad (`0x07`), ratón (`0x02`) o puntero (`0x01`) | Teclado del receptor; ratón del receptor, aunque también lleve consumer y system control |
| 1 | Alguna otra colección estándar (página < `0xFF00`) | Solo multimedia (Consumer `0x0C/0x01`), solo control del sistema |
| 2 | Solo colecciones de fabricante (página ≥ `0xFF00`), o descriptor que no se entiende | HID++/DJ del Logitech |

- **Desempate por id del hub:** primero el id más bajo. Como el id es `16 × receptor + interfaz` (ADR 0030), gana el primer bloque `[[receiver]]` de `hub.toml`, y dentro de él la interfaz más baja.
- Las elegidas se **enlazan en orden de id**, como antes, para que los números de interfaz sigan el orden del receptor. Si la que sale es la última de un receptor (el HID++ es la 2), las demás conservan su número.
- **Identidad del gadget sin cambios:** VID/PID, fabricante y producto son los del dispositivo de id más bajo, aunque una de sus interfaces se quede fuera.
- `--no-hidpp` sigue ocultando el HID++ **a propósito**. Esas interfaces no cuentan como «no expuestas».
- El límite es `Options.max_functions`, que vale `HIDG_MINORS` (4). Los tests lo cambian.

Con los dos receptores de la Zero quedan los ids 0, 1, 16 y 17, y queda fuera el 2 (HID++). El resultado es el mismo que con el apaño, pero sin `--no-hidpp`: con un solo receptor el HID++ vuelve a estar.

### Un `mkdir` fallido no tira el gadget

- Solo un **`ENODEV`** al crear una función significa que `usb_f_hid` no tiene más instancias.
  - **En la función `k` (k > 0):** el gadget vuelve a planificar con un límite de `k` (misma prioridad, `k` interfaces), desmonta lo creado y construye de nuevo. Avisa en el log: `the kernel has room for only k HID functions`.
  - **En la primera función:** no queda ninguna libre, porque otro gadget las tiene todas. No hay gadget, y el log lo dice (`no free HID function: another gadget holds all of usb_f_hid's instances`).
- **Cualquier otro error** (`EBUSY`, `ENOMEM`, un resto que no es un directorio, falta de `libcomposite` o de ConfigFS) no es un límite y no encoge el gadget. Se avisa en el log y se desmonta al momento el árbol a medias, para que no retenga instancias. Ese intento queda sin gadget. La siguiente `DeviceList`, aunque sea la misma, vuelve a probar la lista completa.
- El límite aprendido vale **solo para esa lista de dispositivos**:
  - si llega la misma lista (el hub se reconecta), se conserva el gadget, sin volver a enumerar en el PC;
  - si llega una lista distinta, se reintenta con el límite completo.
  Así, si otro gadget libera un número menor, basta con que cambie el `DeviceList` para recuperarlo. No se reintenta periódicamente: cada intento desmonta el gadget y el PC lo vería desconectarse.
- Si la lista nueva da el mismo gadget (por ejemplo, solo se añade otra interfaz que tampoco cabe), no se reconstruye.

### Aviso en el log y en el widget

- El gadget registra un aviso con las interfaces que no llegan al PC: `not presented to the PC on the USB cable: 2 (Logitech USB Receiver, interface 2)`.
- **Protocolo 9**, un cambio mínimo:
  - `Control::Unexposed { devices: Vec<u8> }` (agente → hub): los ids del `DeviceList` que el agente no puede presentar. Se envía solo cuando ese conjunto cambia, también cuando pasa a vacío. La primera vez de cada sesión se envía solo si no está vacío.
  - `NodeInfo.unexposed: Vec<u8>` en `HubStatus`. El hub lo guarda por nodo, lo olvida al desconectarse y emite un `Status` nuevo cuando cambia. De un nodo que no es `Agent` lo ignora.
  - En agent-core, `HidSink::unexposed()` (vacío por defecto) se consulta tras cada `DeviceList` y tras `devices_ready`. Solo el gadget lo implementa.
- El widget, en la tabla «Equipos», muestra bajo el agente la línea «No llegan a su PC por USB:», con el receptor, la interfaz y su tipo: «logitech, interfaz 2 (HID++ (vendor))».

Se descartó una marca en `DeviceList`, porque ese mensaje va del hub a los nodos y la información nace en el agente. Ponerla en `Hello`/`Screens` tampoco sirve: `Hello` se envía antes del `DeviceList`, y `Screens` es de los helpers.

## Consecuencias

- Con dos receptores el PC del gadget recibe el teclado y el ratón de los dos. Pierde el HID++ del Logitech, que en un PC sin software Logitech no hace nada (ADR 0007), y el aviso lo deja a la vista.
- Con tres receptores de teclado y ratón (6 interfaces de prioridad 0) se quedan fuera las del tercero. Para eso no hay arreglo en el gadget. La alternativa es exponer ese receptor en exclusiva a otro agente (ADR 0030).
- `PROTO_VERSION` 9 y versión 0.7.0: hub, agentes, gadget, helpers y widget se actualizan a la vez.
- En la Zero se puede quitar `--no-hidpp` de `/etc/default/rawkvm-agent-gadget`, salvo que el PC sea Linux (ADR 0007).

## Descartadas

- **Varios gadgets o configuraciones para pasar de cuatro:** el límite cuenta todas las instancias del módulo, no las de cada gadget.
- **Interfaces compuestas (fundir dos descriptores en uno con report IDs):** el PC dejaría de recibir los bytes tal cual del receptor. Eso contradice el principio del proyecto, y un descriptor sin report IDs no puede fundirse sin reescribir los informes.
- **Elegir por número de interfaz o por orden de receptor solamente:** con un receptor de tres interfaces primero, el HID++ entraría antes que el ratón del segundo.
- **Reintentar el límite completo periódicamente:** cada intento fallido re-enumera el gadget en el PC.
