//! Lectura de scripts `bash -c`/`bash -lc` para el clasificador
//! (2026-11-27). En sesiones reales el modelo envuelve lecturas inofensivas
//! en un script — `cd /proyecto && git log -5 && grep -n x src/a.rs | head`
//! — y como el clasificador solo conocía argv planos, cada una pedía
//! confirmación (41 en dos sesiones del T430).
//!
//! [`split_simple_script`] parte un script en comandos simples si y solo si
//! pertenece a un subconjunto estricto del shell: comandos separados por
//! `&&`, `||`, `;`, `|` o saltos de línea; comillas simples y dobles; `\`
//! como escape; y redirecciones únicamente a `/dev/null` (`2>/dev/null`,
//! `>/dev/null`, `&>/dev/null`) o entre descriptores (`2>&1`). Cualquier
//! otra cosa — `$`, comillas invertidas, `(`/`)`, `{`/`}`, `<`, escritura a
//! un archivo, `&` en segundo plano, `~usuario`, `!` — devuelve `None` y el script
//! queda irreversible como antes. Este módulo no decide nada: el
//! clasificador aprueba el script solo si cada comando resultante es una
//! lectura segura por sí solo.

/// Los comandos simples del script, cada uno como argv, o `None` si el
/// script sale del subconjunto permitido. `~` y `~/…` al comienzo de una
/// palabra sin comillas se expanden con `$HOME`, como lo haría bash (los
/// modelos escriben `cd ~/proyectos/x && …` aunque ya estén ahí: 17
/// decisiones al juez en una sesión del T430, 2026-11-27); sin `$HOME`,
/// el script queda fuera.
pub(crate) fn split_simple_script(script: &str) -> Option<Vec<Vec<String>>> {
    split_simple_script_in(script, std::env::var("HOME").ok().as_deref())
}

/// [`split_simple_script`] con el `$HOME` explícito.
pub(crate) fn split_simple_script_in(
    script: &str,
    home: Option<&str>,
) -> Option<Vec<Vec<String>>> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars().peekable();

    fn end_word(word: &mut String, in_word: &mut bool, current: &mut Vec<String>) {
        if *in_word {
            current.push(std::mem::take(word));
            *in_word = false;
        }
    }
    fn end_command(current: &mut Vec<String>, commands: &mut Vec<Vec<String>>) -> Option<()> {
        if current.is_empty() {
            // `&& &&`, `| ;`, un operador al principio: no es un script simple.
            return None;
        }
        commands.push(std::mem::take(current));
        Some(())
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()? {
                            '$' | '`' => return None,
                            ch @ ('"' | '\\') => word.push(ch),
                            ch => {
                                word.push('\\');
                                word.push(ch);
                            }
                        },
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => {
                let next = chars.next()?;
                if next == '\n' {
                    continue;
                }
                in_word = true;
                word.push(next);
            }
            ' ' | '\t' => end_word(&mut word, &mut in_word, &mut current),
            '\n' | ';' => {
                end_word(&mut word, &mut in_word, &mut current);
                if !current.is_empty() {
                    end_command(&mut current, &mut commands)?;
                }
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    end_word(&mut word, &mut in_word, &mut current);
                    end_command(&mut current, &mut commands)?;
                } else if chars.peek() == Some(&'>') {
                    // `&>/dev/null`
                    chars.next();
                    end_word(&mut word, &mut in_word, &mut current);
                    if read_redirect_target(&mut chars)? != "/dev/null" {
                        return None;
                    }
                } else {
                    return None; // segundo plano
                }
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                end_word(&mut word, &mut in_word, &mut current);
                end_command(&mut current, &mut commands)?;
            }
            '>' => {
                // Un descriptor pegado al `>` (`2>`) viene en `word`.
                let fd_prefix = in_word && (word == "1" || word == "2");
                if in_word && !fd_prefix {
                    return None;
                }
                if fd_prefix {
                    word.clear();
                    in_word = false;
                }
                if chars.peek() == Some(&'>') {
                    chars.next(); // `>>` a /dev/null sigue siendo inocuo
                }
                let target = read_redirect_target(&mut chars)?;
                if target != "/dev/null" && target != "&1" && target != "&2" {
                    return None;
                }
            }
            '$' | '`' | '(' | ')' | '{' | '}' | '<' | '!' => return None,
            '~' if !in_word => {
                // Solo `~` solo o `~/`: `~usuario` y `~+` quedan fuera.
                let next = chars.peek().copied();
                if !matches!(next, None | Some('/' | ' ' | '\t' | '\n' | ';' | '|' | '&')) {
                    return None;
                }
                let home = home.filter(|h| h.starts_with('/') && *h != "/")?;
                in_word = true;
                word.push_str(home.trim_end_matches('/'));
            }
            ch => {
                in_word = true;
                word.push(ch);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut current);
    if !current.is_empty() {
        commands.push(current);
    } else if !commands.is_empty() && script.trim_end().ends_with(['|', '&']) {
        return None; // termina en un operador colgado
    }
    if commands.is_empty() { None } else { Some(commands) }
}

/// El destino de una redirección (`/dev/null`, `&1`), sin comillas ni
/// expansiones. Termina en un espacio o en un operador.
fn read_redirect_target(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    while chars.peek() == Some(&' ') {
        chars.next();
    }
    let mut target = String::new();
    while let Some(&ch) = chars.peek() {
        let ends = ch.is_whitespace()
            || matches!(ch, ';' | '|')
            // `&` abre `&1`, pero después de un destino es `&&`.
            || (ch == '&' && !target.is_empty());
        if ends {
            break;
        }
        if matches!(ch, '\'' | '"' | '$' | '`' | '(' | ')' | '<' | '>') {
            return None;
        }
        target.push(ch);
        chars.next();
    }
    (!target.is_empty()).then_some(target)
}

/// `bash -c <script>` / `sh -lc <script>` → el script. `None` para
/// cualquier otra forma (otro programa, banderas desconocidas, varios
/// argumentos después del script).
pub(crate) fn shell_script_of(command: &[String]) -> Option<&str> {
    let (program, rest) = command.split_first()?;
    let name = std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    if !matches!(name, "bash" | "sh") {
        return None;
    }
    let (script, flags) = rest.split_last()?;
    let flags_ok = !flags.is_empty()
        && flags.iter().all(|f| {
            f.len() >= 2 && f.starts_with('-') && f[1..].chars().all(|c| c == 'c' || c == 'l')
        })
        && flags.iter().any(|f| f.contains('c'));
    flags_ok.then_some(script.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_the_real_session_shapes() {
        let got = split_simple_script(
            "cd /p/synthpop && git log --oneline -5 && git status --short | head -20",
        )
        .unwrap();
        assert_eq!(
            got,
            vec![
                argv(&["cd", "/p/synthpop"]),
                argv(&["git", "log", "--oneline", "-5"]),
                argv(&["git", "status", "--short"]),
                argv(&["head", "-20"]),
            ]
        );
        let got = split_simple_script(
            "cd /p && ls && echo '---' && ls crates/*/src 2>/dev/null | head -60",
        )
        .unwrap();
        assert_eq!(got[2], argv(&["echo", "---"]));
        assert_eq!(got[3], argv(&["ls", "crates/*/src"]));
        let got = split_simple_script(r"grep -n 'enum Commands\|^    [A-Z]' crates/cli/src/main.rs").unwrap();
        assert_eq!(got[0][2], r"enum Commands\|^    [A-Z]");
    }

    #[test]
    fn allows_only_harmless_redirections() {
        assert!(split_simple_script("ls x 2>/dev/null").is_some());
        assert!(split_simple_script("ls x >/dev/null 2>&1").is_some());
        assert!(split_simple_script("ls x &>/dev/null").is_some());
        assert!(split_simple_script("cargo test 2>&1 | tail -5").is_some());
        assert_eq!(split_simple_script("echo hola > out.txt"), None);
        assert_eq!(split_simple_script("echo hola >> notes.md"), None);
        assert_eq!(split_simple_script("cat < /etc/passwd"), None);
    }

    #[test]
    fn rejects_expansions_subshells_and_background_jobs() {
        for script in [
            "echo $HOME",
            "cat \"$F\"",
            "echo `id`",
            "ls $(pwd)",
            "(cd x && ls)",
            "{ ls; }",
            "sleep 10 &",
            "cat ~root/.ssh/id_rsa",
            "ls ~+",
            "ls ! x",
            "ls &&",
            "| ls",
            "ls | ",
            "'unterminated",
        ] {
            assert_eq!(split_simple_script(script), None, "{script}");
        }
    }

    #[test]
    fn a_leading_tilde_expands_to_home() {
        assert_eq!(
            split_simple_script_in("cd ~/proyectos/x && ls ~ src", Some("/home/u")).unwrap(),
            vec![argv(&["cd", "/home/u/proyectos/x"]), argv(&["ls", "/home/u", "src"])]
        );
        assert_eq!(
            split_simple_script_in("cat '~/x'", Some("/home/u")).unwrap(),
            vec![argv(&["cat", "~/x"])],
            "a quoted tilde stays literal"
        );
        assert_eq!(split_simple_script_in("cd ~/x", None), None);
    }

    #[test]
    fn a_tilde_inside_a_word_is_literal() {
        assert_eq!(
            split_simple_script("git log HEAD~1").unwrap(),
            vec![argv(&["git", "log", "HEAD~1"])]
        );
    }

    #[test]
    fn recognizes_only_c_style_shell_invocations() {
        assert_eq!(shell_script_of(&argv(&["bash", "-lc", "ls"])), Some("ls"));
        assert_eq!(shell_script_of(&argv(&["/bin/sh", "-c", "ls"])), Some("ls"));
        assert_eq!(shell_script_of(&argv(&["bash", "script.sh"])), None);
        assert_eq!(shell_script_of(&argv(&["zsh", "-c", "ls"])), None);
        assert_eq!(shell_script_of(&argv(&["bash", "-x", "ls"])), None);
    }
}
