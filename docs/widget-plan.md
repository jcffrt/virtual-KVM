# Widget (Tauri 2): plan para revisar

Aprobado por el usuario el 2026-10-22, con estas respuestas: v3 sí; primera entrega solo en el Mac; Svelte; cambio rápido desde la bandeja. **Hechos**: paso 1 (`agent-core::canvas`, `agent-core::widget`), paso 2 (protocolo v3, desplegado) paso 3 (esqueleto Tauri, bandeja, pestaña Hub) paso 4 (pestaña Monitores), paso 5 (Dispositivos y Opciones) y paso 6 (empaquetado y CI). Este último se fue haciendo en los anteriores: `.app` firmada ad hoc en el workflow `widget` (artefacto `rawkvm-widget-macos` con LEEME), arranque automático con LaunchAgent (paso 5) y `docs/widget-instalacion.md`. El widget para Windows queda para después. Base: handoff §9, ADR 0009 (el layout vive en el hub) y la Fase 2 validada entre Mac y Windows.

## Qué es

Una app de bandeja / barra de menús, sobre todo para el Mac (funciona igual en Windows). Se conecta al hub como nodo `Widget` y permite:

- ver el estado (hub, nodos, equipo activo, latencia);
- **colocar los monitores de todos los equipos en un lienzo**, cuando se quiera;
- emparejarse con un hub nuevo.

No tiene dispositivo virtual ni lógica de entrada.

## Principio: nada de protocolo en TypeScript

- **Rust (`src-tauri`)** hace todo lo que toca el protocolo, a través de `agent-core`: descubrimiento mDNS, QUIC, TOFU, mensajes `Control` y el cálculo de enlaces desde el lienzo. Lo expone como **comandos Tauri** (petición/respuesta) y **eventos Tauri** (estado empujado al frontend).
- **TS/Svelte** solo dibuja y arrastra. Recibe datos ya digeridos (nodos, pantallas en unidades lógicas, posiciones del lienzo) y devuelve intenciones («mover el bloque del nodo X a (x, y)», «aplicar», «emparejar con este hub y este token»).
- La geometría que decide qué borde enlaza con cuál está en una función Rust pura con tests de tabla, igual que `agent-core::edge`.

## Piezas

### 1. `agent-core::widget` (Rust, sin Tauri, con tests)

- `WidgetClient`: sesión como `NodeKind::Widget` con reconexión. Reutiliza `handshake`, `resolve_hub` y el config TOFU, con su propio `widget.toml`.
  - Expone un canal de eventos: `Status`, `Screens` por nodo, `Layout`, `DeviceList` y conexión/RTT.
  - Expone comandos: enviar `Layout`, `SwitchTo`, `Ping`.
- `canvas::links(placements, screens, opts) -> Vec<EdgeLink>`:
  1. Cada nodo es un bloque rígido con sus pantallas tal como las reporta su helper; el paso interno entre pantallas lo hace su SO.
  2. Las pantallas se normalizan a unidades lógicas: los puntos del Mac tal cual, los píxeles de Windows entre `scale`.
  3. Dos pantallas de **nodos distintos** cuyos bordes se tocan, con una tolerancia de imán, y se solapan a lo largo del borde dan un `EdgeLink` en cada sentido. Los `from_range`/`to_range` son el tramo solapado.
  - Tests de tabla con el escritorio real: integrada 1470×956 @2x y externa 1720×1440 del Mac, los dos monitores del Windows, y Ubuntu a la derecha más adelante.
- Test e2e contra el hub real: el widget envía `Layout`, el hub lo persiste en `layout.toml` y los helpers lo reciben. Ya existe una prueba parecida con helpers.

### 2. Protocolo v3 (pequeño; hub y nodos se actualizan a la vez otra vez)

- `Layout.canvas: Vec<Placement { node, x, y }>`: la posición de cada bloque en el lienzo. El hub ya persiste el layout del widget; con esto también guarda el lienzo en `layout.toml`. Así cualquier widget, en el Mac o en el Windows, muestra la misma disposición. Sin este campo habría que deducirla de los enlaces, lo que es ambiguo, o guardarla en cada widget, lo que contradice «el hub es la fuente de verdad».
- `SwitchTo` con entrada opcional (`screen`/`enter_edge`/`pos_ratio` como `Option`), para que el menú de la bandeja cambie de equipo sin borde. El destino conserva su puntero, como con el atajo.
- Opcional: `SetOptions { hysteresis_ms, hotkey }` para la pestaña Opciones. Hoy esos valores solo están en `hub.toml`; `EdgeOptions` ya viaja en `Layout`.

### 3. App Tauri (`widget/`)

- `widget/src-tauri`: crate Rust en **su propio workspace**, con dependencias por ruta a `proto` y `agent-core`. Así las dependencias de Tauri (WebKit, etc.) no entran en los builds del hub y los agentes ni en sus jobs de CI.
- `widget/src`: Svelte + TypeScript con Vite.
- **Bandeja / barra de menús**: icono con el equipo activo, menú con los equipos (cambio rápido), «Ajustes…» y «Salir».
- **Ventana de ajustes**, pestañas del handoff §9:
  1. **Hub**:
     - hubs descubiertos por mDNS (nombre, IP, huella);
     - emparejar con token, con TOFU como los agentes;
     - estado, RTT y versión de protocolo;
     - botón «olvidar hub».
  2. **Dispositivos**:
     - `DeviceList` (interfaces del receptor) y los nodos de `Status` (agentes, helpers, índice, SO, RTT);
     - el emparejamiento HID++ es de la fase 3: botón deshabilitado con nota.
  3. **Monitores**:
     - lienzo SVG con un bloque por equipo y sus pantallas vivas: las 2 del Mac, las 2 del Windows y la de Ubuntu cuando exista;
     - arrastrar con imán a bordes y vista previa de los enlaces resultantes (calculados en Rust a cada cambio);
     - botones «Aplicar» (envía `Layout`, el hub lo persiste y reenvía al instante) y «Volver a la semilla del hub».
     - Un equipo sin pantallas reportadas (sin helper) aparece como bloque gris de 1920×1080 «sin helper», con el mismo criterio que el placeholder del hub.
  4. **Opciones**: `push_ms`, `corner_pct`, `dead_zone_px`; con `SetOptions`, también la histéresis y el atajo; arranque automático.
- Arranque automático: Login Item en macOS (plugin `autostart` de Tauri) y clave `Run` en Windows.
- Empaquetado: `.app` firmada ad hoc (`codesign -s -`), en un `.zip` o `.dmg` del CI (job `macos`), y `.exe`/NSIS en Windows más adelante.

## Protocolo v3: propuesta detallada (para revisar antes de tocar los agentes)

Todo lo que el widget necesita del protocolo va junto, para que el redespliegue coordinado sea uno solo: hub y gadget en la Pi, agente y helper del Mac, helper del Windows. `Hello` y `Reject` no cambian: los nodos v2 reciben `VersionMismatch`.

1. **`Layout.canvas: Vec<Placement { node, x, y, scale: Option<f32> }>`**: la disposición del lienzo, con la corrección manual de escala por nodo. El hub la guarda en `layout.toml` (nuevo `[[canvas]]`) junto a los enlaces y la reenvía con cada `Layout`. Con la semilla de `hub.toml` va vacía y el widget propone una con `canvas::initial` a partir de los enlaces en vigor. Los helpers la ignoran.
2. **`Entry { screen, edge, pos_ratio }` en `proto`**, y `SwitchTo { target, entry: Option<Entry> }`, `Activate { entry: Option<Entry> }`. Sin `entry`, el destino conserva su puntero, como con el atajo. Lo usa el cambio rápido desde la bandeja.
3. **Significado de `Screen::scale`**: unidades nativas por unidad lógica. En macOS es `1.0`, porque sus coordenadas ya son puntos; hoy el helper manda la escala Retina (2.0), lo que haría la integrada la mitad de grande en el lienzo. En Windows sigue siendo `dpi/96`. Cambia una línea en el helper macOS.
4. **`HubStatus.version: String`**: la versión y el commit del hub (`0.1.0 (d69e767)`), para la pestaña Hub.
5. **`Control::Settings(HubSettings { hysteresis_ms, hotkey })`**:
   - El hub lo envía a los widgets al conectar y cuando cambia.
   - Un widget lo envía para cambiarlo; el hub lo aplica al momento y lo guarda en `<state_dir>/settings.toml`, que en adelante tiene prioridad sobre `[switching]` de `hub.toml`, igual que `layout.toml` sobre `[[layout]]`.
   - Las `EdgeOptions` siguen viajando en `Layout`.
   - Así la pestaña Opciones no necesitará otra versión de protocolo.

Fuera de v3, a propósito: nombres de monitor (no hacen falta para el lienzo), versión de cada nodo (iría en `Hello`, que está congelado) y emparejamiento HID++ (fase 3; sus mensajes ya existen).

Qué cambia en cada componente:
- hub: `layout.toml` con `canvas`, `settings.toml`, `version` en `Status`, `SwitchTo`/`Activate` con `Entry`;
- `agent-core`: `edge` y `helper` con `Entry`, `run` sin cambios para los sinks;
- helper macOS: `scale = 1.0`;
- helper Windows, gadget, agentes Linux/macOS/Windows: solo recompilar.

## Orden y estimación (sesiones)

| # | Paso | Est. | Prueba |
|---|---|---|---|
| 1 | `agent-core::widget` + `canvas::links` | 1 | tests de tabla + e2e contra el hub |
| 2 | proto v3 (`canvas`, `SwitchTo` opcional) + hub persiste lienzo | 0,5 | tests del router, despliegue conjunto |
| 3 | esqueleto Tauri: bandeja, pestaña Hub (mDNS, emparejar, estado) | 1 | en el Mac |
| 4 | pestaña Monitores (lienzo, imán, aplicar) | 1–2 | reordenar Mac/Windows en caliente y probar el borde |
| 5 | Dispositivos + Opciones (+ `SetOptions`) | 0,5 | — |
| 6 | empaquetado y arranque automático; job de CI | 0,5 | instalar en el Mac desde el artefacto |

## Riesgos

- **Cadena de herramientas**: hace falta Node (para Vite/Svelte) y `tauri-cli`. Si no se quiere Node en el Mac, alternativa: TS sin framework compilado con `esbuild` binario. Por confirmar qué hay instalado.
- **macOS y Gatekeeper**: una `.app` sin Developer ID se abre la primera vez con clic derecho → Abrir (o `xattr -d com.apple.quarantine`). No pide permisos TCC.
- **CI**: el job de Tauri en macOS tarda (WebKit, Svelte). Que solo corra con cambios en `widget/`, igual que el workflow `windows`.
- **Unidades del lienzo**: si la escala que reporta un helper es incorrecta, el bloque sale del tamaño equivocado. Mitigación: mostrar la escala en el bloque y permitir corregirla a mano.
- **Varios widgets a la vez**: gana el último «Aplicar». Es aceptable (un solo usuario); el resto lo ve al recibir `Layout`.

## Preguntas para el usuario

1. ¿Protocolo v3 para guardar el lienzo en el hub y permitir el cambio desde la bandeja? Implica actualizar a la vez hub, gadget, agente y helpers.
2. ¿El widget solo en el Mac o también en el Windows?
3. ¿Svelte con Node, o TS sin framework para no instalar Node?
4. ¿Cambio rápido de equipo desde el menú de la bandeja?
