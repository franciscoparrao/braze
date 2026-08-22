# Depósito OSF de los registros del Paper 2 — materiales y procedimiento

Fecha: 2026-08-22. **Vive en el repo, no en `/tmp`** — los formularios
del Paper 1 se prepararon en julio dentro de `/tmp/osf/` y se
perdieron. Lección aplicada.

## Qué es este depósito y qué NO es

**ES**: archivo permanente e inmutable de los documentos de registro y
sus criterios de decisión, con DOI citable.

**NO ES**: un pre-registro. Los tres estudios ya corrieron. Subirlos
hoy a OSF con la etiqueta "preregistration" sería un registro
retrospectivo — exactamente lo que la palabra excluye. Cualquier texto
del manuscrito, del depósito o de la cover letter debe decir
**"archived"**, nunca "preregistered on OSF".

**Qué prueba la anterioridad, entonces**: el historial público del
repositorio (github.com/franciscoparrao/braze), cuyas fechas de push
registra GitHub y el autor no puede alterar. Verificable por cualquiera:

```
git log --diff-filter=A --format='%ad %h' --date=short -- <archivo>
```

## Auditoría de anterioridad (hecha 2026-08-22, resultado asimétrico)

| Estudio | Registro en repo | Primeros datos | Orden verificable |
|---|---|---|---|
| Study 1 — piloto M1 | 2026-07-31 (`de10627`) | 2026-07-31 (mismo commit) | **NO** |
| Study 2 — project-memory A/B | 2026-08-04 (`c84e1c3`) | 2026-08-06 | **Sí** |
| Replicación en ornith:9b | 2026-08-15 (`b2fb8d9`) | 2026-08-16 (`2359655`) | **Sí** |

El Study 1 entró completo (protocolo + hipótesis + decisión + datos +
síntesis) en un solo commit posterior a su ejecución. Sus documentos
llevan fechas internas anteriores y se escribieron antes del sweep,
pero **eso no es verificable desde afuera** y el manuscrito ya no lo
afirma: §4.5 reporta la asimetría y clasifica el piloto como
exploratorio, apoyando los claims confirmatorios en Study 2 y la
replicación. Corregido en el commit de esta fecha.

## Archivos a depositar

De `docs/` del repositorio, en su estado actual:

1. `paper2-memory-distillation-protocol-2026-07-16.md` — protocolo del
   Study 1 (M1).
2. `hypothesis-2026-07-16-memory-distillation.md` — hipótesis del
   Study 1.
3. `hypothesis-2026-08-04-project-memory-ab.md` — Study 2, con sus
   cinco criterios de decisión y el gate de plomería.
4. `hypothesis-2026-08-15-m1-ornith9b-replication.md` — replicación,
   con su veredicto de cierre.
5. `paper2-honest-outline-2026-08-11.md` — el scoping honesto que
   reencuadró la línea tras los nulos.
6. Este README (declara qué es el depósito y qué no).

## Metadatos sugeridos del depósito

- **Título**: *Registration documents and decision criteria for
  "When Procedural Memory Does Not Pay" (braze project)*
- **Tipo**: Project / Archive — **no** "Preregistration"
- **Autor**: Francisco Parra — ORCID `0009-0006-0435-1854`
- **Filiación**: Universidad de Santiago de Chile
- **Licencia**: CC-BY 4.0
- **Descripción** (texto listo para pegar):

> Archive of the registration documents, decision criteria and honest
> priors for the three studies reported in "When Procedural Memory
> Does Not Pay: The Amortization Frontier of Prompt-Injected Memory
> for Local-First Coding Agents". These documents were committed to
> the project's public Git repository
> (https://github.com/franciscoparrao/braze) before their respective
> sweeps were launched for Study 2 and for the replication of
> Study 1; that commit order is verifiable in the repository's public
> history. For the Study 1 pilot, the documents entered the public
> repository together with its data after the pilot had run, so their
> precedence is not externally verifiable and the pilot is reported as
> exploratory. This deposit provides permanent archival of the
> documents; temporal precedence is established by the repository's
> public commit history, not by the deposit date.

- **Funding**: DICYT, Vicerrectoría de Investigación, Innovación y
  Creación, Universidad de Santiago de Chile — `062619MC_POSTDOC`.

## Procedimiento (10 minutos, sin endorsers)

1. Entrar a osf.io con ORCID o correo institucional (sin requisitos de
   trayectoria ni padrinos, a diferencia de arXiv).
2. *Create new project* con el título de arriba; pegar la descripción.
3. Subir los seis archivos.
4. *Add a DOI* (OSF lo emite en el momento).
5. Pegar el DOI en el `\todo` que queda en `paper2/main.tex` §4.5 y
   recompilar.

## Después del depósito

- Crear el tag de submission del Paper 2 y citarlo en Data
  availability (el otro `\todo`).
- Considerar el mismo procedimiento para el Paper 1 (sus formularios
  de julio se perdieron; habría que rehacerlos desde sus docs).
