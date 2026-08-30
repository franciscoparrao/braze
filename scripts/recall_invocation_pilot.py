#!/usr/bin/env python3
"""Piloto de invocación de memoria — pre-registro
`docs/hypothesis-2026-08-29-recall-invocation.md`.

Mide UNA cosa: ¿un modelo chico consulta una memoria que el system prompt
solo SEÑALIZA (índice de títulos + dónde vive el detalle), cuando la tarea
depende de una convención que solo está ahí?

El índice va en `AGENTS.md` — verificado que llega al system prompt sin
que el modelo lea el archivo. El detalle vive en `project-memory/`, NO en
`.braze/` (ese es Irreversible para el modelo desde v8 P2, y mezclar
permisos con invocación probaría dos palancas a la vez).

Las convenciones son ARBITRARIAS a propósito: si fueran las idiomáticas,
el modelo acertaría sin consultar y la métrica de cumplimiento no
distinguiría nada.
"""

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile

BRAZE = pathlib.Path(__file__).resolve().parent.parent / "target" / "debug" / "braze"
SESSIONS = pathlib.Path.home() / ".local" / "share" / "braze" / "sessions"

# El índice: lista y señala, NUNCA ordena consultar. Una instrucción del
# tipo "debes leer la memoria" mediría obediencia, no invocación.
AGENTS_MD = """# Project instructions

## Memoria del proyecto

Entradas registradas (el detalle de cada una vive en `project-memory/<id>.md`):

- `errors` — Cómo se construyen los errores públicos de este proyecto
- `logging` — Campos obligatorios en los logs
- `tests` — Convención de nombres de test
"""

MEMORY = {
    "errors": """# Cómo se construyen los errores públicos

Los errores públicos SIEMPRE se construyen con `AppError::from_code(u16)`.
Nunca con `AppError::new(...)` ni instanciando variantes directamente.

Códigos asignados en este proyecto:
- entrada inválida / parseo fallido: **422**
- recurso ausente: 404
""",
    "logging": """# Campos obligatorios en los logs

Todo log de este proyecto lleva SIEMPRE el campo estructurado `req_id`.

El nivel por defecto es `debug`, nunca `info`: los logs de `info` están
reservados para el arranque del proceso.

Ejemplo: `tracing::debug!(req_id, "mensaje")`
""",
    "tests": """# Convención de nombres de test

Los tests unitarios se nombran con el prefijo `check_`, nunca con `test_`.

Ejemplo: `fn check_suma_de_positivos()`
""",
}

CARGO_TOML = """[package]
name = "pilot"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"""

LIB_RS = """pub enum AppError {
    Invalid(u16),
}

impl AppError {
    pub fn new(code: u16) -> Self {
        AppError::Invalid(code)
    }

    pub fn from_code(code: u16) -> Self {
        AppError::Invalid(code)
    }
}

pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn handle(req_id: &str) -> i64 {
    add(1, 2)
}
"""

TASKS = [
    {
        "id": "errors",
        "prompt": (
            "Agrega a src/lib.rs una funcion publica "
            "`parse_port(s: &str) -> Result<u16, AppError>` que parsee un "
            "numero de puerto desde el string y devuelva el error apropiado "
            "del proyecto si el string no es valido."
        ),
        # La convención se cumple si construye el error como manda la
        # memoria; `new(` o un código distinto de 422 significan que
        # resolvió con sus propios supuestos.
        "check": lambda src: "from_code(422)" in src.replace(" ", ""),
    },
    {
        "id": "logging",
        "prompt": (
            "Agrega logging a la funcion `handle` de src/lib.rs siguiendo "
            "las convenciones de este proyecto."
        ),
        "check": lambda src: "req_id" in src
        and re.search(r"debug!\s*\(", src) is not None
        and re.search(r"info!\s*\(", src) is None,
    },
    {
        "id": "tests",
        "prompt": (
            "Agrega un test unitario para la funcion `add` de src/lib.rs "
            "siguiendo las convenciones de este proyecto."
        ),
        "check": lambda src: re.search(r"fn\s+check_", src) is not None,
    },
]


def build_fixture(root: pathlib.Path):
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "project-memory").mkdir(parents=True, exist_ok=True)
    (root / "AGENTS.md").write_text(AGENTS_MD)
    (root / "Cargo.toml").write_text(CARGO_TOML)
    (root / "src" / "lib.rs").write_text(LIB_RS)
    for name, body in MEMORY.items():
        (root / "project-memory" / f"{name}.md").write_text(body)


def consulted_memory(session_id: str):
    """(bool, [ids consultados]) leyendo las tool calls del rollout log."""
    path = SESSIONS / f"{session_id}.jsonl"
    if not path.exists():
        return False, []
    seen = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except Exception:
            continue
        if event.get("type") != "assistant_tool_call":
            continue
        # Cualquier herramienta que toque project-memory cuenta como
        # consulta: read_file es la esperada, pero un grep/shell sobre el
        # directorio es la misma conducta y no debe contarse como no-uso.
        blob = json.dumps(event.get("arguments", {}))
        if "project-memory" in blob:
            seen.append(event.get("name"))
    return bool(seen), seen


def run_one(task, rep, model, ollama_url, timeout):
    workdir = pathlib.Path(tempfile.mkdtemp(prefix=f"recall-{task['id']}-{rep}-"))
    try:
        build_fixture(workdir)
        proc = subprocess.run(
            [str(BRAZE), "run", "--output-format", "json", task["prompt"]],
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=timeout,
            # Se hereda el entorno en vez de recortarlo: el guardrail
            # post-edit corre `cargo check`, y sin `cargo` en el PATH la
            # corrida mediría un harness distinto del de producción.
            env={
                **os.environ,
                "BRAZE_DEFAULT_BACKEND": "ollama",
                "BRAZE_OLLAMA_MODEL": model,
                "BRAZE_OLLAMA_BASE_URL": ollama_url,
            },
        )
        session_id, rounds = None, None
        for line in proc.stdout.splitlines():
            line = line.strip()
            if line.startswith("{"):
                try:
                    payload = json.loads(line)
                    session_id = payload.get("session_id")
                    rounds = payload.get("rounds")
                except Exception:
                    pass
        source = (workdir / "src" / "lib.rs").read_text(errors="replace")
        recalled, tools = consulted_memory(session_id) if session_id else (False, [])
        return {
            "task": task["id"],
            "rep": rep,
            "model": model,
            "session_id": session_id,
            "rounds": rounds,
            "recalled": recalled,
            "recall_tools": tools,
            "complied": bool(task["check"](source)),
            "returncode": proc.returncode,
        }
    except subprocess.TimeoutExpired:
        return {
            "task": task["id"],
            "rep": rep,
            "model": model,
            "recalled": False,
            "complied": False,
            "timeout": True,
        }
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="gpt-oss:20b")
    ap.add_argument("--ollama-url", default="http://192.168.1.8:11434")
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--timeout", type=int, default=240)
    ap.add_argument("--output", required=True)
    args = ap.parse_args()

    results = []
    for rep in range(args.reps):
        for task in TASKS:
            record = run_one(task, rep, args.model, args.ollama_url, args.timeout)
            results.append(record)
            print(
                f"  {record['task']} rep{rep}: "
                f"recall={record['recalled']} complied={record['complied']} "
                f"rounds={record.get('rounds')}",
                flush=True,
            )

    done = [r for r in results if not r.get("timeout")]
    recalled = [r for r in done if r["recalled"]]
    summary = {
        "n": len(done),
        "recall_invocation_rate": round(len(recalled) / len(done), 3) if done else None,
        "convention_compliance": round(
            sum(r["complied"] for r in done) / len(done), 3
        )
        if done
        else None,
        "compliance_given_recall": round(
            sum(r["complied"] for r in recalled) / len(recalled), 3
        )
        if recalled
        else None,
        # `None` cuando NO hubo corridas sin consulta — un 0.0 ahí se
        # leería como "consultaron y aun así fallaron", que es lo
        # contrario de lo que pasó.
        "compliance_without_recall": (
            round(
                sum(r["complied"] for r in done if not r["recalled"])
                / len([r for r in done if not r["recalled"]]),
                3,
            )
            if [r for r in done if not r["recalled"]]
            else None
        ),
        "n_without_recall": len([r for r in done if not r["recalled"]]),
        "timeouts": len(results) - len(done),
    }
    pathlib.Path(args.output).write_text(
        json.dumps(
            {
                "schema": "recall-invocation-pilot/1",
                "preregistration": "docs/hypothesis-2026-08-29-recall-invocation.md",
                "model": args.model,
                "reps": args.reps,
                "summary": summary,
                "runs": results,
            },
            indent=1,
            ensure_ascii=False,
        )
    )
    print(json.dumps(summary, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
