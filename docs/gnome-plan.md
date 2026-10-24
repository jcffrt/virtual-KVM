# Ubuntu: cambio por el borde con GNOME

Paso 5 de la Fase 2 (`fase2-plan.md`), aprobado el 2026-10-22: helper dentro del agente Linux y el Ubuntu a la derecha del Windows (semilla `from = 1, edge = "right", to = 3`). **Implementado y verificado en el Ubuntu el 2026-10-23** (extensión ACTIVE, helper conectado, hub con el Ubuntu en el layout; falta la prueba física del cruce). Destino: el Ubuntu del usuario es **26.04.1 LTS con GNOME Shell 50.1**, en Wayland. La extensión declara de la 45 a la 51.

## Por qué hace falta una extensión

En Wayland ningún programa normal puede leer la posición global del puntero ni moverlo: solo el compositor (gnome-shell). El agente Linux (uhid, servicio de usuario) recibe bien teclado y ratón, pero para el borde necesita que gnome-shell le preste esos dos datos. El handoff (§8) ya lo preveía: extensión + D-Bus.

## Reparto

La lógica sigue en Rust, como en macOS y Windows: el agente Linux implementa `EdgePlatform` y usa el mismo `run_helper` / `agent_core::edge`. La extensión es un **puente mínimo** sin lógica de bordes, sobre el bus de sesión, con nombre `cc.olitec.RawKvm.Shell` e interfaz `cc.olitec.RawKvm.Shell1`:

| Miembro | Tipo | Qué hace |
|---|---|---|
| `Monitors()` → `a(iiiidb)` | método | `(x, y, w, h, scale, primary)` por monitor: `global.display.get_monitor_geometry(i)` y `get_monitor_scale(i)`, en píxeles lógicos del layout de GNOME (los mismos que el puntero) |
| `MonitorsChanged` | señal | `Main.layoutManager` `monitors-changed` |
| `Watch(b)` | método | activa o desactiva el sondeo del puntero |
| `Pointer(ii)` | señal | con `Watch(true)`, la posición (`global.get_pointer()`) cada ~8 ms (`GLib.timeout_add`), **solo si cambió** |
| `Warp(ii)` | método | `warp_pointer(x, y)` del seat de Clutter, obtenido de `global.stage.get_context().get_backend()` (47+) o de `Clutter.get_default_backend()` (45, 46) |
| `GetPointer()` → `(ii)` | método | una muestra, para diagnóstico |
| `Version` | propiedad `u` | versión del puente (1) |

`Watch(true)` es un **arrendamiento de 3 s**: el agente lo renueva cada 250 ms mientras pide el puntero (solo mientras ese equipo está activo). Si el agente muere sin `Watch(false)`, la extensión deja de muestrear sola.

- Unidades: GNOME trabaja en píxeles lógicos (con escalado fraccional, el layout es lógico), así que el helper Linux manda `scale = 1.0`, como macOS (protocolo v3, «Pantallas y unidades»).
- Coste: con `Watch(false)` (este equipo inactivo) la extensión no hace nada. Activo, es un `get_pointer` cada 8 ms dentro de gnome-shell y una señal solo cuando el puntero se mueve.
- Alternativa descartada: `Meta.Barrier` / `Layout.PressureBarrier` en cada borde enlazado. Dan «empujar» nativo, pero obligan a recrear barreras con cada layout y meten lógica de bordes en JS.

## Agente Linux como helper

- `rawkvm-agent` (servicio de usuario systemd) añade una tarea `run_helper` en el mismo proceso cuando hay sesión gráfica GNOME (o con `edge = true` en la config). Se conecta al hub como segundo nodo `EdgeHelper { for_node: <su propio node_id> }`.
- La configuración del helper se deriva sola: `node_id` = UUID v5 del del agente, mismo hub, token y huella. No hace falta un segundo emparejamiento.
- D-Bus con `zbus` (Rust puro, tokio). Si el nombre `cc.olitec.RawKvm.Shell` no existe (extensión no activada, X11, otro escritorio), el helper registra el motivo una vez y reintenta cada 10 s. El agente sigue recibiendo teclado y ratón igual.
- `Activate` con entrada → `Warp` y `Watch(true)`; `Deactivate` → aparcado (`Warp` al centro) y `Watch(false)`.

## Extensión

- `gnome-extension/rawkvm@olitec.cc/`: `metadata.json` (`shell-version` de la 45 a la 51: las APIs usadas se comprobaron en el código fuente de mutter y gnome-shell 45.0 … 51.0 y 50.1; solo cambia la forma de llegar al seat de Clutter, que se detecta en tiempo de ejecución) y `extension.js` (ESM, clase `Extension` con `enable()`/`disable()` que exporta y retira el objeto D-Bus con `Gio.DBusExportedObject`).
- Instalación: el paquete `rawkvm-agent` la pone en `/usr/share/gnome-shell/extensions/` y `rawkvm-agent setup` la activa ([ubuntu-instalacion.md](ubuntu-instalacion.md)). **En Wayland hay que cerrar sesión y volver a entrar** la primera vez: gnome-shell no recarga extensiones nuevas en caliente.
- `"session-modes": ["user", "unlock-dialog"]` (desde la versión 2 de la extensión). Sin `unlock-dialog`, gnome-shell desactiva la extensión al bloquear la pantalla: el orquestador vio a la vez `LockedHint=yes` y la extensión en estado INACTIVE. El helper perdía el puntero y las pantallas hasta desbloquear. La extensión no dibuja nada y solo exporta su objeto D-Bus, así que puede seguir activa en la pantalla de bloqueo: se puede entrar y salir por el borde con el Ubuntu bloqueado (para escribir la contraseña con el teclado compartido) y el puntero se aparca igual. `disable()` sigue llamándose al cerrar sesión o al desactivarla.
- Sin firmar ni pasar por extensions.gnome.org (uso personal).

## Pasos y estimación

| # | Paso | Est. | Prueba |
|---|---|---|---|
| 1 | extensión puente (Monitors, Watch/Pointer, Warp) | 0,5 | `gdbus call --session …` desde una terminal del Ubuntu |
| 2 | `agent-linux`: helper con zbus + config derivada | 1 | e2e con un D-Bus falso (`zbus` permite un servidor en proceso); luego en el Ubuntu |
| 3 | instalador y docs | 0,5 | Mac ↔ Windows ↔ Ubuntu por el borde con la semilla `from = 1, edge = "right", to = 3` o desde el widget |

## Riesgos

- **API de GNOME por versión**: `warp_pointer` y `get_pointer` existen en 46, pero la API de extensiones cambia entre versiones (ESM desde la 45). Fijar `shell-version` y probar en cada actualización de Ubuntu.
- **Puntero de uhid y `get_pointer`**: el ratón de uhid es un dispositivo libinput normal, así que GNOME mueve el puntero y `get_pointer` lo ve. No debería haber diferencia con un ratón físico. Por confirmar en el equipo.
- **Escalado fraccional**: si `get_monitor_geometry` y `get_pointer` no estuvieran en las mismas unidades con escalado fraccional, se nota en `--show`. Mitigación: el diagnóstico del agente imprime ambos.
- **Pantalla de bloqueo**: gnome-shell solo mantiene activas en la pantalla de bloqueo las extensiones que declaran `unlock-dialog` en `session-modes`; la nuestra lo declara desde la versión 2 (ver «Extensión»). Una extensión ya instalada se actualiza con el paquete nuevo, cerrando y abriendo sesión; luego, `gnome-extensions info rawkvm@olitec.cc` con la pantalla bloqueada (por SSH) debe seguir diciendo ACTIVE. Sin verificar todavía en el Ubuntu.
- **KDE / X11**: fuera de alcance (handoff §11). Con X11 sería otra vía (XFixes + `XWarpPointer`).

## Instalar

Con el paquete: ver [ubuntu-instalacion.md](ubuntu-instalacion.md). El paquete instala la extensión en `/usr/share/gnome-shell/extensions/rawkvm@olitec.cc/` y `rawkvm-agent setup` la activa (la añade a `enabled-extensions`). **Hay que cerrar sesión y volver a entrar una vez** para que gnome-shell la cargue. Una copia antigua en `~/.local/share/gnome-shell/extensions/` tendría prioridad sobre la del paquete: `setup` la quita.

El agente arranca el helper solo y escribe `~/.config/rawkvm/edge.toml`, derivado de `agent.toml`. `--no-edge` lo desactiva.

## Verificación (copiar tal cual)

Por SSH, primero apuntar al bus de la sesión gráfica del mismo usuario:

```sh
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
```

1. La extensión está cargada. Debe decir `State: ACTIVE` (en español, `Estado: ACTIVE`). Si dice:
   - `INITIALIZED` o no aparece: falta cerrar sesión y volver a entrar.
   - `OUT OF DATE`: la versión de GNOME Shell (`gnome-shell --version`) no está en `shell-version` de `metadata.json`, así que GNOME no la carga aunque esté activada. Hay que instalar una versión de la extensión que la incluya. Hoy son de la 45 a la 51, comprobadas contra el código fuente de cada versión, y el instalador avisa si la tuya no está.
   - `ERROR`: `journalctl --user -b | grep -i rawkvm` muestra el fallo de JavaScript.
   ```sh
   gnome-extensions info rawkvm@olitec.cc
   ```
2. El objeto D-Bus existe y su versión:
   ```sh
   gdbus introspect --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell
   gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
     --method org.freedesktop.DBus.Properties.Get cc.olitec.RawKvm.Shell1 Version
   ```
3. Monitores: `(x, y, ancho, alto, escala, principal)` en píxeles lógicos:
   ```sh
   gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
     --method cc.olitec.RawKvm.Shell1.Monitors
   ```
4. Una muestra del puntero:
   ```sh
   gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
     --method cc.olitec.RawKvm.Shell1.GetPointer
   ```
5. Señales del puntero durante 10 s. Mientras corre, mover el ratón en el Ubuntu: deben salir líneas `cc.olitec.RawKvm.Shell1.Pointer (x, y)` solo mientras se mueve.
   ```sh
   timeout 11 gdbus monitor --session --dest cc.olitec.RawKvm.Shell &
   for i in $(seq 10); do
     gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
       --method cc.olitec.RawKvm.Shell1.Watch true >/dev/null; sleep 1
   done
   gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
     --method cc.olitec.RawKvm.Shell1.Watch false
   ```
6. Mover el puntero a (100, 100): debe saltar a la esquina superior izquierda de la pantalla principal (si está en el origen):
   ```sh
   gdbus call --session --dest cc.olitec.RawKvm.Shell --object-path /cc/olitec/RawKvm/Shell \
     --method cc.olitec.RawKvm.Shell1.Warp 100 100
   ```
7. El agente encontró la extensión y se registró como helper:
   ```sh
   journalctl --user -u rawkvm-agent -n 50 | grep -E 'GNOME|edge|screen'
   ```
   Esperado: `GNOME extension rawkvm@olitec.cc found version=1`, `edge helper configured`, `edge helper connected` y una línea `screen` por monitor. En el hub, `screens reported` del Ubuntu y, con la semilla `from = 1, edge = "right", to = 3`, `layout … links=4`.

## Preguntas resueltas

1. Helper dentro del mismo proceso del agente: **sí**.
2. El Ubuntu va a la derecha del Windows: **sí** (Mac integrada, Mac externa, Windows, Ubuntu).
