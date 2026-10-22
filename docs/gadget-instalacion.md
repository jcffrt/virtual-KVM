# Agente USB gadget (Raspberry Pi → PC por cable)

Una placa Linux con puerto USB en **modo dispositivo** recibe por Wi‑Fi los informes del hub y los reproduce por USB. El PC conectado por cable ve un **Logitech USB Receiver 046D:C52B** con las mismas interfaces y descriptores que el receptor real: **no necesita ningún driver ni software** (ni test-signing en Windows ni Karabiner en macOS). Diseño y límites: [ADR 0007](adr/0007-agente-usb-gadget.md).

Un gadget sirve a **un** PC. Para varios PCs, una placa por PC (o combinar con los agentes de software).

**Es la vía recomendada para Windows** (el driver VHF de [windows-instalacion.md](windows-instalacion.md) queda como alternativa aparcada). La placa puede ser la misma que hace de hub: en la Orange Pi Zero 2W el receptor va en el USB‑C host y el PC en el USB‑C OTG.

### Alimentación (importante con la Orange Pi)

En la Orange Pi Zero 2W (y en la Pi 4 o la Pi Zero 2 W) el puerto OTG es **también la entrada de alimentación**. Si la placa se alimenta solo del PC, se apaga o reinicia cuando el PC se suspende o se apaga, o cuando la BIOS corta los 5 V del USB (ErP/EuP), y si además es el hub, **todos los agentes pierden el hub**. Soluciones:

- Alimentar la placa por los pines **5V (2 o 4) y GND (6)** del conector de 40 pines (comprobar en el pinout de la placa) con una fuente propia de 5 V / 2 A; el OTG sigue funcionando para datos. Lo ideal es un cable USB sin la línea de 5 V (o un adaptador "power blocker") para no enfrentar las dos fuentes; muchos PCs lo toleran, pero no es seguro con todos.
- O comprobar que el PC mantiene 5 V en USB en suspensión y apagado (opción de la BIOS «USB power in S3/S4/S5» y ErP desactivado).

### Cortes de alimentación y la tarjeta SD

Si la placa se alimenta del PC, **cada apagado del PC es un corte brusco**. Linux guarda las escrituras en memoria un tiempo antes de llevarlas a la SD, y Armbian suele montar la raíz con `commit=600` (comprobar con `grep commit /etc/fstab`): hasta **10 minutos**. Un corte poco después de un despliegue ya dejó una vez el binario del hub corrupto y `hub.toml` y la unit en versiones anteriores. Para reducirlo:

- Los instaladores terminan con `sync`. El hub y los agentes escriben sus ficheros (`hub.toml`, `nodes.toml`, `layout.toml`, `agent.toml`) de forma atómica y con `fsync`. Aun así, tras copiar binarios a mano, ejecutar `sync` y **esperar unos segundos** antes de apagar el PC o desenchufar.
- Bajar el intervalo en `/etc/fstab`: cambiar `commit=600` por `commit=5` (o `commit=1`; más escrituras y más desgaste de la SD) en la línea de `/`. Aplicar con `sudo mount -o remount /`. La opción `sync` hace todo muy lento y no se recomienda.
- Lo más seguro es alimentar la placa por el conector de 40 pines (ver arriba): entonces no se apaga con el PC.
- Comprobar qué binario corre: `rawkvm-hub --version` y `rawkvm-agent-gadget --version` muestran la versión y el commit. Lo mismo aparece en la primera línea del log de cada arranque (`rawkvm-hub starting version=…`).

Cuando el PC deja de leer (apagado, suspendido o cable fuera), el agente lo registra una vez («the USB host is not reading reports»), descarta los informes sin esperar y avisa al volver.

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

## 1. Binario

Del artefacto de CI **rawkvm-agent-gadget-aarch64** (Pi Zero 2 W, Pi 3 A+, Pi 4 con SO de 64 bits, Orange Pi) o **rawkvm-agent-gadget-armv7** (Raspberry Pi OS de 32 bits en Pi 2/3/4/Zero 2 W), o compilando en el Mac:

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-agent-gadget
```

## 2. Instalar

```sh
scp target/aarch64-unknown-linux-musl/release/rawkvm-agent-gadget pi@<placa>:
scp -r deploy/gadget pi@<placa>:rawkvm-gadget
ssh pi@<placa> 'sudo sh rawkvm-gadget/install.sh ./rawkvm-agent-gadget --hub 192.168.1.28 --token <token>'
```

`install.sh` (se puede repetir):

1. En una Raspberry Pi añade `dtoverlay=dwc2,dr_mode=peripheral` a `/boot/firmware/config.txt` (o `/boot/config.txt`).
2. Carga `dwc2` (Pi) y `libcomposite` ahora y en cada arranque (`/etc/modules-load.d/rawkvm-gadget.conf`), y monta ConfigFS si hace falta.
3. Instala el binario en `/usr/local/bin`, guarda hub y token en `/etc/rawkvm/agent.toml` y activa `rawkvm-agent-gadget.service` (root).
4. Si cambió el overlay, pide **reiniciar**; el servicio arranca después.

**PC con Linux**: añadir `--no-hidpp` al `ExecStart` de `/etc/systemd/system/rawkvm-agent-gadget.service` (`sudo systemctl edit --full rawkvm-agent-gadget`). Si no, `hid-logitech-dj` del PC pondrá nuestro "receptor" en modo DJ y las interfaces de teclado y ratón quedarán mudas (ver ADR 0007). En Windows y macOS no hace falta.

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
