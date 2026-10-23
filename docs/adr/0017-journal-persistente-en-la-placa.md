# ADR 0017 — Registro persistente en la placa del hub

- Estado: aceptada (2026-10-23), bloque R2.

## Contexto

La Orange Pi se cayó el 2026-10-23 a las 11:16: sin ping ni ARP y con el PC que la alimenta encendido. Armbian y Orange Pi OS tienen `/var/log` en zram (`armbian-ramlog` / `orangepi-ramlog`) y solo lo copian a `/var/log.hdd` periódicamente y al apagar, así que tras el reset no quedó registro de esa sesión. El orquestador lo arregló a mano en la Pi y lo verificó.

## Decisión

- `deploy/hub-linux/install.sh` instala `journald.conf.d/rawkvm.conf` (`Storage=persistent`, `SystemMaxUse=64M`).
- Si existe el servicio ramlog (se detecta `armbian-ramlog` u `orangepi-ramlog`) y `/var/log.hdd`, instala `var-log-journal.mount` a partir de `var-log-journal.mount.in`. La unidad hace un bind de `/var/log.hdd/journal` sobre `/var/log/journal`. Lleva `DefaultDependencies=no`, va `After=`/`Requires=` del servicio ramlog y `Before=systemd-journal-flush.service sysinit.target`. `systemd-journal-flush` tiene `RequiresMountsFor=/var/log/journal`, así que espera al bind.
- Sin ramlog, basta con crear `/var/log/journal`.
- Lo que mirar tras una caída se documenta en `docs/hub-linux-instalacion.md` (`journalctl -b -1`, el kernel del arranque anterior, `last -x`) en vez de un `--diagnose` en el hub: son órdenes del sistema, no del hub.
- Sin watchdog de hardware por ahora (fuera de alcance del brief).

## Consecuencias

- El journal del arranque anterior sobrevive a un corte o a un cuelgue, salvo lo que aún estuviera en la caché de escritura (journald sincroniza cada 5 min, y al momento los mensajes de prioridad crit o superior).
- El journal escribe en la SD de forma continua pero en poca cantidad. El tope de 64 MB limita el espacio.
- La SD guarda el journal en dos sitios a la vez: el bind y la copia periódica del ramlog, que copia `/var/log` (con el bind) sobre `/var/log.hdd`. Como son los mismos ficheros, la copia no cambia nada; queda a revisar si el ramlog los duplica en zram al arrancar.
