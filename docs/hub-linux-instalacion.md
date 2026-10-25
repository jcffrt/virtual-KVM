# Hub en Linux (Orange Pi, Raspberry Pi): instalación, registro y diagnóstico

El hub va en la placa que tiene enchufado el receptor de Logitech. Se instala con un paquete `.deb`. La instalación a mano queda en el [anexo](#anexo-instalación-manual). Decisiones de empaquetado: [ADR 0022](adr/0022-paquetes-deb.md).

Sistemas: Armbian, Orange Pi OS o Raspberry Pi OS. Placas de 64 bits (Orange Pi Zero 2W, Pi 4, Pi Zero 2 W): paquete `arm64`. Raspberry Pi OS de 32 bits: `armhf`.

## Instalar

1. Descargar el paquete y `SHA256SUMS` de la última release ([ADR 0021](adr/0021-distribucion-por-releases.md)):

   ```sh
   v=$(curl -fsSL https://api.github.com/repos/jcffrt/virtual-KVM/releases/latest | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p')
   base=https://github.com/jcffrt/virtual-KVM/releases/download/v$v
   curl -fL -O "$base/rawkvm-hub_${v}_arm64.deb" -O "$base/SHA256SUMS"
   ```

   Alternativas: `gh release download --repo jcffrt/virtual-KVM -p 'rawkvm-hub_*_arm64.deb' -p SHA256SUMS`, o descargarlos en otro equipo y copiarlos con `scp`.
2. Comprobar e instalar:

   ```sh
   sha256sum -c --ignore-missing SHA256SUMS
   sudo apt install ./rawkvm-hub_*_arm64.deb
   ```

3. Ver el **token de emparejamiento**, que piden los agentes una vez:

   ```sh
   sudo rawkvm-hub --show-token
   ```

Qué hace el paquete:

- Instala `/usr/bin/rawkvm-hub` y el servicio `rawkvm-hub.service`, y lo activa y lo arranca. El hub corre como root (`/dev/hidraw*`, ADR 0003).
- Pone en la lista negra `hid_logitech_dj` y `hid_logitech_hidpp` (`/etc/modprobe.d/rawkvm.conf`). En la primera instalación los descarga, reenchufa los dispositivos Logitech y regenera el initramfs si existe, porque en Armbian el receptor se enumera dentro del initramfs. Las actualizaciones no vuelven a hacerlo, salvo que esos módulos estén cargados, así que no cortan el teclado.
- Deja el registro del sistema persistente en la SD (ver [Registro persistente](#registro-persistente)).
- Instala y activa el **vigilante del Wi-Fi**, `rawkvm-netwatch` (ver [Vigilante del Wi-Fi](#vigilante-del-wi-fi)).
- **No** instala `/etc/rawkvm/hub.toml`: el hub lo crea en su primer arranque con un id y un token nuevos. Hay un ejemplo con todos los valores por defecto en `/usr/share/doc/rawkvm-hub/hub.toml.example`. Para cambiar algo, copiar solo esas líneas a `hub.toml` y `sudo systemctl restart rawkvm-hub`.

Comprobar:

```sh
rawkvm-hub --version
systemctl status rawkvm-hub
journalctl -u rawkvm-hub -f     # "DJ mode switched off", "device ... added" x3, "advertising"
```

## Actualizar

Igual que instalar, con el paquete nuevo: `sudo apt install ./rawkvm-hub_<versión>_arm64.deb`. La configuración, el token, el certificado y el layout se conservan, y el servicio se reinicia con el binario nuevo. Todos los agentes deben tener la misma versión de protocolo que el hub (ver `docs/protocol.md`).

## Pasar de la instalación manual al paquete

Antes de la versión 0.5.0 el hub se instalaba con `deploy/hub-linux/install.sh`, en `/usr/local/bin` y con la unidad en `/etc/systemd/system`. El paquete lo detecta y:

- borra `/usr/local/bin/rawkvm-hub` y `/etc/systemd/system/rawkvm-hub.service`, que taparía la del paquete;
- conserva `/etc/rawkvm/hub.toml` y sus copias (`hub.toml.bak-*`), `/var/lib/rawkvm` (certificado, `layout.toml`, `nodes.toml`, ajustes) y los ficheros de ajuste de `/etc/systemd/system/rawkvm-hub.service.d/`: los agentes no tienen que volver a emparejarse;
- no regenera el initramfs ni reenchufa nada: eso ya se hizo con `install.sh`.

## Desinstalar

- `sudo apt remove rawkvm-hub` para el servicio y quita el programa. La configuración, el token y el certificado se quedan, y reinstalar los recupera.
- `sudo apt purge rawkvm-hub` borra además `/etc/rawkvm/hub.toml` y `/var/lib/rawkvm`. **El token y el certificado desaparecen: todos los agentes tendrán que emparejarse de nuevo** con el hub que se instale después. No toca `/etc/rawkvm/agent.toml` (es del gadget, si está en la misma placa) ni las copias `hub.toml.bak-*`.

## Vigilante del Wi-Fi

El firmware del Wi-Fi de la Orange Pi Zero 2W (chip uwe5622, driver `sprdwl`) a veces se cuelga. En el registro del kernel aparece `sprdwl:[WIFI_CMD_TX_DATA]timeout` y, segundos después, `NETDEV WATCHDOG: wlan0 (unisoc_wifi): transmit queue 0 timed out`. La placa sigue en marcha, pero sin radio: los equipos que llegan por la red se quedan sin teclado ni ratón (el PC del gadget USB no). Puede tardar más de diez minutos en volver solo.

`rawkvm-netwatch` (servicio `rawkvm-netwatch.service`, [ADR 0029](adr/0029-vigilante-del-wifi.md)) hace ping a la puerta de enlace cada 10 s. Cuando deja de responder durante 60 s, o en cuanto el kernel registra el cuelgue y el ping falla, escala:

1. **Reinicia el Wi-Fi**: `nmcli radio wifi off` y `on` con NetworkManager; si no, `ip link set wlan0 down` y `up`. Espera 30 s.
2. **Recarga el driver** (`modprobe -r sprdwl_ng` y `modprobe sprdwl_ng`), solo si el módulo está cargado. Espera 30 s.
3. **Reinicia la placa** (`journalctl --flush`, `sync`, `systemctl reboot`). Como máximo 3 veces por hora; después solo lo registra y vuelve a empezar por el paso 1. Así no entra en bucle si el que falla es el router.

Si el ping vuelve en cualquier momento, para y lo registra. No hace nada durante los 2 primeros minutos tras arrancar, mientras se levanta el Wi-Fi, ni mientras otra interfaz (Ethernet) tenga la ruta por defecto. Cada paso queda en el registro con su motivo:

```sh
journalctl -u rawkvm-netwatch            # «no answer from the gateway for 60 s: restarting the Wi-Fi», «network back after 75 s down»
sudo rawkvm-netwatch --check             # ruta, ping, módulos y reinicios de la última hora; no cambia nada
```

Opciones en `/etc/default/rawkvm-netwatch` (después, `sudo systemctl restart rawkvm-netwatch`):

- `--dry-run`: registra los pasos, pero no los da.
- `--modules none`: sin el paso 2.
- `--modules sprdwl_ng,uwe5622_bsp_sdio`: recarga también el módulo de la tarjeta.
- `--max-reboots`, `--fail-after`, `--grace`, etc.: `rawkvm-netwatch --help`.

Para pararlo hasta el siguiente arranque: `sudo systemctl stop rawkvm-netwatch`. Para desactivarlo del todo: `sudo systemctl disable --now rawkvm-netwatch`; las actualizaciones respetan esa decisión.

El **perro guardián del H616** (`sunxi-wdt`, ya activo) reinicia la placa si se cuelga el kernel, pero no ve un cuelgue de la radio: por eso hace falta este vigilante.

## Hora del sistema

La placa no tiene batería de reloj: arranca con una fecha falsa hasta que NTP la corrige. Las líneas del registro de antes de la sincronización llevan esa hora. El hub y `rawkvm-netwatch` lo dicen al pasar: «the system clock jumped by +N s (NTP?)». Para ver los tiempos desde el arranque, sin depender del reloj: `journalctl -b -o short-monotonic`. El hub no espera a NTP a propósito: tiene que funcionar también sin Internet.

## Registro persistente

Armbian y Orange Pi OS montan `/var/log` en RAM (zram, servicio `armbian-ramlog` u `orangepi-ramlog`) y solo lo copian a la SD (`/var/log.hdd`) de vez en cuando. Si la placa se cuelga o pierde la alimentación, el registro de esa sesión desaparece. Así pasó con la caída del 2026-10-23 11:16, que quedó sin causa conocida.

El paquete lo deja persistente (y `install.sh`, en la instalación manual):

- `/etc/systemd/journald.conf.d/rawkvm.conf`: `Storage=persistent`, como mucho 64 MB.
- Si encuentra `armbian-ramlog` u `orangepi-ramlog`, instala `/etc/systemd/system/var-log-journal.mount`. Esta unidad monta `/var/log.hdd/journal` (en la SD) sobre `/var/log/journal` después del servicio ramlog. El journal se escribe directamente en la SD; el resto de `/var/log` sigue en RAM.
- Sin ramlog (Raspberry Pi OS), crea `/var/log/journal`, que ya está en la SD.
- La primera vez reinicia `systemd-journald` y le pasa lo que tenía en memoria (`journalctl --flush`).

Comprobar:

```sh
findmnt /var/log/journal              # con ramlog: origen /var/log.hdd/journal (o el disco raíz)
journalctl --header | grep -m1 'File path'   # /var/log/journal/..., no /run/log/journal
journalctl --list-boots               # tras un reinicio, más de un arranque
```

Escribe en la SD de forma continua pero poco (el hub registra solo cambios de estado). Para volver al registro en RAM: `sudo systemctl disable --now var-log-journal.mount`, borrar `/etc/systemd/journald.conf.d/rawkvm.conf` y `sudo systemctl restart systemd-journald`.

## Después de una caída

Con la placa de vuelta (reset o alimentación):

```sh
journalctl --list-boots                       # el arranque anterior es el -1
journalctl -b -1 -e                           # final del arranque anterior: lo último antes de caer
journalctl -b -1 -k -e                        # solo el kernel (lo que daría dmesg): oops, under-voltage, mmc, brcmfmac/wifi
journalctl -b -1 -p warning                   # avisos y errores de todo el arranque anterior
journalctl -b -1 -u rawkvm-hub -u rawkvm-agent-gadget -e
journalctl -b 0 -k | head -50                 # inicio del arranque actual: cómo arrancó
```

Qué buscar:

- **Termina de golpe sin nada raro**: corte de alimentación, o un cuelgue que no dejó tiempo para escribir. En placas alimentadas desde el USB de un PC, mirar si el PC entró en suspensión o reinició su USB a esa hora.
- **`Under-voltage detected` o `voltage`** (Raspberry Pi): fuente o cable insuficientes.
- **`Oops`, `BUG:`, `Kernel panic`, `watchdog: BUG: soft lockup`**: fallo del kernel; anotar el módulo que aparezca.
- **Errores `mmc` o `EXT4-fs error`**: la SD.
- **Wi‑Fi** (`brcmfmac`, `wlan0: deauthenticating`, `CTRL-EVENT-DISCONNECTED`): la placa siguió viva pero sin red. En ese caso suele responder por cable o por consola serie; sin ping ni ARP desde la LAN no basta para saber si estaba colgada.
- **`rawkvm-hub` con `Main process exited`** y reinicios en bucle: fallo del hub, no de la placa (systemd lo vuelve a arrancar a los 2 s).

`last -x | head` muestra los reinicios y apagados registrados. Si falta un `shutdown` antes de un `reboot`, la caída no fue limpia.

## Anexo: instalación manual

Para placas sin `dpkg`, o para probar un binario recién compilado sin hacer el paquete. Hace lo mismo que el paquete, en las mismas rutas (`/usr/bin`, unidad en `/etc/systemd/system`):

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub   # en el Mac
scp target/aarch64-unknown-linux-musl/release/rawkvm-hub orangepi@<placa>:
scp -r deploy/hub-linux orangepi@<placa>:rawkvm-deploy
ssh orangepi@<placa> 'sudo sh rawkvm-deploy/install.sh ./rawkvm-hub'
```

Para hacer los paquetes en el Mac (lo mismo que `debs.yml`):

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub -p rawkvm-agent-gadget
cargo deb --no-build --no-strip --target aarch64-unknown-linux-musl -p rawkvm-hub
cargo deb --no-build --no-strip --target aarch64-unknown-linux-musl -p rawkvm-agent-gadget
ls target/debian/
```
