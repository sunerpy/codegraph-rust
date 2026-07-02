//! CodeGraph Zed extension — provides the CodeGraph MCP server as a Zed
//! context server.
//!
//! Auto-update contract: this extension NEVER pins a codegraph version. On each
//! launch it resolves the LATEST GitHub release of `sunerpy/codegraph-rust` and
//! caches the downloaded binary under a version-stamped path
//! (`./codegraph-<version>/codegraph[.exe]`). When the CLI ships a new release,
//! existing extension installs pick up the new binary on the next launch — no
//! extension re-publish required. A user who already has `codegraph` on PATH
//! (or wants a project-specific `--path`) can override everything via
//! `.zed/settings.json` `context_servers.codegraph.command`.

use zed_extension_api::{
    self as zed, settings::ContextServerSettings, Command, ContextServerId, DownloadedFileType,
    GithubReleaseOptions, Project, Result,
};

const REPO: &str = "sunerpy/codegraph-rust";
const SERVER_ID: &str = "codegraph";

struct CodeGraphExtension {
    /// In-session memoization of the resolved binary path. The on-disk lookup
    /// is version-keyed (see `resolve_binary`), so a newer release still
    /// upgrades on the next session/launch even though this is `Some`.
    cached_binary_path: Option<String>,
}

impl CodeGraphExtension {
    /// Map the current platform to a codegraph release-asset target triple.
    fn target_triple() -> Result<&'static str> {
        let (os, arch) = zed::current_platform();
        let triple = match (os, arch) {
            (zed::Os::Linux, zed::Architecture::X8664) => "x86_64-unknown-linux-musl",
            (zed::Os::Linux, zed::Architecture::Aarch64) => "aarch64-unknown-linux-musl",
            (zed::Os::Mac, zed::Architecture::X8664) => "x86_64-apple-darwin",
            (zed::Os::Mac, zed::Architecture::Aarch64) => "aarch64-apple-darwin",
            (zed::Os::Windows, zed::Architecture::X8664) => "x86_64-pc-windows-msvc",
            (zed::Os::Windows, zed::Architecture::Aarch64) => "aarch64-pc-windows-msvc",
            (os, arch) => {
                return Err(format!(
                    "unsupported platform for codegraph: {os:?}/{arch:?}"
                ))
            }
        };
        Ok(triple)
    }

    /// The binary file name for the current OS.
    fn binary_name() -> &'static str {
        match zed::current_platform().0 {
            zed::Os::Windows => "codegraph.exe",
            zed::Os::Mac | zed::Os::Linux => "codegraph",
        }
    }

    /// The version-stamped directory that holds a downloaded binary.
    fn version_dir(version: &str) -> String {
        format!("codegraph-{version}")
    }

    /// Path to the binary inside a version-stamped directory.
    fn binary_path_for(version: &str) -> String {
        format!("{}/{}", Self::version_dir(version), Self::binary_name())
    }

    /// Newest existing `./codegraph-*/codegraph[.exe]` on disk, if any. Used as
    /// an offline fallback when `latest_github_release` fails. "Newest" is by
    /// lexicographically-greatest directory name, which matches semantic order
    /// for the zero-padded-free `vX.Y.Z` tags this project uses well enough as
    /// a best-effort offline fallback.
    fn newest_cached_binary() -> Option<String> {
        let name = Self::binary_name();
        let mut candidates: Vec<String> = std::fs::read_dir(".")
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|dir| dir.starts_with("codegraph-"))
            .filter(|dir| std::path::Path::new(&format!("{dir}/{name}")).exists())
            .collect();
        candidates.sort();
        candidates.pop().map(|dir| format!("{dir}/{name}"))
    }

    /// Resolve the binary path, downloading the latest release if needed.
    fn resolve_binary(&mut self) -> Result<String> {
        let triple = Self::target_triple()?;

        // Resolve the LATEST release — never pin a version (auto-update).
        let release = match zed::latest_github_release(
            REPO,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        ) {
            Ok(release) => release,
            // Offline / API failure: fall back to the newest cached binary.
            Err(err) => {
                if let Some(path) = Self::newest_cached_binary() {
                    return Ok(path);
                }
                return Err(format!(
                    "failed to resolve latest codegraph release and no cached binary is available: {err}"
                ));
            }
        };

        let binary_path = Self::binary_path_for(&release.version);

        // Version-stamped cache hit: reuse the already-downloaded binary.
        if std::path::Path::new(&binary_path).exists() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // Find the asset for this platform and archive kind.
        let (extension, file_type) = match zed::current_platform().0 {
            zed::Os::Windows => (".zip", DownloadedFileType::Zip),
            zed::Os::Mac | zed::Os::Linux => (".tar.gz", DownloadedFileType::GzipTar),
        };
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name.contains(triple) && asset.name.ends_with(extension))
            .ok_or_else(|| {
                format!(
                    "no codegraph release asset found for target `{triple}` (expected a name containing the triple and ending in `{extension}`) in release {}",
                    release.version
                )
            })?;

        let version_dir = Self::version_dir(&release.version);
        zed::download_file(&asset.download_url, &version_dir, file_type)?;
        zed::make_file_executable(&binary_path)?;

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for CodeGraphExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn context_server_command(
        &mut self,
        _context_server_id: &ContextServerId,
        project: &Project,
    ) -> Result<Command> {
        // 1. Settings override FIRST: if the user configured a command in
        //    `.zed/settings.json` `context_servers.codegraph.command`, honor it
        //    verbatim and skip any download.
        if let Ok(settings) = ContextServerSettings::for_project(SERVER_ID, project) {
            if let Some(command) = settings.command {
                if let Some(path) = command.path {
                    if !path.is_empty() {
                        return Ok(Command {
                            command: path,
                            args: command.arguments.unwrap_or_default(),
                            env: command
                                .env
                                .map(|env| env.into_iter().collect())
                                .unwrap_or_default(),
                        });
                    }
                }
            }
        }

        // 2. Otherwise resolve (and if necessary download) the latest release.
        let binary = self.resolve_binary()?;

        Ok(Command {
            command: binary,
            args: vec!["serve".into(), "--mcp".into()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(CodeGraphExtension);
