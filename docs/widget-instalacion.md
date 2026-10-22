# Widget de RawKVM (Mac)

App de la barra de menús:
- muestra el hub y los equipos;
- cambia de equipo con un clic;
- empareja con el hub;
- más adelante, coloca los monitores de todos los equipos (pestaña Monitores, en desarrollo).

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

## Ficheros

- `~/Library/Application Support/rawkvm/widget.toml`: hub, token y certificado fijado. Nunca guarda el layout: lo guarda el hub.
- `~/Library/Logs/rawkvm-widget.log`: log.

## Problemas

- **«No se encontró ningún hub»**: el mDNS no llega (otra red, o el router lo filtra). Escribir la IP del hub.
- **«No se puede conectar»** con un mensaje de token o certificado: «Olvidar hub» y volver a emparejar. Si el hub se reinstaló, su certificado cambió.
- **Un hub marcado «incompatible»**: el hub tiene otra versión de protocolo. Actualizar el hub o la app.
