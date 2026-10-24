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

## Revisión (2026-10-23): "desconectado" ya no es definitivo

Visto en la Pi con el hub v6 (778f633). Un sondeo de batería falló mientras el MX Keys estaba dormido para HID++. El hub lo marcó como no conectado, sin aviso en el journal. Su enlace con el receptor seguía activo, así que no llegó ninguna `0x41`, y desde entonces el hub dejó de cambiarle la disposición al conmutar Mac↔Windows. Era una trampa de un solo sentido: los cambios y los sondeos solo atendían a los dispositivos conectados, y la única salida era una `0x41` con el enlace activo.

Ahora "conectado" significa "respondió a la última petición", y deja de ser algo que solo puede deshacer una `0x41`:

- Al conmutar se escribe la disposición también en los dispositivos marcados como desconectados. Si responde, vuelve a estar conectado. Si no, queda igual, solo se registra en debug y se reintenta en el siguiente cambio.
- El sondeo de batería (cada 5 min) también lee los dispositivos desconectados. Si uno responde, vuelve a estar conectado (registro info) y recibe la disposición que se perdió. Las ranuras que estaban dormidas en el arranque, sin índices de funciones, se vuelven a leer en cada sondeo.
- Cuando un dispositivo conectado no responde al sondeo, se avisa con warn, una vez hasta que vuelva.
- Coste: un dispositivo de verdad apagado recibe una petición por cambio y una por sondeo. Cada una cuesta como mucho el tiempo de espera de HID++ (300 ms por intento) en el hilo del receptor, fuera del camino de los informes.
