# Agente USB gadget (Raspberry Pi → PC por cable)

Una placa Linux con puerto USB en **modo dispositivo** recibe por Wi‑Fi los informes del hub y los reproduce por USB. El PC conectado por cable ve un **Logitech USB Receiver 046D:C52B** con las mismas interfaces y descriptores que el receptor real: **no necesita ningún driver ni software** (ni test-signing en Windows ni Karabiner en macOS). Diseño y límites: [ADR 0007](adr/0007-agente-usb-gadget.md).

Un gadget sirve a **un** PC. Para varios PCs, una placa por PC (o combinar con los agentes de software).

Para cambiar de equipo **por el borde de la pantalla** en el PC Windows, instalar además el helper de borde y el widget con el instalador de [windows-instalacion.md](windows-instalacion.md) (sin driver ni administrador).

**Es la vía recomendada para Windows** (el driver VHF de [windows-driver-vhf.md](windows-driver-vhf.md) queda como alternativa aparcada). La placa puede ser la misma que hace de hub: en la Orange Pi Zero 2W el receptor va en el USB‑C host y el PC en el USB‑C OTG.

### Alimentación (importante con la Orange Pi)

En la Orange Pi Zero 2W (y en la Pi 4 o la Pi Zero 2 W) el puerto OTG es **también la entrada de alimentación**. Si la placa se alimenta solo del PC, se apaga o reinicia cuando el PC se suspende o se apaga, o cuando la BIOS corta los 5 V del USB (ErP/EuP), y si además es el hub, **todos los agentes pierden el hub**. Soluciones:

- Alimentar la placa por los pines **5V (2 o 4) y GND (6)** del conector de 40 pines (comprobar en el pinout de la placa) con una fuente propia de 5 V / 2 A; el OTG sigue funcionando para datos. Lo ideal es un cable USB sin la línea de 5 V (o un adaptador "power blocker") para no enfrentar las dos fuentes; muchos PCs lo toleran, pero no es seguro con todos.
- O comprobar que el PC mantiene 5 V en USB en suspensión y apagado (opción de la BIOS «USB power in S3/S4/S5» y ErP desactivado).

### Cortes de alimentación y la tarjeta SD

Si la placa se alimenta del PC, **cada apagado del PC es un corte brusco**. Linux guarda las escrituras en memoria un tiempo antes de llevarlas a la SD, y Armbian suele montar la raíz con `commit=600` (comprobar con `grep commit /etc/fstab`): hasta **10 minutos**. Un corte poco después de un despliegue ya dejó una vez el binario del hub corrupto y `hub.toml` y la unit en versiones anteriores. Para reducirlo:

- Los instaladores terminan con `sync`. El hub y los agentes escriben sus ficheros (`hub.toml`, `nodes.toml`, `layout.toml`, `agent.toml`) de forma atómica y con `fsync`. Aun así, tras copiar binarios a mano, ejecutar `sync` y **esperar unos segundos** antes de apagar el PC o desenchufar.
- Bajar el intervalo en `/etc/fstab`: cambiar `commit=600` por `commit=5` (o `commit=1`; más escrituras y más desgaste de la SD) en la línea de `/`. Aplicar con `sudo mount -o remount /`. La opción `sync` hace todo muy lento y no se recomienda.
- Lo más seguro es alimentar la placa por el conector de 40 pines (ver arriba): entonces no se apaga con el PC.
- **El journal tras un apagado limpio**: en Armbian y Orange Pi OS, `/var/log` vive en RAM (`armbian-ramlog`/`orangepi-ramlog`) y el paquete del hub lleva el journal a la SD. El paquete añade además `XTRA_RSYNC_TO=(… --exclude journal)` y `XTRA_RSYNC_FROM=(--exclude journal)` en `/etc/default/orangepi-ramlog` (o `armbian-ramlog`); sin eso, el ramlog borraba al apagar el journal de ese arranque. Así `journalctl -b -1` muestra el apagado anterior. Con la instalación manual (`install.sh`), añadir esas dos líneas a mano.
- **Apagado limpio** (hub v7, ADR 0025): si la placa se alimenta de un PC Windows, activar en el widget de ese PC **Opciones → Este PC → Apagar el hub cuando este PC se apague**. Al apagar o reiniciar el PC, el widget pide al hub que se apague y espera su confirmación (como mucho 3 s): el hub para el gadget, hace `sync` y `systemctl poweroff`. Cerrar sesión o suspender no lo apaga. Para apagarlo a mano: **Apagar hub** en la pestaña Hub de cualquier widget, o `sudo systemctl poweroff` en la placa por SSH. En los dos casos, esperar a que el LED de la placa se apague antes de desenchufarla. No evita que el hub desaparezca mientras el PC está apagado: para eso, la alimentación propia de arriba.
- Comprobar qué binario corre: `rawkvm-hub --version` y `rawkvm-agent-gadget --version` muestran la versión y el commit. Lo mismo aparece en la primera línea del log de cada arranque (`rawkvm-hub starting version=…`).

Cuando el PC deja de leer (apagado, suspendido o cable fuera), el agente lo registra una vez («the USB host is not reading reports»), descarta los informes sin esperar y avisa al volver.

### Alt y Win cruzadas en Windows (MX Keys)

Desde la Fase 3 (hub v4) no hace falta nada: al cambiar de equipo, el hub pone por HID++ la disposición de teclas del MX Keys en la del sistema del equipo activo (la del `--host-os` del gadget, Windows por defecto; Linux usa también la de Windows), igual que Fn+P / Fn+O. Ver ADR 0014. **Quitad `swap_alt_gui` de `/etc/rawkvm/hub.toml`**: con las dos cosas activas, Alt y Win vuelven a quedar cruzadas (el hub lo avisa al arrancar).

Para forzar otra disposición en un equipo, con su índice (`sudo rawkvm-hub --list-nodes`):

```toml
[[node_options]]
index = 1
os_profile = "windows"   # "macos", "linux" (= Windows) o "none" (no tocar)
```

`[hidpp] os_profile = false` desactiva el cambio para todos. Teclados sin esa función HID++ (el K230) no cambian; para ellos sigue existiendo `swap_alt_gui = true` (ADR 0011), que afecta a todos los teclados del receptor cuando la entrada va a ese equipo. Después de editar, `sudo systemctl restart rawkvm-hub`.

### Logitech Options+ / G HUB en el PC

Si el PC tiene instalados Logitech Options+, Logi Options o G HUB, intentarán hablar HID++ con el «receptor» del gadget y no obtendrán respuesta (el gadget descarta lo que el PC envía). Es inocuo: teclado y ratón funcionan igual, pero esos programas no verán el receptor como gestionable (ni batería, ni emparejar, ni botones configurables). La configuración de los botones se hace en el PC donde esté el receptor real, o no se hace.

## Placas

| Placa | Puerto al PC | Notas |
|---|---|---|
| Raspberry Pi Zero 2 W | micro USB marcado **USB** (no PWR) | Cable micro USB ↔ USB-A/C. Se alimenta del PC por ese mismo cable. |
| Raspberry Pi Zero / Zero W (originales) | micro USB **USB** | El hardware sirve, pero son armv6: no hay binario en CI (compilar para `arm-unknown-linux-musleabihf`). |
| Raspberry Pi 4 B | **USB-C** (el de alimentación) | Alimentación desde el PC: algunos puertos no dan los 1–3 A que pide; usar un puerto USB-C del PC o un Y-cable con alimentación aparte. |
| Raspberry Pi 3 A+ | el USB-A | Necesita cable USB-A ↔ USB-A. **Cuidado**: un cable A‑A con VBUS conectado une las alimentaciones de Pi y PC; alimentar la Pi solo por ese cable o usar un cable/adaptador sin VBUS. |
| **Orange Pi Zero 2W** (primera placa del proyecto) | USB-C de **alimentación** (OTG, `musb`) | Comprobado (kernel 6.1.31): `usb@5100000` con `dr_mode=peripheral`, UDC `musb-hdrc.5.auto`, `CONFIG_USB_CONFIGFS_F_HID=y`; solo hay que cargar `libcomposite`. **No** aplicar el overlay `sun50i-h616-usb0-host`: convierte ese puerto en host. El otro USB-C es solo host. |
| Raspberry Pi 3 B / 3 B+ / 2 / 1 B | — | **No sirven**: el OTG del SoC está detrás del hub/Ethernet LAN9514. |
| Raspberry Pi 5 | USB-C | No probado. |

Sistema: Raspberry Pi OS Bookworm (64 bits recomendado; 32 bits también) o Armbian. Wi‑Fi en la misma LAN que el hub.

## 1. Instalar

Con el paquete `.deb` de la release ([ADR 0022](adr/0022-paquetes-deb.md)): `arm64` para la Orange Pi, la Pi Zero 2 W, la Pi 3 A+ y la Pi 4 con sistema de 64 bits; `armhf` para Raspberry Pi OS de 32 bits. Descargarlo igual que el del hub ([hub-linux-instalacion.md](hub-linux-instalacion.md#instalar)), cambiando el nombre del paquete por `rawkvm-agent-gadget_${v}_arm64.deb`, y en la placa:

```sh
sha256sum -c --ignore-missing SHA256SUMS
sudo apt install ./rawkvm-agent-gadget_*_arm64.deb
```

El paquete (se puede reinstalar cuando se quiera):

1. En una Raspberry Pi añade `dtoverlay=dwc2,dr_mode=peripheral` a `/boot/firmware/config.txt` (o `/boot/config.txt`).
2. Carga `dwc2` (en la Pi) y `libcomposite` ahora y en cada arranque (`/etc/modules-load.d/rawkvm-gadget.conf`), y monta ConfigFS si hace falta.
3. Instala `/usr/bin/rawkvm-agent-gadget` y `rawkvm-agent-gadget.service` (root).
4. **No empareja**: durante la instalación no hay garantía de red. Si la placa aún no está emparejada, no arranca el servicio e indica el paso siguiente.
5. Si cambió el overlay, pide **reiniciar**; el servicio arranca después.

## 2. Emparejar

Una sola vez, con el token del hub (`sudo rawkvm-hub --show-token` en el hub). El token se pide sin mostrarlo en pantalla, así no queda en el historial:

```sh
sudo rawkvm-agent-gadget --pair --hub 192.168.1.28
sudo systemctl enable --now rawkvm-agent-gadget
```

`--pair` se conecta al hub, fija su certificado, guarda todo en `/etc/rawkvm/agent.toml` y termina con «paired with the hub at …». No hay que pulsar Ctrl+C. Sin `--hub`, busca el hub por mDNS. Con `--token-file FICHERO` lee el token de un fichero en vez de pedirlo. Repetir `--pair` sirve también para emparejar con un hub reinstalado (olvida el certificado anterior).

**Sistema del PC**: el agente anuncia al hub el sistema del PC enchufado por USB (no el de la placa) para que el hub ponga la disposición de teclas correcta. Por defecto es Windows. Para otro sistema, en `/etc/default/rawkvm-agent-gadget`:

```sh
RAWKVM_GADGET_ARGS="--host-os macos"
```

**PC con Linux**: `RAWKVM_GADGET_ARGS="--host-os linux --no-hidpp"`. Si no, `hid-logitech-dj` del PC pondrá nuestro «receptor» en modo DJ y las interfaces de teclado y ratón quedarán mudas (ver ADR 0007). En Windows y macOS no hace falta.

Después de cambiarlo: `sudo systemctl restart rawkvm-agent-gadget`.

**Desde la instalación manual** (antes de la 0.5.0: `/usr/local/bin` y la unidad en `/etc/systemd/system`): el paquete borra el binario y la unidad viejos, pasa a `/etc/default/rawkvm-agent-gadget` las opciones que tuviera su `ExecStart` (`--host-os`, `--no-hidpp`) y conserva el emparejamiento. Si ya estaba emparejada, el servicio sigue arrancando solo.

**Desinstalar**: `sudo apt remove rawkvm-agent-gadget` conserva el emparejamiento; `sudo apt purge rawkvm-agent-gadget` lo borra (`/etc/rawkvm/agent.toml`). Ninguno de los dos toca el `hub.toml` del hub, si está en la misma placa, ni el overlay de `config.txt`.

## 3. Comprobar

En la placa:

```sh
journalctl -u rawkvm-agent-gadget -f
```

Debe mostrar `connected addr=192.168.1.28:4433 … index=N`, `USB gadget 046d:c52b "Logitech USB Receiver" bound to <udc> with 3 interfaces` e `interface ready on /dev/hidg0` (y 1, 2).

En el PC, al conectar el cable:

- **Windows**: Administrador de dispositivos → *Teclados* y *Mouse* muestran dispositivos HID nuevos; en *Dispositivos de interfaz humana*, "Dispositivo de entrada USB" con VID_046D&PID_C52B.
- **macOS**: Información del sistema → USB → *USB Receiver* (Logitech). La primera vez, el Asistente de configuración del teclado.
- **Linux**: `lsusb` → `046d:c52b Logitech, Inc. Unifying Receiver`; `sudo evtest` lista *Logitech USB Receiver*.

Después: pasar la entrada a este agente (**Ctrl, Ctrl, N** o si es el único conectado) y teclear / mover el ratón.

## 4. Problemas conocidos

| Síntoma en el log | Causa |
|---|---|
| `waiting for a USB device controller` y después `no USB device controller in /sys/class/udc after 60 s` | Falta el overlay `dwc2` o reiniciar; en otras placas el OTG está en modo host en el device tree. El servicio espera hasta 60 s (`--udc-wait`). |
| `creating /sys/kernel/config/usb_gadget/rawkvm (is libcomposite loaded?)` | `sudo modprobe libcomposite`; ConfigFS no montado. |
| `binding to … (is another gadget using it?)` | Otro gadget usa el UDC (`g_ether`/`g_serial` en `modules-load`, o `usb_gadget` de otra herramienta): quitarlo. |
| `the USB host is not reading reports` | El PC duerme o está apagado, el cable no lleva datos o no está conectado. Los informes se descartan sin esperar hasta `the USB host is reading reports again`. |
| El PC Linux reconoce el receptor pero no escribe | Falta `--no-hidpp` (modo DJ). |
| El gadget sale con menos de 3 interfaces (`bound to … with 1 interfaces`) | El hub no reenvía todo el receptor: en la placa del hub `hid_logitech_dj` se cargó desde el initramfs. Ver «Problemas conocidos» en `docs/fase0-validacion.md` (`update-initramfs -u` y reiniciar). |

El agente espera a que la lista de dispositivos del hub lleve 500 ms sin cambios antes de crear el gadget (el hub anuncia las interfaces una a una al abrirlas), así el PC ve una sola enumeración.

## Anexo: instalación manual

Para placas sin `dpkg`, o para probar un binario recién compilado. Mismas rutas que el paquete:

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-agent-gadget   # en el Mac
scp target/aarch64-unknown-linux-musl/release/rawkvm-agent-gadget pi@<placa>:
scp -r deploy/gadget pi@<placa>:rawkvm-gadget
ssh -t pi@<placa> 'sudo sh rawkvm-gadget/install.sh ./rawkvm-agent-gadget && sudo rawkvm-agent-gadget --pair --hub 192.168.1.28'
```
