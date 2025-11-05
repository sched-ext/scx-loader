use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{env, fs};

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;

#[derive(Parser)]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    Install {
        #[clap(long)]
        destdir: Option<PathBuf>,
    },
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(rename = "install-files")]
    install_files: HashMap<String, InstallConfig>,
}

#[derive(Debug, Deserialize)]
struct InstallConfig {
    target: String,
    dest: String,
}

#[derive(Debug, Deserialize)]
struct Package {
    metadata: Option<Metadata>,
}

#[derive(Debug, Deserialize)]
struct CrateCargo {
    package: Package,
}

#[derive(Debug)]
struct DistConfig {
    prefix: PathBuf,
    datadir: PathBuf,
    sysconfdir: PathBuf,
    libdir: PathBuf,
}

impl DistConfig {
    fn new() -> Self {
        let prefix = env::var("VENDOR_PREFIX").unwrap_or_else(|_| "/usr".to_owned());
        let datadir = env::var("VENDOR_DATADIR").unwrap_or_else(|_| "/usr/share".to_owned());
        let sysconfdir = env::var("VENDOR_SYSCONFDIR").unwrap_or_else(|_| "/etc".to_owned());
        let libdir = env::var("VENDOR_LIBDIR").unwrap_or_else(|_| format!("{prefix}/lib"));

        Self {
            prefix: prefix.into(),
            datadir: datadir.into(),
            sysconfdir: sysconfdir.into(),
            libdir: libdir.into(),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Install { destdir } => {
            let destdir = destdir.as_deref();
            install_files(destdir)?;
        }
    }

    Ok(())
}

fn install_files(destdir: Option<&Path>) -> Result<()> {
    let workspace_path = workspace_path();
    println!("Installing config files...");

    let entries = fs::read_dir(workspace_path.join("crates"))
        .expect("Failed to read directory!")
        .flatten()
        .map(|x| x.path())
        .filter(|x| x.is_dir());

    for entry_path in entries {
        let crate_path = entry_path.join("Cargo.toml");
        if !crate_path.exists() {
            continue;
        }
        let crate_cargo = parse_metadata(&entry_path)?;
        if let Some(install_configs) = crate_cargo.package.metadata.map(|x| x.install_files) {
            install_crate_files(install_configs, destdir)?;
        }
    }
    Ok(())
}

fn install_crate_files(
    install_configs: HashMap<String, InstallConfig>,
    destdir: Option<&Path>,
) -> Result<()> {
    let workspace_path = workspace_path();

    let dist_config = DistConfig::new();

    for (name, config) in install_configs {
        let source_path = workspace_path.join(&config.target);
        let dest_path = prepare_dest_path(destdir, &config.dest, &dist_config);
        println!(
            "Installing '{name}' from '{}' to '{}'",
            source_path.display(),
            dest_path.display()
        );

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).context("Failed to create directories")?;
        }
        fs::copy(&source_path, &dest_path)?;
    }

    Ok(())
}

fn parse_metadata(crate_path: &Path) -> Result<CrateCargo> {
    let crate_cargo_path = crate_path.join("Cargo.toml");
    let file_content = fs::read_to_string(crate_cargo_path)?;
    Ok(toml::from_str(&file_content)?)
}

fn prepare_dest_path(
    destdir: Option<&Path>,
    dest_path: &str,
    defined_paths: &DistConfig,
) -> PathBuf {
    let mut dest_str =
        dest_path.replace("$prefix", defined_paths.prefix.as_path().to_str().unwrap());
    dest_str = dest_str.replace(
        "$datadir",
        defined_paths.datadir.as_path().to_str().unwrap(),
    );
    dest_str = dest_str.replace(
        "$sysconfdir",
        defined_paths.sysconfdir.as_path().to_str().unwrap(),
    );
    dest_str = dest_str.replace("$libdir", defined_paths.libdir.as_path().to_str().unwrap());

    if let Some(destdir_path) = destdir {
        destdir_path.join(dest_str.strip_prefix("/").unwrap_or(&dest_str))
    } else {
        dest_str.into()
    }
}

fn workspace_path() -> PathBuf {
    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());
    Path::new(&manifest_dir)
        .parent()
        .expect("Manifest isn't located in the project root")
        .to_path_buf()
}
