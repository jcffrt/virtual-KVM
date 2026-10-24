# Fase 5: instaladores desde cero (brief para VS Code)

Objetivo: que cada PC y el hub se instalen **desde un sistema limpio con un solo comando**, con el emparejamiento incluido, sin compilar nada y sin seguir a mano los pasos de `docs/*-instalacion.md`. Lo que hoy hacen los scripts de `deploy/` por componente pasa a ser un paso interno.

Encargo del usuario (2026-10-24): «un instalador automático para Mac que haga toda la instalación del agente incluido el Karabiner, permisos necesarios etc. desde cero; otro para Windows del edge detector y widget; otro para Ubuntu (el Pi va al USB del Windows); para el hub, un instalador deb o similar».

Reglas de siempre: código y comentarios en inglés, documentación de usuario en español, fmt / clippy `-D warnings` / tests limpios, una ADR por decisión. Sin cambio de protocolo salvo que sea imprescindible (si lo es, avisar antes: obliga a redesplegar todo).

## 0. Distribución: GitHub Releases

Hoy solo hay artefactos de CI, que caducan y exigen sesión en GitHub. Los instaladores necesitan una URL estable.

- Workflow `release.yml` disparado por etiqueta `v*`. Construye y adjunta a la release:
  - `rawkvm-hub_<ver>_arm64.deb` (y `armhf` si cuesta poco: el gadget ya compila armv7).
  - `rawkvm-agent-gadget_<ver>_arm64.deb` (+ `armhf`).
  - `rawkvm-agent_<ver>_amd64.deb` (agente Linux + extensión de GNOME).
  - `RawKVM-macos-<ver>.tar.gz`: agente, edge, `RawKVM.app`, `deploy/macos/` completo y `setup.sh`. Universal (arm64 + x86_64; `lipo` para los binarios, `--target universal-apple-darwin` para el widget) o un tar por arquitectura, lo que sea más simple.
  - `RawKVM-windows-<ver>.zip`: `rawkvm-edge.exe`, `RawKVM.exe`, `setup.ps1`, `uninstall.ps1`, LEEME.
  - `SHA256SUMS`.
- Los instaladores descargan de la **última release** por la API de GitHub (`releases/latest`), o de una etiqueta dada (`--version v0.5.0`), y **verifican el SHA-256** contra `SHA256SUMS` antes de instalar. También aceptan una ruta local (`--from ./carpeta`) para desarrollo y para los despliegues que hago yo desde el Mac.
- Versión: el workspace está en 0.1.0. Pasar a `0.5.0` en todos los `Cargo.toml` y en `tauri.conf.json`, y que `cargo-deb` y los nombres de fichero la tomen de ahí. La etiqueta la pongo yo cuando esté todo validado.
- ADR 0021: distribución por GitHub Releases, instaladores que descargan y verifican, política de versiones.

## 1. Hub: paquete `.deb` (prioridad 1, es lo que primero pruebo en la Pi)

`cargo-deb` sobre el binario musl estático (sin dependencias). Contenido:

- `/usr/bin/rawkvm-hub` (hoy está en `/usr/local/bin`; el servicio y los docs cambian de ruta).
- `/lib/systemd/system/rawkvm-hub.service`, `/etc/modprobe.d/rawkvm.conf` (blacklist de `hid_logitech_dj`), `/etc/systemd/journald.conf.d/rawkvm.conf`.
- `/etc/rawkvm/hub.toml` como **conffile** con los valores por defecto comentados; si ya existe, dpkg no lo toca.
- `postinst`: lo que hoy hace `deploy/hub-linux/install.sh`: descargar los módulos DJ y reenchufar los dispositivos Logitech, regenerar el initramfs si existe (`update-initramfs -u`), `var-log-journal.mount` **solo** si existe `orangepi-ramlog.service` (plantilla `.in` resuelta en postinst), `systemctl enable --now`, `sync`. Al final imprime cómo ver el token: `sudo rawkvm-hub --show-token`.
- `prerm`/`postrm`: parar el servicio; **no** borrar `/etc/rawkvm` ni `/var/lib/rawkvm` salvo `purge`.
- Migración desde la instalación actual de la Pi: el postinst detecta `/usr/local/bin/rawkvm-hub` y el unit viejo en `/etc/systemd/system/`, los retira y conserva `hub.toml`, `layout.toml`, token y certificado. Lo pruebo yo en la Pi real.
- Lo mismo para el gadget (`rawkvm-agent-gadget.deb`): dwc2 en modo device en Raspberry Pi OS, `modules-load.d`, servicio. El postinst instala y arranca el servicio pero **no empareja** (no hay red garantizada en un postinst); imprime el comando `sudo rawkvm-agent-gadget --pair --hub H` (§ 5) para hacerlo después. En la Pi actual el gadget ya está emparejado y la migración conserva su configuración.
- `docs/hub-linux-instalacion.md`: pasa a «`sudo apt install ./rawkvm-hub_…_arm64.deb`», con la instalación manual como anexo. ADR 0022 si hay decisiones de empaquetado que contar (ruta `/usr/bin`, conffiles, ramlog condicional).

## 2. Ubuntu: `.deb` del agente + `rawkvm-setup`

- `rawkvm-agent_<ver>_amd64.deb` (glibc; construir en `ubuntu-22.04` o con zigbuild `x86_64-unknown-linux-gnu.2.35` para que valga en 22.04 y 24.04): `/usr/bin/rawkvm-agent`, regla udev `/dev/uhid`, `modules-load.d/uhid.conf`, unidad **de usuario** `/usr/lib/systemd/user/rawkvm-agent.service`, extensión de GNOME en `/usr/share/gnome-shell/extensions/rawkvm@olitec.cc/`.
- El postinst no puede emparejar (no hay sesión de usuario ni red garantizada). Añadir el subcomando **`rawkvm-agent setup`** (o binario `rawkvm-setup`), interactivo, que ejecuta el usuario tras el `apt install`:
  1. pide hub (ofrece los que encuentra por mDNS) y token (entrada oculta, o `--token-file`), empareja y fija el certificado;
  2. `systemctl --user enable --now rawkvm-agent`;
  3. `gnome-extensions enable rawkvm@olitec.cc` y avisa de que hay que cerrar sesión una vez (Wayland no recarga extensiones);
  4. comprueba: `/dev/uhid` accesible, servicio activo, «connected» en el journal, extensión `ACTIVE` tras el relogin (`--check` lo repite cuando quiera).
- `setup-linux.sh` de una línea para el que no quiere bajar el deb a mano: descarga la release, verifica, `apt install ./…`, lanza `rawkvm-agent setup`.
- Pruebo yo en el Ubuntu (192.168.1.157; allí no tengo sudo, así que el `apt install` lo hace el usuario o me pasa la contraseña por terminal, nunca por chat).

## 3. Windows (PC alimentado por el gadget): `setup.ps1`

Sin administrador ni driver: edge y widget son por usuario.

- `setup.ps1` en el zip: descarga si hace falta (o se ejecuta desde el zip extraído), `Unblock-File`, instala `rawkvm-edge.exe` y `RawKVM.exe` en `%LOCALAPPDATA%\Programs\RawKVM\`, claves `Run` de HKCU para los dos, acceso directo en el menú Inicio, y arranca ambos.
- Emparejamiento una sola vez para los dos programas: `-Hub` y `-Token` (o entrada oculta si faltan; `-TokenFile`). El edge y el widget comparten hoy formato de configuración distinto en `%LOCALAPPDATA%\rawkvm\`: que el instalador escriba las dos, o mejor, que el widget pueda leer el emparejamiento del edge si ya existe (misma carpeta, mismo hub y token).
- **`-ForNode` debe dejar de ser obligatorio**: el edge, tras emparejar, pide la lista de nodos al hub (el widget ya la recibe en el estado) y si hay exactamente un gadget lo toma; si hay varios, pregunta. `-ForNode` sigue valiendo para forzarlo.
- `-Check`: versión instalada, procesos vivos, «connected» en los logs, cortafuegos (mDNS UDP 5353 entrante si se usa el descubrimiento; el QUIC es saliente y no necesita regla). `uninstall.ps1` quita todo menos la configuración salvo `-Purge`.
- SmartScreen: el exe no está firmado; documentar el aviso «Más información → Ejecutar de todas formas» con captura o texto, y que `Unblock-File` evita el bloqueo al inicio de sesión. Firmar con certificado de pago queda fuera.
- `docs/windows-instalacion.md` y `docs/windows-borde.md` se funden en una guía con el instalador delante y lo manual como anexo. (El driver VHF de `windows.yml` sigue aparcado, ADR 0006; el instalador no lo toca.)

## 4. macOS: `setup.sh` (el más largo; prioridad 4 pero el más valioso)

Un solo script, idempotente, que se puede relanzar tras cada reinicio o tras dar un permiso y continúa donde estaba. Imprime al final una tabla de estado (componente, instalado, permiso, servicio) y qué falta.

1. **Comprobaciones**: macOS ≥ 13, arquitectura, `xcode-select` no hace falta (binarios precompilados), Karabiner-Elements presente o no (si está, usar su daemon, como ya hace `install.sh`).
2. **Karabiner-DriverKit-VirtualHIDDevice**: si no está, descargar el `.pkg` de la versión fijada (8.6.0, con SHA-256 en el script), `sudo installer -pkg`, `Karabiner-VirtualHIDDevice-Manager activate`, abrir el panel de extensiones (`open "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"`), esperar en bucle a `systemextensionsctl list` con `[activated enabled]` y, si macOS pide reiniciar, decirlo y salir con un mensaje claro de «vuelve a ejecutar `setup.sh` tras reiniciar».
3. **Identidad de firma** «RawKVM Local» (`make-signing-identity.sh` integrado), avisando del diálogo «Permitir siempre».
4. **Agente** (`install.sh` actual) y **emparejamiento**: añadir al agente `--pair --hub H --token-file F` (o entrada oculta) que conecta, fija el certificado, escribe la configuración y **sale con 0** al recibir «connected», para no depender del Ctrl+C a mano. El script para el daemon, empareja, lo arranca.
5. **Monitorización de entrada** (LED): no se puede conceder por script (TCC). El script abre el panel (`open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"`) y el Finder con el binario seleccionado (`open -R /usr/local/bin/rawkvm-agent`), explica el arrastre, hace `kickstart` y espera a ver «watching the keyboard LEDs» en el log, con tiempo límite y la opción de saltarlo (`--skip-led`). Si detecta el caso ad hoc → certificado (tccd «Failed to match existing code requirement»), propone el `tccutil reset ListenEvent` con la advertencia de que afecta a todas las apps.
6. **Edge** (`install-edge.sh`) como LaunchAgent del usuario. Verificar qué permiso necesita `rawkvm-edge` para mover el puntero y leer su posición (¿Accesibilidad?) y, si lo necesita, mismo tratamiento: abrir el panel, esperar, comprobar en su log.
7. **Widget** (`install-widget.sh`): `~/Applications/RawKVM.app`, firmado, inicio de sesión (LaunchAgent o `SMAppService`, lo que ya se use), primer arranque para que salte el diálogo de Red local y comprobación de que conecta (hoy «No route to host» es el síntoma de que falta).
8. `uninstall.sh` completo (agente, edge, widget, plists, configuración con `--purge`; **nunca** desinstala Karabiner por defecto, `--karabiner` para hacerlo).
9. `docs/macos-instalacion.md`: el instalador delante; los pasos manuales quedan como anexo de diagnóstico.

Pruebo yo en el Mac con el usuario delante: `uninstall.sh --purge` y `setup.sh` desde cero. El MacBook tiene teclado propio, así que no hay riesgo de quedarse sin entrada.

## 5. Común a todos

- **Token**: nunca en la línea de comandos en la documentación (queda en el historial del shell). Entrada oculta o fichero. `--token` sigue existiendo por compatibilidad.
- **Emparejamiento sin Ctrl+C**: `--pair` en todos los agentes y helpers que hoy exigen «Ctrl+C tras connected» (agente macOS, agente Linux, gadget). El edge ya tiene `--configure-only`.
- `--check` en todos: diagnóstico sin tocar nada, con salida apta para pegarme en el chat.
- Idempotentes: relanzar actualiza binarios y conserva configuración y emparejamiento.
- `docs/instalacion.md` nuevo: **la** página de entrada en español, un apartado por plataforma con el comando único y el enlace a la guía larga. `README` enlaza a ella.
- CI: `shellcheck` para los `.sh`, `PSScriptAnalyzer` para los `.ps1`, `lintian` sobre los `.deb` (avisos, no errores), y una prueba de instalación del `.deb` del hub en un contenedor arm64 con QEMU si cabe en tiempo; si no, al menos `dpkg -i` en amd64 con el `.deb` cruzado y `--force-architecture` para validar scripts.

## Orden de entrega y validación

1. `release.yml` + versión 0.5.0 + `.deb` del hub y del gadget (§ 0, § 1). Yo migro la Pi real con el `.deb`.
2. Ubuntu `.deb` + `rawkvm-agent setup` (§ 2). Lo pruebo en el Ubuntu.
3. Windows `setup.ps1` con `-ForNode` automático (§ 3). Lo prueba el usuario en el GAMER conmigo.
4. macOS `setup.sh` (§ 4). Lo probamos en el Mac desde cero.

Entregar cada bloque con su commit, CI en verde y la lista de lo que queda sin verificar en hardware, como hasta ahora. Si algún punto exige cambio de protocolo o toca algo que no está aquí, avisar antes de hacerlo.
