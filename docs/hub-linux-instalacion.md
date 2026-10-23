# Hub en Linux (Orange Pi, Raspberry Pi): registro y diagnóstico

La instalación del hub está en [fase0-validacion.md](fase0-validacion.md) §2 (`deploy/hub-linux/install.sh`). Aquí, el registro persistente y qué mirar cuando la placa se cae.

## Registro persistente

Armbian y Orange Pi OS montan `/var/log` en RAM (zram, servicio `armbian-ramlog` u `orangepi-ramlog`) y solo lo copian a la SD (`/var/log.hdd`) de vez en cuando. Si la placa se cuelga o pierde la alimentación, el registro de esa sesión desaparece. Así pasó con la caída del 2026-10-23 11:16, que quedó sin causa conocida.

`install.sh` lo deja persistente:

- `/etc/systemd/journald.conf.d/rawkvm.conf`: `Storage=persistent`, como mucho 64 MB.
- Si encuentra `armbian-ramlog` u `orangepi-ramlog`, instala `/etc/systemd/system/var-log-journal.mount`. Esta unidad monta `/var/log.hdd/journal` (en la SD) sobre `/var/log/journal` después del servicio ramlog. El journal se escribe directamente en la SD; el resto de `/var/log` sigue en RAM.
- Sin ramlog (Raspberry Pi OS), crea `/var/log/journal`, que ya está en la SD.
- Reinicia `systemd-journald` y le pasa lo que tenía en memoria (`journalctl --flush`).

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
