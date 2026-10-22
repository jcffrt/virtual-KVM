# ADR 0003 — El hub lee el receptor con `hid-generic` (modo A) y bloquea su evdev

- Estado: aceptada (2026-10-22), validada en el Pi.

## Contexto

Con `hid-logitech-dj` cargado (por defecto), el receptor entra en modo DJ: las interfaces 0 y 1 quedan sin driver ni hidraw y el kernel sintetiza un HID por dispositivo emparejado con un descriptor propio. El handoff prefiere reenviar el receptor tal cual. Desvincular el driver sin enlazar otro deja la interfaz sin hidraw. Además el **modo DJ persiste en el receptor** aunque se descargue el driver y se resetee el USB.

## Decisión

1. **Modo A (`match = "receiver"`, por defecto)**: `blacklist hid_logitech_dj` y `hid_logitech_hidpp` (`/etc/modprobe.d/rawkvm.conf`) para que `hid-generic` tome las tres interfaces. Se reenvían las tres (0 teclado boot, 1 ratón/consumer/system, 2 HID++/DJ) con su descriptor crudo; `HidDevice.id` = número de interfaz.
2. Al abrir la interfaz con el informe DJ corto (ID 0x20, página vendor), el hub escribe `20 FF 80 00 00 00 00 00 00 00 00 00 00 00 00` (cambio de modo con bitfield vacío) para sacar el receptor del modo DJ (`force_normal_mode = true`). Verificado en el Pi: el teclado vuelve al instante a emitir informes boot por la interfaz 0.
3. El hub hace `EVIOCGRAB` sobre todos los `/dev/input/event*` de las interfaces reenviadas para que la consola del Pi no consuma las teclas. Se repite en cada escaneo (cada 2 s) porque los nodos evdev pueden aparecer después del hidraw; el grab se libera al cerrar el fd (al salir el proceso).
4. **Modo B (`match = "dj-children"`)** queda como opción: reenvía los HID sintetizados por `hid-logitech-dj` (hijos del receptor en sysfs), con el VID/PID de cada dispositivo. No se fuerza el modo normal en este modo.

La identidad sale de los ioctls de hidraw (`HIDIOCGRDESC`, `HIDIOCGRAWINFO`, `HIDIOCGRAWNAME`, `HIDIOCGRAWPHYS`); sysfs aporta el driver, `bInterfaceNumber`, el HID padre y los nodos evdev.

## Revisión (initramfs y driver distinto de hid-generic)

En Armbian (y Raspberry Pi OS con initramfs) el receptor se enumera dentro del initramfs, que lleva su propia copia de `modprobe.d`: tras reiniciar se cargaba `hid_logitech_dj` igualmente, el hub abría solo la interfaz 2 (bound a `logitech-djreceiver`) y el comando DJ-off no servía. Desde entonces:

- `deploy/hub-linux/install.sh` ejecuta `update-initramfs -u` (si existe) tras instalar el conf.
- `[receiver] require_hid_generic = true` (por defecto): en modo A el hub no abre interfaces con otro driver y lo registra a nivel error, una vez por interfaz y driver, con la causa probable. Con `false` se reenvían con un aviso, como antes.

## Consecuencias

- El servicio corre como **root** (hidraw, evdev y escritura en HID++).
- El Pi deja de poder usar el teclado localmente mientras el hub corre (deseado).
- Las teclas de todos los teclados emparejados salen multiplexadas por la interfaz 0: en los PCs destino aparece un único teclado "Logitech USB Receiver", como si el receptor estuviera enchufado allí.
