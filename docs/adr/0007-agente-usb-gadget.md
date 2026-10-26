# ADR 0007 — Agente USB gadget: el receptor reproducido por USB

- Estado: aceptada (2026-10-22). Compilado y probado sobre un ConfigFS simulado; pendiente de validar en una Raspberry Pi.

## Contexto

Los agentes de Windows y macOS necesitan un driver (test-signing) o Karabiner. Una placa Linux con controlador USB en modo dispositivo puede, con el gadget HID de ConfigFS (`libcomposite` + `usb_f_hid`), presentarse a un PC como un dispositivo HID cualquiera. Si presenta exactamente el receptor (VID/PID, interfaces, descriptores), el PC no necesita nada y recibe los mismos bytes que con el receptor enchufado: el reenvío más puro del proyecto.

Datos del kernel (`drivers/usb/gadget/function/f_hid.c`, v6.12): atributos `subclass`, `protocol`, `report_length`, `report_desc`, `no_out_endpoint`, `dev`; `write()` trunca a `report_length` y con `O_NONBLOCK` devuelve `EAGAIN` mientras el IN anterior no se ha enviado; `bInterval` 4 (1 ms en alta velocidad); los OUT del host se encolan (4 peticiones) hasta que alguien lee.

## Decisión

1. **Agente aparte** (`crates/agent-gadget`), un `HidSink` más sobre `agent-core` (config, mDNS, TOFU, reconexión, release-all, SIGTERM). Un nuevo hook `HidSink::devices_ready()` (vacío por defecto) le entrega la `DeviceList` completa, porque el gadget se construye de una vez.
2. **Árbol ConfigFS** `/sys/kernel/config/usb_gadget/rawkvm`: VID/PID y `product` del primer `HidDevice`, `manufacturer` "Logitech" (para 046D), `bcdUSB` 2.00, `bcdDevice` 1.00 (el hub no transmite el real), `MaxPower` 98 mA y `bmAttributes` 0xA0 como el receptor; una función `hid.usb<id>` por dispositivo, enlazadas en orden de id para que los números de interfaz coincidan (0 teclado, 1 ratón, 2 HID++). Subclase/protocolo boot según el descriptor (teclado 1/1, ratón 1/2, resto 0/0). `report_length` = mayor input report + byte de ID. Se construye como lista de operaciones (funciones puras, testeadas sobre un directorio temporal) y se desmonta en el orden que exige ConfigFS (UDC, enlaces, configs, funciones, strings).
3. **Informes**: `write()` del informe tal cual (con su byte de ID si el descriptor usa IDs, igual que hidraw) en `/dev/hidgN`, localizado por el atributo `dev` y `/sys/dev/char`. `EAGAIN`: reintento cada 1 ms hasta 8 ms (ratón) o 50 ms (resto) y después se descarta, para no bloquear si el PC duerme. Un hilo por interfaz drena y descarta los OUT del host (LEDs, HID++).
4. **Reconexiones**: si la `DeviceList` nueva es igual a la actual se conserva el gadget (el PC no ve desconexión USB cuando se reconecta el hub); `destroy_all` solo envía "release all". Al salir: release-all y desmontaje; al arrancar se desmonta un gadget que haya dejado un proceso muerto.
5. **HID++ (interfaz 2)**: expuesta por defecto, `--no-hidpp` la oculta. Un host **Linux** carga `hid-logitech-dj` para 046D:C52B por USB, pone el receptor en modo DJ por HID++ (nuestro gadget lo reenvía como output report y se descarta) y espera los informes DJ multiplexados: las interfaces 0/1 se ignoran — la misma trampa que en el hub (ADR 0003). Por eso en hosts Linux se recomienda `--no-hidpp`. En Windows sin Logitech Options+ y en macOS no hay driver que lo haga.

## Revisión (estabilización de la lista)

El hub envía una `DeviceList` nueva cada vez que abre una interfaz (~60 ms entre ellas). En la Orange Pi el gadget se reconstruyó tres veces en 80 ms (1, 2 y 3 interfaces), con tres re-enumeraciones en el PC. `HidSink::settle_time()` (por defecto cero) permite a un sink esperar a que la lista lleve ese tiempo sin cambios antes de `devices_ready()`; el gadget usa 500 ms, también al arrancar. Una lista idéntica a la anterior no reinicia la espera.

## Por qué no en el hub

El hub de la Orange Pi tiene un segundo USB-C con OTG en modo periférico y podría ser a la vez gadget para un PC. Se deja para más adelante: el hub es el componente que no debe fallar (routing, atajo de emergencia) y el gadget añade estado USB y dependencias del device tree. El módulo (`configfs`, `hidg`, `sink`) es una librería y el hub podría reutilizarlo con una opción `gadget = true`, enviando al gadget local cuando ese PC sea el destino activo, sin Wi‑Fi de por medio.

## Límites

- **Un gadget = un PC.** Cada PC por cable necesita su placa.
- **Cuatro interfaces HID como mucho** (`HIDG_MINORS` de `usb_f_hid`, contando todos los gadgets de la placa). Si hay más, se eligen por prioridad: ver [ADR 0031](0031-gadget-con-mas-de-cuatro-interfaces.md).
- **Latencia**: Wi‑Fi hub→placa (RTT 4–10 ms medido) + 1 ms de sondeo USB. Sin la ventaja del cable entre el receptor y el PC.
- El PC ve un receptor sin dispositivos emparejados: el software Logitech no podrá gestionarlo.
- Las salidas del host (LED de Bloq Mayús) no llegan al teclado real.
- No probado en hardware.
