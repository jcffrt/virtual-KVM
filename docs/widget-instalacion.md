# Widget de RawKVM (Mac)

App de la barra de menús:
- muestra el hub y los equipos;
- cambia de equipo con un clic;
- empareja con el hub;
- coloca los monitores de todos los equipos (pestaña Monitores).

No mueve el teclado ni el ratón: eso lo hacen el agente y el helper de borde. Diseño: [ADR 0010](adr/0010-widget-y-protocolo-v3.md).

## Instalar

Del artefacto de CI **rawkvm-widget-macos**:

1. Descomprimir `RawKVM.app.zip` y mover `RawKVM.app` a `/Applications`.
2. La app no está firmada con Developer ID. La primera vez, abrirla con clic derecho → **Abrir** → **Abrir**, o quitar la cuarentena desde el Terminal:
   ```sh
   xattr -dr com.apple.quarantine /Applications/RawKVM.app
   ```
3. No pide permisos de macOS.

Compilar en el Mac (necesita Node 22 y Rust):

```sh
cd widget
npm ci
npx tauri build --bundles app      # src-tauri/target/release/bundle/macos/RawKVM.app
```

## Usar

- **Barra de menús**:
  - el icono de dos monitores lleva al lado el número del equipo activo;
  - el menú lista los equipos (1 Windows, 2 Mac…) con el activo marcado: elegir otro cambia la entrada al momento, sin pasar por el borde, y el puntero de ese equipo se queda donde estaba;
  - «Ajustes…» abre la ventana;
  - «Salir de RawKVM» cierra la app. Cerrar la ventana solo la oculta.
- **Pestaña Hub**:
  - **Sin emparejar**: «Buscar hubs en la red» (mDNS), elegir el hub o escribir su dirección, pegar el token (`sudo rawkvm-hub --show-token` en la Pi) y pulsar **Emparejar**. El certificado del hub se fija en la primera conexión, como en los agentes.
  - **Emparejado**: estado, latencia, versión del hub y lista de equipos: número, sistema, tipo y pantallas conocidas. Un agente sin helper de borde sale **en gris** («sin helper»): se puede cambiar a él por el menú o con el atajo, pero no por el borde. «Usar» cambia a ese equipo. «Olvidar hub» borra el emparejamiento.

- **Pestaña Monitores**:
  - un bloque por equipo con sus pantallas reales (Mac: integrada y externa; Windows: sus dos monitores), en unidades lógicas;
  - un equipo sin helper de borde sale como bloque gris discontinuo de 1920×1080;
  - **arrastra** cada bloque: al soltarlo cerca de otro, se pega a su lado y alinea los bordes superiores;
  - las **líneas azules** marcan dónde el puntero pasará de un equipo a otro (solo en el tramo en que los lados se tocan);
  - **Aplicar** envía la disposición al hub, que la guarda (`/var/lib/rawkvm/layout.toml`) y la aplica al instante en los helpers, sin reiniciar nada;
  - **Descartar cambios** vuelve a lo que tiene el hub;
  - **Volver a la semilla del hub** borra la disposición guardada y el hub vuelve al `[[layout]]` de `hub.toml`;
  - **Escala**: cada equipo se dibuja dividiendo sus medidas por la escala que informa su helper (macOS ya informa en puntos, 1; Windows, su escalado de pantalla). Si un bloque sale de tamaño equivocado, se corrige a mano; vacío = la informada. La corrección se guarda en el hub con la disposición.
  - Mientras el hub use la semilla, la pestaña muestra una propuesta hecha a partir de ella. Hasta pulsar Aplicar no cambia nada.

## Ficheros

- `~/Library/Application Support/rawkvm/widget.toml`: hub, token y certificado fijado. Nunca guarda el layout: lo guarda el hub.
- `~/Library/Logs/rawkvm-widget.log`: log.

## Problemas

- **«No se encontró ningún hub»**: el mDNS no llega (otra red, o el router lo filtra). Escribir la IP del hub.
- **«No se puede conectar»** con un mensaje de token o certificado: «Olvidar hub» y volver a emparejar. Si el hub se reinstaló, su certificado cambió.
- **Un hub marcado «incompatible»**: el hub tiene otra versión de protocolo. Actualizar el hub o la app.
