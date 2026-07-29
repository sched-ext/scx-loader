// SPDX-License-Identifier: GPL-2.0

//! Custom scheduler-argument expansion.
//!
//! Deliberately reproduces scxctl's `--args` pipeline end to end, so a
//! string typed into the TUI field means exactly what the same string
//! means on the scxctl command line: the raw input is first split on
//! commas (mirroring clap's `value_delimiter(',')`, kept for the
//! historical comma-separated format), each chunk is then shell-split
//! via `shell-words`, and the results are flattened in order.
//!
//! There is deliberately no shared helper crate between the two clients;
//! the semantics are pinned instead by a test vector copied verbatim in
//! both directions (see the note on the shared tables below).

/// Why the field input failed to expand into scheduler arguments.
/// Mirrors scxctl's `ArgsExpandError`.
#[derive(Debug, PartialEq)]
pub enum ArgsExpandError {
    /// A chunk failed shell-style parsing, e.g. an unclosed quote. This
    /// also covers quotes that span a comma: the comma split happens
    /// before quoting is interpreted, so each side of the comma arrives
    /// here as its own unbalanced chunk. The payload is a display-ready
    /// message.
    Parse(String),
    /// The input expanded to zero arguments (e.g. whitespace only).
    /// Passing an empty argument list to `StartSchedulerWithArgs` would
    /// silently mean something other than what the user typed, so the
    /// client rejects it instead of forwarding it to the daemon.
    Empty,
}

/// Expands the raw field input into the final argument list passed to
/// `scx_loader`: comma split first (clap-compatible), then shell-split
/// per chunk, flattened — scxctl's pipeline, reproduced 1:1.
pub fn expand_input(input: &str) -> Result<Vec<String>, ArgsExpandError> {
    let chunks: Vec<String> = input.split(',').map(str::to_owned).collect();
    expand_scheduler_args(&chunks)
}

/// The chunk expansion itself, identical to scxctl's
/// `expand_scheduler_args`; kept as a separate function so the shared
/// test vector (whose inputs are pre-split chunks) applies verbatim.
fn expand_scheduler_args(raw: &[String]) -> Result<Vec<String>, ArgsExpandError> {
    let mut expanded = Vec::new();
    for chunk in raw {
        let tokens = shell_words::split(chunk)
            .map_err(|err| ArgsExpandError::Parse(format!("{err} in '{chunk}'")))?;
        expanded.extend(tokens);
    }
    if expanded.is_empty() {
        return Err(ArgsExpandError::Empty);
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /*
     * Shared semantics vector, copied verbatim from scxctl's
     * `args_expansion_shared_vector_ok` / `_errors`. Both clients must
     * agree on these outcomes; there is deliberately no shared helper
     * crate. When touching this table, copy it verbatim into the scxctl
     * tests (and vice versa).
     *
     * Inputs are the chunks as produced by clap's value_delimiter(','),
     * i.e. already comma-split.
     */
    #[test]
    fn args_expansion_shared_vector_ok() {
        let cases: &[(&[&str], &[&str])] = &[
            // Comma style (the historical documented format): still supported.
            (&["--slice-us", "5000"], &["--slice-us", "5000"]),
            // Whitespace inside a single chunk now separates arguments.
            (
                &["--verbose --slice-us 5000"],
                &["--verbose", "--slice-us", "5000"],
            ),
            // Mixed: comma split first (clap), then shell split per chunk.
            (
                &["--verbose", "--slice-us 5000"],
                &["--verbose", "--slice-us", "5000"],
            ),
            // Double quotes are interpreted: value with a space stays one token.
            (&["--name \"foo bar\""], &["--name", "foo bar"]),
            // Single quotes likewise.
            (&["--name 'foo bar'"], &["--name", "foo bar"]),
            // Backslash escapes a space.
            (&["--path /tmp/a\\ b"], &["--path", "/tmp/a b"]),
            // An explicit empty token is explicit: passed through as-is.
            (&["\"\""], &[""]),
            // Empty values remain visible when attached to another option.
            (&["--name \"\""], &["--name", ""]),
        ];
        for (input, expected) in cases {
            let input: Vec<String> = input.iter().map(ToString::to_string).collect();
            let expected: Vec<String> = expected.iter().map(ToString::to_string).collect();
            assert_eq!(
                expand_scheduler_args(&input),
                Ok(expected),
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn args_expansion_shared_vector_errors() {
        // An unclosed quote in a chunk is a parse error.
        let unclosed = vec!["--name \"foo".to_string()];
        assert!(matches!(
            expand_scheduler_args(&unclosed),
            Err(ArgsExpandError::Parse(_))
        ));

        // A quoted region spanning a comma cannot survive clap's earlier
        // comma split: each side arrives as an unbalanced chunk. Surfacing
        // a parse error here (instead of silently mangled tokens) is the
        // intended behavior.
        let quote_spanning_comma = vec!["\"foo".to_string(), "bar\"".to_string()];
        assert!(matches!(
            expand_scheduler_args(&quote_spanning_comma),
            Err(ArgsExpandError::Parse(_))
        ));

        // Whitespace-only input expands to nothing: rejected client-side
        // instead of sending an empty list to the daemon.
        let blank = vec!["   ".to_string()];
        assert_eq!(expand_scheduler_args(&blank), Err(ArgsExpandError::Empty));

        // clap turns `--args ""` into a single empty chunk; same outcome.
        let empty_chunk = vec![String::new()];
        assert_eq!(
            expand_scheduler_args(&empty_chunk),
            Err(ArgsExpandError::Empty)
        );
    }

    /// Full-pipeline checks on top of the shared vector: the exact string
    /// a user would pass to `scxctl --args=...` typed into the field
    /// yields the same final tokens, comma split included.
    #[test]
    fn field_input_matches_the_scxctl_pipeline() {
        assert_eq!(
            expand_input("-s 20000,-m powersave,-I 100,-t 100"),
            Ok(["-s", "20000", "-m", "powersave", "-I", "100", "-t", "100"]
                .map(str::to_string)
                .to_vec())
        );
        assert_eq!(
            expand_input("--verbose --slice-us 5000"),
            Ok(["--verbose", "--slice-us", "5000"]
                .map(str::to_string)
                .to_vec())
        );
        // A quoted region spanning a comma: parse error, as on the CLI.
        assert!(matches!(
            expand_input("--name \"foo,bar\""),
            Err(ArgsExpandError::Parse(_))
        ));
        // Whitespace-only and empty input: rejected client-side.
        assert_eq!(expand_input("   "), Err(ArgsExpandError::Empty));
        assert_eq!(expand_input(""), Err(ArgsExpandError::Empty));
    }
}
