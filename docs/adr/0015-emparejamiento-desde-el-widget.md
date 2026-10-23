# ADR 0015 — Emparejamiento desde el widget

- Estado: aceptada (2026-10-23), Fase 3 bloque D.

## Contexto

Emparejar un teclado o ratón nuevo con el receptor exigía hasta ahora conectarlo a un PC con Solaar o Logi Options. El hub ya tiene el canal HID++ (ADR 0013) y el hilo del receptor (ADR 0014).

## Decisión

- Protocolo v4:
  - `ReceiverDevices { devices }` (hub → widgets, al conectar y al cambiar), con ranura, nombre, tipo, PID inalámbrico, si está conectado, si admite disposición y la disposición actual.
  - `Unpair { slot }` (widget → hub).
  - `PairingStart`, `PairingStop` y `PairingEvent` ya existían.
- Registros del receptor, contrastados con Solaar `receiver.py` (`set_lock`, `unpair_device`) y `notifications.py`:
  - Abrir: `0xB2` `01 00 <timeout>`, 30 s desde el widget.
  - Cerrar: `02 00 00`. Desemparejar: `03 <ranura> 00`.
  - Notificaciones `0x4A` (bloqueo abierto o cerrado, con el código de error), `0x41` (conexión de una ranura) y `0x40 02` (desemparejado).
- Eventos:
  - `Started` al abrir.
  - `DeviceFound` cuando se conecta una ranura que no estaba.
  - Al cerrarse el bloqueo: `Paired` si se encontró algo; `Failed` con el motivo si el receptor da error; `TimedOut` si no apareció nadie.
  - Si el receptor no cierra en el tiempo pedido más 5 s, `TimedOut`.
- Seguridad: solo los nodos `Widget`, que ya presentaron el token, pueden emparejar, desemparejar o cambiar ajustes. Las órdenes de agentes y helpers se ignoran.
- Modo A: emparejar no cambia `DeviceList`, porque los dispositivos son las interfaces del receptor y un teclado nuevo escribe por ellas sin más. Comprobado en el test del router.
- Widget, pestaña Dispositivos: lista con «Olvidar» (doble clic de confirmación, porque WKWebView no tiene `confirm()`), botón «Emparejar» con cuenta atrás y el resultado.

## Consecuencias

- Un dispositivo nuevo se empareja y se olvida sin conectar el receptor a otro PC.
- Sin probar en hardware: el MX Keys y el MX Ergo ya están emparejados, así que probarlo exige emparejar uno de nuevo o el K230 tras olvidarlo.
