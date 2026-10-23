# ADR 0014 — Disposición del teclado por HID++ al conmutar

- Estado: aceptada (2026-10-23), Fase 3 bloque C. Sustituye en la práctica a ADR 0011.

## Contexto

La prueba de la Fase 3 B en la Pi (`fixtures/hidpp/`) dio estos resultados con el MX Keys de la ranura 3 (HID++ 4.5):
- Tiene `MULTIPLATFORM` (0x4531) modificable, con cuatro plataformas: Windows, macOS, iOS y Android. **No hay plataforma Linux.**
- `setCurrentPlatform` cambia la disposición **en caliente**: el teclado lo confirma al momento con una notificación, y Alt/Win quedan en su sitio en el Windows. Al volver a macOS ocurre lo contrario.
- `HOSTS_INFO` 0x1815 (función 8) no hizo falta.
- El MX Ergo tiene `HOSTS_INFO` pero no `MULTIPLATFORM`; el K230 estaba dormido.

## Decisión

- Un hilo `receiver_ctl` es dueño del canal HID++ cuando la interfaz está abierta. Al arrancar activa las notificaciones del receptor (registro `0x00` = `00 09 00`, como Solaar y el driver DJ) y lee las seis ranuras. El router le manda la disposición que toca en cada conmutación. El hilo nunca bloquea el reenvío, y de una ráfaga de cambios solo aplica el último.
- Mecanismo: **MULTIPLATFORM**, función 3 `FF <plataforma>`, en cada dispositivo que la tenga modificable y esté conectado. No escribe si ya está en esa plataforma. Si un fallo deja el dispositivo como no alcanzable, se avisa una vez por ranura y mensaje, sin reintentos en bucle. Un dispositivo dormido recibe la disposición cuando se reconecta (notificación `0x41`).
- La disposición sale del SO del agente activo (`Hello.os`): Windows → Windows, macOS → macOS, y **Linux → Linux si el teclado la tiene, si no Windows** (el usuario usa en Ubuntu la misma disposición que en Windows). `Other` → no tocar.
- Configuración:
  - `[hidpp] os_profile = true` (por defecto).
  - `[[node_options]] os_profile = "windows" | "macos" | "linux" | "none"` para forzar otra disposición en un agente.
  - `swap_alt_gui` sigue existiendo (ADR 0011) para teclados sin MULTIPLATFORM. El hub avisa si las dos opciones están activas a la vez, porque entonces Alt y GUI vuelven a quedar cruzadas.
- `ReceiverDevices.os_profile` informa de la disposición actual a los widgets (pestaña Dispositivos).

## Consecuencias

- El MX Keys sigue al equipo activo sin tocar los informes, que vuelven a reenviarse crudos.
- Teclados sin MULTIPLATFORM (el K230) no cambian.
- Logi Options+ en un PC no ve el teclado (sigue fuera de alcance).
