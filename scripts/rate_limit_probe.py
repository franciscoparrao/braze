#!/usr/bin/env python3
"""Sonda de rate limit para gateways de modelos (OpenCode Zen, OpenRouter).

Mide QUÉ TIPO de límite impone un proveedor sobre un modelo: ráfaga por
minuto o cuota diaria. La distinción decide si los modelos gratuitos
sirven para un asistente interactivo o solo para benchmarking, y cómo
espaciar un sweep largo.

MÉTODO. Dos fases:

  1. Ráfaga: llamadas mínimas seguidas (prompt corto, `max_tokens` bajo,
     sin tools, sin streaming) hasta el primer 429. Registra hora,
     latencia, status, TODAS las cabeceras y el cuerpo de los errores.
  2. Recuperación: espera los intervalos de `--backoff` (minutos) y en
     cada uno intenta de nuevo. Si el proveedor responde, cuenta cuántas
     llamadas seguidas acepta antes del siguiente 429.

Un límite que se recupera al minuto es de ráfaga; uno que sigue
rechazando a los 60 minutos es de cuota. `--cross-check` prueba OTRO
modelo en el instante del 429: si responde, el límite es por modelo; si
también rechaza, es por cuenta.

POR QUÉ UN SCRIPT Y NO UN SUBCOMANDO DE braze-bench. Tres razones, todas
verificadas antes de decidir:

  - El requisito de registrar cabeceras y cuerpo del 429 exige HTTP
    directo: `OpenRouterBackend` no los expone, solo mapea el status a
    `ModelError`.
  - `reqwest` no es dependencia de `braze-bench`; agregarla para una
    sonda de medición es peso permanente por un uso puntual.
  - "Respetar el circuit breaker" resulta un no-problema: su registry es
    un `static` POR PROCESO (`circuit_breaker.rs:334`), así que una
    sonda aparte no lo comparte con el bench; y un 429 es
    `Outcome::Neutral` (`circuit_breaker.rs:117`, "429 means 'slow
    down', not 'I'm down'"), así que ni siquiera podría abrirlo.

Tampoco usa el retry H-19, por diseño: acá el 429 es el dato, no un
error a reintentar.

Uso:
    python3 scripts/rate_limit_probe.py --target zen:hy3-free \\
        --cross-check zen:laguna-s-2.1-free --output docs/probe.json

La key se resuelve como en braze: variable de entorno primero
(`BRAZE_ZEN_API_KEY` / `BRAZE_OPENROUTER_API_KEY`), luego
`~/.config/braze/config.json`. Nunca se escribe en la salida.
"""

import argparse
import json
import os
import pathlib
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

CONFIG = pathlib.Path.home() / ".config" / "braze" / "config.json"

BACKENDS = {
    "zen": {
        "base_url": "https://opencode.ai/zen/v1",
        "env": "BRAZE_ZEN_API_KEY",
        "config_key": "zen_api_key",
        "config_base": "zen_base_url",
    },
    "openrouter": {
        "base_url": "https://openrouter.ai/api/v1",
        "env": "BRAZE_OPENROUTER_API_KEY",
        "config_key": "openrouter_api_key",
        "config_base": "openrouter_base_url",
    },
}


def now():
    return datetime.now(timezone.utc).isoformat()


def load_config():
    try:
        return json.loads(CONFIG.read_text())
    except Exception:
        return {}


def resolve(backend, cfg):
    """(base_url, api_key). Precedencia env > config, como braze."""
    spec = BACKENDS.get(backend)
    if not spec:
        sys.exit(f"backend desconocido: {backend} (conocidos: {', '.join(BACKENDS)})")
    key = os.environ.get(spec["env"]) or cfg.get(spec["config_key"])
    if not key:
        sys.exit(f"falta la API key: exporta {spec['env']} o pon {spec['config_key']} en {CONFIG}")
    base = (
        os.environ.get(spec["env"].replace("API_KEY", "BASE_URL"))
        or cfg.get(spec["config_base"])
        or spec["base_url"]
    )
    return base.rstrip("/"), key


def call(base_url, api_key, model, timeout=60):
    """Una llamada mínima. Devuelve el registro, nunca lanza."""
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1,
            "stream": False,
        }
    ).encode()
    req = urllib.request.Request(
        f"{base_url}/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "content-type": "application/json",
            # Sin esto, Cloudflare responde 403 "error code: 1010"
            # (bloqueo por fingerprint del cliente) al User-Agent
            # que urllib manda por default. Medido contra Zen el
            # 2026-08-29: curl pasaba y urllib no, y el 403 no era ni
            # autorización ni cuota.
            "user-agent": "braze-rate-limit-probe/1 (+https://github.com/franciscoparrao/braze)",
        },
        method="POST",
    )
    t0 = time.monotonic()
    rec = {"at": now(), "model": model}
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            rec["status"] = r.status
            rec["headers"] = dict(r.headers)
            r.read()
    except urllib.error.HTTPError as e:
        rec["status"] = e.code
        rec["headers"] = dict(e.headers)
        try:
            # El cuerpo del error puede traer el mensaje del límite —
            # capado porque un 5xx a veces devuelve una página entera.
            rec["body"] = e.read().decode("utf-8", "replace")[:2000]
        except Exception:
            rec["body"] = "(cuerpo ilegible)"
    except Exception as e:  # timeout, DNS, TLS
        rec["status"] = None
        rec["error"] = f"{type(e).__name__}: {e}"
    rec["latency_ms"] = round((time.monotonic() - t0) * 1000)
    return rec


def burst(base_url, api_key, model, max_calls, log, label):
    """Llama hasta el primer 429, un fallo duro, o `max_calls`.

    Devuelve (n_aceptadas, registro_que_cortó). Un 400/500 corta y se
    documenta en vez de insistir: el traspaso lo pide explícitamente, y
    un 5xx repetido no distingue "límite" de "proveedor caído"."""
    accepted = 0
    for i in range(max_calls):
        rec = call(base_url, api_key, model)
        rec["phase"] = label
        rec["index"] = i
        log.append(rec)
        st = rec.get("status")
        if st == 429:
            print(f"    #{i + 1} → 429 tras {accepted} aceptadas", flush=True)
            return accepted, rec
        if st == 200:
            accepted += 1
            print(f"    #{i + 1} → 200 ({rec['latency_ms']} ms)", flush=True)
        else:
            print(f"    #{i + 1} → {st} — corto acá, ver el documento", flush=True)
            return accepted, rec
        time.sleep(0.5)  # no saturar; el objetivo es el límite, no un DoS
    return accepted, None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", required=True, help="backend:modelo, p.ej. zen:hy3-free")
    ap.add_argument("--cross-check", help="otro backend:modelo a probar en el instante del 429")
    ap.add_argument("--max-calls", type=int, default=40)
    ap.add_argument("--backoff", default="1,5,15,60", help="minutos entre reintentos")
    ap.add_argument("--output", required=True)
    args = ap.parse_args()

    cfg = load_config()
    backend, _, model = args.target.partition(":")
    base_url, api_key = resolve(backend, cfg)
    started = now()
    log = []

    print(f"[{started}] sonda sobre {args.target}", flush=True)
    print("  fase 1: ráfaga hasta el primer 429", flush=True)
    first_accepted, blocker = burst(base_url, api_key, model, args.max_calls, log, "burst")

    cross = None
    if blocker is not None and blocker.get("status") == 429 and args.cross_check:
        cb, _, cm = args.cross_check.partition(":")
        cbase, ckey = resolve(cb, cfg)
        print(f"  cross-check inmediato: {args.cross_check}", flush=True)
        cross = call(cbase, ckey, cm)
        cross["phase"] = "cross_check"
        cross["target"] = args.cross_check
        log.append(cross)
        print(f"    → {cross.get('status')}", flush=True)

    recoveries = []
    if blocker is not None and blocker.get("status") == 429:
        waited = 0
        for mins in [int(m) for m in args.backoff.split(",") if m.strip()]:
            delta = mins - waited
            print(f"  esperando hasta t+{mins} min…", flush=True)
            time.sleep(delta * 60)
            waited = mins
            n, blk = burst(base_url, api_key, model, args.max_calls, log, f"retry_{mins}min")
            recoveries.append(
                {
                    "after_minutes": mins,
                    "recovered": n > 0,
                    "accepted_before_next_429": n,
                    "stopped_by": blk.get("status") if blk else None,
                }
            )
            if n > 0:
                print(f"    recuperó: {n} llamadas aceptadas", flush=True)
            else:
                print("    sigue limitado", flush=True)
    else:
        print("  no hubo 429: sin fase de recuperación", flush=True)

    out = {
        "schema": "rate-limit-probe/1",
        "target": args.target,
        "cross_check": args.cross_check,
        "base_url": base_url,
        "started_at": started,
        "finished_at": now(),
        "max_calls": args.max_calls,
        "backoff_minutes": args.backoff,
        "summary": {
            "calls_until_first_429": first_accepted,
            "hit_429": bool(blocker and blocker.get("status") == 429),
            "stopped_by_status": blocker.get("status") if blocker else None,
            "cross_check_status": cross.get("status") if cross else None,
            "recoveries": recoveries,
        },
        "calls": log,
    }
    pathlib.Path(args.output).write_text(json.dumps(out, indent=1, ensure_ascii=False))
    print(f"[{out['finished_at']}] JSON en {args.output}", flush=True)


if __name__ == "__main__":
    main()
