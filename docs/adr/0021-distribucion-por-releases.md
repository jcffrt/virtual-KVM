# ADR 0021 — Distribución por GitHub Releases

- Estado: aceptada (2026-10-24), Fase 5.

## Contexto

Hasta la Fase 4 los binarios salían de los artefactos de CI, que caducan a los 90 días, exigen sesión en GitHub y no tienen nombre de versión. Los instaladores de la Fase 5 necesitan un origen estable y comprobable. Además, el repositorio es **privado**: la API de GitHub y los assets de una release piden autenticación, y un `curl` anónimo devuelve 404.

## Decisión

- **Releases por etiqueta.** `release.yml` se dispara con una etiqueta `v*`. Construye los paquetes, comprueba que la etiqueta coincide con la versión de `Cargo.toml`, calcula `SHA256SUMS` y lo publica todo como release de GitHub (`gh release create --verify-tag`). Lanzado a mano (`workflow_dispatch`), solo construye y prueba, sin publicar. La etiqueta la pone el orquestador cuando todo está validado en hardware.
- **Contenido.** En la 0.5.0: `rawkvm-hub_<ver>_{arm64,armhf}.deb`, `rawkvm-agent-gadget_<ver>_{arm64,armhf}.deb` y `SHA256SUMS`. El `.deb` del agente de Linux, el tar de macOS y el zip de Windows se añaden en sus bloques (Fase 5.2 a 5.4).
- **Versión.** Una sola, la de `[workspace.package]` en `Cargo.toml`, igual en `widget/src-tauri/Cargo.toml`, `tauri.conf.json` y `package.json`. Pasa de 0.1.0 a **0.5.0** (Fase 5). `cargo-deb` y los nombres de los ficheros la toman de ahí. El protocolo tiene su propia versión (`PROTO_VERSION`): una release nueva no implica cambiarlo.
- **Descarga en los instaladores**, en este orden:
  1. `--from CARPETA`: ficheros ya descargados o compilados (desarrollo y despliegues del orquestador).
  2. `gh release download` si `gh` está instalado y autenticado.
  3. `curl` a la API (`releases/latest`, o la etiqueta de `--version vX.Y.Z`), con `GH_TOKEN` si existe o anónimo (esto último solo si el repositorio llega a ser público).

  Siempre se comprueba el SHA-256 contra `SHA256SUMS` antes de instalar.
- **Documentación.** Mientras el repositorio sea privado, la vía principal es `gh release download`, y la alternativa, descargar desde el navegador y copiar con `scp` (o `--from`).

## Decisión pendiente del usuario: hacer público el repositorio

Es lo único que da una URL estable sin credenciales, que es lo que pide «un solo comando desde un sistema limpio». Antes de hacerlo hay que revisar **todo el historial** (no solo la última versión), porque lo que salga queda indexado:

- **Tokens y secretos**: tokens de emparejamiento reales (64 hex) en docs, fixtures, logs o mensajes de commit; claves o certificados (`*.der`, `*.pem`, `*.p12`); ficheros `agent.toml`, `hub.toml`, `widget.toml` o `nodes.toml` copiados de una máquina.
- **Huellas**: `hub_fingerprint` reales (permiten reconocer el hub).
- **Red**: direcciones IP y MAC de la LAN, nombres de la red Wi‑Fi.
- **Máquinas y personas**: nombres de equipos (hostname, `node_name`), nombres de usuario y rutas de `/Users/...` o `/home/...`, correos.
- **Trazas**: capturas `--hidpp-trace` y `.hidrec` con texto tecleado (`--dump` imprime teclas), números de serie de los dispositivos.

Herramientas: `git log -p --all | grep -E '[0-9a-f]{64}'`, `gitleaks detect`, y `git filter-repo` para limpiar lo que aparezca, antes de cambiar la visibilidad.

## Consecuencias

- Hay una URL versionada para cada componente, y una suma para comprobarlo.
- Mientras el repositorio sea privado, cada equipo necesita `gh` autenticado o que se le copien los ficheros: «desde cero» no es del todo cierto.
- Los artefactos de CI siguen existiendo para probar commits sin etiqueta.
