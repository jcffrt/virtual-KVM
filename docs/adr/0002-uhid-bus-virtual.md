# ADR 0002 — El agente Linux crea los dispositivos uhid con `BUS_VIRTUAL`

- Estado: aceptada (2026-10-22). Pendiente de confirmar en el Ubuntu real.

## Contexto

El agente recrea cada `HidDevice` del hub con `UHID_CREATE2`, con el mismo descriptor, nombre y VID/PID (`046D:C52B` en modo `receiver`). El kernel asocia drivers por bus+VID+PID: con `BUS_USB`, `hid-logitech-dj` reclamaría el "receptor" virtual, lo pondría en modo DJ por HID++ (que nuestro uhid no implementa) y descartaría los informes crudos de teclado y ratón. En modo `dj-children` (VID/PID del dispositivo, p. ej. `046D:408A`) sería `hid-logitech-hidpp` quien se asociaría.

## Decisión

Por defecto `BUS_VIRTUAL` (0x06). Ningún driver específico de Logitech declara ese bus, así que se asocia `hid-generic` y los informes llegan a `hid-input` tal cual. VID, PID y nombre se conservan: `evtest` y libinput ven "Logitech USB Receiver". `--bus usb` queda como opción para experimentar.

Módulo `uhid` propio en lugar del crate `uhid-virt`: son ~150 líneas, sin `unsafe` (los eventos se construyen como bytes en los offsets del ABI, que es fijo), y necesitamos responder `UHID_GET_REPORT`/`UHID_SET_REPORT` en un hilo propio para que el kernel no espere 5 s por petición durante el probe. `GET_REPORT` responde `EIO` y `SET_REPORT` éxito, porque no guardamos estado de feature/output reports.

## Consecuencias

- Se pierde lo que aportan los drivers Logitech en el PC destino (batería por HID++, rueda de alta resolución). La rueda y los botones estándar funcionan con `hid-generic`.
- Los informes de salida del host (LED de Bloq Mayús, peticiones HID++) se ignoran en la Fase 0; reenviarlos al hub requeriría un mensaje nuevo.
- `/dev/uhid` debe ser escribible por el usuario: regla udev `GROUP="input"`. Ser del grupo `input` permite leer todos los teclados del sistema; es el compromiso habitual en escritorios de un solo usuario. Si preocupa, cambiar la regla a un grupo dedicado `rawkvm`.
