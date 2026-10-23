# ADR 0019 — Atajo de emergencia configurable

- Estado: aceptada (2026-10-23), Fase 4.2.

## Contexto

El atajo era fijo: Ctrl, Ctrl y después 1..4 (`hotkey: bool` en `HubSettings` y en `[switching]`). El brief pide elegir el modificador y añadir «volver al anterior» y «liberar teclas».

## Decisión

- **Protocolo v6**: `HubSettings.hotkey` pasa a ser `Hotkey { enabled, modifier, previous, release_keys }`, con `modifier` `Ctrl | Shift | Alt | Gui` (en TOML `"ctrl"`, `"shift"`, `"alt"`, `"gui"`). Por defecto: Ctrl y las dos opciones activadas.
- **Detección** (`hub::hotkey::Detector`): igual que antes, pero con la máscara del modificador elegido. Ctrl es `0x11`, Mayús `0x22`, Alt `0x44` y GUI `0x88`, izquierda o derecha. Una pulsación cuenta si se pulsa y suelta en 400 ms sin nada más; dos en 500 ms arman el atajo durante 1,5 s. La tecla siguiente devuelve una `Action`:
  - 1..4 (fila o teclado numérico): `Agent(n)`.
  - 0 (fila o teclado numérico), si `previous`: `Previous`.
  - Esc, si `release_keys`: `ReleaseKeys`.
  - Cualquier otra tecla desarma el atajo y llega al equipo.
  - Las teclas que completan el atajo no se reenvían.
- **«Anterior»** es el agente conectado que estuvo activo más recientemente, sin contar el actual. Sale de `history`, la misma lista que usa la vuelta atrás al caer el agente activo. Repetir «0» alterna entre los dos últimos.
- **«Liberar teclas»** usa `release_everywhere`, lo mismo que el botón del widget.
- **`hub.toml`, en `[switching]`**: `hotkey` (bool, como antes), `hotkey_modifier`, `hotkey_previous` y `hotkey_release_keys`.
- **`settings.toml`**: el que escribía un hub v5 tiene `hotkey = true|false` y se sigue leyendo (deserialización `untagged`); el resto de opciones del atajo quedan por defecto.
- **Widget**:
  - en Opciones, selector de tecla y dos casillas;
  - en la pestaña Hub, una columna «Atajo» (`Ctrl, Ctrl, 2`);
  - en el menú de la barra, una línea con el atajo y sus extras.

## Consecuencias

- Las dos pulsaciones del modificador siguen llegando al equipo activo (el hub no las retiene: no sabe todavía si serán un atajo). Con Ctrl o Mayús no hacen nada. Con Alt o GUI, Windows abre el menú Inicio o marca el menú de la ventana. El widget lo avisa al elegirlos.
- Un «0» o un Esc tras dos pulsaciones del modificador ya no llegan al equipo si su opción está activa.
