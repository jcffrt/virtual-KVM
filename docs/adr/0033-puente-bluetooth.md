# ADR 0033 — Puente Bluetooth: un receptor del hub expuesto a un PC por Bluetooth

- Estado: aceptada en parte (2026-10-26).
  - **Hecho, con tests y sin hardware:**
    - fusión de descriptores;
    - estado del PC vinculado;
    - protocolo 11, en la rama `fase7`;
    - el hub;
    - el `HidSink` del puente;
    - la persistencia;
    - el widget.
  - **Pendiente de la prueba de concepto** ([fase7-prueba-de-concepto.md](../fase7-prueba-de-concepto.md)):
    - la implementación con BlueZ;
    - el *override* de `bluetoothd`;
    - el `.deb`;
    - la guía de instalación.
- Plan: [fase7-plan.md](../fase7-plan.md). Motivación: [puente-bluetooth-analisis.md](../puente-bluetooth-analisis.md), capas B y C. La exposición por receptor viene del [ADR 0030](0030-varios-receptores-y-exposicion.md).

## Contexto

La fase 6 deja exponer un receptor en exclusiva a un agente. La fase 7 añade un agente que no reproduce los informes en un dispositivo virtual de su propio PC, sino que se presenta a **otro** PC como un teclado y un ratón Bluetooth clásicos (HID BR/EDR). Ese PC no necesita agente, driver ni cable.

El primer puente es Linux con BlueZ (`rawkvm-agent-bt`). Es probable un segundo, una app Android sobre `BluetoothHidDevice` (Android 9+), que hable el mismo protocolo. Por eso todo lo que no depende de la pila Bluetooth está separado de ella:

- El **protocolo** no supone BlueZ.
- La **lógica** común vive en el crate `bridge-core`, sin E/S ni tokio.
- Las **reglas de fusión** quedan escritas aquí con detalle suficiente para reimplementarlas.

## Decisión

### El puente en el hub (protocolo 11)

- El puente es un nodo `Agent` con **`Os::Bridge`**.
  - **Sin índice de atajo:** `assigned_index = 0`. Queda en `nodes.toml` con el índice 0, que admite varios puentes, para que siga siendo un agente conocido (destino válido de una exposición) mientras está desconectado.
  - **Nunca es el nodo activo:**
    - no se activa al conectar;
    - no es candidato a reserva cuando cae el activo;
    - `switch_locked` rechaza un `SwitchTo` hacia él, venga de donde venga, también de un widget;
    - no recibe `Activate` ni `Deactivate`.
  - **Recibe entrada solo** como destino exclusivo de uno o más receptores. Sirve de destino en `SetExposure`, porque es un agente conocido.
  - Su **`DeviceList` lleva solo los dispositivos de sus receptores**. El hub se lo envía al conectar, cuando cambian los dispositivos y cuando una exposición cambia de destino. Así el descriptor fusionado sigue exactamente lo que presenta, sin un mensaje nuevo.
- **Perfil del teclado:** el hub no conoce el sistema del PC del puente, así que no aplica perfil HID++ (ADR 0014). El teclado conserva el que tenga.
- **Mensajes nuevos:**
  - `BtStatus { status: BridgeStatus }`, del puente al hub. `BridgeStatus { bonded: Option<String>, connected, discoverable_secs: Option<u32>, repair_needed }`.
    - El puente lo envía al empezar cada sesión con el hub y en cada cambio. La cuenta atrás de `discoverable_secs` no cuenta como cambio: el widget descuenta solo.
    - El hub guarda el último y lo muestra en **`NodeInfo.bridge`**. No lo reenvía como mensaje: el `Status` ya llega a los widgets al conectar y en cada cambio.
    - De un nodo que no es puente se ignora.
  - `BtDiscoverable { node, secs }` y `BtForget { node }`, del widget al hub y del hub al puente `node`.
    - `secs = 0` cierra el emparejamiento.
    - El hub solo los atiende de widgets, y solo si `node` es un puente conectado.
- **Neutral respecto a la pila:** ningún campo supone BlueZ. `bonded` es el nombre del PC o, si no lo hay, su dirección. La identidad del descriptor es un hash definido abajo.
- **Compatibilidad:** `Os::Bridge` es una variante nueva de un tipo que va en `Hello`. Un hub anterior no puede decodificar ese `Hello` y cierra la conexión sin enviar `VersionMismatch`. Es aceptable: no existen puentes anteriores al protocolo 11.

### Fusión de descriptores (`bridge_core::merge`)

Bluetooth admite **un descriptor por dispositivo**, y el receptor tiene uno por interfaz. Las reglas, para quien las reimplemente:

1. **Entrada:** los `HidDevice` que el hub envía al puente, ordenados por `id` ascendente.
2. **Lectura de items** (HID 1.11, §6.2.2):
   - **Item corto:** el prefijo da el tamaño en los bits 0–1 (0, 1, 2 o 4 bytes de datos), el tipo en los bits 2–3 (0 main, 1 global, 2 local) y la etiqueta en los bits 4–7.
   - **Item largo:** prefijo `0xFE`, un byte de tamaño, uno de etiqueta y los datos.
   - **Descriptor truncado:** es un error.
3. **Interfaz con IDs propios:** la que tiene algún item global `Report ID` (prefijo `0x85`).
4. **Segmentos de nivel superior:** cada uno son los items desde el final del segmento anterior hasta el `Collection` de profundidad 0 inclusive, más su `End Collection`. Los items que quedan tras el último segmento son la **cola**.
5. **Página de la colección:**
   - Por defecto, la `Usage Page` global vigente en el `Collection`, siguiendo `Push`/`Pop`.
   - Si el último `Usage` local antes del `Collection` tiene 4 bytes, se toman sus 16 bits altos.
6. **Qué colecciones se conservan:** las de página **< `0xFF00`**. Las de fabricante (HID++) no se exponen.
7. **Segmento conservado:** se copian sus items tal cual, con dos cambios:
   - **Interfaz con IDs:** el valor `v` de cada `Report ID` se cambia por el ID fusionado de `(interfaz, v)`.
   - **Interfaz sin IDs:** justo detrás del item `Collection` de profundidad 0 se inserta `85 id`, con el ID fusionado de `(interfaz, ninguno)`. Es el mismo ID para todas sus colecciones conservadas.
8. **Segmento descartado y cola:** se emiten solo sus items **globales**, salvo `Report ID`. Así los segmentos siguientes ven el estado global que esperan.
9. **Una interfaz sin ningún segmento conservado** (la HID++) **no deja ni un byte** ni ningún ID. Añadirla o quitarla no cambia el descriptor.
10. **Entre dos interfaces que dejan bytes** se ponen a cero las globales: `15 00 25 00 35 00 45 00 55 00 65 00 75 00 95 00` (mínimo y máximo lógico y físico, exponente, unidad, tamaño y número de informes). Así ninguna hereda los valores de la anterior. `Usage Page` no se toca: toda colección la fija antes de usarla.
11. **IDs fusionados:** se asignan 1, 2, 3… en el orden en que aparecen las parejas `(interfaz, ID original o ninguno)`. Más de 255 informes es un error. Ninguna colección conservada es un error, y el puente no presenta nada.
12. **Informe de entrada** de la interfaz `d`, tal como llega del hub (con su byte de ID si la interfaz usa IDs): se envía `[ID fusionado] + datos sin el ID original`. Si su ID no está en la tabla (HID++), se descarta.
13. **Informe de salida** del PC (los LEDs), `[ID fusionado] + datos`: va a la interfaz `d` como `[ID original] + datos`, o solo `datos` si `d` no usa IDs. Sigue el camino de salida de siempre hacia el hub (ADR 0012).
14. **Identidad del descriptor:** el hash **FNV-1a de 64 bits** de los bytes fusionados (base `0xcbf29ce484222325`, primo `0x100000001b3`).

Tests: los descriptores reales del C52B y `fixtures/two-receivers.hidrec`. Comprueban:

- IDs únicos y con la longitud original;
- el LED del teclado bajo su ID;
- la ida y vuelta de los informes de teclado y de ratón;
- que el HID++ queda fuera sin huella.

### El PC vinculado (`bridge_core::host`)

- **Un PC a la vez.**
  - Emparejar otro olvida el anterior (`remove` en la pila).
  - Una conexión de un PC que no es el vinculado se cierra.
- **Olvidar** desde el widget (`BtForget`), o cuando el PC quita el dispositivo (*virtual cable unplug*): se borra el vínculo en la pila y en disco.
- **Reconexión iniciada por el dispositivo:** con un PC vinculado y sin conexión, el puente intenta conectar a 1, 2, 5, 10, 30 y 60 s, y después cada 60 s.
  - No lo intenta mientras está visible para emparejar.
  - Al conectar se reinicia la espera.
  - Tras perder la conexión, el primer intento es a 1 s, para la vuelta de suspensión.
- **Volver a emparejar (`repair_needed`):** Windows y macOS guardan el descriptor de cada dispositivo emparejado. El puente guarda el hash del descriptor con el que se emparejó el PC. Si el descriptor vigente es distinto (otro receptor expuesto al puente, otra interfaz), `repair_needed` se activa y **no se envía entrada**: el PC la interpretaría con el descriptor viejo. El widget pide quitar el dispositivo en el PC y volver a emparejarlo. Si el descriptor vuelve a ser el de antes, se sigue sin más.
- **Sin receptores expuestos** (`DeviceList` vacío): se mantiene el descriptor publicado para que el PC siga vinculado. Simplemente no llega nada que enviar.
- **Persistencia:** `/var/lib/rawkvm/agent-bt.toml` guarda `bonded` (dirección), `name` y `descriptor` (el hash en hexadecimal).
  - Se escribe de forma atómica.
  - Un fichero ilegible es un error al arrancar: es mejor eso que olvidar el PC sin avisar.
  - Las claves Bluetooth son cosa de la pila (BlueZ: `/var/lib/bluetooth`).
- **La pila, tras traits:** `bridge_core::host::Radio` (visible, olvidar, conectar) y `rawkvm_agent_bt::stack::HidStack` (publicar descriptor, enviar informe, cerrar). Los eventos van por un canal: emparejado, conectado, desconectado, salida y desenchufado. Los tests usan una pila simulada; la de BlueZ llega tras la prueba de concepto.

### Agente (`rawkvm-agent-bt`)

- **`BridgeSink`:** es el `HidSink` sobre agent-core y convierte lo que recibe en órdenes para una tarea propia que posee la pila. Espera 500 ms a que la lista se asiente, como el gadget, porque cambiar el descriptor cuesta un reemparejado.
- **agent-core** gana tres ganchos genéricos de `HidSink`:
  - `take_control`: un canal propio hacia el hub, que se conserva entre sesiones;
  - `connected`: avisa de cada sesión nueva;
  - `on_control`: entrega los mensajes que el núcleo no atiende.
- **HID clásico con BlueZ** (prueba de concepto):
  - registro SDP propio con `ProfileManager1.RegisterProfile`: subclase `0xC0`, `HIDVirtualCable`, `HIDReconnectInitiate`, `HIDNormallyConnectable`, `HIDBootDevice = false`;
  - sockets L2CAP en los PSM `0x11` y `0x13`, con cifrado;
  - agente de emparejado NoInputNoOutput (Just Works);
  - `bluetoothd --noplugin=input`;
  - clase de dispositivo `0x0005C0`.
- **Canal de control:**

  | Petición del PC | Respuesta |
  |---|---|
  | `SET_REPORT` | éxito; si es de salida, el informe va al hub |
  | `GET_REPORT` de entrada | último informe de ese ID, o «ID no válido» |
  | `GET_PROTOCOL` | protocolo de informes |
  | `SET_PROTOCOL` | éxito |
  | Lo demás | «no soportado» |

### Widget

- En «Equipos», bajo un puente:
  - el PC vinculado y si está conectado;
  - «Hacer visible 60 s» con cuenta atrás, o «Parar» mientras está visible;
  - «Olvidar PC», con confirmación;
  - el aviso de volver a emparejar.
- Un puente no muestra número, pantallas ni «Usar».
- En el selector de exposición aparece como «*nombre* (Bluetooth)».

## Consecuencias

- Un PC sin agente puede usar un receptor del hub por Bluetooth. A cambio:
  - la tasa de informes es de unos 100–130/s, frente a 1000 por USB;
  - hay más latencia;
  - es un solo PC, y cambiar de PC es olvidar y volver a emparejar.
- Cambiar qué receptores van a un puente cambia su descriptor y obliga a volver a emparejarlo en el PC. El widget lo avisa.
- El protocolo 11 sube la versión de todos los nodos a la vez, como siempre.
- `bridge-core` sirve para un puente Android, ya sea por FFI o reimplementando las reglas de arriba.

## Descartadas

- **BLE / HOGP:** obliga a trocear el descriptor en características GATT y no aporta nada aquí.
- **Reenviar `BtStatus` como mensaje a los widgets:** `NodeInfo.bridge` llega en el `Status` que ya se envía al conectar y en cada cambio.
- **Un mensaje nuevo para decir al puente cuáles son sus receptores:** basta con filtrar su `DeviceList`.
- **Enviar entrada con `repair_needed`:** el PC la interpretaría con el descriptor viejo.
- **Despublicar el dispositivo cuando no le queda receptor:** el PC perdería el vínculo y habría que volver a emparejar al recuperarlo.
