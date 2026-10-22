# ADR 0004 — Reenvío redundante de informes que no son de ratón

- Estado: aceptada (2026-10-22)

## Contexto

Los informes viajan en datagramas QUIC, que pueden perderse en Wi‑Fi. Un informe de teclado es un estado completo, así que una pérdida se corrige con el siguiente… salvo si se pierde el último (la liberación de una tecla): la tecla queda pulsada en el destino y autorrepite. El handoff pide "nunca descartar teclado".

## Decisión

- El hub envía cada informe que no sea de ratón (teclado, consumer, system, HID++) dos veces: inmediatamente y a los `resend_ms` (15 ms; 0 lo desactiva), con el mismo `seq`.
- El agente descarta todo datagrama cuyo `seq` no sea más nuevo que el último de ese dispositivo (duplicados y llegadas tardías). Aplicar un estado de teclado antiguo después de uno nuevo dejaría teclas pulsadas.
- Los informes de ratón (colección Generic Desktop / Mouse) no se repiten: una copia tardía de movimiento relativo no sirve.

## Consecuencias

- El tráfico de teclado se duplica (despreciable: decenas de bytes por pulsación).
- Un informe perdido y su copia perdida siguen siendo posibles; `Deactivate` libera todo de forma fiable al cambiar de destino. Si en la validación se ven teclas pegadas, la siguiente medida sería enviar el estado de teclado por un stream fiable.
- Como `seq` es por dispositivo, un informe consumer perdido cuya copia llega después de un informe de ratón posterior se descarta. Aceptable en la Fase 0.
