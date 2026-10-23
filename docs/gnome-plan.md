# Ubuntu: cambio por el borde con GNOME

Paso 5 de la Fase 2 (`fase2-plan.md`), aprobado el 2026-10-22: helper dentro del agente Linux y el Ubuntu a la derecha del Windows (semilla `from = 1, edge = "right", to = 3`). **Implementado; sin probar en el Ubuntu.** Destino: Ubuntu 24.04, GNOME Shell 46, Wayland.

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
| `Warp(ii)` | método | `Clutter.get_default_backend().get_default_seat().warp_pointer(x, y)` |
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

- `gnome-extension/rawkvm@olitec.cc/`: `metadata.json` (`shell-version: ["46"]`, ampliable a 47/48 tras probar) y `extension.js` (ESM, clase `Extension` con `enable()`/`disable()` que exporta y retira el objeto D-Bus con `Gio.DBusExportedObject`).
- Instalación en `deploy/linux/install.sh`: copiar a `~/.local/share/gnome-shell/extensions/`, `gnome-extensions enable rawkvm@olitec.cc`. **En Wayland hay que cerrar sesión y volver a entrar** la primera vez: gnome-shell no recarga extensiones nuevas en caliente.
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
- **Pantalla de bloqueo**: con la sesión bloqueada, gnome-shell no ejecuta extensiones de usuario: no hay cambio por borde. Queda el atajo, como en Windows.
- **KDE / X11**: fuera de alcance (handoff §11). Con X11 sería otra vía (XFixes + `XWarpPointer`).

## Instalar

En el Ubuntu, con el binario `rawkvm-agent` (x86-64) y las carpetas `deploy/linux` y `gnome-extension` del repositorio:

```sh
scp target/x86_64-unknown-linux-gnu/release/rawkvm-agent <usuario>@<ubuntu>:
scp -r deploy/linux gnome-extension <usuario>@<ubuntu>:rawkvm/
# en el Ubuntu, con la sesión gráfica abierta (o por SSH, ver abajo):
sh rawkvm/linux/install.sh ./rawkvm-agent      # pide sudo para la regla udev de uhid
~/.local/bin/rawkvm-agent --hub 192.168.1.28 --token <hex>   # una vez; Ctrl+C tras «connected»
systemctl --user start rawkvm-agent
```

`install.sh` busca la extensión junto a sí mismo, en `../gnome-extension/` (la copia de arriba) o en `../../gnome-extension/` (el repositorio). Copia la extensión a `~/.local/share/gnome-shell/extensions/` y la añade a `enabled-extensions`. **Hay que cerrar sesión y volver a entrar una vez** para que gnome-shell la cargue.

El agente arranca el helper solo y escribe `~/.config/rawkvm/edge.toml`, derivado de `agent.toml`. `--no-edge` lo desactiva.

## Verificación (copiar tal cual)

Por SSH, primero apuntar al bus de la sesión gráfica del mismo usuario:

```sh
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
```

1. La extensión está cargada (debe decir `State: ACTIVE`; si dice `INITIALIZED` o no aparece, falta cerrar sesión):
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
