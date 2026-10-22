# Validación de la Fase 0

Objetivo (handoff §10): teclear en el Logitech conectado al Pi escribe en el Ubuntu por Wi‑Fi, y `evtest` muestra el dispositivo con nombre Logitech. Además medimos RTT y pérdidas antes de pasar a la Fase 1.

## 0. Requisitos

- Orange Pi `192.168.1.28` con el receptor Unifying enumerado (`lsusb` muestra `046d:c52b`). Si se usa el USB‑C OTG, debe estar en modo host (overlay `usb0-host`); en modo periférico `musb-hdrc` queda en `b_idle` y el receptor no aparece.
- Los dispositivos Logitech en el canal Easy‑Switch del receptor del Pi.
- PC Ubuntu 24.04 en la misma LAN.
- En el Mac: `zig` y `cargo-zigbuild` (ya instalados).

## 1. Compilar (en el Mac)

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub
cargo zigbuild --release --target x86_64-unknown-linux-gnu -p rawkvm-agent-linux
```

Binarios: `target/aarch64-unknown-linux-musl/release/rawkvm-hub` y `target/x86_64-unknown-linux-gnu/release/rawkvm-agent`.

## 2. Instalar el hub

```sh
scp target/aarch64-unknown-linux-musl/release/rawkvm-hub orangepi@192.168.1.28:
scp -r deploy/hub-linux orangepi@192.168.1.28:rawkvm-deploy
ssh orangepi@192.168.1.28 'sudo sh rawkvm-deploy/install.sh ./rawkvm-hub'
```

`install.sh` pone la blacklist de `hid_logitech_dj`/`hid_logitech_hidpp`, descarga esos módulos, resetea el receptor, instala y arranca `rawkvm-hub.service` e imprime el **token de emparejamiento**. Volver a verlo: `sudo rawkvm-hub --show-token`.

Comprobaciones en el Pi:

```sh
sudo systemctl stop rawkvm-hub
sudo rawkvm-hub --list          # tres nodos [forward] 046d:c52b, driver hid-generic, interfaces 0/1/2
sudo rawkvm-hub --dump          # teclear y mover el ratón: aparecen informes [0] de 8 B y [1] de ID 02
sudo systemctl start rawkvm-hub
journalctl -u rawkvm-hub -f     # "DJ mode switched off", "device ... added" x3, "advertising"
```

`--dump` imprime las teclas: solo para diagnóstico. `--dump --record captura.hidrec` además lo graba para reproducirlo luego con `--replay` (por ejemplo en el Mac, sin receptor).

## 3. Instalar el agente en Ubuntu

```sh
scp target/x86_64-unknown-linux-gnu/release/rawkvm-agent usuario@ubuntu:
scp -r deploy/linux usuario@ubuntu:rawkvm-deploy
# en el Ubuntu:
sh rawkvm-deploy/install.sh ./rawkvm-agent
# (si añadió el usuario al grupo input: cerrar sesión y volver a entrar)
~/.local/bin/rawkvm-agent --hub 192.168.1.28 --token <token>
```

Debe mostrar `hub certificate pinned`, `connected`, tres `virtual device created` y `active`. Sin `--hub` lo busca por mDNS. Con Ctrl+C y después `systemctl --user start rawkvm-agent` queda como servicio (`journalctl --user -u rawkvm-agent -f`).

## 4. Criterios de aceptación

1. **Escritura**: abrir un editor en Ubuntu y teclear en el MX Keys / K230 → aparece el texto. Mover el MX Ergo → se mueve el puntero.
2. **evtest**: `sudo evtest` lista `Logitech USB Receiver` (varias entradas: teclado, ratón, consumer/system). Elegir la de teclado y comprobar `EV_KEY` al pulsar.
3. **Modificadores**: Ctrl, Shift, Alt, AltGr y Super se comportan como con el receptor enchufado directamente al Ubuntu.
4. **El Pi no consume teclas**: en la consola del Pi (HDMI) no aparece nada al teclear.
5. **Sin teclas pegadas**: mantener una tecla, detener el agente con Ctrl+C → no queda autorrepitiendo (los dispositivos virtuales se destruyen).

## 5. Latencia y pérdidas

El agente registra cada 10 s:

```
stats rtt_ms=4.2 quic_rtt_ms=3.9 received=1843 lost=0 dup_or_late=212 active=true
```

- `rtt_ms`: ida y vuelta de `Ping` sobre el stream de control (EWMA). La latencia de un informe es ≈ `rtt_ms / 2` más el procesado (< 1 ms). Objetivo del handoff: ≤ 15 ms p95 extremo a extremo ⇒ `rtt_ms` estable por debajo de ~25 ms.
- `lost`: huecos de `seq` (datagramas perdidos). `dup_or_late`: sobre todo las copias redundantes de teclado (ADR 0004), es normal que crezca al teclear.

Prueba recomendada, 5 minutos cada una, anotando el peor `rtt_ms` y `lost`:

| Escenario | rtt_ms (mín / máx) | lost | Notas |
|---|---|---|---|
| Reposo | | | |
| Moviendo el trackball sin parar | | | |
| Tecleando | | | |
| Con una descarga grande en otro equipo de la red | | | |

Si `rtt_ms` supera 25 ms con frecuencia, probar 5 GHz, desactivar el ahorro de energía del Wi‑Fi del Pi (`sudo iw dev wlan0 set power_save off`) y repetir antes de la Fase 1.

## 6. Atajo de emergencia

Con un segundo agente conectado (otro PC Linux), pulsar **Ctrl, Ctrl, 2**: la entrada pasa al agente con índice 2 (el índice aparece en el log `connected ... index=N`). El `2` no se escribe en ninguno.

## 7. Problemas conocidos

- `--list` no muestra nodos `[forward]` y sí `logitech-djreceiver`: la blacklist no está activa; repetir `install.sh` o reiniciar.
- El agente dice `the hub certificate is not the pinned one`: se reinstaló el hub y cambió su certificado. Ejecutar `rawkvm-agent --unpin` una vez.
- `opening /dev/uhid`: falta la regla udev, el módulo `uhid` o pertenecer al grupo `input`.
