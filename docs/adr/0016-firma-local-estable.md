# ADR 0016 — Firma local estable de los binarios del Mac

- Estado: aceptada (2026-10-23), bloque R1.

## Contexto

El agente, el helper de borde y el widget iban firmados ad hoc. El requisito designado de una firma ad hoc es el `cdhash`, que cambia con cada compilación. macOS (TCC y el permiso de Red local) reconoce los programas por ese requisito, así que cada versión es "otro programa":

- tras el despliegue v4 el widget dejó de llegar al hub (`No route to host`) con el interruptor de Red local activo; lanzado desde una shell con permiso, conectaba;
- la Monitorización de entrada del agente (LED de Bloq Mayús, ADR 0012) corre el mismo riesgo en cada reinstalación.

Un Developer ID de Apple resolvería esto y la cuarentena, pero cuesta una cuenta de pago y no hace falta para un uso local.

## Decisión

- `deploy/macos/make-signing-identity.sh` crea una vez, en el llavero de inicio de sesión del usuario, un certificado autofirmado **RawKVM Local** (RSA 2048, 10 años, EKU `codeSigning`) con `/usr/bin/openssl` (LibreSSL), lo importa con `security import -T /usr/bin/codesign` y lo marca de confianza para firma de código en el dominio de administración (`security add-trusted-cert -d -r trustRoot -p codeSign`, con sudo). Es idempotente.
- `deploy/macos/signing.sh`, compartido por `install.sh`, `install-edge.sh` e `install-widget.sh` (nuevo), firma con esa identidad si el usuario que invoca (`$SUDO_USER` bajo sudo) la tiene, y ad hoc avisando si no. Bajo sudo firma una copia temporal como ese usuario (la clave está en su llavero) y después la instala como root.
- Identificadores fijos: `cc.olitec.rawkvm.agent` y `cc.olitec.rawkvm.edge` para los binarios (si no, `codesign` usa el nombre del fichero); la app usa su `CFBundleIdentifier`.
- El requisito designado pasa a `identifier "…" and certificate leaf = H"<sha1 del certificado>"`, igual en todas las versiones firmadas con el mismo certificado.
- El widget se instala en `~/Applications` con `install-widget.sh`, que firma una copia antes de tocar la instalada. CI sigue produciendo artefactos ad hoc y empaqueta los tres scripts junto a la app.
- No se usa `security set-key-partition-list`, que pediría la contraseña del llavero en el terminal: la primera firma muestra el diálogo de acceso a la clave y se responde «Permitir siempre».

## Consecuencias

- Los permisos se dan una vez por Mac y sobreviven a las actualizaciones. Al pasar de ad hoc a «RawKVM Local» hay que darlos una última vez.
- Ese último paso no es igual para los dos permisos (comprobado en el Mac del usuario con v6). Red local del widget se renovó con una sola concesión. Monitorización de entrada del agente quedó denegada con el interruptor encendido, incluso volviendo a arrastrar el binario. `tccd` registra `Failed to match existing code requirement for subject /usr/local/bin/rawkvm-agent and service kTCCServiceListenEvent`: la entrada conserva el csreq de la firma ad hoc y a un daemon no se le muestra el diálogo de reautorización. Solución, una vez: `sudo tccutil reset ListenEvent` (afecta a todas las apps), volver a arrastrar el binario y `launchctl kickstart -k`. `install.sh` lo avisa cuando el agente instalado antes estaba firmado ad hoc y el nuevo no.
- Si se borra el certificado y se crea otro, el requisito cambia y hay que volver a dar los permisos.
- La cuarentena sigue: los artefactos descargados se abren con `xattr -dr com.apple.quarantine` (lo hace `install-widget.sh`) o clic derecho → Abrir.
- Comprobado en el Mac de desarrollo: `codesign` firma con un certificado así aunque aún no sea de confianza, y el requisito sale por certificado. Comprobado con el usuario en el despliegue v6: Red local sobrevivió a la reinstalación sin volver a concederla, y Monitorización de entrada funciona (`watching the keyboard LEDs`) tras el reset único descrito arriba.
