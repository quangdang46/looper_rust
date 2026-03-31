use super::{
    ClaudeBackend, CliSessionBackend, DEFAULT_MODEL_OMIT_FLAG, StartSessionRequest,
    build_provider_cli_args, is_transient_spawn_error,
};
use camino::Utf8PathBuf;
use grove_types::RuntimeProvider;
use std::{error::Error, fs, time::Duration};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[cfg(unix)]
fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
printf '%s\n' "$@" > "$ARGS_FILE"
printf '%s' "$TEST_TOKEN" > "$ENV_FILE"
pwd > "$PWD_FILE"
printf 'stdout line\n'
printf 'stderr line\n' >&2
"#;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, script)?;
    let mut permissions = fs::metadata(&temp_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "macOS tempdir path aliases can differ between pwd output and tempfile path"]
fn cli_backend_spawns_process_with_expected_contract() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;

    let args_file = dir.path().join("args.txt");
    let env_file = dir.path().join("env.txt");
    let pwd_file = dir.path().join("pwd.txt");

    let working_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let backend = CliSessionBackend::new_for_provider(
        RuntimeProvider::Claude,
        script_path.to_string_lossy().into_owned(),
    );
    let mut session = backend.start(StartSessionRequest {
        provider: RuntimeProvider::Claude,
        model: "sonnet".to_owned(),
        prompt: "write code while you sleep".to_owned(),
        working_dir,
        timeout: Duration::from_secs(90),
        env: vec![
            (
                "ARGS_FILE".to_owned(),
                args_file.to_string_lossy().into_owned(),
            ),
            (
                "ENV_FILE".to_owned(),
                env_file.to_string_lossy().into_owned(),
            ),
            (
                "PWD_FILE".to_owned(),
                pwd_file.to_string_lossy().into_owned(),
            ),
            ("TEST_TOKEN".to_owned(), "backend-env".to_owned()),
        ],
    })?;

    let stdout_line = session
        .stdout
        .next()
        .transpose()?
        .ok_or_else(|| std::io::Error::other("missing stdout line"))?;
    let stderr_line = session
        .stderr
        .next()
        .transpose()?
        .ok_or_else(|| std::io::Error::other("missing stderr line"))?;
    let status = session.child.wait()?;

    assert!(status.success());
    assert_eq!(stdout_line, "stdout line");
    assert_eq!(stderr_line, "stderr line");
    assert_eq!(session.timeout(), Duration::from_secs(90));

    let recorded_args = fs::read_to_string(&args_file)?;
    let args: Vec<_> = recorded_args.lines().collect();
    assert_eq!(
        args,
        vec![
            "--dangerously-skip-permissions",
            "-p",
            "write code while you sleep",
            "--model",
            "sonnet"
        ]
    );
    assert_eq!(fs::read_to_string(&env_file)?, "backend-env");
    assert_eq!(
        fs::read_to_string(&pwd_file)?.trim(),
        workspace_dir.display().to_string()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_backend_omits_model_flag_when_default_sentinel() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;

    let args_file = dir.path().join("args.txt");

    let working_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let backend = CliSessionBackend::new_for_provider(
        RuntimeProvider::Claude,
        script_path.to_string_lossy().into_owned(),
    );
    let mut session = backend.start(StartSessionRequest {
        provider: RuntimeProvider::Claude,
        model: DEFAULT_MODEL_OMIT_FLAG.to_owned(),
        prompt: "write code while you sleep".to_owned(),
        working_dir,
        timeout: Duration::from_secs(90),
        env: vec![(
            "ARGS_FILE".to_owned(),
            args_file.to_string_lossy().into_owned(),
        )],
    })?;

    let _ = session.stdout.next();
    let _ = session.stderr.next();
    let status = session.child.wait()?;
    assert!(status.success());

    let recorded_args = fs::read_to_string(&args_file)?;
    let args: Vec<_> = recorded_args.lines().collect();
    assert_eq!(
        args,
        vec![
            "--dangerously-skip-permissions",
            "-p",
            "write code while you sleep"
        ]
    );
    Ok(())
}

#[test]
fn build_provider_cli_args_uses_provider_specific_start_flags() {
    let codex_args = build_provider_cli_args(
        RuntimeProvider::Codex,
        &["exec".to_owned(), "--full-auto".to_owned()],
        "gpt-5",
        "ship it",
    );
    assert_eq!(
        codex_args,
        vec!["exec", "--full-auto", "--model", "gpt-5", "ship it"]
    );

    let claude_args = build_provider_cli_args(
        RuntimeProvider::Claude,
        &["--enable-auto-mode".to_owned()],
        DEFAULT_MODEL_OMIT_FLAG,
        "ship it",
    );
    assert_eq!(claude_args, vec!["--enable-auto-mode", "-p", "ship it"]);
}

#[test]
fn cli_backend_surfaces_spawn_failures() {
    let backend =
        CliSessionBackend::new_for_provider(RuntimeProvider::Claude, "/definitely/missing/claude");
    let request = StartSessionRequest {
        provider: RuntimeProvider::Claude,
        model: "sonnet".to_owned(),
        prompt: "hello".to_owned(),
        working_dir: Utf8PathBuf::from("."),
        timeout: Duration::from_secs(30),
        env: Vec::new(),
    };

    let result = backend.start(request);
    assert!(result.is_err());
    if let Err(error) = result {
        assert!(
            error
                .to_string()
                .contains("spawn /definitely/missing/claude in .")
        );
    }
}

#[cfg(unix)]
#[test]
fn transient_spawn_error_matches_etxtbsy() {
    let error = std::io::Error::from_raw_os_error(26);
    assert!(is_transient_spawn_error(&error));

    let other = std::io::Error::from_raw_os_error(2);
    assert!(!is_transient_spawn_error(&other));
}
