use camino::{Utf8Path, Utf8PathBuf};

use crate::{
    ConfigError, GroveConfig,
    defaults::{
        DEFAULT_ARTIFACTS_DIR_NAME, DEFAULT_CHECKPOINTS_DIR_NAME, DEFAULT_GROVE_DIR_NAME,
        DEFAULT_LOGS_DIR_NAME, DEFAULT_PROMPTS_DIR_NAME, DEFAULT_TMP_DIR_NAME,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrovePaths {
    config_path: Utf8PathBuf,
    workspace_root: Utf8PathBuf,
    grove_dir: Utf8PathBuf,
    db_path: Utf8PathBuf,
    transcript_dir: Utf8PathBuf,
    startup_prompt_path: Utf8PathBuf,
}

impl GrovePaths {
    pub fn from_config(config: &GroveConfig, config_path: &Utf8Path) -> Result<Self, ConfigError> {
        let config_dir = config_path.parent().unwrap_or(Utf8Path::new("."));
        let workspace_root = resolve_against(config_dir, &config.runtime.workspace_root);
        if !workspace_root.exists() {
            return Err(ConfigError::WorkspaceNotFound {
                path: workspace_root.to_string(),
            });
        }

        let grove_dir = workspace_root.join(DEFAULT_GROVE_DIR_NAME);
        let db_path = resolve_against(&workspace_root, &config.memory.db_path);
        let transcript_dir = resolve_against(&workspace_root, &config.memory.transcript_dir);
        let startup_prompt_path =
            resolve_against(&workspace_root, &config.runtime.startup_prompt_path);

        Ok(Self {
            config_path: config_path.to_owned(),
            workspace_root,
            grove_dir,
            db_path,
            transcript_dir,
            startup_prompt_path,
        })
    }

    #[must_use]
    pub fn config_path(&self) -> &Utf8Path {
        &self.config_path
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Utf8Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn grove_dir(&self) -> &Utf8Path {
        &self.grove_dir
    }

    #[must_use]
    pub fn db_path(&self) -> &Utf8Path {
        &self.db_path
    }

    #[must_use]
    pub fn transcript_dir(&self) -> &Utf8Path {
        &self.transcript_dir
    }

    #[must_use]
    pub fn prompts_dir(&self) -> Utf8PathBuf {
        self.grove_dir.join(DEFAULT_PROMPTS_DIR_NAME)
    }

    #[must_use]
    pub fn startup_prompt_path(&self) -> &Utf8Path {
        &self.startup_prompt_path
    }

    #[must_use]
    pub fn checkpoints_dir(&self) -> Utf8PathBuf {
        self.grove_dir.join(DEFAULT_CHECKPOINTS_DIR_NAME)
    }

    #[must_use]
    pub fn artifacts_dir(&self) -> Utf8PathBuf {
        self.grove_dir.join(DEFAULT_ARTIFACTS_DIR_NAME)
    }

    #[must_use]
    pub fn logs_dir(&self) -> Utf8PathBuf {
        self.grove_dir.join(DEFAULT_LOGS_DIR_NAME)
    }

    #[must_use]
    pub fn tmp_dir(&self) -> Utf8PathBuf {
        self.grove_dir.join(DEFAULT_TMP_DIR_NAME)
    }

    #[must_use]
    pub fn managed_paths(&self) -> Vec<(&'static str, Utf8PathBuf)> {
        vec![
            ("memory.db_path", self.db_path.clone()),
            ("memory.transcript_dir", self.transcript_dir.clone()),
            (
                "runtime.startup_prompt_path",
                self.startup_prompt_path.clone(),
            ),
            ("prompts_dir", self.prompts_dir()),
            ("checkpoints_dir", self.checkpoints_dir()),
            ("artifacts_dir", self.artifacts_dir()),
            ("logs_dir", self.logs_dir()),
            ("tmp_dir", self.tmp_dir()),
        ]
    }

    #[must_use]
    pub fn managed_reset_paths(&self) -> Vec<(&'static str, Utf8PathBuf)> {
        let mut paths = vec![
            ("memory.db_path", self.db_path.clone()),
            (
                "memory.db_path_wal",
                Utf8PathBuf::from(format!("{}-wal", self.db_path)),
            ),
            (
                "memory.db_path_shm",
                Utf8PathBuf::from(format!("{}-shm", self.db_path)),
            ),
            (
                "memory.db_path_journal",
                Utf8PathBuf::from(format!("{}-journal", self.db_path)),
            ),
            ("memory.transcript_dir", self.transcript_dir.clone()),
            ("prompts_dir", self.prompts_dir()),
            ("checkpoints_dir", self.checkpoints_dir()),
            ("artifacts_dir", self.artifacts_dir()),
            ("logs_dir", self.logs_dir()),
            ("tmp_dir", self.tmp_dir()),
        ];
        paths.sort_by(|a, b| b.1.as_str().len().cmp(&a.1.as_str().len()));
        paths.dedup_by(|a, b| a.1 == b.1);
        paths
    }

    #[must_use]
    pub fn initialization_markers(&self) -> Vec<(&'static str, Utf8PathBuf)> {
        let mut markers = vec![("grove_dir", self.grove_dir.clone())];
        markers.extend(self.managed_reset_paths());
        markers
    }
}

fn resolve_against(base: &Utf8Path, value: &str) -> Utf8PathBuf {
    let path = Utf8PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
