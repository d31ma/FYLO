use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// `fylo link [--check | --undo] [--project <dir>] [--artifacts <dir>]`
/// (ADR 0019 section 2): point the Turso SDKs a project installed at FYLO's
/// builds of their native libraries, so the SDK opens a FYLO root as its
/// database.
///
/// The artifacts directory holds what `bindings/turso/build.sh` produced
/// for one Turso version and platform: `fylo-link.json`, `turso.<triple>.node`
/// and `libturso_sync_sdk_kit.<ext>`. It defaults to `$FYLO_LINK_ARTIFACTS`,
/// then `../lib/fylo-link` beside the `fylo` executable.
///
/// Placements, each a symlink to the artifact, never a package file or a
/// lockfile:
/// - `@tursodatabase/database`: `turso.<triple>.node` next to its `index.js`,
///   which the SDK's loader tries before its platform package. `npm ci` and
///   `bun install` remove it, so the project's `postinstall` should run
///   `fylo link`.
/// - `tursogo`: `libturso_sync_sdk_kit.<ext>` in the project directory, which
///   `InitLibrary` with `LoadStrategy: "system"` searches after
///   `DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`.
///
/// `--check` verifies each placement and the SDK's version; `--undo` removes
/// only the symlinks that point at FYLO's artifacts.
pub struct LinkCommand;

struct Artifacts {
    directory: PathBuf,
    turso: String,
    triple: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Link,
    Check,
    Undo,
}

/// One Turso SDK found in the project.
struct Sdk {
    name: &'static str,
    installed: String,
    wanted: String,
    install: String,
    placement: PathBuf,
    artifact: PathBuf,
    advice: Vec<String>,
}

impl LinkCommand {
    const JS_PACKAGE: &'static str = "@tursodatabase/database";
    const GO_MODULE: &'static str = "turso.tech/database/tursogo";
    const MANIFEST: &'static str = "fylo-link.json";

    /// Run `fylo link` with the arguments after `link`.
    ///
    /// # Errors
    ///
    /// Fails with the problems found: no SDK, a version mismatch (with the
    /// install command), a file in the way, or a missing link under
    /// `--check`.
    pub fn run(arguments: &[String]) -> Result<String, String> {
        let mode = match (
            arguments.iter().any(|argument| argument == "--check"),
            arguments.iter().any(|argument| argument == "--undo"),
        ) {
            (false, false) => Mode::Link,
            (true, false) => Mode::Check,
            (false, true) => Mode::Undo,
            (true, true) => return Err("fylo link takes --check or --undo, not both".into()),
        };
        let project = Self::option(arguments, "--project")
            .map_or_else(std::env::current_dir, |path| Ok(PathBuf::from(path)))
            .map_err(|error| format!("cannot resolve the project directory: {error}"))?;
        let artifacts = Artifacts::find(Self::option(arguments, "--artifacts"))?;
        let sdks = Self::detect(&project, &artifacts)?;
        if sdks.is_empty() {
            return Err(format!(
                "no Turso SDK found in {}: looked for node_modules/{} and a go.mod requiring {}",
                project.display(),
                Self::JS_PACKAGE,
                Self::GO_MODULE
            ));
        }
        let mut report = Vec::new();
        let mut problems = Vec::new();
        for sdk in &sdks {
            match mode {
                Mode::Link => sdk.link(&mut report, &mut problems),
                Mode::Check => sdk.check(&mut report, &mut problems),
                Mode::Undo => sdk.undo(&mut report, &mut problems),
            }
        }
        if problems.is_empty() {
            Ok(report.join("\n"))
        } else {
            report.extend(problems);
            Err(report.join("\n"))
        }
    }

    fn option<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1))
            .map(String::as_str)
    }

    fn detect(project: &Path, artifacts: &Artifacts) -> Result<Vec<Sdk>, String> {
        let mut sdks = Vec::new();
        let package = project.join("node_modules").join(Self::JS_PACKAGE);
        if let Some(installed) = Self::npm_version(&package.join("package.json"))? {
            let wanted = artifacts.turso.clone();
            let postinstall = Self::npm_postinstall(&project.join("package.json"));
            let mut advice = Vec::new();
            if !postinstall.contains("fylo link") {
                advice.push(
                    "  add \"postinstall\": \"fylo link\" to package.json's scripts: npm ci and bun install remove the link"
                        .to_owned(),
                );
            }
            sdks.push(Sdk {
                name: Self::JS_PACKAGE,
                install: format!(
                    "npm install --save-exact {0}@{wanted}  (or: bun add --exact {0}@{wanted})",
                    Self::JS_PACKAGE
                ),
                installed,
                wanted,
                placement: package.join(format!("turso.{}.node", artifacts.triple)),
                artifact: artifacts
                    .directory
                    .join(format!("turso.{}.node", artifacts.triple)),
                advice,
            });
        }
        if let Some(installed) = Self::go_version(&project.join("go.mod"))? {
            let library = format!("libturso_sync_sdk_kit.{}", artifacts.extension());
            let wanted = format!("v{}", artifacts.turso);
            sdks.push(Sdk {
                name: Self::GO_MODULE,
                install: format!("go get {}@{wanted}", Self::GO_MODULE),
                installed,
                wanted,
                placement: project.join(&library),
                artifact: artifacts.directory.join(&library),
                advice: vec![
                    "  call turso.InitLibrary(turso_libs.LoadTursoLibraryConfig{LoadStrategy: \"system\"}) (package turso.tech/database/tursogo) before opening, and run from the project directory (or put it on DYLD_LIBRARY_PATH / LD_LIBRARY_PATH)"
                        .to_owned(),
                ],
            });
        }
        Ok(sdks)
    }

    fn npm_version(manifest: &Path) -> Result<Option<String>, String> {
        let Ok(text) = fs::read_to_string(manifest) else {
            return Ok(None);
        };
        let package: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid JSON: {error}", manifest.display()))?;
        Ok(package["version"].as_str().map(ToOwned::to_owned))
    }

    fn npm_postinstall(manifest: &Path) -> String {
        fs::read_to_string(manifest)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|package| {
                package["scripts"]["postinstall"]
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default()
    }

    /// The `tursogo` version `go.mod` requires, direct or indirect.
    fn go_version(manifest: &Path) -> Result<Option<String>, String> {
        let Ok(text) = fs::read_to_string(manifest) else {
            return Ok(None);
        };
        for line in text.lines() {
            let mut words = line.trim().trim_start_matches("require").split_whitespace();
            if words.next() == Some(Self::GO_MODULE) {
                return words
                    .next()
                    .map(|version| Some(version.to_owned()))
                    .ok_or_else(|| {
                        format!(
                            "{} names {} without a version",
                            manifest.display(),
                            Self::GO_MODULE
                        )
                    });
            }
        }
        Ok(None)
    }
}

impl Artifacts {
    fn find(explicit: Option<&str>) -> Result<Self, String> {
        let directory = match explicit
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("FYLO_LINK_ARTIFACTS").map(PathBuf::from))
        {
            Some(directory) => directory,
            None => std::env::current_exe()
                .map_err(|error| format!("cannot locate the fylo executable: {error}"))?
                .parent()
                .map(|bin| bin.join("../lib/fylo-link"))
                .ok_or("the fylo executable has no directory")?,
        };
        let manifest_path = directory.join(LinkCommand::MANIFEST);
        let text = fs::read_to_string(&manifest_path).map_err(|error| {
            format!(
                "no FYLO-built Turso libraries at {} ({error}); pass --artifacts or set FYLO_LINK_ARTIFACTS",
                directory.display()
            )
        })?;
        let manifest: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid JSON: {error}", manifest_path.display()))?;
        let field = |name: &str| {
            manifest[name]
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{} has no \"{name}\"", manifest_path.display()))
        };
        Ok(Self {
            directory: fs::canonicalize(&directory).unwrap_or(directory),
            turso: field("turso")?,
            triple: field("triple")?,
        })
    }

    fn extension(&self) -> &'static str {
        if self.triple.starts_with("darwin") {
            "dylib"
        } else {
            "so"
        }
    }

    /// Whether `target` is one of these artifacts, or a FYLO artifact of any
    /// build: a file with a `fylo-link.json` beside it.
    fn owns(target: &Path) -> bool {
        target
            .parent()
            .is_some_and(|directory| directory.join(LinkCommand::MANIFEST).is_file())
    }
}

impl Sdk {
    fn version_problem(&self) -> Option<String> {
        (self.installed != self.wanted).then(|| {
            format!(
                "{}: version {} is installed, and this FYLO build links {}; run: {}",
                self.name, self.installed, self.wanted, self.install
            )
        })
    }

    /// The placement's current symlink target, if it is a symlink.
    fn current_target(&self) -> Option<PathBuf> {
        fs::read_link(&self.placement).ok().map(|target| {
            if target.is_absolute() {
                target
            } else {
                self.placement
                    .parent()
                    .map_or_else(|| target.clone(), |parent| parent.join(&target))
            }
        })
    }

    fn link(&self, report: &mut Vec<String>, problems: &mut Vec<String>) {
        if let Some(problem) = self.version_problem() {
            problems.push(problem);
            return;
        }
        if !self.artifact.is_file() {
            problems.push(format!(
                "{}: the FYLO build {} is missing",
                self.name,
                self.artifact.display()
            ));
            return;
        }
        match self.current_target() {
            Some(target) if target == self.artifact => {
                report.push(format!(
                    "{}: already linked ({})",
                    self.name,
                    self.placement.display()
                ));
                report.extend(self.advice.iter().cloned());
                return;
            }
            Some(target) if Artifacts::owns(&target) => {
                if let Err(error) = fs::remove_file(&self.placement) {
                    problems.push(format!(
                        "{}: cannot replace {}: {error}",
                        self.name,
                        self.placement.display()
                    ));
                    return;
                }
            }
            Some(_) => {
                problems.push(format!(
                    "{}: {} is a symlink that FYLO did not place; remove it first",
                    self.name,
                    self.placement.display()
                ));
                return;
            }
            None if fs::symlink_metadata(&self.placement).is_ok() => {
                problems.push(format!(
                    "{}: {} is a file that FYLO did not place; remove it first",
                    self.name,
                    self.placement.display()
                ));
                return;
            }
            None => {}
        }
        match Self::symlink(&self.artifact, &self.placement) {
            Ok(()) => {
                report.push(format!(
                    "{}: linked {} -> {}",
                    self.name,
                    self.placement.display(),
                    self.artifact.display()
                ));
                report.extend(self.advice.iter().cloned());
            }
            Err(error) => problems.push(format!(
                "{}: cannot link {}: {error}",
                self.name,
                self.placement.display()
            )),
        }
    }

    fn check(&self, report: &mut Vec<String>, problems: &mut Vec<String>) {
        if let Some(problem) = self.version_problem() {
            problems.push(problem);
        }
        match self.current_target() {
            Some(target) if target == self.artifact && target.is_file() => {
                report.push(format!("{}: linked, version {}", self.name, self.installed));
            }
            Some(target) => problems.push(format!(
                "{}: {} points at {}, not {}; run fylo link",
                self.name,
                self.placement.display(),
                target.display(),
                self.artifact.display()
            )),
            None => problems.push(format!(
                "{}: not linked ({} is missing); run fylo link",
                self.name,
                self.placement.display()
            )),
        }
    }

    fn undo(&self, report: &mut Vec<String>, problems: &mut Vec<String>) {
        match self.current_target() {
            Some(target) if Artifacts::owns(&target) || target == self.artifact => {
                match fs::remove_file(&self.placement) {
                    Ok(()) => report.push(format!(
                        "{}: removed {}",
                        self.name,
                        self.placement.display()
                    )),
                    Err(error) => problems.push(format!(
                        "{}: cannot remove {}: {error}",
                        self.name,
                        self.placement.display()
                    )),
                }
            }
            Some(_) | None if fs::symlink_metadata(&self.placement).is_ok() => {
                report.push(format!(
                    "{}: left {} alone (FYLO did not place it)",
                    self.name,
                    self.placement.display()
                ));
            }
            _ => report.push(format!("{}: nothing to undo", self.name)),
        }
    }

    #[cfg(unix)]
    fn symlink(artifact: &Path, placement: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(artifact, placement)
    }

    #[cfg(not(unix))]
    fn symlink(_artifact: &Path, _placement: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "fylo link supports macOS and Linux in this release",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::LinkCommand;

    struct Fixture {
        base: PathBuf,
        artifacts: PathBuf,
        project: PathBuf,
    }

    impl Fixture {
        /// FYLO artifacts for Turso 0.8.3, and a project with
        /// `@tursodatabase/database` at `js` and `tursogo` at `go`.
        fn new(js: Option<&str>, go: Option<&str>) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = std::env::temp_dir().join(format!(
                "fylo-link-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&base);
            let artifacts = base.join("artifacts");
            let project = base.join("project");
            fs::create_dir_all(&artifacts).unwrap();
            fs::create_dir_all(&project).unwrap();
            fs::write(
                artifacts.join("fylo-link.json"),
                r#"{"turso":"0.8.3","triple":"darwin-arm64"}"#,
            )
            .unwrap();
            fs::write(artifacts.join("turso.darwin-arm64.node"), "node").unwrap();
            fs::write(artifacts.join("libturso_sync_sdk_kit.dylib"), "dylib").unwrap();
            if let Some(version) = js {
                let package = project.join("node_modules/@tursodatabase/database");
                fs::create_dir_all(&package).unwrap();
                fs::write(
                    package.join("package.json"),
                    format!(r#"{{"name":"@tursodatabase/database","version":"{version}"}}"#),
                )
                .unwrap();
            }
            if let Some(version) = go {
                fs::write(
                    project.join("go.mod"),
                    format!(
                        "module app\n\nrequire (\n\tturso.tech/database/tursogo {version}\n)\n"
                    ),
                )
                .unwrap();
            }
            Self {
                base,
                artifacts,
                project,
            }
        }

        fn run(&self, flag: Option<&str>) -> Result<String, String> {
            let mut arguments: Vec<String> = flag.iter().map(ToString::to_string).collect();
            arguments.extend([
                "--project".to_owned(),
                self.project.display().to_string(),
                "--artifacts".to_owned(),
                self.artifacts.display().to_string(),
            ]);
            LinkCommand::run(&arguments)
        }

        fn node(&self) -> PathBuf {
            self.project
                .join("node_modules/@tursodatabase/database/turso.darwin-arm64.node")
        }

        fn dylib(&self) -> PathBuf {
            self.project.join("libturso_sync_sdk_kit.dylib")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn is_symlink(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }

    #[test]
    fn link_places_symlinks_check_verifies_them_and_undo_removes_only_them() {
        let fixture = Fixture::new(Some("0.8.3"), Some("v0.8.3"));
        assert!(fixture.run(Some("--check")).is_err());
        let linked = fixture.run(None).unwrap();
        assert!(linked.contains("postinstall"), "{linked}");
        assert!(linked.contains("LoadStrategy"), "{linked}");
        assert!(is_symlink(&fixture.node()) && is_symlink(&fixture.dylib()));
        assert!(fixture.run(None).unwrap().contains("already linked"));
        fixture.run(Some("--check")).unwrap();
        fixture.run(Some("--undo")).unwrap();
        assert!(!fixture.node().exists() && !fixture.dylib().exists());
        assert!(fixture.run(Some("--check")).is_err());
        assert!(
            fixture.base.join("project/go.mod").is_file(),
            "go.mod is never touched"
        );
    }

    #[test]
    fn a_version_mismatch_is_refused_with_the_install_command() {
        let fixture = Fixture::new(Some("0.8.2"), Some("v0.9.0"));
        let refused = fixture.run(None).unwrap_err();
        assert!(
            refused.contains("npm install --save-exact @tursodatabase/database@0.8.3"),
            "{refused}"
        );
        assert!(
            refused.contains("go get turso.tech/database/tursogo@v0.8.3"),
            "{refused}"
        );
        assert!(!fixture.node().exists() && !fixture.dylib().exists());
    }

    #[test]
    fn files_fylo_did_not_place_are_refused_and_left_alone() {
        let fixture = Fixture::new(Some("0.8.3"), None);
        fs::write(fixture.node(), "someone else's addon").unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("FYLO did not place"), "{refused}");
        let undone = fixture.run(Some("--undo")).unwrap();
        assert!(undone.contains("left"), "{undone}");
        assert_eq!(
            fs::read_to_string(fixture.node()).unwrap(),
            "someone else's addon"
        );
    }

    #[test]
    fn a_project_without_a_turso_sdk_is_reported() {
        let fixture = Fixture::new(None, None);
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("no Turso SDK found"), "{refused}");
    }
}
