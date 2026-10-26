# ADR 0030 — Varios receptores, de cualquier fabricante, con exposición por receptor

- Estado: aceptada (2026-10-26). Plan: [fase6-plan.md](../fase6-plan.md); motivación: [puente-bluetooth-analisis.md](../puente-bluetooth-analisis.md), capa A.

## Contexto

Hasta la fase 6 el hub reenviaba las interfaces de **un** receptor, descrito por un bloque `[receiver]` con un solo `vid`. Eso impedía usar un ratón o un teclado de otra marca (un dongle 2,4 GHz genérico) y no dejaba entregar un receptor entero a un único equipo, que es lo que necesitará el agente puente Bluetooth de la fase 7: sus informes deben ir siempre a ese agente, con independencia de cuál sea el activo.

## Decisión

### Configuración

- `hub.toml` admite varios bloques **`[[receiver]]`**. La tabla única `[receiver]` de los hubs anteriores sigue cargando, como una lista de un elemento con los mismos valores. La configuración que el hub genera en el primer arranque y el ejemplo del `.deb` usan ya la lista. `hub.toml` no es un conffile: la actualización no lo toca.
- Cada bloque tiene `name`, que por defecto es `receiver-N`, donde N es la **posición del bloque contando desde 0**. Es el mismo número que el índice del protocolo (`HidDevice.receiver`), para que coincidan en los registros y en el widget.
- **Los valores por defecto dependen del fabricante.** Con `vid = 0x046d` (o sin `vid`), los de siempre: lista de productos Logitech, `force_normal_mode` y `require_hid_generic` activados. Con otro `vid`: cualquier producto (`pids = []`), y `force_normal_mode` y `require_hid_generic` desactivados. Sin esto, un bloque de otra marca con solo `vid` habría heredado los `pids` de Logitech y no habría casado con nada, y además habría exigido `hid-generic`. Para Linux, ese requisito solo tiene sentido frente a `hid-logitech-dj`; `hidraw` entrega informes crudos con cualquier driver.
- `force_normal_mode = true` escrito a mano en un bloque de otra marca se **avisa una vez al arrancar** y no se aplica.
- **Validación al cargar** (el hub no arranca si falla):
  - al menos un bloque y como mucho 16;
  - nombres distintos;
  - dos bloques **no pueden solaparse**: mismo `vid` y `pids` vacíos en alguno o con algún producto en común.

  Así cada interfaz pertenece a un solo receptor.

### Identificadores de dispositivo

- En modo `receiver`, el id de una interfaz es **`16 × receptor + número de interfaz USB`**. Para el primer receptor queda igual que antes (0 teclado, 1 ratón, 2 HID++). El id sigue siendo único y estable entre reinicios del hub, sea cual sea el orden en que se enchufen los receptores. El formato del datagrama no cambia: el id sigue siendo su primer byte. Si ese hueco está ocupado (modo `dj-children`, o una interfaz ≥ 16), se toma el primer id libre, como antes.

### HID++ solo Logitech, y de un único receptor

- El cambio de modo DJ (`force_normal_mode`), el hilo del receptor (dispositivos emparejados, emparejar y olvidar, batería, velocidad del puntero, Fn, perfil del teclado) y las órdenes `--hidpp-*` funcionan **solo con el primer bloque Logitech**. Si hay varios Logitech, los demás se reenvían igual, pero sin HID++. El soporte HID++ para varios receptores queda fuera de esta fase.
- `ReceiverDevices`, `Unpair`, `SetPointerSpeed`, `SetFnSwap` y el emparejamiento se refieren a ese receptor. Sin receptor Logitech, el widget recibe el mismo `PairingEvent::Failed` que cuando falta la interfaz HID++.

### Exposición por receptor

- El router guarda, por receptor, una **exposición**:
  - `Network`: lo de siempre, los informes van al agente activo;
  - `Exclusive { agent }`: siempre a ese agente.
- **Exclusivo con el agente conectado:** los informes van a él, sea cual sea el activo. Le llegan con su `swap_alt_gui` y con la copia redundante, igual que a un agente activo.
- **Exclusivo con el agente desconectado:** los informes **se descartan**; no van al activo, porque la exclusividad es la razón de ser del modo. `HubStatus.held` lista esos receptores y el widget lo muestra. El modo no cambia al desconectarse el agente: cuando vuelva, recibe la entrada otra vez.
- **El atajo no actúa** sobre los informes de un receptor exclusivo. El atajo de la red tampoco los ve: un dígito tecleado tras «modificador, modificador» en un teclado exclusivo llega a su agente como cualquier tecla.
- **Única excepción, la vía de escape:** modificador, modificador, `Esc` en un teclado exclusivo devuelve **ese receptor** a `Network` y suelta sus teclas en el agente que lo tenía. El `Esc` no se reenvía. La vía de escape:
  - funciona **aunque el atajo esté desactivado** o tenga desactivada la acción `Esc`, porque es la única salida sin el widget si ese es el único teclado;
  - usa el modificador configurado;
  - tiene su propio detector por receptor, separado del de la red.
- **Los bordes no actúan** sobre un puntero movido por un ratón exclusivo. Los bordes los detectan los helpers en cada PC, sobre la posición del puntero del sistema, y no saben qué ratón lo movió. Por eso el hub recuerda, por agente, si el último informe de ratón que le envió era de un receptor exclusivo, e ignora los `SwitchTo` de ese agente o de su helper mientras sea así. Basta un movimiento del ratón de la red para que los bordes vuelvan a funcionar. Los `SwitchTo` de un widget no se ven afectados.
- Al cambiar de agente activo:
  - los «nada pulsado» se envían solo de los dispositivos de receptores `Network`: las teclas mantenidas en un receptor exclusivo no se cortan;
  - los LEDs que se reescriben son solo los de esos receptores.
- **LEDs y perfil del teclado:** los informes de salida de un dispositivo exclusivo se escriben solo si vienen de su agente exclusivo. Al cambiar la exposición, se escribe el último estado del nuevo destino. Si el receptor HID++ pasa a exclusivo, el perfil del teclado (ADR 0014) sigue al agente exclusivo y no cambia con los cambios de activo.
- **Al cambiar la exposición**, el hub suelta las teclas de ese receptor en el destino anterior, reescribe los LEDs para el nuevo, guarda y envía `Settings` a todos los widgets y un `Status` nuevo.
- `DeviceList` sigue llegando igual a todos los nodos. `Activate` y `Deactivate` no cambian.

### Protocolo (versión 8)

- `HidDevice.receiver: u8`.
- `HubSettings.exposures: Vec<ReceiverExposure { receiver, name, mode }>`, con una entrada por bloque de la configuración. La rellena el hub; en un `Settings` de un widget se **ignora**: la exposición solo cambia con `SetExposure`. `HubSettings` deja de ser `Copy`.
- `SetExposure { receiver, mode }` (widget → hub). El hub lo **rechaza**, sin cambiar nada, si:
  - no tiene ese receptor;
  - el `agent` no es un agente conocido: conectado como `Agent`, o en `nodes.toml` si está desconectado (un helper o un widget no lo son).

  Al rechazarlo, registra un aviso y devuelve al widget que lo pidió el `Settings` sin cambios, para que su selector vuelva atrás. No hace falta un mensaje de error nuevo.
- `HubStatus.held: Vec<u8>`. El plan pedía decirlo «en `Status`», y este mensaje ya se reenvía cuando un nodo se conecta o se desconecta, que es justo cuando cambia.

### Persistencia

- `settings.toml` gana entradas `[[exposure]]` con `receiver = "<name>"` y `exclusive = "<uuid>"`. Solo se guardan las exclusivas: un receptor sin entrada es `Network`.
- Se guardan **por nombre**, no por índice, para que sobrevivan a reordenar los bloques de `hub.toml`. Una entrada con un nombre que ya no existe se avisa y se ignora. Una con un agente que no está en `nodes.toml` se carga igualmente, con un aviso: su entrada se descarta hasta que ese agente se conecte, o hasta pulsar modificador, modificador, `Esc`.

### `.hidrec`

- El formato no cambia. Al reproducir, cada dispositivo toma el receptor del primer bloque de la configuración que casa con su `vid` y su `pid`; si no casa ninguno, se avisa y se usa el receptor 0. `fixtures/two-receivers.hidrec` trae dos receptores: el Logitech C52B real y uno de otra marca, sintético, `248a:8367`.

### Widget

- En la pestaña Hub, una tabla «Receptores» con su número, su nombre, las interfaces que tiene abiertas y un selector: «Red local (todos los equipos)» o «Exclusivo → *agente*», con los agentes conectados más el exclusivo actual si está desconectado. Avisa cuando la entrada se está descartando y recuerda la vía de escape.

## Consecuencias

- Un dongle de otra marca funciona por red con solo un bloque `[[receiver]]` con su `vid`, sin HID++.
- La fase 7 (agente puente Bluetooth) solo tiene que conectarse como agente y que el widget le asigne un receptor en exclusiva.
- **`PROTO_VERSION` 8:** el hub, todos los agentes, el gadget, los helpers y el widget se actualizan a la vez.
- Riesgo: con un receptor exclusivo y su agente apagado, ese teclado no hace nada salvo la vía de escape. Es intencionado.

## Descartadas

- **Persistir la exposición por índice:** se rompe al reordenar los bloques.
- **Enviar al agente activo los informes de un receptor exclusivo cuyo agente no está:** contradice la exclusividad, y el puente Bluetooth recibiría a veces entrada y a veces no.
- **Que los helpers informen de qué ratón movió el puntero:** no pueden saberlo, porque el sistema funde todos los ratones en un puntero. La heurística del último informe de ratón es suficiente y vive solo en el hub.
- **Un mensaje de error específico para `SetExposure` rechazado:** devolver `Settings` sin cambios basta para el widget.
