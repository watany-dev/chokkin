//! Commands in shell scripts and supervisor configs anywhere in the tree
//! (#1240), and in Dockerfiles below the root (#1061).
//!
//! The walk lists the candidates and this reads them on every run, outside
//! the config-scan cache. A file at the root runs in the project's own tree,
//! so it is read like `scripts/`. A file below it (`docker/entrypoint.sh`,
//! `deploy/supervisord.conf`) may run in an image or service tree that the
//! root's import roots and manifest do not describe, so only what it says
//! about the root's dependencies is kept: the distributions its command words
//! and arguments name, and the server apps it loads. Its binaries are not
//! usages (an undeclared one is no CHK008), and its scripts, modules,
//! classes, opaque commands and installs are dropped.
//!
//! A dbt `profiles.yml` anywhere in the tree names the adapters dbt loads
//! (#1256); it adds those distributions and nothing else.
//!
//! A candidate that is neither a shell script (`.sh` / `.bash` / `.zsh` /
//! `.ksh`, or a sh-family shebang) nor a supervisor config (a `[program:x]`,
//! `[eventlistener:x]` or `[fcgi-program:x]` section) adds nothing, and so
//! does an unreadable one.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use crate::config::ChokkinConfig;
use crate::manifest::normalize_distribution_name;
use crate::resolver::{VenvIndex, build_binary_map};
use crate::sources::{is_dbt_profiles, is_dockerfile, is_requirements_dir_file};

use super::commands::{KnownBinary, SourceHits};
use super::config_scan::{ConfigScanResult, is_shell_script, shell_script_commands};
use super::config_text::origin_at;
use super::dbt_profiles;
use super::task_files::scan_dockerfile;
use super::types::{CommandTarget, RequirementsRef};

/// Sections whose `command` supervisord runs.
const SUPERVISOR_SECTIONS: [&str; 3] = ["program:", "eventlistener:", "fcgi-program:"];

/// Hits of the command files at `paths` (root-relative). `binaries` maps a
/// binary to its distribution, for the files below the root.
fn scan(root: &Path, paths: &[String], binaries: &BTreeMap<String, String>) -> SourceHits {
    let mut hits = SourceHits::default();
    let known = |name: &str| binaries.contains_key(name);
    for rel in paths {
        if is_requirements_dir_file(Path::new(rel)) {
            hits.requirement_files.push(RequirementsRef {
                path: rel.clone(),
                origin: origin_at(rel, 0, "requirements directory"),
                missing_ok: false,
            });
            continue;
        }
        let Some(contents) = read_candidate(&root.join(rel)) else {
            continue;
        };
        if is_dbt_profiles(Path::new(rel)) {
            hits.distributions
                .extend(dbt_profiles::adapter_distributions(&contents));
            continue;
        }
        if rel.contains('/') {
            let mut file_hits = SourceHits::default();
            scan_file(rel, &contents, &known, &mut file_hits);
            merge_below_root(&mut hits, file_hits, binaries);
        } else {
            scan_file(rel, &contents, &known, &mut hits);
        }
    }
    hits
}

/// The file's text, or `None` for a file without an extension that does
/// not start with a shebang: such a file (`LICENSE`, a compiled binary) is
/// not read past its first two bytes.
fn read_candidate(path: &Path) -> Option<String> {
    if path.extension().is_none() && !is_dockerfile(path) {
        let mut head = [0_u8; 2];
        std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
        if &head != b"#!" {
            return None;
        }
    }
    std::fs::read_to_string(path).ok()
}

fn scan_file(rel: &str, contents: &str, known: KnownBinary<'_>, hits: &mut SourceHits) {
    let path = Path::new(rel);
    let conf = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("conf"));
    if is_dockerfile(path) {
        scan_dockerfile(rel, contents, known, hits);
    } else if conf {
        scan_supervisor_config(rel, contents, known, hits);
    } else if is_shell_script(path, contents) {
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("script");
        for (index, command) in shell_script_commands(contents) {
            hits.push_command(&command, known, &origin_at(rel, index, label));
        }
    }
}

/// `command` of each program, event listener and `FastCGI` program, with its
/// indented continuation lines (a blank line does not end them, as in
/// supervisord's parser); supervisord runs it without a shell, and a `;`
/// that starts a line or follows whitespace starts a comment.
fn scan_supervisor_config(
    rel: &str,
    contents: &str,
    known: KnownBinary<'_>,
    hits: &mut SourceHits,
) {
    let mut program: Option<String> = None;
    let mut command: Option<(usize, String, String)> = None;
    let flush = |command: &mut Option<(usize, String, String)>, hits: &mut SourceHits| {
        if let Some((index, label, text)) = command.take() {
            hits.push_deploy_command(&text, known, &origin_at(rel, index, &label));
        }
    };
    for (index, line) in contents.lines().enumerate() {
        let trimmed = without_comment(line).trim();
        let trimmed = if trimmed.starts_with('#') {
            ""
        } else {
            trimmed
        };
        if let Some((_, _, value)) = command.as_mut()
            && (trimmed.is_empty() || line.starts_with([' ', '\t']))
        {
            value.push(' ');
            value.push_str(trimmed);
            continue;
        }
        flush(&mut command, hits);
        if let Some(section) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            let section = section.trim();
            program = SUPERVISOR_SECTIONS
                .iter()
                .any(|prefix| section.starts_with(prefix))
                .then(|| section.to_owned());
            continue;
        }
        if let Some(section) = &program
            && let Some((key, value)) = trimmed.split_once(['=', ':'])
            && key.trim() == "command"
        {
            command = Some((
                index,
                format!("supervisor {section}"),
                value.trim().to_owned(),
            ));
        }
    }
    flush(&mut command, hits);
}

/// `line` up to a `;` that starts it or follows whitespace.
fn without_comment(line: &str) -> &str {
    line.match_indices(';')
        .find(|(at, _)| line[..*at].trim().is_empty() || line[..*at].ends_with(char::is_whitespace))
        .map_or(line, |(at, _)| &line[..at])
}

/// The hits of the command files at `paths` added to the config scan's
/// result, which the cache stores without them.
pub(super) fn scan_into(
    result: &mut ConfigScanResult,
    root: &Path,
    paths: &[String],
    config: &ChokkinConfig,
) {
    let hits = scan(
        root,
        paths,
        &build_binary_map(config, &VenvIndex::default()),
    );
    result.binary_usages.extend(hits.binaries);
    result.used_distributions.extend(
        hits.distributions
            .iter()
            .map(|name| normalize_distribution_name(name)),
    );
    result.declared_distributions.extend(hits.installed);
    result.requirement_files.extend(hits.requirement_files);
    result.tool_configs.extend(hits.tool_configs);
    result.module_refs.extend(hits.module_refs);
    result.command_targets.extend(hits.command_targets);
    result.used_symbols.extend(hits.used_symbols);
    for list in [
        &mut result.used_distributions,
        &mut result.declared_distributions,
    ] {
        list.sort();
        list.dedup();
    }
}

/// A file below the root keeps the distributions its binaries and arguments
/// name and the server apps it loads (module doc).
fn merge_below_root(hits: &mut SourceHits, file: SourceHits, binaries: &BTreeMap<String, String>) {
    hits.distributions.extend(
        file.binaries
            .iter()
            .filter_map(|usage| binaries.get(&usage.binary).cloned()),
    );
    hits.distributions.extend(file.distributions);
    hits.command_targets.extend(
        file.command_targets
            .into_iter()
            .filter(|command| matches!(command.target, CommandTarget::App { .. })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ReferenceOrigin;

    fn known_map() -> BTreeMap<String, String> {
        [
            ("celery", "celery"),
            ("gunicorn", "gunicorn"),
            ("supervisord", "supervisor"),
            ("pytest", "pytest"),
        ]
        .into_iter()
        .map(|(binary, dist)| (binary.to_owned(), dist.to_owned()))
        .collect()
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    fn labels(origins: impl Iterator<Item = ReferenceOrigin>) -> Vec<String> {
        origins
            .map(|origin| format!("{}:{}", origin.file, origin.line.unwrap_or_default()))
            .collect()
    }

    /// wger starts flower and a gevent worker from extensionless scripts
    /// under `extras/docker/`, and gunicorn from its entrypoint (#1240).
    #[test]
    fn scripts_below_the_root_mark_distributions_used_without_usages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "extras/docker/celery/start-flower",
            "#!/bin/bash\nset -o errexit\ncelery --app wger --broker=\"${B}\" flower --basic_auth=a:b\n",
        );
        write(
            root,
            "extras/docker/celery/start-worker",
            "#!/bin/bash\ncelery --app wger worker \\\n    --pool=gevent \\\n    --loglevel INFO\n",
        );
        write(
            root,
            "extras/docker/entrypoint.sh",
            "python3 tools/migrate.py\npytest -q\ngunicorn wger.wsgi:application --bind 0.0.0.0:$PORT\n",
        );
        write(root, "extras/docker/LICENSE", "celery flower\n");
        write(root, "extras/docker/tool", "\x7fELF celery");
        let paths: Vec<String> = [
            "extras/docker/LICENSE",
            "extras/docker/celery/start-flower",
            "extras/docker/celery/start-worker",
            "extras/docker/entrypoint.sh",
            "extras/docker/missing.sh",
            "extras/docker/tool",
        ]
        .map(str::to_owned)
        .to_vec();
        let hits = scan(root, &paths, &known_map());
        let mut distributions = hits.distributions.clone();
        distributions.sort();
        distributions.dedup();
        assert_eq!(
            distributions,
            ["celery", "flower", "gevent", "gunicorn", "pytest"]
        );
        assert!(hits.binaries.is_empty());
        assert!(hits.module_refs.is_empty());
        let targets: Vec<_> = hits
            .command_targets
            .iter()
            .map(|command| &command.target)
            .collect();
        assert_eq!(
            targets,
            [&CommandTarget::App {
                module: "wger.wsgi".to_owned(),
                symbol: Some("application".to_owned()),
            }]
        );
    }

    /// `KiroCrew`'s `cli.sh` prints usage text through `cat <<'EOF'` and runs
    /// Python through `python - <<'PY'`; neither body is shell commands, but a
    /// body fed to `bash` is, and the line after the delimiter is again.
    #[test]
    fn heredoc_bodies_run_only_when_fed_to_a_shell() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "cli.sh",
            "usage() {\n  cat <<'EOF'\npytest is in the wheel\n  EOF\nEOF\n}\n\
             python3 - \"$A\" <<-PY\n\tcelery = 1\n\tPY\n\
             echo $(( 1 << BITS )) \"a <<EOF\"\ngunicorn app:app\n\
             bash <<EOF\npytest -q\nEOF\n",
        );
        let hits = scan(root, &["cli.sh".to_owned()], &known_map());
        let mut binaries: Vec<(String, Option<u32>)> = hits
            .binaries
            .iter()
            .map(|usage| (usage.binary.clone(), usage.origin.line))
            .collect();
        binaries.sort();
        assert_eq!(
            binaries,
            [
                ("gunicorn".to_owned(), Some(11)),
                ("pytest".to_owned(), Some(13))
            ]
        );
    }

    /// redash runs `supervisord -c worker.conf` (#1240); a root-level file
    /// is read like `scripts/`, with usages and installs.
    #[test]
    fn root_files_keep_usages_and_supervisor_programs_are_commands() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "worker.conf",
            "[supervisord]\nlogfile=/dev/null ; no log\n\n[program:worker]\ncommand=celery -A app worker\n\n  -P eventlet\t; pytest runs elsewhere\n; numprocs=2\nnumprocs=%(ENV_N)s\n\n[eventlistener:check]\ncommand = pytest --ds=app.settings ; health\n[unix_http_server]\ncommand=gunicorn app:app\n",
        );
        write(root, "nginx.conf", "server { listen 80; }\n");
        write(
            root,
            "entrypoint.sh",
            "pip install supervisor-stdout\nexec supervisord -c worker.conf\n",
        );
        let paths: Vec<String> = ["entrypoint.sh", "nginx.conf", "worker.conf"]
            .map(str::to_owned)
            .to_vec();
        let hits = scan(root, &paths, &known_map());
        let binaries: Vec<_> = hits
            .binaries
            .iter()
            .map(|usage| usage.binary.as_str())
            .collect();
        assert_eq!(binaries, ["supervisord", "celery", "pytest"]);
        assert_eq!(
            labels(hits.binaries.iter().map(|usage| usage.origin.clone())),
            ["entrypoint.sh:2", "worker.conf:5", "worker.conf:12"]
        );
        assert_eq!(hits.binaries[1].origin.label, "supervisor program:worker");
        assert_eq!(hits.distributions, ["eventlet"]);
        let modules: Vec<_> = hits.module_refs.iter().map(|m| m.module.as_str()).collect();
        assert_eq!(modules, ["app.settings"]);
        assert_eq!(hits.installed, ["supervisor-stdout"]);
        assert!(
            hits.command_targets
                .iter()
                .all(|command| command.deploy || command.origin.file == "entrypoint.sh")
        );
    }

    /// A Dockerfile below the root keeps its server apps (#1061) and, like
    /// a script there, the distributions its commands name.
    #[test]
    fn nested_dockerfiles_keep_apps_and_distributions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "svc/Dockerfile",
            "FROM python\nRUN pip install -r requirements.txt && pytest\n\
             CMD [\"python\", \"run.py\"]\nCMD [\"gunicorn\", \"-k\", \"gevent\", \"app.main:app\"]\n",
        );
        let paths = ["svc/Dockerfile".to_owned(), "gone/Dockerfile".to_owned()];
        let hits = scan(root, &paths, &known_map());
        assert_eq!(hits.distributions, ["pytest", "gunicorn", "gevent"]);
        assert!(hits.binaries.is_empty() && hits.installed.is_empty());
        let found: Vec<_> = hits
            .command_targets
            .iter()
            .map(|command| {
                format!(
                    "{:?} {}:{:?} {}",
                    command.target, command.origin.file, command.origin.line, command.origin.label
                )
            })
            .collect();
        assert_eq!(
            found,
            [r#"App { module: "app.main", symbol: Some("app") } svc/Dockerfile:Some(4) CMD"#]
        );
    }
}
