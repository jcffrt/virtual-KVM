# ADR 0009 — Cambio por borde: helpers de borde, layout en el hub, protocolo v2

- Estado: aceptada (2026-10-22). Hechos: pasos 1 (protocolo y hub) y 2 (`agent-core::edge`), y el helper macOS (`crates/edge-macos`). Helper Windows (`crates/edge-windows`) también hecho. Pendiente: Linux (extensión GNOME).

## Contexto

Arquitectura definitiva: el PC Windows recibe por el gadget USB y no ejecuta nada; el agente macOS es un LaunchDaemon root, fuera de la sesión gráfica. Ninguno de los dos puede leer ni mover el puntero. El usuario quiere poder cambiar la disposición de monitores y PCs en cualquier momento desde el widget, sin reiniciar nada. Disposición actual, de izquierda a derecha: Mac integrada, Mac externa 2560×1440, Windows (gadget), Ubuntu (más adelante).

## Decisión

1. **Helper de borde**: nodo propio `NodeKind::EdgeHelper { for_node }` que corre en la sesión del usuario y actúa en nombre del agente `for_node`. El hub:
   - le da índice 0 y no lo trata como agente: no recibe informes, el atajo no lo selecciona y no hereda la entrada si cae el activo;
   - atribuye sus `Screens` al agente;
   - acepta su `SwitchTo` solo si su agente es el activo;
   - le reenvía los `Activate`/`Deactivate` de su agente, y un `Activate` sin borde al conectarse si su agente ya está activo.
2. **Protocolo v2** con rechazo claro de nodos viejos. `SwitchTo` y `Activate` llevan `screen` porque el Mac tiene dos pantallas. `Layout` lleva `options`. `Hello` y las variantes hasta `Reject` quedan congeladas para que siempre se pueda responder `VersionMismatch`.
3. **El layout vive en el hub**:
   - Semilla en `hub.toml` por índice de agente (los índices son estables, ADR 0008):
     ```toml
     [[layout]]
     from = 2          # Mac
     edge = "right"
     to = 1            # Windows (gadget)

     [[layout]]        # Ubuntu, cuando exista; mientras no reporte pantallas se ignora
     from = 1
     edge = "right"
     to = 3
     ```
     Opcionales: `from_screen`, `to_screen`, `from_range`, `to_range` (fracciones 0..1 del borde) y `oneway`. Cada enlace funciona en los dos sentidos salvo `oneway = true`.
   - **Regla de pantalla por defecto**: sin `from_screen`, el enlace sale de la pantalla del nodo más exterior en la dirección del borde (para `right`, la de borde derecho más a la derecha). Sin `to_screen`, entra por la pantalla del destino más exterior en el lado opuesto (para `right`, su pantalla más a la izquierda). Los empates se resuelven por menor índice. Con el Mac, `right` elige la externa sin configurar nada.
   - Un `Layout` de un widget es la fuente de verdad desde ese momento. Se persiste en `<state_dir>/layout.toml`, se reenvía a todos al instante y en el siguiente arranque tiene prioridad sobre `hub.toml`. `--load-layout` más `systemctl reload` (SIGHUP) permite probarlo antes de que exista el widget.
   - Los helpers no guardan layout: aplican el último `Layout` recibido.
4. **Opciones** (`[edge]` en `hub.toml`, o en el layout del widget):
   - `push_ms = 0`: cambio inmediato al tocar el borde (decisión del usuario); con más de 0, el puntero tiene que quedarse ese tiempo en el borde.
   - `corner_pct = 3`: los extremos de cada borde no conmutan.
   - `dead_zone_px = 2`.
5. **Reglas del helper** (`agent-core::edge::EdgeSwitcher`, lógica pura):
   - Tocar el borde es estar en su último píxel dentro de una pantalla propia y dentro del rango del enlace.
   - Un solo `SwitchTo` por contacto: si el hub lo ignora, no se repite hasta que el puntero se separe del borde.
   - Al entrar, el puntero se coloca a `dead_zone_px` del borde, a la altura `pos_ratio`, y ese borde queda desarmado hasta que el puntero se aleja más que eso.
   - Al desactivarse, el puntero se aparca en el centro de su pantalla.
   - En el Windows el gadget sigue inyectando movimiento relativo: el helper solo coloca el puntero al entrar.

## Helper macOS

- `agent-core::helper::run_helper` hace la sesión común: handshake como `EdgeHelper`, `Screens` al conectar y cada 2 s si cambian, sondeo cada 8 ms solo mientras su agente está activo, y `warp` en `Activate`/`Deactivate`. Cada SO aporta un `EdgePlatform` (pantallas, posición del puntero, warp).
- macOS (`crates/edge-macos/src/cg.rs`), todo sin permisos TCC:
  - pantallas con `CGGetActiveDisplayList`/`CGDisplayBounds`, en puntos globales con origen en la esquina superior izquierda de la principal;
  - escala con `CGDisplayModeGetPixelWidth`;
  - posición con `CGEventGetLocation` de un evento nulo;
  - warp con `CGWarpMouseCursorPosition` + `CGAssociateMouseAndMouseCursorPosition(1)` y supresión de eventos locales a 0 (si no, macOS ignora el ratón unos 250 ms tras el warp).
- Índice de pantalla: la principal 0 y las demás de izquierda a derecha.
- LaunchAgent por usuario (`LimitLoadToSessionType Aqua`) con configuración propia (`edge.toml`). El instalador la rellena, con sudo, desde la del agente (`--from-agent`), que es solo de root.
- En este Mac, con `--show`: integrada 1470×956 a 2x en (0, 0) y externa 1720×1440 a 1x en (1470, −251).

## Helper Windows

- `crates/edge-windows`, binario `rawkvm-edge.exe` (subsistema GUI, sin ventana; `AttachConsole` para los comandos):
  - DPI por monitor v2, así que todo va en píxeles físicos del escritorio virtual;
  - monitores con `EnumDisplayMonitors`/`GetMonitorInfoW` y escala con `GetDpiForMonitor`;
  - lectura con `GetCursorPos` (sondeo, no `WH_MOUSE_LL`) y colocación con `SetCursorPos`.
- Sin driver, sin servicio (un servicio vive en la sesión 0 y no ve el escritorio) y sin administrador. Arranque con `HKCU\...\Run`. El programador de tareas «al iniciar sesión» pide administrador.
- `for_node` es el `node_id` del gadget, que se lee en la Pi con `rawkvm-hub --list-nodes`. El PC no tiene ningún fichero del agente.
- Se compila con MinGW en el job `edge-windows` del CI (artefacto `rawkvm-edge-windows`), sin el WDK.

## Alternativas descartadas

- `on_behalf_of: Option<Uuid>` suelto en `Hello`: admitiría combinaciones sin sentido (un widget en nombre de alguien).
- Layout guardado en cada helper: el widget no podría cambiarlo en caliente y habría copias divergentes.
- Que el helper hable con su agente por un socket local en vez de con el hub: el PC del gadget no tiene agente.

## Consecuencias

- Hub, los cuatro agentes y los helpers se actualizan a la vez. Un agente v1 recibe `VersionMismatch` y se detiene hasta reinstalarlo.
- Las pantallas de un agente desconectado se conservan en el hub; un `SwitchTo` hacia él se ignora y el puntero se queda en el borde.
