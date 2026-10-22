# Descriptores reales del receptor Unifying del hub

Capturados el 2026-10-22 en la Orange Pi Zero 2W (Armbian, kernel 6.1.31) desde
`/sys/bus/hid/devices/<id>/report_descriptor`, con `hid-logitech-dj` cargado
(modo DJ). Receptor `046D:C52B` en el bus USB 4, puerto 1.

| Fichero | Dispositivo | Driver en el Pi | Bytes | Contenido |
|---|---|---|---|---|
| `0003-046D-C52B.0003.bin` | Receptor, interfaz 2 (HID++/DJ) | `logitech-djreceiver` | 93 | Vendor 0xFF00: HID++ corto 0x10 (6 B), largo 0x11 (19 B); DJ corto 0x20 (14 B), largo 0x21 (31 B) |
| `0003-046D-406F.0004.bin` | Logitech MX Ergo (trackball), sintetizado por el kernel | `logitech-hidpp-device` | 288 | Teclado ID 1 (7 B: mods + 6 teclas), ratón ID 2 (7 B), consumer ID 3 (4 B), system ID 4 (1 B), HID++ 0x10/0x11, DJ 0x20/0x21 |
| `0003-046D-400D.0005.bin` | Logitech K230 (teclado), sintetizado | `logitech-hidpp-device` | 190 | Teclado ID 1, consumer ID 3, HID++, DJ (sin ratón ni system) |
| `0003-046D-408A.0006.bin` | Logitech MX Keys (teclado), sintetizado | `logitech-hidpp-device` | 288 | Igual que el MX Ergo (plantilla genérica del driver) |

## Modo normal (sin `hid_logitech_dj`, ficheros `*.nodj.bin`)

Capturados tras `blacklist hid_logitech_dj hid_logitech_hidpp` y reset USB
(`/sys/bus/usb/devices/4-1/authorized`). `hid-generic` toma las tres
interfaces, cada una con hidraw y evdev propios.

| Fichero | Interfaz | hidraw / evdev | Bytes | Contenido |
|---|---|---|---|---|
| `0003-046D-C52B.0007.nodj.bin` | 0, teclado boot | hidraw0, event3 | 59 | Teclado estándar sin report ID: 8 B (mods, reservado, 6 teclas). Multiplexa K230 y MX Keys |
| `0003-046D-C52B.0008.nodj.bin` | 1, ratón | hidraw1, event4..6 | 148 | Ratón ID 2 (7 B: 16 botones, X/Y 12 bit, rueda, pan), consumer ID 3 (4 B), system ID 4 (1 B), vendor 0xFFBC ID 8 (1 B) |
| `0003-046D-C52B.0009.nodj.bin` | 2, HID++ | hidraw2 | 93 | Igual que en modo DJ |

En modo DJ las interfaces 0 y 1 quedan sin driver y sin hidraw: el receptor
deja de emitir por ellas y multiplexa todo por la interfaz 2.

Los parsea `crates/proto/tests/fixtures.rs` (`cargo test -p rawkvm-proto --test fixtures -- --nocapture`).

Los ficheros se llaman como el dispositivo HID en sysfs (`0003:046D:C52B.0007`) con `:` cambiado por `-`, para que el repositorio se pueda clonar en Windows.
