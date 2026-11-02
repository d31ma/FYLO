use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// `fylo link [--check | --undo] [--method <auto|symlink|hardlink|copy>]
/// [--project <dir>] [--artifacts <dir>]` (ADR 0019 section 2): point the
/// Turso SDKs a project installed at FYLO's builds of their native libraries,
/// so the SDK opens a FYLO root as its database.
///
/// The artifacts directory holds what `bindings/turso/build.sh` produced
/// for one Turso version and platform: `fylo-link.json`, `turso.<triple>.node`
/// and the SDK-kit library (`libturso_sync_sdk_kit.dylib` or `.so`, or
/// `turso_sync_sdk_kit.dll`). It defaults to `$FYLO_LINK_ARTIFACTS`, then
/// `../lib/fylo-link` beside the `fylo` executable.
///
/// Placements, never a package file or a lockfile:
/// - `@tursodatabase/database`: `turso.<triple>.node` next to its `index.js`,
///   which the SDK's loader tries before its platform package. `npm ci` and
///   `bun install` remove it, so the project's `postinstall` should run
///   `fylo link`.
/// - `tursogo`: the SDK-kit library in the project directory, which
///   `InitLibrary` with `LoadStrategy: "system"` searches after
///   `DYLD_LIBRARY_PATH`, `LD_LIBRARY_PATH` or `PATH`.
/// - `pyturso` (in the project's `.venv` or `venv`, else `$VIRTUAL_ENV`): a
///   `fylo_turso.pth` import hook in its `site-packages`, which loads FYLO's
///   `_turso.abi3.so` for `turso._turso`. A reinstall of pyturso keeps it.
/// - `tech.turso:turso` (named in `build.gradle(.kts)` or `pom.xml`): the JNI
///   library in the project directory, which the driver loads from
///   `java.library.path` before its own copy.
/// - .NET (`Turso.Data.Sqlite.Provider`, `Turso.Data.Common` or
///   `Turso.EntityFrameworkCore.Sqlite` in a `.csproj`): the SDK-kit library
///   in the project directory as `turso_sdk_kit`, which the app loads with
///   `NativeLibrary.SetDllImportResolver` (`fylo link` prints the line).
///
/// Each placement is a symlink when the platform allows one. Without the
/// right to create symlinks (Windows outside Developer Mode) `--method auto`
/// falls back to a hard link (same volume), then to a copy checked by
/// SHA-256. A hard link or a copy has a `<file>.fylo-link` record beside it
/// naming its artifact and hash, which is how `--check` verifies it and how
/// `--undo` knows it placed it.
///
/// `fylo-link.json` pins each artifact's SHA-256. `fylo link` and `--check`
/// refuse an artifact that no longer matches it, and on Unix an artifacts
/// directory, manifest or artifact other users can write: whoever can write
/// there decides what code every linked app loads.
///
/// `--check` verifies each placement, its hash, and the SDK's version;
/// `--undo` removes only what `fylo link` placed and nobody changed since.
/// A library a running process loaded cannot be replaced or removed on
/// Windows until that process exits.
pub struct LinkCommand;

struct Artifacts {
    directory: PathBuf,
    turso: String,
    triple: String,
    /// The SHA-256 of each artifact, by file name, as the build recorded it.
    sha256: serde_json::Map<String, Value>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Link,
    Check,
    Undo,
}

/// How a placement is made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
    Symlink,
    HardLink,
    Copy,
}

/// The file operations a link uses; tests replace them to refuse symlinks
/// or hard links as Windows does.
#[derive(Clone, Copy)]
struct Placer {
    methods: &'static [Method],
    symlink: fn(&Path, &Path) -> io::Result<()>,
    hard_link: fn(&Path, &Path) -> io::Result<()>,
}

/// What `fylo link` wrote beside a hard link or a copy.
struct Record {
    artifact: PathBuf,
    sha256: String,
    method: Method,
}

/// What is at a placement now.
enum Current {
    Missing,
    /// FYLO's import hook; `true` when it names this artifact.
    Hook(bool),
    OwnedSymlink(PathBuf),
    ForeignSymlink(PathBuf),
    OwnedFile(Record),
    ForeignFile,
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
    /// The text of an import-hook placement, written rather than linked.
    hook: Option<String>,
}

impl LinkCommand {
    const JS_PACKAGE: &'static str = "@tursodatabase/database";
    const GO_MODULE: &'static str = "turso.tech/database/tursogo";
    const PYTHON_DISTRIBUTION: &'static str = "pyturso";
    const PYTHON_HOOK: &'static str = "fylo_turso.pth";
    const PYTHON_HOOK_MARKER: &'static str = "# fylo link (ADR 0019)";
    const JAVA_PACKAGE: &'static str = "tech.turso:turso";
    const DOTNET_PACKAGES: [&'static str; 3] = [
        "Turso.Data.Sqlite.Provider",
        "Turso.Data.Common",
        "Turso.EntityFrameworkCore.Sqlite",
    ];
    const MANIFEST: &'static str = "fylo-link.json";
    const RECORD_SUFFIX: &'static str = "fylo-link";

    /// Run `fylo link` with the arguments after `link`.
    ///
    /// # Errors
    ///
    /// Fails with the problems found: no SDK, a version mismatch (with the
    /// install command), a file in the way, or a missing or changed link
    /// under `--check`.
    pub fn run(arguments: &[String]) -> Result<String, String> {
        let methods = Self::methods(Self::option(arguments, "--method").unwrap_or("auto"))?;
        Self::run_with(
            arguments,
            Placer {
                methods,
                symlink: Placer::system_symlink,
                hard_link: |artifact, placement| fs::hard_link(artifact, placement),
            },
        )
    }

    fn methods(name: &str) -> Result<&'static [Method], String> {
        match name {
            "auto" => Ok(&[Method::Symlink, Method::HardLink, Method::Copy]),
            "symlink" => Ok(&[Method::Symlink]),
            "hardlink" => Ok(&[Method::HardLink]),
            "copy" => Ok(&[Method::Copy]),
            other => Err(format!(
                "fylo link --method takes auto, symlink, hardlink or copy, not {other}"
            )),
        }
    }

    fn run_with(arguments: &[String], placer: Placer) -> Result<String, String> {
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
            if mode != Mode::Undo
                && let Some(problem) = artifacts.problem(&sdk.artifact)
            {
                problems.push(format!("{}: {problem}", sdk.name));
                continue;
            }
            match mode {
                Mode::Link => sdk.link(placer, &mut report, &mut problems),
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
        let package = Self::JS_PACKAGE
            .split('/')
            .fold(project.join("node_modules"), |path, part| path.join(part));
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
                hook: None,
            });
        }
        if let Some(installed) = Self::go_version(&project.join("go.mod"))? {
            let library = artifacts.go_library();
            let wanted = format!("v{}", artifacts.turso);
            sdks.push(Sdk {
                name: Self::GO_MODULE,
                install: format!("go get {}@{wanted}", Self::GO_MODULE),
                installed,
                wanted,
                placement: project.join(library),
                artifact: artifacts.directory.join(library),
                advice: vec![
                    "  call turso.InitLibrary(turso_libs.LoadTursoLibraryConfig{LoadStrategy: \"system\"}) (package turso.tech/database/tursogo) before opening".to_owned(),
                    artifacts.search_advice(project),
                ],
                hook: None,
            });
        }
        sdks.extend(Self::detect_python(project, artifacts)?);
        sdks.extend(Self::detect_java(project, artifacts)?);
        sdks.extend(Self::detect_dotnet(project, artifacts)?);
        Ok(sdks)
    }

    /// pyturso in the project's virtual environment: the `.pth` hook that
    /// maps `turso._turso` to FYLO's extension module.
    fn detect_python(project: &Path, artifacts: &Artifacts) -> Result<Option<Sdk>, String> {
        let Some((site_packages, installed)) = Self::python_install(project) else {
            return Ok(None);
        };
        let wanted = artifacts.turso.clone();
        let artifact = artifacts.directory.join(Artifacts::PYTHON_EXTENSION);
        let literal = serde_json::to_string(&artifact.display().to_string())
            .map_err(|error| format!("cannot quote {}: {error}", artifact.display()))?;
        Ok(Some(Sdk {
            name: Self::PYTHON_DISTRIBUTION,
            install: format!(
                "pip install {0}=={wanted}  (or: uv pip install {0}=={wanted})",
                Self::PYTHON_DISTRIBUTION
            ),
            installed,
            wanted,
            placement: site_packages.join(Self::PYTHON_HOOK),
            hook: Some(format!(
                "{}: turso._turso loads FYLO's build of it.\nimport sys; sys.meta_path.insert(0, type(\"FyloTurso\", (), {{\"find_spec\": staticmethod(lambda name, path=None, target=None: __import__(\"importlib.util\").util.spec_from_file_location(name, {literal}) if name == \"turso._turso\" else None)}})())\n",
                Self::PYTHON_HOOK_MARKER
            )),
            artifact,
            advice: vec![
                "  every interpreter of this environment loads FYLO's turso._turso; fylo link --undo removes the hook".to_owned(),
            ],
        }))
    }

    /// The first virtual environment with pyturso installed: its
    /// `site-packages` and pyturso's version.
    fn python_install(project: &Path) -> Option<(PathBuf, String)> {
        let environments = [
            Some(project.join(".venv")),
            Some(project.join("venv")),
            std::env::var_os("VIRTUAL_ENV").map(PathBuf::from),
        ];
        environments
            .into_iter()
            .flatten()
            .flat_map(|environment| Self::site_packages(&environment))
            .find_map(|site_packages| {
                Self::python_version(&site_packages).map(|version| (site_packages, version))
            })
    }

    /// `Lib/site-packages` (Windows) and `lib/python3.*/site-packages`.
    fn site_packages(environment: &Path) -> Vec<PathBuf> {
        let mut found = vec![environment.join("Lib").join("site-packages")];
        if let Ok(entries) = fs::read_dir(environment.join("lib")) {
            found.extend(
                entries
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with("python"))
                    .map(|entry| entry.path().join("site-packages")),
            );
        }
        found.into_iter().filter(|path| path.is_dir()).collect()
    }

    /// pyturso's version from its `pyturso-<version>.dist-info` directory.
    fn python_version(site_packages: &Path) -> Option<String> {
        let prefix = format!("{}-", Self::PYTHON_DISTRIBUTION);
        fs::read_dir(site_packages)
            .ok()?
            .flatten()
            .find_map(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(".dist-info"))
                    .map(ToOwned::to_owned)
            })
    }

    /// `tech.turso:turso` named by the Gradle or Maven build: the JNI library
    /// in the project directory, for `java.library.path`.
    fn detect_java(project: &Path, artifacts: &Artifacts) -> Result<Option<Sdk>, String> {
        let Some(installed) = Self::java_version(project)? else {
            return Ok(None);
        };
        let library = artifacts.java_library();
        let directory = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
        Ok(Some(Sdk {
            name: Self::JAVA_PACKAGE,
            install: format!(
                "depend on {}:{} in build.gradle(.kts) or pom.xml",
                Self::JAVA_PACKAGE,
                artifacts.turso
            ),
            installed,
            wanted: artifacts.turso.clone(),
            placement: project.join(library),
            artifact: artifacts.directory.join(library),
            advice: vec![format!(
                "  run java with -Djava.library.path={}: the driver loads _turso_java from java.library.path before its own copy, and falls back to stock Turso without it",
                directory.display()
            )],
            hook: None,
        }))
    }

    /// The `tech.turso:turso` version in `build.gradle.kts`, `build.gradle`
    /// or `pom.xml`.
    fn java_version(project: &Path) -> Result<Option<String>, String> {
        for file in ["build.gradle.kts", "build.gradle"] {
            let Ok(text) = fs::read_to_string(project.join(file)) else {
                continue;
            };
            let coordinate = format!("{}:", Self::JAVA_PACKAGE);
            if let Some(start) = text.find(&coordinate) {
                let version: String = text[start + coordinate.len()..]
                    .chars()
                    .take_while(|character| !matches!(character, '"' | '\'' | ')' | ' ' | '\n'))
                    .collect();
                return Self::named_version(&project.join(file), version);
            }
        }
        let Ok(pom) = fs::read_to_string(project.join("pom.xml")) else {
            return Ok(None);
        };
        let dependency = pom.split("<dependency>").find(|block| {
            block.contains("<groupId>tech.turso</groupId>")
                && block.contains("<artifactId>turso</artifactId>")
        });
        let Some(dependency) = dependency else {
            return Ok(None);
        };
        let version = Self::between(dependency, "<version>", "</version>").unwrap_or_default();
        Self::named_version(&project.join("pom.xml"), version.trim().to_owned())
    }

    fn named_version(manifest: &Path, version: String) -> Result<Option<String>, String> {
        if version.is_empty() {
            return Err(format!(
                "{} names {} without a version",
                manifest.display(),
                Self::JAVA_PACKAGE
            ));
        }
        Ok(Some(version))
    }

    fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
        let rest = &text[text.find(start)? + start.len()..];
        Some(&rest[..rest.find(end)?])
    }

    /// A Turso .NET package in a `.csproj` in the project directory: the
    /// SDK-kit library as `turso_sdk_kit` there, for the app's resolver.
    fn detect_dotnet(project: &Path, artifacts: &Artifacts) -> Result<Option<Sdk>, String> {
        let Some((package, installed)) = Self::dotnet_package(project) else {
            return Ok(None);
        };
        let placement = project.join(artifacts.dotnet_library());
        let absolute = std::path::absolute(&placement).unwrap_or_else(|_| placement.clone());
        let literal = serde_json::to_string(&absolute.display().to_string())
            .map_err(|error| format!("cannot quote {}: {error}", absolute.display()))?;
        Ok(Some(Sdk {
            name: package,
            install: format!("dotnet add package {package} --version {}", artifacts.turso),
            installed,
            wanted: artifacts.turso.clone(),
            artifact: artifacts.directory.join(artifacts.go_library()),
            placement,
            advice: vec![format!(
                "  before the first Turso call: NativeLibrary.SetDllImportResolver(Assembly.Load(\"Turso.Raw\"), (name, _, _) => name == \"turso_sdk_kit\" ? NativeLibrary.Load({literal}) : IntPtr.Zero);  (System.Reflection, System.Runtime.InteropServices)"
            )],
            hook: None,
        }))
    }

    /// The first Turso package a `.csproj` in the project references, and
    /// its version.
    fn dotnet_package(project: &Path) -> Option<(&'static str, String)> {
        let projects = fs::read_dir(project).ok()?.flatten().filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "csproj")
        });
        projects
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .find_map(|text| {
                Self::DOTNET_PACKAGES.into_iter().find_map(|package| {
                    let element = Self::between(&text, &format!("Include=\"{package}\""), ">")?;
                    let version = Self::between(element, "Version=\"", "\"")?;
                    Some((package, version.to_owned()))
                })
            })
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

    fn sha256(path: &Path) -> io::Result<String> {
        let digest = Sha256::digest(fs::read(path)?);
        Ok(digest
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }))
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
        if Self::writable_by_others(&directory) || Self::writable_by_others(&manifest_path) {
            return Err(format!(
                "{} or its {} is writable by other users; fylo link refuses to link from it",
                directory.display(),
                LinkCommand::MANIFEST
            ));
        }
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
            sha256: manifest["sha256"].as_object().cloned().unwrap_or_default(),
        })
    }

    /// Why `artifact` must not be linked: other users can write it, the
    /// manifest pins no hash for it, or it no longer has that hash. A missing
    /// artifact is reported where it is used.
    fn problem(&self, artifact: &Path) -> Option<String> {
        let name = artifact.file_name()?.to_string_lossy().into_owned();
        if Self::writable_by_others(artifact) {
            return Some(format!(
                "{} is writable by other users; fylo link refuses to link it",
                artifact.display()
            ));
        }
        let Some(pinned) = self.sha256.get(&name).and_then(Value::as_str) else {
            return Some(format!(
                "{} pins no SHA-256 for {name}; rebuild the artifacts",
                LinkCommand::MANIFEST
            ));
        };
        match LinkCommand::sha256(artifact) {
            Ok(actual) if actual != pinned => Some(format!(
                "{} does not match the SHA-256 {} pins: it changed after the build",
                artifact.display(),
                LinkCommand::MANIFEST
            )),
            _ => None,
        }
    }

    #[cfg(unix)]
    fn writable_by_others(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o002 != 0)
    }

    #[cfg(not(unix))]
    fn writable_by_others(_path: &Path) -> bool {
        false
    }

    const PYTHON_EXTENSION: &'static str = "_turso.abi3.so";

    /// The JNI library `System.loadLibrary("_turso_java")` looks for.
    fn java_library(&self) -> &'static str {
        if self.triple.starts_with("win32") {
            "_turso_java.dll"
        } else if self.triple.starts_with("darwin") {
            "lib_turso_java.dylib"
        } else {
            "lib_turso_java.so"
        }
    }

    /// The name .NET's `DllImport("turso_sdk_kit")` loads; the library is the
    /// SDK kit tursogo uses, as Turso's own .NET packages ship it.
    fn dotnet_library(&self) -> &'static str {
        if self.triple.starts_with("win32") {
            "turso_sdk_kit.dll"
        } else if self.triple.starts_with("darwin") {
            "libturso_sdk_kit.dylib"
        } else {
            "libturso_sdk_kit.so"
        }
    }

    /// The SDK-kit library tursogo loads on this artifact's platform.
    fn go_library(&self) -> &'static str {
        if self.triple.starts_with("win32") {
            "turso_sync_sdk_kit.dll"
        } else if self.triple.starts_with("darwin") {
            "libturso_sync_sdk_kit.dylib"
        } else {
            "libturso_sync_sdk_kit.so"
        }
    }

    /// Where tursogo's "system" search finds the library. On Linux it must be
    /// an absolute directory first on `LD_LIBRARY_PATH`: tursogo joins each
    /// entry with the file name and hands the result to `dlopen`, so with the
    /// variable unset (or `.`) it finds the library in the working directory
    /// but passes a bare name, which Linux does not look up there. tursogo
    /// reads the variable itself when `InitLibrary` runs, so the app may also
    /// set it with `os.Setenv` first.
    fn search_advice(&self, project: &Path) -> String {
        if self.triple.starts_with("linux") {
            let directory = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
            format!(
                "  and call os.Setenv(\"LD_LIBRARY_PATH\", {0:?}) before InitLibrary, or run with LD_LIBRARY_PATH={0} first (export LD_LIBRARY_PATH={0}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}): on Linux tursogo finds the library only through an absolute LD_LIBRARY_PATH entry. Prefer os.Setenv: the dynamic loader reads an exported LD_LIBRARY_PATH for every library the process loads, so a file in the project directory could stand in for a system library",
                directory.display()
            )
        } else if self.triple.starts_with("win32") {
            "  and run from the project directory (or put it on PATH)".to_owned()
        } else {
            "  and run from the project directory (or put it on DYLD_LIBRARY_PATH)".to_owned()
        }
    }

    /// Whether `target` is a FYLO artifact of any build: a file with a
    /// `fylo-link.json` beside it.
    fn owns(target: &Path) -> bool {
        target
            .parent()
            .is_some_and(|directory| directory.join(LinkCommand::MANIFEST).is_file())
    }
}

impl Method {
    const fn name(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::HardLink => "hardlink",
            Self::Copy => "copy",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Symlink, Self::HardLink, Self::Copy]
            .into_iter()
            .find(|method| method.name() == name)
    }
}

impl Placer {
    #[cfg(unix)]
    fn system_symlink(artifact: &Path, placement: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(artifact, placement)
    }

    #[cfg(windows)]
    fn system_symlink(artifact: &Path, placement: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(artifact, placement)
    }

    #[cfg(not(any(unix, windows)))]
    fn system_symlink(_artifact: &Path, _placement: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

impl Record {
    fn path(placement: &Path) -> PathBuf {
        let mut name = placement.as_os_str().to_owned();
        name.push(".");
        name.push(LinkCommand::RECORD_SUFFIX);
        PathBuf::from(name)
    }

    fn read(placement: &Path) -> Option<Self> {
        let record: Value =
            serde_json::from_str(&fs::read_to_string(Self::path(placement)).ok()?).ok()?;
        Some(Self {
            artifact: PathBuf::from(record["artifact"].as_str()?),
            sha256: record["sha256"].as_str()?.to_owned(),
            method: Method::parse(record["method"].as_str()?)?,
        })
    }

    fn write(&self, placement: &Path) -> io::Result<()> {
        let record = json!({
            "artifact": self.artifact.display().to_string(),
            "sha256": self.sha256,
            "method": self.method.name(),
        });
        fs::write(Self::path(placement), format!("{record}\n"))
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

    fn current(&self) -> Current {
        let Ok(metadata) = fs::symlink_metadata(&self.placement) else {
            return Current::Missing;
        };
        if let Some(hook) = &self.hook {
            return Self::hook_state(&self.placement, hook);
        }
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&self.placement).unwrap_or_default();
            let target = if target.is_absolute() {
                target
            } else {
                self.placement
                    .parent()
                    .map_or_else(|| target.clone(), |parent| parent.join(&target))
            };
            return if Artifacts::owns(&target) {
                Current::OwnedSymlink(target)
            } else {
                Current::ForeignSymlink(target)
            };
        }
        Record::read(&self.placement).map_or(Current::ForeignFile, Current::OwnedFile)
    }

    /// An import-hook placement: FYLO's when it starts with the marker.
    fn hook_state(placement: &Path, hook: &str) -> Current {
        match fs::read_to_string(placement) {
            Ok(text) if text == hook => Current::Hook(true),
            Ok(text) if text.starts_with(LinkCommand::PYTHON_HOOK_MARKER) => Current::Hook(false),
            _ => Current::ForeignFile,
        }
    }

    /// Whether a symlink's target is this SDK's artifact. The same path can
    /// come back in another form (on Windows `\\?\C:\…` for `C:\…`), so
    /// both sides are compared resolved.
    fn is_artifact(&self, target: &Path) -> bool {
        target == self.artifact
            || fs::canonicalize(target).is_ok_and(|resolved| {
                fs::canonicalize(&self.artifact).is_ok_and(|built| resolved == built)
            })
    }

    /// Whether the placement is this SDK's artifact, byte for byte.
    fn matches_artifact(&self) -> bool {
        match (
            LinkCommand::sha256(&self.placement),
            LinkCommand::sha256(&self.artifact),
        ) {
            (Ok(placed), Ok(built)) => placed == built,
            _ => false,
        }
    }

    fn remove(&self) -> io::Result<()> {
        fs::remove_file(&self.placement)?;
        match fs::remove_file(Record::path(&self.placement)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn in_use_hint(error: &io::Error) -> &'static str {
        if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied {
            " (a running process may have it loaded; stop it and try again)"
        } else {
            ""
        }
    }

    fn link(&self, placer: Placer, report: &mut Vec<String>, problems: &mut Vec<String>) {
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
        let up_to_date = match self.current() {
            Current::Hook(current) => current,
            Current::OwnedSymlink(target) => self.is_artifact(&target),
            Current::OwnedFile(record) => {
                record.artifact == self.artifact && self.matches_artifact()
            }
            Current::ForeignSymlink(_) | Current::ForeignFile => {
                problems.push(format!(
                    "{}: {} is something FYLO did not place; remove it first",
                    self.name,
                    self.placement.display()
                ));
                return;
            }
            Current::Missing => false,
        };
        if up_to_date {
            report.push(format!(
                "{}: already linked ({})",
                self.name,
                self.placement.display()
            ));
            report.extend(self.advice.iter().cloned());
            return;
        }
        if !matches!(self.current(), Current::Missing)
            && let Err(error) = self.remove()
        {
            problems.push(format!(
                "{}: cannot replace {}: {error}{}",
                self.name,
                self.placement.display(),
                Self::in_use_hint(&error)
            ));
            return;
        }
        if let Some(hook) = &self.hook {
            self.write_hook(hook, report, problems);
            return;
        }
        let mut failures = Vec::new();
        for method in placer.methods {
            match self.place(*method, placer) {
                Ok(()) => {
                    report.push(format!(
                        "{}: linked {} -> {} ({}{})",
                        self.name,
                        self.placement.display(),
                        self.artifact.display(),
                        method.name(),
                        if failures.is_empty() {
                            String::new()
                        } else {
                            format!(", after: {}", failures.join("; "))
                        }
                    ));
                    report.extend(self.advice.iter().cloned());
                    return;
                }
                Err(error) => failures.push(format!("{} failed: {error}", method.name())),
            }
        }
        problems.push(format!(
            "{}: cannot link {}: {}",
            self.name,
            self.placement.display(),
            failures.join("; ")
        ));
    }

    fn write_hook(&self, hook: &str, report: &mut Vec<String>, problems: &mut Vec<String>) {
        match fs::write(&self.placement, hook) {
            Ok(()) => {
                report.push(format!(
                    "{}: linked {} -> {} (import hook)",
                    self.name,
                    self.placement.display(),
                    self.artifact.display()
                ));
                report.extend(self.advice.iter().cloned());
            }
            Err(error) => problems.push(format!(
                "{}: cannot write {}: {error}",
                self.name,
                self.placement.display()
            )),
        }
    }

    fn place(&self, method: Method, placer: Placer) -> io::Result<()> {
        match method {
            Method::Symlink => return (placer.symlink)(&self.artifact, &self.placement),
            Method::HardLink => (placer.hard_link)(&self.artifact, &self.placement)?,
            Method::Copy => {
                fs::copy(&self.artifact, &self.placement)?;
            }
        }
        let sha256 = LinkCommand::sha256(&self.artifact)?;
        if LinkCommand::sha256(&self.placement)? != sha256 {
            let _ = fs::remove_file(&self.placement);
            return Err(io::Error::other(
                "the placed file does not hash to the artifact",
            ));
        }
        Record {
            artifact: self.artifact.clone(),
            sha256,
            method,
        }
        .write(&self.placement)
    }

    fn check(&self, report: &mut Vec<String>, problems: &mut Vec<String>) {
        if let Some(problem) = self.version_problem() {
            problems.push(problem);
        }
        match self.current() {
            Current::Hook(true) if self.artifact.is_file() => {
                report.push(format!(
                    "{}: linked (import hook), version {}",
                    self.name, self.installed
                ));
            }
            Current::Hook(_) => {
                problems.push(format!(
                    "{}: {} does not load {}; run fylo link",
                    self.name,
                    self.placement.display(),
                    self.artifact.display()
                ));
            }
            Current::OwnedSymlink(target) if self.is_artifact(&target) && target.is_file() => {
                report.push(format!(
                    "{}: linked (symlink), version {}",
                    self.name, self.installed
                ));
            }
            Current::OwnedFile(record)
                if record.artifact == self.artifact && self.matches_artifact() =>
            {
                report.push(format!(
                    "{}: linked ({}, SHA-256 verified), version {}",
                    self.name,
                    record.method.name(),
                    self.installed
                ));
            }
            Current::OwnedFile(record) => {
                problems.push(format!(
                    "{}: {} ({}) no longer hashes to {}; run fylo link",
                    self.name,
                    self.placement.display(),
                    record.method.name(),
                    self.artifact.display()
                ));
            }
            Current::OwnedSymlink(target) | Current::ForeignSymlink(target) => {
                problems.push(format!(
                    "{}: {} points at {}, not {}; run fylo link",
                    self.name,
                    self.placement.display(),
                    target.display(),
                    self.artifact.display()
                ));
            }
            Current::ForeignFile => {
                problems.push(format!(
                    "{}: {} is a file FYLO did not place",
                    self.name,
                    self.placement.display()
                ));
            }
            Current::Missing => {
                problems.push(format!(
                    "{}: not linked ({} is missing); run fylo link",
                    self.name,
                    self.placement.display()
                ));
            }
        }
    }

    fn undo(&self, report: &mut Vec<String>, problems: &mut Vec<String>) {
        let removable = match self.current() {
            Current::Hook(_) | Current::OwnedSymlink(_) => true,
            Current::OwnedFile(record) => {
                let unchanged = LinkCommand::sha256(&self.placement)
                    .is_ok_and(|placed| placed == record.sha256)
                    || self.matches_artifact();
                if !unchanged {
                    report.push(format!(
                        "{}: left {} alone: it changed since fylo link placed it",
                        self.name,
                        self.placement.display()
                    ));
                }
                unchanged
            }
            Current::ForeignSymlink(_) | Current::ForeignFile => {
                report.push(format!(
                    "{}: left {} alone (FYLO did not place it)",
                    self.name,
                    self.placement.display()
                ));
                false
            }
            Current::Missing => {
                report.push(format!("{}: nothing to undo", self.name));
                false
            }
        };
        if !removable {
            return;
        }
        match self.remove() {
            Ok(()) => report.push(format!(
                "{}: removed {}",
                self.name,
                self.placement.display()
            )),
            Err(error) => problems.push(format!(
                "{}: cannot remove {}: {error}{}",
                self.name,
                self.placement.display(),
                Self::in_use_hint(&error)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{LinkCommand, Method, Placer, Record};

    struct Fixture {
        base: PathBuf,
        artifacts: PathBuf,
        project: PathBuf,
        triple: &'static str,
    }

    impl Fixture {
        /// FYLO artifacts for Turso 0.8.3 on `triple`, and a project with
        /// `@tursodatabase/database` at `js` and `tursogo` at `go`.
        fn on(triple: &'static str, js: Option<&str>, go: Option<&str>) -> Self {
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
            let fixture = Self {
                base,
                artifacts,
                project,
                triple,
            };
            fs::write(fixture.artifact_node(), "node").unwrap();
            fs::write(fixture.artifacts.join(fixture.library()), "library").unwrap();
            fs::write(fixture.artifacts.join("_turso.abi3.so"), "python").unwrap();
            fs::write(fixture.artifacts.join("lib_turso_java.dylib"), "java").unwrap();
            fixture.pin();
            if let Some(version) = js {
                let package = fixture.project.join("node_modules/@tursodatabase/database");
                fs::create_dir_all(&package).unwrap();
                fs::write(
                    package.join("package.json"),
                    format!(r#"{{"name":"@tursodatabase/database","version":"{version}"}}"#),
                )
                .unwrap();
            }
            if let Some(version) = go {
                fs::write(
                    fixture.project.join("go.mod"),
                    format!(
                        "module app\n\nrequire (\n\tturso.tech/database/tursogo {version}\n)\n"
                    ),
                )
                .unwrap();
            }
            fixture
        }

        /// Write `fylo-link.json` with the SHA-256 of every artifact, as
        /// `build.sh` does.
        fn pin(&self) {
            let mut pinned = serde_json::Map::new();
            for entry in fs::read_dir(&self.artifacts).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name != "fylo-link.json" {
                    pinned.insert(name, LinkCommand::sha256(&entry.path()).unwrap().into());
                }
            }
            let manifest =
                serde_json::json!({"turso": "0.8.3", "triple": self.triple, "sha256": pinned});
            fs::write(self.artifacts.join("fylo-link.json"), manifest.to_string()).unwrap();
        }

        fn new(js: Option<&str>, go: Option<&str>) -> Self {
            Self::on("darwin-arm64", js, go)
        }

        fn library(&self) -> &'static str {
            match self.triple {
                "win32-arm64-msvc" | "win32-x64-msvc" => "turso_sync_sdk_kit.dll",
                "darwin-arm64" => "libturso_sync_sdk_kit.dylib",
                _ => "libturso_sync_sdk_kit.so",
            }
        }

        fn arguments(&self, flag: Option<&str>) -> Vec<String> {
            let mut arguments: Vec<String> = flag.iter().map(ToString::to_string).collect();
            arguments.extend([
                "--project".to_owned(),
                self.project.display().to_string(),
                "--artifacts".to_owned(),
                self.artifacts.display().to_string(),
            ]);
            arguments
        }

        fn run(&self, flag: Option<&str>) -> Result<String, String> {
            LinkCommand::run(&self.arguments(flag))
        }

        fn run_with(&self, flag: Option<&str>, placer: Placer) -> Result<String, String> {
            LinkCommand::run_with(&self.arguments(flag), placer)
        }

        fn artifact_node(&self) -> PathBuf {
            self.artifacts.join(format!("turso.{}.node", self.triple))
        }

        fn node(&self) -> PathBuf {
            self.project.join(format!(
                "node_modules/@tursodatabase/database/turso.{}.node",
                self.triple
            ))
        }

        fn dylib(&self) -> PathBuf {
            self.project.join(self.library())
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

    /// Windows without Developer Mode: `ERROR_PRIVILEGE_NOT_HELD`.
    fn symlinks_not_permitted(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "A required privilege is not held by the client. (os error 1314)",
        ))
    }

    /// Another volume: `ERROR_NOT_SAME_DEVICE`.
    fn hard_links_across_volumes(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "The system cannot move the file to a different disk drive. (os error 17)",
        ))
    }

    fn windows_like(hard_link: fn(&Path, &Path) -> io::Result<()>) -> Placer {
        Placer {
            methods: &[Method::Symlink, Method::HardLink, Method::Copy],
            symlink: symlinks_not_permitted,
            hard_link,
        }
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
    fn without_symlink_rights_auto_falls_back_to_a_hard_link_with_a_record() {
        let fixture = Fixture::new(Some("0.8.3"), Some("v0.8.3"));
        let linked = fixture
            .run_with(
                None,
                windows_like(|artifact, placement| fs::hard_link(artifact, placement)),
            )
            .unwrap();
        assert!(
            linked.contains("(hardlink, after: symlink failed"),
            "{linked}"
        );
        assert!(!is_symlink(&fixture.node()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(fixture.node()).unwrap().ino(),
                fs::metadata(fixture.artifact_node()).unwrap().ino(),
                "a hard link shares the artifact's file"
            );
        }
        assert!(Record::path(&fixture.node()).is_file());
        let checked = fixture.run(Some("--check")).unwrap();
        assert!(checked.contains("hardlink, SHA-256 verified"), "{checked}");
        assert!(
            fixture
                .run_with(
                    None,
                    windows_like(|artifact, placement| fs::hard_link(artifact, placement))
                )
                .unwrap()
                .contains("already linked")
        );
        fixture.run(Some("--undo")).unwrap();
        assert!(!fixture.node().exists() && !Record::path(&fixture.node()).exists());
        assert!(
            fixture.artifact_node().is_file(),
            "undo never touches the artifact"
        );
    }

    #[test]
    fn across_volumes_auto_falls_back_to_a_copy_that_check_hashes() {
        let fixture = Fixture::on("win32-arm64-msvc", Some("0.8.3"), Some("v0.8.3"));
        let linked = fixture
            .run_with(None, windows_like(hard_links_across_volumes))
            .unwrap();
        assert!(linked.contains("(copy, after: symlink failed"), "{linked}");
        assert!(linked.contains("hardlink failed"), "{linked}");
        assert!(linked.contains("put it on PATH"), "{linked}");
        assert_eq!(fs::read_to_string(fixture.dylib()).unwrap(), "library");
        assert!(fixture.dylib().ends_with("turso_sync_sdk_kit.dll"));
        fixture.run(Some("--check")).unwrap();
        fs::write(fixture.artifact_node(), "rebuilt node").unwrap();
        fixture.pin();
        let stale = fixture.run(Some("--check")).unwrap_err();
        assert!(stale.contains("no longer hashes"), "{stale}");
        fixture
            .run_with(None, windows_like(hard_links_across_volumes))
            .unwrap();
        assert_eq!(fs::read_to_string(fixture.node()).unwrap(), "rebuilt node");
        fs::write(fixture.node(), "edited by hand").unwrap();
        let undone = fixture.run(Some("--undo")).unwrap();
        assert!(
            undone.contains("changed since fylo link placed it"),
            "{undone}"
        );
        assert_eq!(
            fs::read_to_string(fixture.node()).unwrap(),
            "edited by hand"
        );
        assert!(!fixture.dylib().exists(), "the unchanged copy is removed");
    }

    #[test]
    fn an_explicit_method_is_used_alone() {
        let fixture = Fixture::new(Some("0.8.3"), None);
        fixture.run(Some("--method")).unwrap_err();
        let copied = LinkCommand::run(
            &[
                fixture.arguments(None),
                vec!["--method".into(), "copy".into()],
            ]
            .concat(),
        )
        .unwrap();
        assert!(copied.contains("(copy)"), "{copied}");
        fixture.run(Some("--undo")).unwrap();
        let refused = fixture
            .run_with(
                None,
                Placer {
                    methods: &[Method::Symlink],
                    symlink: symlinks_not_permitted,
                    hard_link: hard_links_across_volumes,
                },
            )
            .unwrap_err();
        assert!(refused.contains("cannot link"), "{refused}");
        assert!(!fixture.node().exists());
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
    #[cfg(unix)]
    fn a_symlink_to_the_artifact_written_another_way_is_recognized() {
        let fixture = Fixture::new(Some("0.8.3"), None);
        let roundabout = fixture
            .artifacts
            .join("..")
            .join("artifacts")
            .join("turso.darwin-arm64.node");
        std::os::unix::fs::symlink(&roundabout, fixture.node()).unwrap();
        assert!(fixture.run(None).unwrap().contains("already linked"));
        fixture.run(Some("--check")).unwrap();
        fixture.run(Some("--undo")).unwrap();
        assert!(!is_symlink(&fixture.node()));
    }

    #[test]
    fn on_linux_go_is_told_the_absolute_ld_library_path() {
        let fixture = Fixture::on("linux-arm64-gnu", None, Some("v0.8.3"));
        let linked = fixture.run(None).unwrap();
        let directory = fixture.project.display().to_string();
        assert!(
            linked.contains(&format!("LD_LIBRARY_PATH={directory} first")),
            "{linked}"
        );
        assert!(
            linked.contains(&format!("os.Setenv(\"LD_LIBRARY_PATH\", {directory:?})")),
            "{linked}"
        );
        assert!(fixture.dylib().ends_with("libturso_sync_sdk_kit.so"));
        fixture.run(Some("--undo")).unwrap();
    }

    fn site_packages(fixture: &Fixture, version: &str) -> PathBuf {
        let site_packages = fixture.project.join(".venv/lib/python3.14/site-packages");
        fs::create_dir_all(site_packages.join(format!("pyturso-{version}.dist-info"))).unwrap();
        site_packages
    }

    #[test]
    fn pyturso_gets_an_import_hook_that_check_verifies_and_undo_removes() {
        let fixture = Fixture::new(None, None);
        let hook = site_packages(&fixture, "0.8.3").join("fylo_turso.pth");
        let linked = fixture.run(None).unwrap();
        assert!(linked.contains("pyturso: linked"), "{linked}");
        assert!(linked.contains("(import hook)"), "{linked}");
        let text = fs::read_to_string(&hook).unwrap();
        assert!(text.starts_with("# fylo link (ADR 0019)"), "{text}");
        assert!(text.contains("\nimport sys; "), "{text}");
        assert!(text.contains("\"turso._turso\""), "{text}");
        assert!(
            text.contains(&format!(
                "{:?}",
                fs::canonicalize(&fixture.artifacts)
                    .unwrap()
                    .join("_turso.abi3.so")
                    .display()
                    .to_string()
            )),
            "{text}"
        );
        assert!(fixture.run(None).unwrap().contains("already linked"));
        let checked = fixture.run(Some("--check")).unwrap();
        assert!(
            checked.contains("linked (import hook), version 0.8.3"),
            "{checked}"
        );
        fixture.run(Some("--undo")).unwrap();
        assert!(!hook.exists());
        assert!(fixture.run(Some("--check")).is_err());
    }

    #[test]
    fn a_pth_file_fylo_did_not_write_is_refused_and_a_stale_hook_is_replaced() {
        let fixture = Fixture::new(None, None);
        let hook = site_packages(&fixture, "0.8.3").join("fylo_turso.pth");
        fs::write(&hook, "import something_else\n").unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("FYLO did not place"), "{refused}");
        fs::write(&hook, "# fylo link (ADR 0019): an older build\n").unwrap();
        let stale = fixture.run(Some("--check")).unwrap_err();
        assert!(stale.contains("does not load"), "{stale}");
        fixture.run(None).unwrap();
        fixture.run(Some("--check")).unwrap();
    }

    #[test]
    fn a_pyturso_version_mismatch_is_refused_with_the_install_command() {
        let fixture = Fixture::new(None, None);
        site_packages(&fixture, "0.8.2");
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("pip install pyturso==0.8.3"), "{refused}");
    }

    #[test]
    fn the_java_driver_from_gradle_gets_its_jni_library_in_the_project() {
        let fixture = Fixture::new(None, None);
        fs::write(
            fixture.project.join("build.gradle.kts"),
            "dependencies {\n    implementation(\"tech.turso:turso:0.8.3\")\n}\n",
        )
        .unwrap();
        let linked = fixture.run(None).unwrap();
        assert!(linked.contains("tech.turso:turso: linked"), "{linked}");
        assert!(
            linked.contains(&format!(
                "-Djava.library.path={}",
                fixture.project.display()
            )),
            "{linked}"
        );
        assert!(is_symlink(&fixture.project.join("lib_turso_java.dylib")));
        fixture.run(Some("--check")).unwrap();
        fixture.run(Some("--undo")).unwrap();
        assert!(!fixture.project.join("lib_turso_java.dylib").exists());
    }

    #[test]
    fn the_java_driver_version_is_read_from_a_pom() {
        let fixture = Fixture::new(None, None);
        fs::write(
            fixture.project.join("pom.xml"),
            "<project><dependencies><dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId><version>2.0.17</version></dependency>\n<dependency>\n  <groupId>tech.turso</groupId>\n  <artifactId>turso</artifactId>\n  <version> 0.8.2 </version>\n</dependency></dependencies></project>\n",
        )
        .unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("version 0.8.2 is installed"), "{refused}");
        assert!(refused.contains("tech.turso:turso:0.8.3"), "{refused}");
    }

    #[test]
    fn a_dotnet_project_gets_the_sdk_kit_as_turso_sdk_kit_and_the_resolver_line() {
        let fixture = Fixture::new(None, None);
        fs::write(
            fixture.project.join("App.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup>\n  <PackageReference Include=\"Turso.Data.Sqlite.Provider\" Version=\"0.8.3\" />\n</ItemGroup></Project>\n",
        )
        .unwrap();
        let linked = fixture.run(None).unwrap();
        assert!(
            linked.contains("Turso.Data.Sqlite.Provider: linked"),
            "{linked}"
        );
        assert!(
            linked.contains("SetDllImportResolver(Assembly.Load(\"Turso.Raw\")"),
            "{linked}"
        );
        let placement = fixture.project.join("libturso_sdk_kit.dylib");
        assert_eq!(fs::read_to_string(&placement).unwrap(), "library");
        assert!(
            linked.contains(&format!(
                "NativeLibrary.Load({:?})",
                placement.display().to_string()
            )),
            "{linked}"
        );
        fixture.run(Some("--undo")).unwrap();
        assert!(!placement.exists());
    }

    #[test]
    fn an_artifact_changed_after_the_build_is_refused_by_link_and_check() {
        let fixture = Fixture::new(Some("0.8.3"), None);
        fixture.run(None).unwrap();
        fs::write(fixture.artifact_node(), "swapped").unwrap();
        let checked = fixture.run(Some("--check")).unwrap_err();
        assert!(checked.contains("changed after the build"), "{checked}");
        fixture.run(Some("--undo")).unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("changed after the build"), "{refused}");
        assert!(!fixture.node().exists());
    }

    #[test]
    fn an_artifact_without_a_pinned_hash_is_refused() {
        let fixture = Fixture::new(Some("0.8.3"), None);
        fs::write(
            fixture.artifacts.join("fylo-link.json"),
            r#"{"turso":"0.8.3","triple":"darwin-arm64"}"#,
        )
        .unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("pins no SHA-256"), "{refused}");
    }

    #[test]
    #[cfg(unix)]
    fn artifacts_other_users_can_write_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new(Some("0.8.3"), None);
        fs::set_permissions(fixture.artifact_node(), fs::Permissions::from_mode(0o666)).unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("writable by other users"), "{refused}");
        fs::set_permissions(fixture.artifact_node(), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&fixture.artifacts, fs::Permissions::from_mode(0o777)).unwrap();
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("writable by other users"), "{refused}");
        fs::set_permissions(&fixture.artifacts, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_project_without_a_turso_sdk_is_reported() {
        let fixture = Fixture::new(None, None);
        let refused = fixture.run(None).unwrap_err();
        assert!(refused.contains("no Turso SDK found"), "{refused}");
    }
}
