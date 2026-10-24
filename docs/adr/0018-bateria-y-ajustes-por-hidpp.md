# ADR 0018 — Batería, velocidad del puntero y Fn por HID++

- Estado: aceptada (2026-10-23), Fase 4.1.

## Contexto

El brief pedía batería por `UNIFIED_BATTERY` 0x1004 y DPI por `ADJUSTABLE_DPI` 0x2201. La enumeración real con IFeatureSet (`fixtures/hidpp/features-slot1.txt`, `features-slot3.txt`) muestra que ni el MX Ergo ni el MX Keys tienen esas dos features. Sí tienen:
- `BATTERY_STATUS` 0x1000, los dos;
- `POINTER_SPEED` 0x2205, el MX Ergo;
- 0x40A3 (`K375S_FN_INVERSION` en Solaar), el MX Keys;
- `BACKLIGHT2` 0x1982, el MX Keys.

## Decisión

Formatos contrastados con Solaar (`hidpp20.py`, `settings_templates.py`, `settings.py`, `notifications.py`, `common.py`):

- **Batería, 0x1000**:
  - Función 0 `getBatteryLevelStatus` → nivel %, siguiente nivel %, estado. Un nivel 0 es desconocido. Estados: 0 descargando, 1 cargando, 2 casi llena, 3 llena, 4 carga lenta, 5–7 errores.
  - El evento 0 (B3 = `0x00`, id de software 0) tiene el mismo formato.
  - El hub la lee al conectarse el dispositivo (en la lectura de la ranura), cada 5 minutos (también de los marcados como desconectados, que vuelven a conectado si responden; ver la revisión del ADR 0014) y en cada evento. Todo en el hilo del receptor, nunca en el camino de los informes.
  - Grabado: MX Ergo `32 14 00` (50 %, siguiente 20 %, descargando), MX Keys `05 00 00` (5 %).
- **Velocidad del puntero, 0x2205**:
  - Función 0 lee un u16 BE en 1/256 (256 = 1×). La función 1 lo escribe.
  - El hub lo limita a 0x2E..=0x1FF, el rango de Solaar.
  - Es un multiplicador, no DPI. El MX Ergo cambia de DPI con su botón y no expone 0x2201.
- **Fn, 0x40A3**:
  - Función 0 con el host actual (función 0 de HOSTS_INFO, byte 3; 0xFF sin HOSTS_INFO) → host, estado. La función 1 escribe host y estado.
  - Estado 1: F1…F12 dan su función especial sin Fn.
- **Protocolo v5**:
  - `PairedDevice` gana `battery`, `pointer_speed` y `fn_swap`.
  - Mensajes nuevos widget → hub `SetPointerSpeed { slot, value }` y `SetFnSwap { slot, on }`, solo de widgets.
  - Reemplazan al `SetDpi` del brief.
- **Eventos**: `receiver_ctl::is_notification` acepta también los eventos HID++ 2.0 de los dispositivos (índice de feature 1..0x3F en B2 e id de software 0 en B3). El hilo del receptor usa solo los de batería. Esos eventos se siguen reenviando a los agentes como antes.
- **Fuera de este bloque**:
  - `BACKLIGHT2` 0x1982: escribirla exige reescribir la configuración entera (modo, efecto, nivel, duraciones; clase `Backlight` de Solaar). No cabe sin crecer bastante.
  - La batería de los dispositivos HID++ 1.0 (el K230): registros `0x07`/`0x0D`, sin dispositivo despierto para grabarlo.

## Consecuencias

- El widget muestra la batería por tramos (no un porcentaje fino) y deja ajustar la velocidad del MX Ergo y las F del MX Keys.
- No sabemos si el dispositivo guarda la velocidad del puntero y el Fn al apagarse. Solaar los vuelve a aplicar al conectar. Si se pierden, el siguiente paso es guardarlos en el hub y reaplicarlos al conectar.
- 0x40A3 grabado en el MX Keys (`mxkeys-40a3-0.txt`): `00 01 01 01`, es decir, host 0 y estado 1 (los dos bytes siguientes no se usan). El valor de 0x2205 en los tests sigue siendo inventado (el índice sí es el real): falta grabarlo con el MX Ergo despierto.
