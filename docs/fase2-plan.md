# Fase 2: cambio de equipo por el borde de pantalla

Plan del 2026-10-22, adaptado a la arquitectura definitiva (STATUS.md) y **aprobado por el usuario**. **Pasos 1 a 4 hechos y validados en hardware**: ida y vuelta Mac ↔ Windows por el borde, criterio de la fase para esos dos equipos (STATUS, «Fase 2: ida y vuelta»; ADR 0009). Las respuestas a las preguntas están en STATUS.md. Cambios respecto al borrador: el layout se escribe como `[[layout]]` (no `[[layout.link]]`); un layout del widget se persiste en `layout.toml` y manda; `SwitchTo`/`Activate` llevan `screen`; la esquina se configura como `corner_pct`.

## Topología

| Equipo | Recibe la entrada | Quién detecta el borde y coloca el puntero |
|---|---|---|
| PC Windows | gadget USB de la Orange Pi (sin software) | **helper de usuario** nuevo, sin driver |
| Mac | agente macOS (LaunchDaemon root + Karabiner) | **helper de usuario** nuevo (LaunchAgent en la sesión) |
| Ubuntu | agente Linux (uhid) | extensión GNOME + el propio agente, o un helper |

En ningún equipo puede el proceso que recibe la entrada leer y mover el puntero. El gadget no corre nada en el PC, y el agente macOS es un daemon root fuera de la sesión gráfica. Por eso el borde lo vigila otro proceso en la sesión del usuario, que habla con el hub como **nodo propio que actúa en nombre del** nodo HID de esa máquina.

## Lo que ya existe

- `proto`: `Screen`, `EdgeLink`, `Edge::opposite`, `Control::{Screens, Layout, SwitchTo, Activate{enter_edge,pos_ratio}, Deactivate}`.
- Hub: `SwitchTo` aceptado solo del agente activo (o de un widget), con histéresis de 150 ms. Antes del cambio envía release-all y `Deactivate` al origen. También reparte `Screens` de los agentes a los widgets y `Layout` del widget a todos.
- Índices estables por nodo (`nodes.toml`, ADR 0008): sirven para escribir el layout a mano.

## Cambios propuestos

### 1. Protocolo y hub (base de todo lo demás)

- `NodeKind::EdgeHelper { for_node: Uuid }` en `Hello` y `PROTO_VERSION` 2, lo que obliga a actualizar hub y agentes a la vez. Es preferible a un `on_behalf_of: Option<Uuid>` suelto, que tendría sentido en agentes y widgets.
- Hub:
  - Acepta el `SwitchTo` de un helper cuando su `for_node` es el activo.
  - Reenvía `Activate`/`Deactivate` del `for_node` también a sus helpers, que colocan o aparcan el puntero.
  - Atribuye al `for_node` los `Screens` que envía el helper.
  - No asigna índice a los helpers ni los cuenta como agentes; un helper sin su nodo conectado queda inerte.
- `[[layout.link]]` en `hub.toml`, escrito a mano hasta que exista el widget. Los nodos se indican por índice, y el enlace inverso se genera solo:
  ```toml
  [[layout.link]]
  from = 2          # Mac
  edge = "right"
  to = 1            # Windows (gadget)
  # opcional: from_screen, to_screen, rangos 0.0..1.0
  ```
  El hub lo traduce a `EdgeLink` con los `Screens` recibidos y lo envía como `Layout`.
- Tests: router con helpers (SwitchTo de un helper inactivo ignorado, Activate reenviado, helper sin nodo) y traducción del layout.

### 2. Lógica de borde común (`agent-core::edge`, pura y con tests de tabla)

Entrada: pantallas propias, `EdgeLink`s y posición del puntero. Salida: `SwitchTo` o nada, y la posición de entrada al activar.

- **Detección**: el puntero toca el último píxel de un borde dentro del rango enlazado.
- **Esquinas**: se excluye un 3 % en cada extremo (botón cerrar de Windows, Hot Corners del Mac).
- **Histéresis**: 150 ms en el hub, como ya está, y también en el helper para no enviar en ráfaga.
- **Zona muerta**: 2 px tras entrar (handoff §4.4). No se vuelve a disparar hasta que el puntero se haya separado del borde de entrada.
- **Entrada**: `Activate{enter_edge, pos_ratio}` coloca el puntero a 2 px del borde, a la altura `pos_ratio` dentro del rango de destino.
- **Aparcado**: al recibir `Deactivate`, el puntero del origen va al centro de su pantalla. Así no queda en el borde y no rebota si alguien mueve un ratón local de ese PC.
- Tests de tabla: multi-monitor con distintas resoluciones y escalas, rangos parciales, esquinas, ida y vuelta sin rebote.

### 3. Helper macOS (`crates/edge-macos`, LaunchAgent por usuario)

- Lee la posición con **polling** de `CGEventGetLocation(CGEventCreate(NULL))` a 120 Hz, en lugar de `CGEventTap`. Así **no pide Input Monitoring**. Esto importa porque con firma ad-hoc el permiso TCC se pierde en cada recompilación.
- Mueve el puntero con `CGWarpMouseCursorPosition` + `CGAssociateMouseAndMouseCursorPosition(true)`, que no necesita Accessibility (por confirmar en macOS 15).
- Pantallas con `CGGetActiveDisplayList`/`CGDisplayBounds` (puntos lógicos) y cambios de configuración con `CGDisplayRegisterReconfigurationCallback`.
- `for_node` = `node_id` del agente macOS, que el instalador lee de `/Library/Application Support/rawkvm/agent.toml`.

### 4. Helper Windows (`rawkvm-edge.exe`, sin driver)

- Reutiliza de `agent-windows` el cliente, la config y el `data_dir`, sin `driver`/`sink`/`setup`. Habrá que separar el crate (`agent-windows` → `windows-common` + bins) o usar una feature `driver`.
- Lee la posición con polling de `GetCursorPos` a 125 Hz. Es más simple que `WH_MOUSE_LL` y no mete un hook en la cadena de todo el ratón, pero se puede cambiar si el polling da problemas.
- Mueve el puntero con `SetCursorPos`, con `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` y pantallas con `EnumDisplayMonitors` y `WM_DISPLAYCHANGE`.
- Arranca en la sesión del usuario con una tarea programada «al iniciar sesión» (no un servicio, que vive en la sesión 0 y no ve el escritorio).
- `for_node` = `node_id` del gadget: `rawkvm-hub --list-nodes` en la Pi, o se elige por nombre en `configure`.

### 5. Ubuntu (cuando haya máquina)

- Extensión GNOME Shell (ESM, GNOME 46 en Ubuntu 24.04). Lee la posición con `global.get_pointer()` en un temporizador de unos 8 ms y mueve el puntero con `Clutter.get_default_backend().get_default_seat().warp_pointer()`.
- Habla con el agente Linux por D-Bus de sesión (`cc.olitec.RawKvm.Edge`). Como el agente Linux ya corre en la sesión de usuario, puede hacer él de helper y no necesita `EdgeHelper`.

### 6. Widget Tauri (después)

Lienzo de monitores que sustituye el `[[layout.link]]` manual y envía `Layout` como nodo `Widget`. Incluye también estado, latencia y token. Va al final porque el layout a mano basta para un escritorio fijo de dos o tres equipos.

## Orden y estimación (en sesiones de trabajo)

| # | Paso | Estimación | Se puede probar con | Estado |
|---|---|---|---|---|
| 1 | proto v2 + hub (helpers, layout en hub.toml) | 1 | tests del router | hecho (0015faa) |
| 2 | `agent-core::edge` | 1 | tests de tabla | hecho (0015faa) |
| 3 | helper macOS | 1 | Mac + atajo para volver | hecho (a0d80e4), validado |
| 4 | helper Windows | 1–2 | ida y vuelta Mac ↔ Windows: **criterio de la fase** | hecho (2dc5694, d69e767), **validado**: Windows → Mac → Windows → Mac con el puntero a la altura correcta |
| 5 | extensión GNOME | 2 | cuando haya Ubuntu | hecho (`docs/gnome-plan.md`), sin probar en el Ubuntu |
| 6 | widget Tauri | 2–3 | — | plan en `docs/widget-plan.md` |

Los pasos 1 a 4 dan el cambio por borde entre Mac y Windows sin widget.

## Riesgos

- **macOS, permisos**: si el polling o el warp acaban necesitando Input Monitoring o Accessibility, la firma ad-hoc obliga a volver a conceder el permiso tras cada actualización. Mitigación: polling sin tap; si hace falta, una firma estable (certificado autofirmado propio en el llavero) para que el TCC no cambie.
- **macOS, puntero fantasma**: tras un warp, macOS suprime el movimiento unos 250 ms si no se reasocia. `CGAssociateMouseAndMouseCursorPosition(true)` lo evita; hay que comprobarlo con el dispositivo de Karabiner.
- **Windows, escritorio seguro**: en la pantalla de bloqueo, UAC o Ctrl+Alt+Supr el helper no ve el puntero, así que no hay cambio por borde. Queda el atajo Ctrl, Ctrl, N, que lo interpreta el hub. Lo mismo si el helper no arrancó.
- **Windows, UIPI**: `SetCursorPos` sobre una ventana elevada en primer plano puede fallar. Por confirmar; si falla, se documenta.
- **Wayland**: el warp solo es posible desde el compositor. La API de extensiones cambia en cada versión de GNOME, y KDE y wlroots quedan fuera.
- **Red del PC Windows**: el helper necesita llegar al hub por la LAN. Si el PC no está en la misma red, una alternativa futura es una función USB de red o serie en el gadget para hablar con la Pi por el mismo cable.
- **Latencia del cambio**: helper → hub → destino son dos saltos Wi-Fi (RTT medido de 4 a 10 ms). Cabe en el objetivo de 50 ms.
- **Orange Pi como único punto de fallo**: si se apaga con el PC (ver alimentación en `gadget-instalacion.md`), todo deja de funcionar, no solo el borde.

## Preguntas para el usuario

1. Disposición física: ¿qué pantalla queda a la izquierda o derecha de cuál, y cuántos monitores tiene cada equipo?
2. ¿El PC Windows está en la misma LAN que la Orange Pi (cable o Wi-Fi)?
3. ¿Cambio inmediato al tocar el borde, o hay que «empujar» (unos 100 ms de contacto o dos toques) para evitar cambios accidentales?
4. ¿Se puede instalar en el Windows un `.exe` sin firmar que arranca al iniciar sesión?

## Después de la Fase 2: primer objetivo de la Fase 3

Cambiar el perfil de sistema operativo del teclado al conmutar, por HID++: feature `HOSTS_INFO` (0x1815), `setHostOsVersion`, como hace Logitech Options. El MX Keys guarda el perfil macOS en el canal del receptor de la Pi, así que Alt y Win salen cruzadas en el Windows. Mientras tanto, el parche provisional es `[[node_options]] swap_alt_gui` (ADR 0011).

