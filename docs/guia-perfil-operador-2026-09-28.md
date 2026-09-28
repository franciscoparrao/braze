# Guía de uso: braze como agente diario (perfil operador)

Escrita el 2026-09-28, al cerrar los cinco puntos del perfil operador
(commits `173194a` → `2d2fde6`). Es la guía de arranque para usar braze en
vez de OpenCode o Claude Code en el trabajo real, e ir reportando lo que
falle. Todo lo que dice acá está verificado en vivo con `glm-5.3-flash`
por OpenCode Go.

## 1. Qué tienes configurado

Todo vive en `~/.config/braze/`, fuera de cualquier repo:

| archivo | qué hace |
|---|---|
| `config.json` | backend por default `zen` → OpenCode Go con `glm-5.3-flash`; skills desde `~/.claude/skills`; memoria de proyecto; `references` a `~/.claude/*` y `~/vault`; hooks de sesión; tools web; tope de AGENTS.md 48 KB |
| `policy.toml` | permisos permanentes: qué corre sin preguntar, qué se deniega siempre, qué hosts web se permiten |
| `AGENTS.md` | enlace a `~/.claude/CLAUDE.md`: tus instrucciones globales, inyectadas antes de las del proyecto |

El binario instalado es `~/.cargo/bin/braze` (y `braze-bench`). Después de
un `git pull` con cambios de código hay que reinstalarlo:

```
cd ~/proyectos/braze && cargo install --path crates/braze-cli --force
```

## 2. Arrancar una sesión

```
cd ~/proyectos/<proyecto>
braze chat --tui          # interfaz completa (recomendado)
braze chat                # chat plano, sin interfaz
braze run "una tarea"     # un solo turno, imprime y sale
```

- braze imprime el `session id` al inicio; `braze chat --resume <id>`
  continúa esa conversación con sus permisos ya aprobados.
- `/model` cambia de backend o modelo a mitad de sesión (`/model zen:kimi-k3`);
  `/skills` abre el picker; `/permissions`, `/tasks`, `/help`, `/quit`.
- Para que el proyecto tenga instrucciones propias, enlaza su `CLAUDE.md`:
  `ln -s CLAUDE.md AGENTS.md`. Braze lee `AGENTS.md`, no `CLAUDE.md`.
- Al arrancar, y tras cada compactación, corre `context_resume.sh` y lee
  el `MEMORY.md` nativo del proyecto: el modelo parte sabiendo la tarea
  actual. Si no hay `session_state` para el directorio, esa sección no
  aparece; no es error.

## 3. Skills

- `$nombre argumentos` o `/nombre argumentos`. Los argumentos reemplazan
  `$ARGUMENTS` en el cuerpo de la skill, como en Claude Code.
- Se cargan solo por mención explícita. No hay carga automática por
  descripción, a propósito.
- Máximo 3 skills por turno y 4.000 tokens por cuerpo (config `skills`).
- Una skill que pide `Agent` (subagentes) no va a poder lanzarlo: braze no
  tiene subagente genérico todavía. Las que piden `WebFetch`/`WebSearch`
  sí funcionan (`web_fetch`/`web_search`). LSP no existe.

## 4. Permisos

Lo que no está en la política y no es de solo lectura pide confirmación:

```
run `curl --version`
¿Permitir? [y = sí / N = no / a = siempre]:
```

- `y` aprueba esta vez (y se recuerda dentro de la sesión).
- `a` aprueba y agrega una regla `allow` a `policy.toml`, que vale en el
  acto para toda la sesión y para las siguientes. La regla generaliza como
  Claude Code: el programa (`python3`), `git commit*` para multiplexores,
  el directorio para archivos fuera del proyecto, el host para URLs.
- En la TUI es la tecla `a` en el overlay.
- Sin terminal (`braze run`, cron, pipes) todo lo que pregunte se deniega.
  Si algo debe correr sin supervisión, tiene que estar en la política.
- `braze permissions policy` valida y lista la política.
  `braze permissions suggest` mina las sesiones pasadas y propone reglas
  TOML listas para pegar.
- Lo que hoy pide confirmación siempre: `bash -c`, `sh -c`, pipes,
  `git push`, `rm -rf`, escrituras fuera del proyecto. Lo que se deniega
  siempre: leer o escribir `~/.config/braze/`, secretos (`.env`, `.ssh`,
  `.pem`), `sudo`, `dd`, `mkfs`.

## 5. Web

`web_search` (DuckDuckGo, sin API key) y `web_fetch` (HTML a texto, tope
200 KB). Solo los hosts de la regla `web-allowed-hosts` pasan sin preguntar:
buscador, docs.rs, crates.io, GitHub, Wikipedia, arXiv, PyPI, MDN,
Stack Overflow, readthedocs. Cualquier otro host pregunta; responde `a`
para agregarlo. Para abrir todo: `match = ["*"]` en esa regla, aceptando
que el modelo podría exfiltrar datos por una URL.

## 6. Elegir el modelo de Go

Go ofrece hoy 30 modelos (`/zen/go/v1/models`): entre otros
`glm-5.3-flash`, `glm-5.3`, `deepseek-v4-flash`, `deepseek-v4.1-flash`,
`deepseek-v4-pro`, `kimi-k2.7-code`, `kimi-k3`, `qwen3.8-flash`,
`qwen3.8-max`, `minimax-m3`, `grok-4.7`, `mimo-v2.6-pro`. Todos son
OpenAI-compatibles y entran por el backend `zen` con la misma key.

El criterio no es "el más grande" sino el que mejor completa las tareas de
braze por menos rondas y latencia, porque el harness está tuneado para
modelos que siguen el tool calling limpio. El instrumento para decidirlo
es el bench del proyecto, con la suite chica primero:

```
cd ~/proyectos/braze
braze-bench crates/braze-bench/suites/fast-core.toml \
  --backends "zen:glm-5.3-flash,zen:deepseek-v4.1-flash,zen:kimi-k2.7-code,zen:qwen3.8-flash" \
  --repetitions 2 --seed 7 --task-timeout-secs 300 \
  --output docs/sweep-go-fast-core-$(date +%F).json
```

`fast-core.toml` son 13 tareas; con 4 modelos y 2 repeticiones son 104
corridas, unos minutos y una fracción de la cuota diaria de Go. El reporte
da pass rate, `pass^k`, rondas y latencia por modelo, y la comparación
pareada contra el primer brazo. Si dos empatan, gana el más rápido; si
quieres discriminar más, repite con `discriminating.toml` (34 tareas).

Para cambiar el default: `zen_model` en `config.json`, `--model <id>` en
la línea de comandos, o `/model zen:<id>` en la TUI.

Punto de partida hasta tener el sweep: `glm-5.3-flash` (verificado en
todo lo anterior) y, como segunda opción, `deepseek-v4.1-flash`, porque
la familia DeepSeek flash fue la mejor del proyecto vía OpenRouter (49/50
en `default.toml`).

## 7. Qué reportar cuando algo falle

Con esto puedo reproducirlo sin preguntas:

1. El comando exacto y el directorio.
2. El `session id` que braze imprimió. El rollout completo está en
   `~/.local/share/braze/sessions/<id>.jsonl`; puedes pegar la ruta y lo
   leo.
3. Si es raro, la misma sesión con trazas:
   `RUST_LOG=braze=info,braze_engine=info braze chat …` (van a stderr).
4. Qué esperabas que pasara. Sobre todo si una skill hizo algo distinto
   que en Claude Code.

## 8. Límites conocidos hoy

- Sin subagente genérico ni LSP.
- Las skills están escritas para Claude; un modelo de Go puede seguirlas
  peor. Ese es el riesgo real del plan, no la ingeniería.
- `web_search` depende del HTML de DuckDuckGo: si cambia, falla con un
  error claro en vez de inventar resultados.
- El discovery de skills salta `.trash/` y prefiere el `SKILL.md` más
  superficial ante duplicados; si una skill "no aparece", revisa que su
  `SKILL.md` tenga `name:` y `description:`.
- Compactación: se dispara a 40 eventos tácticos; el resumen es
  extractivo salvo que configures `enable_lead_summary`.
