#!/usr/bin/env python3
"""Inspector liviano para los logs de sesión de braze (rollout JSONL).

braze_session::SessionStore expone `list_sessions()`, pero ningún
subcomando de `braze-cli` lo conecta (no existe `braze sessions ...`) —
este script llena ese hueco desde afuera, sin tocar el binario Rust, para
poder revisar sesiones durante una prueba de usabilidad o al debuggear
un comportamiento inesperado.

Uso:
    tools/braze_sessions.py list [--session-dir PATH]
    tools/braze_sessions.py show <session_id_o_ruta> [--session-dir PATH] [--full]

Por defecto usa la misma resolución que
`braze_config::paths::default_session_dir`: $XDG_DATA_HOME/braze/sessions,
si no $HOME/.local/share/braze/sessions, si no el directorio temporal del
sistema.
"""

import argparse
import json
import os
import sys
import tempfile
from datetime import datetime
from pathlib import Path

TRUNCATE_CHARS = 500


def default_session_dir() -> Path:
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:
        return Path(xdg) / "braze" / "sessions"
    home = os.environ.get("HOME")
    if home:
        return Path(home) / ".local" / "share" / "braze" / "sessions"
    return Path(tempfile.gettempdir()) / "braze-sessions"


def load_events(path: Path) -> list[dict]:
    events = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                events.append({"type": "_unparsable", "raw": line})
    return events


def first_user_message(events: list[dict]) -> str:
    for event in events:
        if event.get("type") == "user_message":
            return event.get("text", "")
    return ""


def cmd_list(args: argparse.Namespace) -> int:
    session_dir = Path(args.session_dir) if args.session_dir else default_session_dir()
    if not session_dir.exists():
        print(f"no existe el directorio de sesiones: {session_dir}", file=sys.stderr)
        return 1

    files = sorted(session_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        print(f"sin sesiones en {session_dir}")
        return 0

    print(f"{'session':<38} {'modificado':<20} {'eventos':>7}  primer mensaje")
    for path in files:
        events = load_events(path)
        mtime = datetime.fromtimestamp(path.stat().st_mtime).strftime("%Y-%m-%d %H:%M:%S")
        msg = first_user_message(events)
        msg = (msg[:60] + "…") if len(msg) > 60 else msg
        print(f"{path.stem:<38} {mtime:<20} {len(events):>7}  {msg}")
    return 0


def truncate(value, full: bool) -> str:
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    if full or len(text) <= TRUNCATE_CHARS:
        return text
    omitted = len(text) - TRUNCATE_CHARS
    return f"{text[:TRUNCATE_CHARS]}… [{omitted} chars más, usa --full]"


def render_event(event: dict, full: bool) -> str:
    kind = event.get("type")

    if kind == "user_message":
        return f"USER: {event['text']}"
    if kind == "assistant_text":
        return f"ASSISTANT: {event['text']}"
    if kind == "assistant_tool_call":
        args_repr = json.dumps(event.get("arguments", {}), ensure_ascii=False)
        return f"TOOL_CALL  {event['name']}({truncate(args_repr, full)})  [id={event['id']}]"
    if kind == "tool_call_started":
        mode = "background" if event.get("background") else "foreground"
        return f"  … iniciado ({mode})"
    if kind == "tool_call_completed":
        result = event.get("result", {})
        status = "ERROR" if result.get("is_error") else "ok"
        return f"  -> RESULT [{status}]: {truncate(result.get('content', ''), full)}"
    if kind == "usage":
        stop = event.get("stop_reason") or "?"
        return f"  (usage: in={event.get('input_tokens')} out={event.get('output_tokens')} stop={stop})"
    if kind == "plan_created":
        return f"PLAN: {truncate(event.get('plan', ''), full)}"
    if kind == "compaction_occurred":
        return f"[compactación: ~{event.get('dropped_tokens_estimate')} tokens plegados en el resumen]"
    if kind == "permission_requested":
        reversibility = "reversible" if event.get("reversible") else "IRREVERSIBLE"
        return f"PERMISO solicitado ({reversibility}): {event.get('action')}"
    if kind == "permission_decided":
        decision = "permitido" if event.get("allowed") else "denegado"
        return f"PERMISO {decision}: {event.get('action')}"
    if kind == "_unparsable":
        return f"[línea no parseable]: {event.get('raw')}"
    return f"[{kind or '?'}] {json.dumps(event, ensure_ascii=False)}"


def resolve_session_path(session_dir: Path, ident: str) -> Path:
    candidate = Path(ident)
    if candidate.exists():
        return candidate
    return session_dir / f"{ident}.jsonl"


def cmd_show(args: argparse.Namespace) -> int:
    session_dir = Path(args.session_dir) if args.session_dir else default_session_dir()
    path = resolve_session_path(session_dir, args.session)
    if not path.exists():
        print(f"no se encontró la sesión: {path}", file=sys.stderr)
        return 1

    events = load_events(path)
    print(f"=== {path.stem} ({len(events)} eventos) ===\n")
    for event in events:
        print(render_event(event, args.full))
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_list = sub.add_parser("list", help="lista las sesiones persistidas, más reciente primero")
    p_list.add_argument("--session-dir", help="override del directorio de sesiones")
    p_list.set_defaults(func=cmd_list)

    p_show = sub.add_parser("show", help="imprime el transcript legible de una sesión")
    p_show.add_argument("session", help="session id (uuid) o ruta directa al .jsonl")
    p_show.add_argument("--session-dir", help="override del directorio de sesiones")
    p_show.add_argument("--full", action="store_true", help="no truncar resultados largos de tools")
    p_show.set_defaults(func=cmd_show)

    args = parser.parse_args()
    sys.exit(args.func(args) or 0)


if __name__ == "__main__":
    main()
