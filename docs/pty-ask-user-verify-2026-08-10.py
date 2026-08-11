#!/usr/bin/env python3
"""Verificación en vivo del overlay ask_user de la TUI (backlog v8).

Técnica pty del proyecto (PLAN.md § verificación TUI): pty real +
pyte para reconstruir la pantalla, respuesta manual a ESC[6n (la TUI
consulta posición del cursor y sin respuesta no arranca), waits con
deadline, y waitpid para detectar salida (no kill(pid,0)).

Flujo: braze chat --tui contra openrouter:deepseek/deepseek-v4-flash,
prompt que induce ask_user con opciones A/B, esperar el overlay,
seleccionar con "2", esperar que el modelo cite la selección, /quit.
"""

import os
import pty
import re
import select
import signal
import sys
import time

import pyte

BRAZE = os.path.expanduser("~/proyectos/braze/target/release/braze")
COLS, ROWS = 100, 30
CSI_DSR = b"\x1b[6n"

screen = pyte.HistoryScreen(COLS, ROWS, history=2000)
stream = pyte.ByteStream(screen)

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.environ["COLUMNS"], os.environ["LINES"] = str(COLS), str(ROWS)
    os.execv(BRAZE, [BRAZE, "chat", "--tui", "--backend", "openrouter",
                     "--model", "deepseek/deepseek-v4-flash"])
    os._exit(127)

import fcntl, struct, termios
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

raw = bytearray()

def pump(timeout=0.2):
    """Lee lo disponible, alimenta pyte y responde ESC[6n si aparece."""
    r, _, _ = select.select([fd], [], [], timeout)
    if fd in r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        raw.extend(chunk)
        stream.feed(chunk)
        if CSI_DSR in chunk:
            # La TUI pregunta dónde está el cursor; contestar (fila;col).
            os.write(fd, f"\x1b[{ROWS};1R".encode())
    return True

def visible():
    return "\n".join(screen.display)

def everything():
    hist = "\n".join("".join(c.data for c in line.values())
                     for line in screen.history.top)
    return hist + "\n" + visible()

def wait_for(pattern, deadline_s, label):
    t0 = time.time()
    while time.time() - t0 < deadline_s:
        pump(0.2)
        if re.search(pattern, everything(), re.IGNORECASE):
            print(f"  ✓ {label}  ({time.time()-t0:.1f}s)")
            return True
    print(f"  ✗ TIMEOUT {label} ({deadline_s}s)")
    print("--- pantalla al fallar ---")
    print(visible())
    return False

def send(text, settle=0.3):
    os.write(fd, text.encode())
    t0 = time.time()
    while time.time() - t0 < settle:
        pump(0.1)

ok = True
print("== pty ask_user overlay ==")

# 1. La TUI arranca (banner + composer).
ok &= wait_for(r"braze", 20, "TUI arrancó (banner)")

# 2. Prompt que induce ask_user.
# Paráfrasis a propósito: las palabras de las opciones (p.ej. "azul",
# "rojo") las genera el MODELO — nada de lo que tecleamos puede
# satisfacer los asserts (lección del primer intento: el eco del
# composer daba falsos verdes a 0,1s).
send("Usa la herramienta ask_user para preguntarme que color prefiero, "
     "ofreciendo exactamente dos opciones de una sola palabra cada una: "
     "el color del cielo despejado, y el color de un tomate maduro. "
     "Cuando yo responda, repite mi eleccion en MAYUSCULAS entre "
     "corchetes.")
send("\r", settle=0.5)

# 3. El overlay rinde opciones NUMERADAS (mi prompt no contiene "1)" ni
#    "2)" — el patrón numerado solo puede venir del overlay).
# Ancla EXCLUSIVA del overlay: su línea de ayuda de teclas. El intento
# anterior usó un patrón numerado que "v0.1.0" del banner satisfacía
# (tercer falso-match del guion — cada assert necesita un texto que NADIE
# más pinta).
ok &= wait_for(r"Enter responder", 120, "overlay ask_user visible (ayuda de teclas)")
ok &= wait_for(r"1\.\s+\w+\s*\n\s*2\.\s+\w+", 5, "dos opciones numeradas del overlay")
print("--- pantalla con overlay ---")
print(visible())

# 4. Seleccionar la opción 1 Y confirmar con Enter — el overlay lo
#    dice en pantalla: "1-2 elegir · Enter responder" (el primer intento
#    quedó seleccionado sin responder por mandar el dígito solo).
send("1", settle=0.5)
send("\r", settle=1.0)

# 5. El modelo cita la elección EN MAYÚSCULAS entre corchetes — el
#    prompt tecleado va en minúsculas, así que esto es eco-seguro.
ok &= wait_for(r"\[[A-ZÁÉÍÓÚÑ]{3,}\]", 120, "el modelo cita la elección [MAYÚSCULAS]")

# 6. Salir limpio y verificar terminación real con waitpid.
send("/quit", settle=0.4)
send("\r", settle=0.5)
t0 = time.time()
exited = False
while time.time() - t0 < 25:
    pump(0.1)
    done, status = os.waitpid(pid, os.WNOHANG)
    if done == pid:
        exited = True
        print(f"  ✓ salida limpia (status={status}) via waitpid")
        break
if not exited:
    print("  ✗ el proceso no salió; enviando SIGKILL")
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    ok = False

print("\n== RESULTADO:", "PASS" if ok else "FAIL", "==")
sys.exit(0 if ok else 1)
