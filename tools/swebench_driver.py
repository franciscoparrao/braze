#!/usr/bin/env python3
"""Driver del ancla SWE-bench Lite — diseño pre-registrado en
docs/swebench-lite-anchor-design-2026-07-19.md (comprometido ANTES de
implementar esto).

braze-bench no tiene fixtures de tipo repositorio; este driver prepara
un checkout por (instancia, brazo, rep), invoca `braze run
--output-format json` EN ese checkout, registra el `git diff` como
model_patch y produce (a) un JSON de corridas con la metadata de
reproducibilidad (commit de braze, digests, versión de Ollama, wall
time, clasificación de transporte) y (b) un predictions.jsonl en el
formato del harness OFICIAL de SWE-bench, que hace el grading (Docker)
— el grader no es autoral, ese es el punto del ancla.

Solo stdlib. Subcomandos:

  fetch    descarga las 300 instancias del split test vía la
           datasets-server API de HF → tools/swebench_cache/lite_test.json
  sample   muestra determinística (seed 42 sobre instance_id ordenado,
           n=20) → docs/swebench-lite-sample-2026-07-19.txt
  run      corre UN brazo completo (20 instancias × --reps), un run a
           la vez — braze en Nitro, repos locales
  grade    invoca el harness oficial si `swebench` está instalado; si
           no, imprime el comando exacto

Postura de permisos: `braze run` corre con stdin cerrado — las acciones
Reversible dentro del checkout (write/edit) pasan por el allowlist; una
confirmación irresoluble se deniega sola (EOF), y el modelo se adapta o
falla: mismo posture que el bench. Fallos de transporte (request nunca
llegó / stream muerto <1s, criterio del paper) se clasifican por el
stderr de braze y cuentan contra la regla pre-registrada del 2%.
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CACHE = REPO_ROOT / "tools" / "swebench_cache"
LITE_JSON = CACHE / "lite_test.json"
SAMPLE_FILE = REPO_ROOT / "docs" / "swebench-lite-sample-2026-07-19.txt"
DATASET = "princeton-nlp/SWE-bench_Lite"
SAMPLE_SEED = 42
SAMPLE_N = 20
PROMPT_CAP = 4000
RUN_TIMEOUT_SECS = 600
INSTRUCTION = (
    "\n\nFix the issue described above by editing the repository. "
    "Do not run tests."
)
TRANSPORT_MARKERS = (
    "request to model backend failed",
    "stream failed",
    "error sending request",
)


def http_json(url: str):
    with urllib.request.urlopen(url, timeout=60) as r:
        return json.load(r)


def cmd_fetch(_args):
    CACHE.mkdir(parents=True, exist_ok=True)
    rows = []
    offset = 0
    while True:
        url = (
            "https://datasets-server.huggingface.co/rows?"
            f"dataset={urllib.parse.quote(DATASET, safe='')}"
            f"&config=default&split=test&offset={offset}&length=100"
        )
        page = http_json(url)
        batch = [r["row"] for r in page.get("rows", [])]
        if not batch:
            break
        rows.extend(batch)
        offset += len(batch)
        print(f"  {len(rows)} instancias…")
        if len(batch) < 100:
            break
    LITE_JSON.write_text(json.dumps(rows, ensure_ascii=False))
    print(f"escrito: {LITE_JSON} ({len(rows)} instancias)")


def load_instances():
    if not LITE_JSON.exists():
        sys.exit("falta el cache — corre `fetch` primero")
    return json.loads(LITE_JSON.read_text())


def cmd_sample(_args):
    rows = load_instances()
    ids = sorted(r["instance_id"] for r in rows)
    sample = sorted(random.Random(SAMPLE_SEED).sample(ids, SAMPLE_N))
    SAMPLE_FILE.write_text("\n".join(sample) + "\n")
    print(f"escrito: {SAMPLE_FILE}")
    for i in sample:
        print(f"  {i}")


def load_sample():
    if not SAMPLE_FILE.exists():
        sys.exit("falta la muestra — corre `sample` primero")
    return SAMPLE_FILE.read_text().split()


def sh(args, cwd=None, timeout=None, env=None, input_=None):
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        input=input_,
        stdin=subprocess.DEVNULL if input_ is None else None,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def bare_clone(repo: str) -> Path:
    dest = CACHE / "repos" / (repo.replace("/", "__") + ".git")
    if not dest.exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        print(f"  clonando {repo}…")
        r = sh(
            ["git", "clone", "--bare", f"https://github.com/{repo}.git", str(dest)],
            timeout=1800,
        )
        if r.returncode != 0:
            sys.exit(f"clone de {repo} falló: {r.stderr[-400:]}")
    return dest


def checkout(repo: str, commit: str, dest: Path):
    bare = bare_clone(repo)
    dest.parent.mkdir(parents=True, exist_ok=True)
    r = sh(["git", "clone", "--shared", str(bare), str(dest)], timeout=600)
    if r.returncode != 0:
        raise RuntimeError(f"clone local falló: {r.stderr[-300:]}")
    r = sh(["git", "-C", str(dest), "checkout", "--quiet", commit], timeout=300)
    if r.returncode != 0:
        # el commit puede no estar en el bare cacheado si el remoto avanzó
        sh(["git", "-C", str(bare), "fetch", "origin", commit], timeout=900)
        r = sh(["git", "-C", str(dest), "checkout", "--quiet", commit], timeout=300)
        if r.returncode != 0:
            raise RuntimeError(f"checkout {commit} falló: {r.stderr[-300:]}")


def arm_flags(arm: str):
    """'ollama:llama3.2:1b[+lead:ollama:gemma4:e4b]' → flags de braze run."""
    lead = None
    executor = arm
    if "+lead:" in arm:
        executor, lead = arm.split("+lead:", 1)
    backend, model = executor.split(":", 1)
    flags = ["--backend", backend, "--model", model]
    if lead:
        flags += ["--lead", lead]
    return flags


def collect_metadata(ollama_base: str, models):
    meta = {
        "braze_git_commit": sh(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT
        ).stdout.strip(),
        "ollama_server_version": None,
        "ollama_model_digests": [],
        "design": "docs/swebench-lite-anchor-design-2026-07-19.md",
        "sample_seed": SAMPLE_SEED,
        "prompt_cap_chars": PROMPT_CAP,
        "run_timeout_secs": RUN_TIMEOUT_SECS,
    }
    try:
        meta["ollama_server_version"] = http_json(f"{ollama_base}/api/version").get(
            "version"
        )
        tags = http_json(f"{ollama_base}/api/tags").get("models", [])
        by_name = {m["name"]: m.get("digest") for m in tags}
        meta["ollama_model_digests"] = [
            {"model": m, "digest": by_name.get(m)} for m in sorted(models)
        ]
    except Exception as e:  # best-effort, igual que RunMetadata
        meta["metadata_error"] = str(e)[:200]
    return meta


def cmd_run(args):
    rows = {r["instance_id"]: r for r in load_instances()}
    sample = load_sample()
    braze = REPO_ROOT / "target" / "release" / "braze"
    if not braze.exists():
        sys.exit("falta target/release/braze — `cargo build -p braze-cli --release`")

    arm_slug = re.sub(r"[^a-z0-9]+", "_", args.arm.lower()).strip("_")
    date = time.strftime("%Y-%m-%d")
    out_json = REPO_ROOT / "docs" / f"swebench-lite-run-{arm_slug}-{date}.json"
    preds_path = REPO_ROOT / "docs" / f"swebench-lite-preds-{arm_slug}-{date}.jsonl"

    models = set()
    for part in [args.arm.split("+lead:")[0]] + (
        [args.arm.split("+lead:")[1]] if "+lead:" in args.arm else []
    ):
        models.add(part.split(":", 1)[1])

    runs, preds = [], []
    # reanudación: si el JSON ya existe, saltar (instancia, rep) hechas
    if out_json.exists():
        prev = json.loads(out_json.read_text())
        runs = prev.get("runs", [])
        preds = [json.loads(l) for l in preds_path.read_text().splitlines()] if preds_path.exists() else []
    done = {(r["instance_id"], r["rep"]) for r in runs}

    env = dict(os.environ)
    env.setdefault("BRAZE_OLLAMA_BASE_URL", "http://192.168.1.8:11434")
    env.setdefault("BRAZE_OLLAMA_TRANSPORT_RETRIES", "6")
    env.setdefault("BRAZE_CIRCUIT_BREAKER", "off")

    total = len(sample) * args.reps
    n = 0
    for instance_id in sample:
        inst = rows[instance_id]
        prompt = inst["problem_statement"]
        truncated = len(prompt) > PROMPT_CAP
        if truncated:
            prompt = prompt[:PROMPT_CAP] + "\n[...issue text truncated...]"
        prompt += INSTRUCTION

        for rep in range(args.reps):
            n += 1
            if (instance_id, rep) in done:
                continue
            work = CACHE / "work" / arm_slug / f"{instance_id}__rep{rep}"
            if work.exists():
                sh(["rm", "-rf", str(work)])
            record = {
                "instance_id": instance_id,
                "arm": args.arm,
                "rep": rep,
                "repo": inst["repo"],
                "base_commit": inst["base_commit"],
                "prompt_truncated": truncated,
            }
            t0 = time.time()
            try:
                checkout(inst["repo"], inst["base_commit"], work)
                run_env = dict(env)
                run_env["XDG_DATA_HOME"] = str(work / ".braze-data")
                r = sh(
                    [str(braze), "run", prompt, "--output-format", "json"]
                    + arm_flags(args.arm),
                    cwd=work,
                    timeout=RUN_TIMEOUT_SECS,
                    env=run_env,
                )
                record["exit_code"] = r.returncode
                record["stderr_tail"] = r.stderr[-400:]
                if r.returncode == 0:
                    try:
                        record["braze_json"] = json.loads(
                            r.stdout.strip().splitlines()[-1]
                        )
                    except Exception:
                        record["braze_stdout_tail"] = r.stdout[-400:]
                diff = sh(
                    ["git", "-C", str(work), "diff"], timeout=120
                ).stdout
                record["patch_bytes"] = len(diff)
                preds.append(
                    {
                        "instance_id": instance_id,
                        "model_name_or_path": args.arm,
                        "model_patch": diff,
                    }
                )
            except subprocess.TimeoutExpired:
                record["timeout"] = True
                record["exit_code"] = None
                diff = sh(["git", "-C", str(work), "diff"], timeout=120).stdout if work.exists() else ""
                record["patch_bytes"] = len(diff)
                preds.append(
                    {
                        "instance_id": instance_id,
                        "model_name_or_path": args.arm,
                        "model_patch": diff,
                    }
                )
            except Exception as e:
                record["driver_error"] = str(e)[:300]
            record["wall_secs"] = round(time.time() - t0, 1)
            stderr = record.get("stderr_tail", "")
            record["transport_failure"] = bool(
                record.get("exit_code") not in (0, None)
                and any(m in stderr for m in TRANSPORT_MARKERS)
            )
            runs.append(record)
            sh(["rm", "-rf", str(work)])

            out = {
                "metadata": collect_metadata(
                    env["BRAZE_OLLAMA_BASE_URL"], models
                ),
                "runs": runs,
            }
            out_json.write_text(json.dumps(out, ensure_ascii=False, indent=1))
            preds_path.write_text(
                "\n".join(json.dumps(p, ensure_ascii=False) for p in preds) + "\n"
            )
            transport = sum(1 for x in runs if x.get("transport_failure"))
            print(
                f"[{n}/{total}] {instance_id} rep{rep} "
                f"exit={record.get('exit_code')} patch={record.get('patch_bytes', 0)}B "
                f"wall={record['wall_secs']}s transporte_acum={transport}"
            )

    transport = sum(1 for x in runs if x.get("transport_failure"))
    rate = 100 * transport / max(1, len(runs))
    print(f"\nbrazo completo: {len(runs)} runs, transporte {transport} ({rate:.1f}%)")
    print(f"regla del 2%: {'OK' if rate <= 2 else 'EXCEDIDA — brazo inválido, re-correr'}")
    print(f"runs: {out_json}\npredictions: {preds_path}")


def cmd_grade(args):
    cmd = [
        sys.executable,
        "-m",
        "swebench.harness.run_evaluation",
        "--dataset_name",
        DATASET,
        "--predictions_path",
        args.predictions,
        "--max_workers",
        "4",
        "--run_id",
        args.run_id,
    ]
    try:
        import swebench  # noqa: F401
    except ImportError:
        print("swebench no instalado. Instala y corre:")
        print("  pip install swebench")
        print("  " + " ".join(cmd))
        return
    print("corriendo el harness oficial (Docker)…")
    os.execvp(cmd[0], cmd)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("fetch")
    sub.add_parser("sample")
    runp = sub.add_parser("run")
    runp.add_argument("--arm", required=True, help="ej: ollama:llama3.2:1b+lead:ollama:gemma4:e4b")
    runp.add_argument("--reps", type=int, default=2)
    gradep = sub.add_parser("grade")
    gradep.add_argument("--predictions", required=True)
    gradep.add_argument("--run-id", required=True)
    args = ap.parse_args()
    {"fetch": cmd_fetch, "sample": cmd_sample, "run": cmd_run, "grade": cmd_grade}[
        args.cmd
    ](args)


if __name__ == "__main__":
    main()
