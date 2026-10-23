# ADR 0020 — El widget también en Windows

- Estado: aceptada (2026-10-23), Fase 4.3.

## Contexto

El widget (Tauri 2 + Svelte, ADR 0010) solo se compilaba para macOS. En el PC GAMER (Windows 11) no había forma de cambiar de equipo con un clic, ver los dispositivos ni colocar los monitores. El helper de borde de Windows es un proceso aparte, sin UI, y así se queda.

## Decisión

- **Mismo código.** Lo específico de cada sistema está en `lib.rs` y `tray_icon.rs`, con `cfg!(windows)`:
  - **Bandeja**: el área de notificación de Windows no muestra texto junto al icono (`set_title` es solo de macOS) ni iconos plantilla. `tray_icon::render` dibuja en tiempo de ejecución un cuadro azul de 32×32 con el número del equipo activo (fuente de 5×7 escalada ×3) o, sin equipo activo, el glifo de `tray.png` en blanco. El azul se lee en barras claras y oscuras.
  - **Clics**: el izquierdo abre la ventana y el derecho el menú, como es costumbre en Windows (`show_menu_on_left_click(false)`). En macOS el clic abre el menú, como antes.
  - **Ficheros**: `widget.toml` y el log van en `%LOCALAPPDATA%\rawkvm\`, no en `%APPDATA%` (perfil móvil), porque el certificado fijado es de este PC. En macOS no cambian.
  - **Inicio automático**: el mismo plugin, que en Windows usa la clave `Run` de HKCU.
  - **Una sola instancia** (`tauri-plugin-single-instance`, en los dos sistemas): abrirlo otra vez solo muestra la ventana. Sin ello, el inicio automático más un arranque a mano dejaban dos iconos.
  - **Front end**: `View.platform` para los textos («Este PC» o «Este Mac»).
- **CI**: job `windows` en `widget.yml` (windows-latest): clippy, tests, `tauri build --no-bundle`, y el artefacto `RawKVM-windows` con `RawKVM.exe`, `LEEME.md` y `COMMIT.txt`. El exe va sin firmar: SmartScreen avisa una vez, y el usuario lo acepta. Sin instalador MSI ni NSIS: un exe portable basta, y WebView2 ya viene con Windows 10 y 11.

## Consecuencias

- El GAMER tiene bandeja con el número activo, menú de cambio rápido y las pestañas Hub, Dispositivos, Monitores y Opciones.
- Si se mueve el exe hay que volver a activar el inicio automático, porque la clave guarda la ruta.
- La firma estable del Mac (ADR 0016) no tiene equivalente en Windows. Windows no liga permisos a la firma para esto: el cortafuegos recuerda la regla por ruta.
