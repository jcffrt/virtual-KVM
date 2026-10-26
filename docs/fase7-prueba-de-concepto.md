# Fase 7, hito 1: prueba de concepto Bluetooth

Pendiente de ejecutar (2026-10-26): la RPi 3B+ prevista se averió. **No usar la Orange Pi Zero 2W**: su Bluetooth es el Unisoc UWE5622, el mismo chip del Wi‑Fi que se cuelga. Hace falta una Raspberry con Broadcom/Cypress (Zero 2 W, 3B+, 4 o 5) con Raspberry Pi OS y BlueZ 5. Plan: [fase7-plan.md](fase7-plan.md).

## Qué mide

`rawkvm-bt-poc` (crate `agent-bt`) se anuncia como «RawKVM PoC»: un teclado (report ID 1, con LED) y un ratón (report ID 2) Bluetooth clásicos, con un registro SDP propio. Escucha en los PSM 0x11 (control) y 0x13 (interrupción) y empareja como NoInputNoOutput (Just Works).

Cuando el PC conecta:

1. **Tasa.** Durante 10 s envía informes de ratón tan rápido como los acepta el enlace, en vaivén, para que el puntero acabe donde empezó. Registra `RESULT rate` con los informes por segundo y el envío más lento.
2. **Latencia.** Pulsa Bloq Mayús 10 veces y mide el tiempo hasta el informe de LED que devuelve el PC. Registra `RESULT latency` con el mínimo, la mediana y el máximo de ida y vuelta. Como las pulsaciones son pares, Bloq Mayús queda como estaba.
3. **Círculo.** Mueve el puntero en círculo 15 s, para verlo.

Después sigue conectado hasta Ctrl+C, y repite las pruebas si el PC reconecta.

## Compilar

```sh
cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-agent-bt
# target/aarch64-unknown-linux-musl/release/rawkvm-bt-poc (estático, ~3 MB)
```

## Pasos en la placa

1. Copiar el binario: `scp rawkvm-bt-poc <placa>:/tmp/` y luego `chmod +x /tmp/rawkvm-bt-poc`.
2. Comprobar el controlador: `bluetoothctl show`. Si no aparece, mirar que `/boot/firmware/config.txt` no tenga `dtoverlay=disable-bt`, y que `hciuart` y `bluetooth` estén activos.
3. Arrancar `bluetoothd` sin su plugin HID, que ocupa el PSM 0x11. Antes, comprobar la ruta con `systemctl cat bluetooth | grep ExecStart`:

   ```sh
   sudo mkdir -p /etc/systemd/system/bluetooth.service.d
   printf '[Service]\nExecStart=\nExecStart=/usr/libexec/bluetooth/bluetoothd --noplugin=input\n' \
     | sudo tee /etc/systemd/system/bluetooth.service.d/rawkvm-poc.conf
   ```

4. Poner la clase de dispositivo teclado + puntero. Sin ella el PC puede mostrar otro icono, y el binario lo avisa:

   ```sh
   sudo sed -i 's/^#\?Class = .*/Class = 0x0005C0/' /etc/bluetooth/main.conf
   ```

5. Reiniciar el servicio: `sudo systemctl daemon-reload && sudo systemctl restart bluetooth`.
6. Lanzar el binario: `sudo /tmp/rawkvm-bt-poc`, o `sudo RUST_LOG=debug /tmp/rawkvm-bt-poc` para ver el canal de control. Queda visible 180 s. Opciones: `--discoverable-secs`, `--rate-secs`, `--latency-rounds`, `--circle-secs` y `--name`.
7. Emparejar desde el PC:
   - **Windows:** Configuración > Bluetooth > Agregar dispositivo > Bluetooth.
   - **macOS:** Ajustes del Sistema > Bluetooth.

   Guardar todas las líneas `RESULT` y los `WARN`. Si el sistema no acepta el dispositivo, guardar lo que muestra el PC y el log con `RUST_LOG=debug`.
8. Para probar otro PC:
   - olvidar el dispositivo en el PC anterior;
   - en la placa, `bluetoothctl remove <dirección>`;
   - volver a lanzar el binario.
9. Deshacer al terminar:
   - `sudo rm /etc/systemd/system/bluetooth.service.d/rawkvm-poc.conf`;
   - devolver la línea de `main.conf` a `#Class = 0x000100`;
   - `sudo systemctl daemon-reload && sudo systemctl restart bluetooth`.

## Criterio

- **Seguir con el hito 2** si Windows y macOS emparejan, el teclado y el ratón funcionan y la tasa es de unos 100 informes/s o más.
- **Parar** si Windows o macOS no aceptan el dispositivo, y explicar por qué en STATUS.
