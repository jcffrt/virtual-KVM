# ADR 0010 — Widget (Tauri 2) y protocolo v3

- Estado: aceptada (2026-10-22). Plan: `docs/widget-plan.md`. Paso 1 (`agent-core::canvas`, `agent-core::widget`) y paso 2 (v3) hechos.

## Contexto

El usuario quiere poder cambiar en cualquier momento, desde un widget, la posición relativa de monitores y PCs, y cambiar de equipo desde la bandeja. El layout ya vive en el hub (ADR 0009). La primera entrega es solo para el Mac, con Svelte.

## Decisión

1. **Nada de protocolo en TypeScript**. `agent-core::widget` lleva la sesión `Widget` con reconexión y TOFU, y mantiene el estado (`WidgetState`). `agent-core::canvas` hace la geometría pura:
   - bloques rígidos por nodo en unidades lógicas;
   - enlaces entre lados que se tocan de nodos distintos, con los rangos del tramo solapado;
   - imán;
   - disposición inicial a partir de los enlaces en vigor.

   La app Tauri expone esto como comandos y eventos; Svelte solo dibuja.
2. **Protocolo v3**, todo junto para un único redespliegue:
   - `Layout.canvas: Vec<Placement>`: el lienzo, persistido por el hub en `layout.toml` con los enlaces.
   - `Entry { screen, edge, pos_ratio }` en `proto`, y `SwitchTo { target, entry: Option<Entry> }` / `Activate { entry: Option<Entry> }`. El cambio desde la bandeja va sin `entry` (el destino conserva su puntero) y sin histéresis.
   - `Screen`: `x/y/w/h` en las unidades del helper (puntos en macOS, píxeles físicos en Windows). `scale` = unidades nativas por unidad lógica, solo para el lienzo: el helper macOS pasa a mandar 1.0; `--show` sigue mostrando el factor Retina.
   - `HubStatus.version` (versión y commit del hub) y `NodeInfo.screens` (0 = agente sin helper, en gris en la pestaña Hub).
   - `Control::Settings(HubSettings { hysteresis_ms, hotkey })`: el hub lo envía a los widgets; si un widget lo cambia, el hub lo aplica y lo guarda en `<state_dir>/settings.toml`, que manda sobre `[switching]`.
3. El widget guarda solo su configuración local (hub, token, huella) en `~/Library/Application Support/rawkvm/widget.toml`, nunca layout. Log en `~/Library/Logs/rawkvm-widget.log`.

## Consecuencias

- Redespliegue conjunto de hub, gadget, agente y helper del Mac, y helper de Windows (reinstalar el `.exe`). Los nodos v2 reciben `VersionMismatch`.
- Fuera de v3: nombres de monitor, versión por nodo (iría en `Hello`, congelado) y HID++ (fase 3).
