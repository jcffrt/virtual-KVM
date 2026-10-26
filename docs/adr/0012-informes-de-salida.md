# ADR 0012 — Informes de salida del PC al teclado real (LEDs)

- Estado: aceptada (2026-10-23), Fase 3 bloque A.

## Contexto

El LED de Bloq Mayús del MX Keys no se enciende en ningún PC: los informes de salida que cada SO manda a su teclado virtual se descartaban en los agentes. El receptor está en la Pi, así que el único que puede escribirlos en el teclado es el hub.

## Decisión

- **Transporte**: datagrama QUIC agente → hub con el formato de siempre y el flag nuevo `FLAG_OUTPUT = 0x02`. Es lo que menos cambia y la pérdida se cubre igual que en la otra dirección: cada informe va dos veces (30 ms) con el mismo `seq`, el hub descarta duplicados y atrasados, y el agente reenvía su último estado al empezar cada sesión. Protocolo v4, con los mensajes de la Fase 3 D ya definidos para hacer un solo salto.
- **Hub**: acepta solo de agentes, solo informes que el descriptor de la interfaz declara (report ID y longitud) y de páginas estándar. Los de HID++ (página vendor) no pasan nunca: ese canal es del hub (bloque B). Guarda el último estado por agente; escribe solo el del **activo**, y al activar uno escribe el suyo (o ceros si nunca mandó). Límite de 50 escrituras por segundo e interfaz. Escribe desde un hilo por interfaz con cola acotada, porque un `write()` de hidraw puede ser un `SET_REPORT` por el endpoint de control y no debe frenar el router.
- **Agentes**:
  - Linux: `UHID_OUTPUT` (el camino de los LED de hid-input en uhid, que no tiene `raw_request`) y `UHID_SET_REPORT` de tipo salida, al que se le quita el byte de número de informe que `__hid_request` reserva aunque sea 0.
  - Gadget: lo que entrega `read()` de `/dev/hidgN`. La función tiene endpoint OUT (por defecto), así que el informe llega tal cual por interrupt OUT; con endpoint OUT, f_hid rechaza los `SET_REPORT` de salida.
  - macOS: el daemon de Karabiner 8.6.0 no notifica LEDs al cliente (el protocolo de cliente solo tiene respuestas de estado del driver), pero el driver devuelve cada informe de LED que fija macOS (ID 5) como informe de entrada en la página de LEDs (ID 6). El agente abre el teclado virtual con IOHIDManager sin capturarlo, como Karabiner-Elements, y traduce Bloq Num y Bloq Mayús al byte del teclado boot. Necesita el permiso de Monitorización de entrada; sin él, solo un aviso.
    - **0.8.1:** Karabiner vuelve a crear el teclado virtual cada vez que cambian sus parámetros, y el agente los cambia en cuanto recibe la lista de dispositivos del hub: el teclado toma el vid y el pid del receptor. Antes, el vigilante abría el primer teclado virtual y se quedaba en él. Su `IOHIDManager`, sin programar en un run loop, devolvía siempre la lista inicial, así que nunca veía que ese teclado había desaparecido, y desde el primer arranque no llegaba ningún LED del Mac. Ahora enumera cada 5 s con un `IOHIDManager` nuevo y compara el id de registro del teclado virtual. Si cambia, cierra el viejo y abre el nuevo, con el estado de LEDs puesto a desconocido. Comprobado en macOS 27 con Karabiner 8.6.0: al cambiar el estado de Bloq Mayús del sistema, macOS manda el `SetReport` de LEDs al teclado virtual y su eco (informe 6, página 0x08) llega a un proceso que lo tiene abierto. `CGEventSourceFlagsState` no refleja ese cambio, así que sondear los modificadores no habría servido.

## Consecuencias

- El LED sigue al PC activo, incluido el Mac si tiene el permiso.
- En el Mac el estado no se conoce hasta el primer cambio tras arrancar el agente, y Bloq Despl no existe.
- Logitech Options en un PC sigue sin poder hablar HID++ con el teclado (fuera de alcance).
