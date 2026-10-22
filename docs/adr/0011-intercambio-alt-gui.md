# ADR 0011 — Intercambio opcional de Alt y GUI por agente destino

- Estado: aceptada (2026-10-22), como **parche provisional** hasta la Fase 3 (HID++).

## Contexto

El MX Keys guarda un perfil de sistema operativo por canal del receptor. En el canal del receptor de la Pi tiene el perfil macOS, así que envía las teclas físicas Opción/Alt y Cmd/Win con los usages que espera un Mac. En el Mac va bien, pero en el Windows, por el gadget, Alt y Win quedan cruzadas. La solución de verdad es cambiar el perfil del teclado por HID++ al conmutar, como hace Logitech Options: feature `HOSTS_INFO` (0x1815), `setHostOsVersion`. Es el **primer objetivo de la Fase 3**.

## Decisión

- `hub.toml`, por índice de agente (los índices son estables, ADR 0008), desactivado por defecto:
  ```toml
  [[node_options]]
  index = 1
  swap_alt_gui = true
  ```
  Va en `hub.toml` y no en `nodes.toml` porque es configuración del usuario: `nodes.toml` lo reescribe el hub.
- `KeyboardLayout::swap_alt_gui` (proto), aplicado por el router solo a los informes de la interfaz de teclado (los que encajan con el `KeyboardLayout` del descriptor y su report ID) y solo cuando el destino activo tiene la opción:
  - byte de modificadores: LeftAlt 0x04 ↔ LeftGUI 0x08 y RightAlt 0x40 ↔ RightGUI 0x80;
  - en el array de teclas, los usages 0xE2 ↔ 0xE3 y 0xE6 ↔ 0xE7;
  - el resto del informe y los informes de otras interfaces o report IDs no se tocan.
- El atajo de emergencia se evalúa sobre el informe **original**, antes del intercambio. La copia redundante lleva los mismos bytes ya intercambiados. El release-all no cambia: son ceros.
- Al arrancar, el hub registra un aviso por cada agente con la opción activa.

## Consecuencias

- Es una **desviación explícita del reenvío crudo**, limitada al agente configurado.
- Afecta a **todos los teclados que entran por esa interfaz**, también al K230: si alguno ya tiene el perfil correcto para ese equipo, quedará cruzado él.
- Se retira cuando la Fase 3 cambie el perfil del teclado por HID++.
