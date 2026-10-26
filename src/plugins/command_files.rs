//! Commands in shell scripts and supervisor configs anywhere in the tree
//! (#1240), and the server apps of Dockerfiles below the root (#1061).
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
//! A candidate that is neither a shell script (`.sh` / `.bash`, or a
//! sh-family shebang) nor a supervisor config (a `[program:x]`,
//! `[eventlistener:x]` or `[fcgi-program:x]` section) adds nothing, and so
//! does an unreadable one.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use crate::config::ChokkinConfig;
use crate::manifest::normalize_distribution_name;
use crate::resolver::{VenvIndex, build_binary_map};
use crate::sources::is_dockerfile;

use super::commands::{KnownBinary, SourceHits};
use super::config_scan::{ConfigScanResult, is_shell_script, strip_shell_comment};
use super::config_text::{logical_lines, origin_at};
use super::task_files::nested_dockerfile_apps;
use super::types::CommandTarget;

/// Sections whose `command` supervisord runs.
const SUPERVISOR_SECTIONS: [&str; 3] = ["program:", "eventlistener:", "fcgi-program:"];

/// Hits of the command files at `paths` (root-relative). `binaries` maps a
/// binary to its distribution, for the files below the root.
fn scan(root: &Path, paths: &[String], binaries: &BTreeMap<String, String>) -> SourceHits {
    let dockerfiles: Vec<String> = paths
        .iter()
        .filter(|rel| is_dockerfile(Path::new(rel)))
        .cloned()
        .collect();
    let mut hits = SourceHits::default();
    hits.command_targets
        .extend(nested_dockerfile_apps(root, &dockerfiles));
    let known = |name: &str| binaries.contains_key(name);
    for rel in paths.iter().filter(|rel| !is_dockerfile(Path::new(rel))) {
        let Some(contents) = read_candidate(&root.join(rel)) else {
            continue;
        };
        let at_root = !rel.contains('/');
        let mut file_hits = SourceHits::default();
        scan_file(rel, &contents, &known, &mut file_hits);
        if at_root {
            merge(&mut hits, file_hits);
        } else {
            merge_below_root(&mut hits, file_hits, binaries);
        }
    }
    hits
}

/// The file's text, or `None` for a file without an extension that does
/// not start with a shebang: such a file (`LICENSE`, a compiled binary) is
/// not read past its first two bytes.
fn read_candidate(path: &Path) -> Option<String> {
    if path.extension().is_none() {
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
    if conf {
        scan_supervisor_config(rel, contents, known, hits);
    } else if is_shell_script(path, contents) {
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("script");
        for (index, line) in logical_lines(contents) {
            hits.push_command(
                strip_shell_comment(&line),
                known,
                &origin_at(rel, index, label),
            );
        }
    }
}

/// `command` of each program, event listener and `FastCGI` program, with its
/// indented continuation lines; supervisord runs it without a shell, and a
/// `;` after whitespace starts a comment.
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
        let text = line.find(" ;").map_or(line, |end| &line[..end]);
        let trimmed = text.trim();
        if let Some((_, _, value)) = command.as_mut()
            && line.starts_with([' ', '\t'])
            && !trimmed.is_empty()
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

fn merge(hits: &mut SourceHits, file: SourceHits) {
    hits.binaries.extend(file.binaries);
    hits.distributions.extend(file.distributions);
    hits.module_refs.extend(file.module_refs);
    hits.command_targets.extend(file.command_targets);
    hits.installed.extend(file.installed);
    hits.used_symbols.extend(file.used_symbols);
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

    /// redash runs `supervisord -c worker.conf` (#1240); a root-level file
    /// is read like `scripts/`, with usages and installs.
    #[test]
    fn root_files_keep_usages_and_supervisor_programs_are_commands() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "worker.conf",
            "[supervisord]\nlogfile=/dev/null ; no log\n\n[program:worker]\ncommand=celery -A app worker\n  -P eventlet\nnumprocs=%(ENV_N)s\n\n[eventlistener:check]\ncommand = pytest --ds=app.settings ; health\n[unix_http_server]\ncommand=gunicorn app:app\n",
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
            ["entrypoint.sh:2", "worker.conf:5", "worker.conf:10"]
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
}
