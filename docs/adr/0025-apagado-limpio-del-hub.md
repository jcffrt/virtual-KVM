# ADR 0025 — Apagado limpio del hub desde el widget y al apagar el PC que lo alimenta (protocolo v7)

- Estado: aceptada (2026-10-25), Fase 5.3b.

## Contexto

La Orange Pi del hub se alimenta del USB del GAMER (el puerto OTG es a la vez datos del gadget y alimentación). Cada apagado del PC es un corte brusco para la placa. La tarjeta SD ya sufrió uno ([gadget-instalacion.md](../gadget-instalacion.md), «Cortes de alimentación»). El 2026-10-24 la placa además murió tres veces seguidas a los 15-40 s de arrancar, por falta de corriente en un puerto (STATUS, «La Pi se apaga a los pocos segundos de arrancar»).

El usuario pide dos cosas:

1. un botón para apagar el hub desde el widget;
2. que el hub se apague solo cuando se apaga el PC que lo alimenta.

Hasta ahora la única vía era `sudo systemctl poweroff` por SSH.

## Decisión

- **Protocolo v7.** Dos mensajes nuevos, al final de `Control` para no mover los índices de postcard:
  - `Shutdown` (widget → hub): apagar la placa.
  - `ShuttingDown { reason }` (hub → todos los nodos): el hub se apaga. Para el widget que lo pidió es también la confirmación.

  `PROTO_VERSION` pasa a 7. Hub, gadget, agentes, helpers y widgets se despliegan a la vez; un nodo v6 recibe `Reject(VersionMismatch)`, como en cada subida.
- **Hub.**
  - **Quién puede pedirlo:** acepta `Shutdown` solo de nodos `Widget`, que ya presentaron el token. De un agente o un helper lo ignora, como el resto de órdenes de widget.
  - **Al recibirlo:**
    - registra un `warn` con el nodo que lo pidió;
    - libera todas las teclas en todos los agentes (`RELEASE_ALL`);
    - envía `ShuttingDown` a todos;
    - pasa la petición a `poweroff::serve`.
  - **Lo que hace `poweroff::serve`:**
    1. espera 0,5 s para que los mensajes salgan;
    2. para el gadget (`systemctl stop rawkvm-agent-gadget.service`, como mucho 3 s; si la placa no lo tiene, nada), y el PC del cable ve desaparecer el receptor limpiamente;
    3. `sync`;
    4. `systemctl poweroff`. systemd para entonces el servicio del hub, que suelta el receptor.
  - **Permisos:** el hub ya corre como root (por hidraw y EVIOCGRAB): no hace falta sudo ni polkit. Hablar con systemd por su socket funciona con `ProtectSystem=strict` y `NoNewPrivileges=yes`.
  - **Si `systemctl` falla:** lo registra (`error`) y el hub sigue en marcha. Una petición posterior lo intenta otra vez. Mientras el apagado está en curso, otro `Shutdown` solo recibe `ShuttingDown` como respuesta.
- **Agentes, helpers y widgets.**
  - Al recibir `ShuttingDown`, terminan la sesión con `End::HubDown`. El agente suelta antes sus teclas y dispositivos.
  - En vez del backoff normal (0,5 → 10 s), reintentan cada 30 s (`HUB_DOWN_RETRY`) hasta que el hub vuelve a aceptar una conexión. Los fallos de conexión de ese tiempo se registran en `debug`, no en `warn`.
  - En el log queda `the hub is powering off (requested from GAMER)` y después `hub off; reconnecting in 30.0 s`. `--check` muestra esta línea como `warn hub`.
  - Para no repetir el bucle tres veces, el ritmo vive en `agent_core::Reconnect`, que comparten agente, helper y widget.
- **Widget.**
  - Botón **Apagar hub** en la pestaña Hub, con un segundo clic de confirmación («Todos los equipos se quedan sin teclado ni ratón…»).
  - Después, el estado pasa a **Hub apagado**, con el aviso de esperar a que el LED verde deje de parpadear antes de desenchufar (en la Orange Pi Zero 2W el rojo es la alimentación y no se apaga).
  - La bandeja dice «hub apagado» en vez de «sin conexión».
- **Windows: al apagar el PC que alimenta la placa.**
  - **La opción.** En la pestaña Opciones, apartado «Este PC», **Apagar el hub cuando este PC se apague**, desactivada por defecto. Se guarda en `widget.toml` (`power_off_hub_with_pc`) porque es de este PC, no del hub.
  - **Cómo se entera de que se apaga.** Una ventana de nivel superior oculta, en su propio hilo, recibe `WM_QUERYENDSESSION` y `WM_ENDSESSION`. Una ventana solo de mensajes no los recibe, y el bucle de Tauri no debe bloquearse.
  - **Cerrar sesión no cuenta.** Con `ENDSESSION_LOGOFF` el PC sigue encendido y alimentando la placa, así que no hace nada.
  - **Al apagar o reiniciar, con la opción activada:**
    1. registra un motivo de bloqueo («Apagando el hub de RawKVM»);
    2. envía `Shutdown`;
    3. espera la confirmación (`ShuttingDown`) o 3 s, lo que llegue antes, y deja seguir a Windows.

    Nunca bloquea el apagado: Windows muestra «estas aplicaciones impiden apagar» hacia los 5 s.
  - **Log.** Registra cada mensaje con su hora y su `lParam`, también con la opción desactivada, para medir el margen.
  - **Suspender queda fuera.** No se hace nada con `PBT_APMSUSPEND`: apagar el hub al suspender dejaría sin teclado al Mac y al Ubuntu. Si se añade algún día, será una segunda opción aparte, también desactivada por defecto.
- **Sin widget**, como hasta ahora: `sudo systemctl poweroff` en la placa por SSH.

## Consecuencias

- La SD deja de recibir cortes bruscos, siempre que el PC mantenga los 5 V el tiempo que Armbian tarda en apagarse (unos 6 s). Sin la confirmación, la petición sale como mucho 3 s antes de que Windows siga. El `sync` del hub llega antes que `poweroff`, así que en el peor caso los datos ya están en la SD.
- Cuánto tarda el GAMER en cortar los 5 V desde el aviso a las aplicaciones está por medir. Si no basta, se documenta y la opción se queda, porque al menos hace `sync`. Esto no sustituye a la alimentación propia por el conector de 40 pines, lo único que evita que el hub desaparezca mientras el PC está apagado.
- Tras un apagado, el hub vuelve solo al darle corriente. Los nodos tardan hasta 30 s en verlo.
- Con Inicio rápido de Windows, «Apagar» también avisa a las aplicaciones. Qué `lParam` lleva en el GAMER se verá en el log.
- Probado en el CI:
  - en el router, que solo un widget lo pide, que se liberan las teclas, que avisa a todos y que no se repite;
  - de punta a punta contra el hub real en localhost: el widget pide, el hub entrega la petición de apagado, el agente suelta sus dispositivos, el widget muestra «hub apagado» y nadie reconecta al ritmo normal;
  - `ending()` distingue el cierre de sesión.
- Sin probar en el CI: `systemctl poweroff` en la placa y los mensajes de Windows en un apagado real.
