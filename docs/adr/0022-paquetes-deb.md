# ADR 0022 — Paquetes .deb del hub y del gadget

- Estado: aceptada (2026-10-24), Fase 5.1.

## Contexto

El hub y el gadget se instalaban con `deploy/hub-linux/install.sh` y `deploy/gadget/install.sh`: binario en `/usr/local/bin`, unidad en `/etc/systemd/system` y opciones del gadget editando el `ExecStart`. Funciona, pero no hay actualización ni desinstalación limpias, y cada placa nueva repite pasos a mano. La Pi actual (Orange Pi Zero 2W, Armbian) tiene hub y gadget en la misma placa, instalados así.

## Decisión

- **`cargo-deb`** sobre los binarios musl estáticos (`cargo zigbuild`), con `--no-build --no-strip`: el perfil release ya quita los símbolos y el `strip` del anfitrión no entiende ARM. Arquitecturas `arm64` y `armhf`. `Depends: systemd` (el binario no enlaza bibliotecas). Revisión vacía, para que el nombre sea `rawkvm-hub_0.5.0_arm64.deb`.
- **Rutas de paquete.** `/usr/bin` y unidades en `/lib/systemd/system`. La instalación manual pasa también a `/usr/bin`, para que el servicio sea el mismo.
- **Conffiles**: `/etc/modprobe.d/rawkvm.conf`, `/etc/systemd/journald.conf.d/rawkvm.conf` y `/etc/default/rawkvm-agent-gadget`. Los dos primeros tienen el mismo contenido que dejó `install.sh`, para que dpkg no pregunte al migrar.
- **`hub.toml` no es conffile.** El hub lo reescribe en su primer arranque (id y token), y un conffile modificado por el programa haría preguntar a dpkg en las actualizaciones y perdería los comentarios. El paquete trae `/usr/share/doc/rawkvm-hub/hub.toml.example`, comentado y con los valores por defecto reales: un test (`packaged_example_matches_the_defaults`) lo comprueba contra `HubConfig::default()`.
- **Opciones del gadget** en `/etc/default/rawkvm-agent-gadget` (`RAWKVM_GADGET_ARGS`), que la unidad lee con `EnvironmentFile`. La unidad de `/lib` se sobrescribe en cada actualización y no debe editarse.
- **postinst del hub**: lo que hacía `install.sh`, con dos cambios. Solo descarga los módulos DJ y reenchufa los dispositivos Logitech si están cargados, y regenera el initramfs en la primera instalación o en ese caso: una actualización no corta el teclado ni tarda un minuto. El montaje del journal en la SD (ADR 0017) solo se genera si hay `armbian-ramlog` u `orangepi-ramlog`, y journald solo se reinicia si algo cambió. Termina con `sync` e indica cómo ver el token (`sudo rawkvm-hub --show-token`), sin imprimirlo en el registro de apt.
- **postinst del gadget**: el modo dispositivo del OTG (overlay `dwc2` en la Raspberry Pi, `modules-load.d`). **No empareja**: en un postinst no hay red garantizada ni terminal. Si no hay token, no arranca el servicio e indica `sudo rawkvm-agent-gadget --pair --hub H`. `--pair` (en `agent-core`, `pairing.rs`) conecta una vez, fija el certificado y sale con 0. El token se pide sin eco o se lee con `--token-file`. Así no queda en el historial.
- **Migración desde la instalación manual**: los postinst borran el binario de `/usr/local/bin` y la unidad de `/etc/systemd/system` (con su enlace de `multi-user.target.wants`), y pasan a `/etc/default/rawkvm-agent-gadget` las opciones extra del `ExecStart` del gadget. No tocan `hub.toml` ni sus copias, `agent.toml`, `/var/lib/rawkvm` ni los ficheros de ajuste de `rawkvm-hub.service.d/`.
- **remove y purge.** `remove` para el servicio y conserva todo. `purge` borra solo lo de cada paquete, porque `/etc/rawkvm` es compartido: el hub, `hub.toml` y `/var/lib/rawkvm` (y avisa de que todos los agentes tendrán que volver a emparejarse); el gadget, `agent.toml` y su `modules-load.d`. `/etc/rawkvm` solo se borra si queda vacía, y el overlay de `config.txt` se queda.
- **CI** (`debs.yml`, llamado desde `ci.yml` en cada push y desde `release.yml`): construye, pasa lintian (solo avisos) e instala los paquetes `arm64` en un contenedor Debian `arm64` con QEMU (`deploy/deb/test-install.sh`). Empieza desde una copia de la instalación manual de la Pi y recorre migración, reinstalación, `remove`, `purge` e instalación limpia. Comprueba que el hub nuevo lee el `hub.toml` existente sin reescribirlo. El contenedor no tiene systemd en marcha: los scripts se saltan `systemctl` y lo demás se prueba igual.

## Consecuencias

- Instalar, actualizar y desinstalar es `apt install ./…deb` y `apt remove/purge`.
- Lo que el contenedor no prueba: el servicio arrancando bajo systemd, el reenchufado real del receptor, el initramfs, el montaje del journal y el modo dispositivo del OTG. Eso se valida en la Pi real.
- lintian solo da avisos, y se aceptan: binario estático (es la decisión de diseño), `systemctl` directo en los scripts en vez de `deb-systemd-helper` (no son paquetes del archivo de Debian), sin `changelog` ni página de manual, y `copyright` sin aviso.
- `deploy/*/install.sh` quedan como anexo para placas sin dpkg y para probar binarios sueltos.
