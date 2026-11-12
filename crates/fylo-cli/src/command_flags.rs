use crate::pack_command::PackCommand;
use crate::segment_command::SegmentCommand;

/// The options one `fylo` subcommand reads: options that take a value and
/// options that stand alone.
///
/// An option a subcommand does not read, or a value option given without its
/// value, is a configuration error, so a misspelled or unsupported option is
/// never ignored. `fylo identity` checks its own arguments, and an unknown
/// subcommand gets the usage text from the dispatcher.
pub struct CommandFlags {
    command: String,
    values: Vec<&'static str>,
    switches: Vec<&'static str>,
}

impl CommandFlags {
    const HOST_VALUES: [&'static str; 8] = [
        "--config",
        "--filesystem-profile",
        "--root",
        "--schema-dir",
        "--shard-width",
        "--hot-set-bytes",
        "--identity",
        "--trust-anchor",
    ];
    const HOST_SWITCHES: [&'static str; 5] = [
        "--strict-schema",
        "--trust-on-first-use",
        "--worm",
        "--help",
        "-h",
    ];
    const OUTPUT_SWITCHES: [&'static str; 3] = ["--json", "--no-pager", "--version"];
    const EXEC_VALUES: [&'static str; 5] = [
        "--request",
        "--deadline-ms",
        "--cleanup-reserve-ms",
        "--max-request-bytes",
        "--max-response-bytes",
    ];
    const EXEC_SWITCHES: [&'static str; 3] = ["--invocation", "--loop", "--exclusive-root"];
    const VERSION_VALUES: [&'static str; 1] = ["--output"];
    const ACCESS_VALUES: [&'static str; 6] = [
        "--grantee",
        "--key",
        "--actions",
        "--rule",
        "--label",
        "--collection",
    ];
    const ACCESS_SWITCHES: [&'static str; 2] = ["--all-collections", "--database"];
    const LINK_VALUES: [&'static str; 3] = ["--method", "--project", "--artifacts"];
    const LINK_SWITCHES: [&'static str; 4] = ["--check", "--undo", "--help", "-h"];
    const PACK_VALUES: [&'static str; 5] =
        ["--out", "--admin", "--grants", "--collection", "--new-key"];
    const PACK_SWITCHES: [&'static str; 7] = [
        "--sealed",
        "--encrypt",
        "--unprotected",
        "--protect",
        "--drop-record-access",
        "--keep-record-access-as-metadata",
        "--decrypt",
    ];
    const SEGMENT_VALUES: [&'static str; 2] = ["--to", "--out"];
    const SEGMENT_SWITCHES: [&'static str; 1] = ["--shrink"];
    const DOCUMENT_VALUES: [&'static str; 12] = [
        "--limit",
        "--collection",
        "--id",
        "--queries",
        "--query",
        "--statement",
        "-m",
        "--message",
        "--width",
        "--page-size",
        "--align",
        "--out",
    ];
    const DOCUMENT_SWITCHES: [&'static str; 4] =
        ["-b", "--force", "--id-only", "--include-unversioned"];
    const DOCUMENT_COMMANDS: [&'static str; 26] = [
        "log",
        "verify-history",
        "inspect",
        "get",
        "get-file",
        "get-deleted",
        "get-deleted-file",
        "scan-index",
        "verify-index",
        "find",
        "sql",
        "checkout",
        "branch",
        "commit",
        "status",
        "diff",
        "restore-commit",
        "merge",
        "latest",
        "rebuild",
        "reshard",
        "verify",
        "deleted",
        "restore",
        "schema",
        "index",
    ];

    /// Refuse an option the subcommand does not read, and a value option
    /// with no value after it.
    ///
    /// # Errors
    /// Names the first such option.
    pub fn check(arguments: &[String]) -> Result<(), String> {
        let flags = Self::for_arguments(arguments);
        flags.map_or(Ok(()), |flags| flags.check_all(arguments))
    }

    fn for_arguments(arguments: &[String]) -> Option<Self> {
        let command = arguments.first().map_or("", String::as_str);
        let (values, switches) = Self::options_of(command)?;
        Some(Self {
            command: command.to_owned(),
            values,
            switches,
        })
    }

    fn options_of(command: &str) -> Option<(Vec<&'static str>, Vec<&'static str>)> {
        match command {
            "exec" => Some(Self::host_options(&Self::EXEC_VALUES, &Self::EXEC_SWITCHES)),
            "version" => Some(Self::output_options(&Self::VERSION_VALUES, &[])),
            "access" => Some(Self::output_options(
                &Self::ACCESS_VALUES,
                &Self::ACCESS_SWITCHES,
            )),
            "link" => Some((Self::LINK_VALUES.to_vec(), Self::LINK_SWITCHES.to_vec())),
            name if PackCommand::COMMANDS.contains(&name) => Some(Self::output_options(
                &Self::PACK_VALUES,
                &Self::PACK_SWITCHES,
            )),
            name if SegmentCommand::COMMANDS.contains(&name) => Some(Self::output_options(
                &Self::SEGMENT_VALUES,
                &Self::SEGMENT_SWITCHES,
            )),
            name if Self::DOCUMENT_COMMANDS.contains(&name) || super::is_sql_command(name) => Some(
                Self::output_options(&Self::DOCUMENT_VALUES, &Self::DOCUMENT_SWITCHES),
            ),
            _ => None,
        }
    }

    fn host_options(
        values: &[&'static str],
        switches: &[&'static str],
    ) -> (Vec<&'static str>, Vec<&'static str>) {
        (
            [&Self::HOST_VALUES[..], values].concat(),
            [&Self::HOST_SWITCHES[..], switches].concat(),
        )
    }

    fn output_options(
        values: &[&'static str],
        switches: &[&'static str],
    ) -> (Vec<&'static str>, Vec<&'static str>) {
        Self::host_options(values, &[&Self::OUTPUT_SWITCHES[..], switches].concat())
    }

    fn check_all(&self, arguments: &[String]) -> Result<(), String> {
        let mut index = 1;
        while index < arguments.len() {
            index += self.step(arguments, index)?;
        }
        Ok(())
    }

    /// How many arguments the option at `index` uses: two for an option and
    /// its value, one for a switch or a positional value.
    fn step(&self, arguments: &[String], index: usize) -> Result<usize, String> {
        let argument = arguments[index].as_str();
        match argument {
            _ if self.values.contains(&argument) => Self::value_step(arguments, index),
            _ if self.switches.contains(&argument) => Ok(1),
            _ if Self::is_option(argument) => Err(self.unknown(argument)),
            _ => Ok(1),
        }
    }

    fn value_step(arguments: &[String], index: usize) -> Result<usize, String> {
        arguments
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .map(|_| 2)
            .ok_or_else(|| format!("missing value for {}", arguments[index]))
    }

    fn is_option(argument: &str) -> bool {
        argument.starts_with('-') && argument != "-"
    }

    fn unknown(&self, argument: &str) -> String {
        format!(
            "unknown option {argument} for fylo {}; run fylo --help to see the options",
            self.command
        )
    }
}
