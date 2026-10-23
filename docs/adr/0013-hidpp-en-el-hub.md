# ADR 0013 — Transporte HID++ en el hub y perfil de SO del teclado

- Estado: aceptada (2026-10-23), Fase 3 bloque B. La parte C (cambiar el perfil al conmutar) depende de la prueba en la Pi.

## Contexto

Para sustituir el parche Alt/GUI (ADR 0011) y emparejar desde el widget, el hub tiene que hablar HID++ con el receptor y sus dispositivos por la interfaz 2 (`hidraw2`), que hoy reenvía entera al agente activo. El brief pedía `HOSTS_INFO` (0x1815) `setHostOsVersion`, pero Solaar solo implementa sus funciones 0, 1, 3 y 4. Las de SO (7 `getHostOsVersion`, 8 `setHostOsVersion`) solo se conocen por la ingeniería inversa de Logi Options+ que publica OpenLogi (`crates/openlogi-hidpp/src/feature/hosts_info.rs`), sin captura en hardware. En cambio, `MULTIPLATFORM` (0x4531), que es lo que cambia la disposición de teclas al pulsar Fn+P / Fn+O, sí está en Solaar (ajuste «Set OS») y en un parche del kernel (`hid-logitech-hidpp`, 2026).

## Decisión

- `hub::hidpp`: petición y respuesta correlacionadas igual que Solaar `base.request` y el kernel. La respuesta debe traer B1..B3 de la petición (y B4 en las lecturas de páginas de 0xB5); los errores son `8F` (1.0) y `FF` (2.0). Software id **0x0C**: no lo usan el kernel (0x01), Solaar (0x0B), el firmware (0x0F) ni otras herramientas conocidas, y las notificaciones (id 0) nunca encajan. Una petición en vuelo a la vez, 300 ms y 2 reintentos. Las peticiones bloquean el hilo que llama (CLI o una tarea propia), nunca un hilo lector.
- En el hub en marcha, el lector de la interfaz HID++ entrega primero cada informe a `Hidpp::on_report`. Las respuestas a nuestras peticiones no se reenvían; todo lo demás sigue yendo al agente activo como antes. Se escribe por el mismo `HidrawWriter` de la Fase 3 A.
- Lectura de ranuras: `0xB5` páginas `0x20+n-1` (PID inalámbrico big endian, tipo), `0x30+n-1` (serie) y `0x40+n-1` (nombre); ping de IRoot para la versión (un error `01` significa HID++ 1.0; `04`, `08` y `09` significan no alcanzable), y `getFeature` para 0x1815, 0x4531 y 0x4530.
- Perfil de SO, en este orden (`--hidpp-via auto`):
  1. `MULTIPLATFORM` 0x4531, verificado en código: función 0 (flags; `0x02` = se puede fijar; nº de descriptores; plataforma actual en el byte 6), función 1 (descriptor: plataforma, máscara de SO), función 3 `FF <plataforma>`.
  2. `HOSTS_INFO` 0x1815 función 8, sin verificar: solo si el bit `0x10` del byte 0 de la función 0 lo anuncia. Tipos: 1 Windows, 3 Linux, 6 macOS.
  3. `DUALPLATFORM` 0x4530, función 2 (0 iOS/macOS, 1 Android/Windows).
- **CLI antes que router**: `--hidpp-list`, `--hidpp-set-os <ranura> <so>`, `--hidpp-call` (sondeo crudo) y `--hidpp-trace` (para grabar fixtures). El router no cambia el perfil hasta que la prueba en la Pi diga qué feature tiene el MX Keys y si cambia en caliente.

## Consecuencias

- Si la prueba sale bien, C llamará a `set_os` al activar cada agente y `swap_alt_gui` quedará solo como alternativa.
- Si el teclado solo aplica el cambio al reconectar, o lo ignora, se mantiene ADR 0011 y queda documentado.
- Los intercambios reales se graban con `--hidpp-trace` en `fixtures/hidpp/` para los tests.
