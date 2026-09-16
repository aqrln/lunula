use std::{
    collections::HashSet,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, mpsc},
    thread,
};

use anyhow::{Context, Result, anyhow};
use argh::FromArgs;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use toml_edit::DocumentMut;
use tracing::{debug, info};

const DEFAULT_TARGETS: &[&str] = &["riscv64gc-unknown-none-elf"];

/// Generate or regenerate workspaces for the given targets
/// (regenerates all existing workspaces if no targets were provided).
#[derive(FromArgs)]
#[argh(subcommand, name = "workspace")]
pub struct WorkspaceCommand {
    /// target triple to generate a workspace for (can be repeated)
    #[argh(option)]
    target: Vec<String>,
    /// include the default targets
    #[argh(switch)]
    default: bool,
}

impl WorkspaceCommand {
    pub fn run(self) -> Result<()> {
        let targets = self.resolve_targets()?;
        if targets.is_empty() {
            anyhow::bail!(
                "no targets were provided and no previously generated workspaces were found"
            );
        }

        thread::scope(|s| {
            let (tx, rx) = mpsc::channel();

            for target in targets {
                let tx = tx.clone();
                s.spawn(move || {
                    let result = create_workspace(&target)
                        .with_context(|| anyhow!("failed to create workspace for {target}"));
                    tx.send((target, result))
                        .expect("receiver thread should not exit before workers");
                });
            }

            drop(tx);

            let mut combined_err = None;
            for (target, result) in rx {
                match result {
                    Ok(()) => info!("created {target}"),
                    Err(err) => {
                        eprintln!("failed to create {target}");
                        let first = combined_err.is_none();
                        let out = combined_err.get_or_insert(String::new());
                        if !first {
                            writeln!(out, "\n").expect("writing to a string should succeed");
                        }
                        writeln!(out, "{err:?}").expect("writing to a string should succeed");
                    }
                }
            }

            match combined_err {
                Some(err) => Err(anyhow!("{err}")),
                None => Ok(()),
            }
        })
    }

    fn resolve_targets(&self) -> Result<Vec<String>> {
        let requested_targets = self
            .default
            .then_some(DEFAULT_TARGETS)
            .into_flat_iter()
            .copied()
            .chain(self.target.iter().map(String::as_str))
            .map(ToOwned::to_owned)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        if !requested_targets.is_empty() {
            return Ok(requested_targets);
        }

        fs::read_dir(workspaces_dir()?)?.map(|entry| {
            let entry = entry?;
            if entry.metadata()?.is_dir() {
                Ok(Some(entry.file_name().into_string().map_err(|s| anyhow!("workspace directory '{s:?}' must be a valid target triple, got non utf-8 data"))?))
            } else {
                Ok(None)
            }
        }).filter_map(|result| result.transpose()).collect()
    }
}

fn project_root() -> Result<&'static Path> {
    static PROJECT_ROOT: LazyLock<Option<&Path>> =
        LazyLock::new(|| Path::new(env!("CARGO_MANIFEST_DIR")).parent());
    PROJECT_ROOT.ok_or_else(|| anyhow!("CARGO_MANIFEST_DIR can't be root"))
}

fn workspaces_dir() -> Result<&'static Path> {
    static WORKSPACES_DIR: LazyLock<Option<PathBuf>> =
        LazyLock::new(|| project_root().ok().map(|root| root.join("workspaces")));
    WORKSPACES_DIR
        .as_deref()
        .ok_or_else(|| anyhow!("CARGO_MANIFEST_DIR can't be root"))
}

fn create_workspace(target: &str) -> Result<()> {
    let project_dir = project_root()?;
    let workspace_dir = workspaces_dir()?.join(target);
    fs::create_dir_all(&workspace_dir)?;

    // `.min_depth(Some(1))` breaks .gitignore while `filter_entry` is not applied
    // to entries at depth == 0 so this needs to be filtered separately. yeah.
    let exclude_toplevel = |entry: std::result::Result<ignore::DirEntry, ignore::Error>| match entry
    {
        Ok(entry) if entry.depth() == 0 => None,
        other => Some(other),
    };

    let expected_symlinks = WalkBuilder::new(project_dir)
        .overrides(
            OverrideBuilder::new(project_dir)
                .add("!/.cargo/")?
                .add("!/.git/")?
                .add("!/.jj/")?
                .add("!/workspaces/")?
                .build()?,
        )
        .hidden(false)
        .max_depth(Some(1))
        .build()
        .filter_map(exclude_toplevel)
        .map(|entry| Ok(entry?.file_name().to_owned()))
        .collect::<Result<HashSet<_>>>()?;

    debug!(?expected_symlinks);

    let actual_symlinks = WalkBuilder::new(&workspace_dir)
        .hidden(false)
        .max_depth(Some(1))
        .filter_entry(|entry| entry.path_is_symlink())
        .build()
        .filter_map(exclude_toplevel)
        .map(|entry| Ok(entry?.file_name().to_owned()))
        .collect::<Result<HashSet<_>>>()?;

    debug!(?actual_symlinks);

    for missing_symlink in expected_symlinks.difference(&actual_symlinks) {
        std::os::unix::fs::symlink(
            project_dir.join(missing_symlink),
            workspace_dir.join(missing_symlink),
        )
        .with_context(|| format!("failed to symlink {}", missing_symlink.display()))?;
    }

    for obsolete_symlink in actual_symlinks.difference(&expected_symlinks) {
        fs::remove_file(workspace_dir.join(obsolete_symlink))
            .with_context(|| format!("failed to remove {}", obsolete_symlink.display()))?;
    }

    let mut cargo_config = fs::read_to_string(project_dir.join(".cargo").join("config.toml"))?
        .parse::<DocumentMut>()?;
    cargo_config["build"]["target"] = toml_edit::value(target);

    let workspace_cargo_config_dir = workspace_dir.join(".cargo");
    fs::create_dir_all(&workspace_cargo_config_dir)?;
    fs::write(
        workspace_cargo_config_dir.join("config.toml"),
        cargo_config.to_string(),
    )?;

    Ok(())
}
