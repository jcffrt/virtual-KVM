# ADR 0029 — Vigilante del Wi-Fi en la placa del hub

- Estado: aceptada (2026-10-25), tras dos cuelgues del Wi-Fi el mismo día.

## Contexto

El 2026-10-25, a las 08:37 y a las 14:35 (hora local), el firmware del Wi-Fi de la Orange Pi Zero 2W (uwe5622, driver unisoc `sprdwl`) dejó de transmitir. En el kernel apareció:

1. `sprdwl:[WIFI_CMD_TX_DATA]timeout`;
2. `sprdwl_atcmd_assert … reason:3`;
3. 4 s después, `NETDEV WATCHDOG: wlan0 (unisoc_wifi): transmit queue 0 timed out`.

La placa siguió viva: el hub funcionaba y el gadget USB seguía dando teclado a GAMER. Solo murió la radio. El Mac y el Ubuntu se quedaron sin teclado ni ratón. La primera vez se recuperó sola en unos 12 minutos; la segunda, el usuario desenchufó la placa a los 8.

Además, la placa no tiene batería de reloj. Arranca con una fecha falsa hasta que NTP la corrige, y las líneas del registro de antes llevan esa hora.

## Decisión

- **`rawkvm-netwatch`**: un segundo binario del paquete del hub, en Rust, con su servicio `rawkvm-netwatch.service`.
  - **Lógica:** la decisión está separada de la E/S (`rawkvm_hub::netwatch::Watch`: recibe observaciones y devuelve acciones) y probada con tests sin hardware.
  - **Qué observa:** cada 10 s, la ruta por defecto (`ip route show default`) y un ping a la puerta de enlace por `wlan0`. Además, un hilo lee `/dev/kmsg` desde el final y marca las dos firmas del cuelgue (`WIFI_CMD_TX_DATA]timeout`, `transmit queue 0 timed out`).
  - **Cuándo actúa:** cuando la puerta de enlace no responde durante 60 s, o en cuanto hay una firma del kernel y el ping falla (la firma sola, con ping, solo se registra). Sin ruta por defecto también cuenta como caído.
  - **Escalada**, con 30 s de espera entre pasos:
    1. reiniciar el Wi-Fi: con NetworkManager en marcha, `nmcli radio wifi off/on`; sin él, `ip link set wlan0 down/up`;
    2. recargar los módulos de `--modules` (por defecto `sprdwl_ng`), solo si están cargados; si no hay ninguno, el paso se salta;
    3. `journalctl --flush`, `sync` y `systemctl reboot`.

    Cada orden tiene un tiempo máximo. Si un `modprobe -r` se queda bloqueado en el kernel, se abandona y la escalada sigue.
  - **Límites:**
    - 2 minutos de gracia tras arrancar, mientras se levanta el Wi-Fi;
    - como máximo 3 reinicios por hora. Las horas se guardan en `/var/lib/rawkvm-netwatch/reboots`, y una hora en el futuro también cuenta, porque el reloj arranca atrasado. Pasado el límite solo registra y vuelve al paso 1, para no entrar en bucle si el que falla es el router;
    - no hace nada mientras otra interfaz (Ethernet) tenga la ruta por defecto, ni si la placa no tiene `wlan0`.
  - **`--dry-run`** registra los pasos sin darlos. **`--check`** muestra la ruta, el ping, los módulos y los reinicios recientes, sin cambiar nada.
- **Paquete:**
  - Instala el binario, la unidad y el conffile `/etc/default/rawkvm-netwatch`.
  - Lo activa una sola vez. La marca está en `/var/lib/rawkvm-netwatch`, así que un `systemctl disable` del usuario sobrevive a las actualizaciones. `systemctl stop` lo para hasta el siguiente arranque.
  - `purge` borra el estado, la marca y las opciones.
- **Hora:** el hub y `rawkvm-netwatch` registran cuándo salta el reloj de pared respecto al monotónico, más de 30 s: «the system clock jumped by +N s (NTP?)». La guía recuerda `journalctl -o short-monotonic`.

Descartadas:

- **Dejar que se recupere sola.** Tarda más de diez minutos, o no vuelve, y mientras tanto los equipos de red se quedan sin teclado.
- **El perro guardián del H616 (`sunxi-wdt`).** Ya está activo, a 16 s, pero solo cubre cuelgues del kernel; con la radio colgada el kernel sigue sano.
- **Un script de shell.** La escalada tiene estado (tiempos, pasos, límite de reinicios). En Rust se prueba con tests unitarios en la CI y comparte el binario estático y el empaquetado del hub.
- **Hacer que el hub espere a NTP** (`systemd-time-wait-sync`, `After=time-sync.target`). Sin Internet, el hub no arrancaría, y tiene que funcionar solo en la LAN.
- **Apagar el Bluetooth** (mismo chip), que el orquestador ya probó en la placa. Puede reducir los cuelgues, pero no los cubre.

## Consecuencias

- Un cuelgue de la radio dura, en el peor caso, unos 2 minutos hasta el reinicio, más lo que tarde en arrancar, en lugar de 10 o más.
- Si la causa es el router, la placa se reinicia hasta 3 veces en una hora y después solo lo registra.
- **Sin probar en hardware.** No se sabe si `modprobe -r sprdwl_ng` funciona en esta placa, ni si `uwe5622_bsp_sdio` se deja descargar. Por eso el paso 2 recarga solo `sprdwl_ng`, y si se bloquea se abandona y se pasa al reinicio. Hay que comprobarlo en un rato sin uso.
